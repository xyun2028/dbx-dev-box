//! 单个源的下载。只管"从这一个地址把字节拿下来"，
//! 换源重试的策略在 store 层，二者分开才好测。
//!
//! 支持断点续传：`dest` 已存在且非空时发 `Range` 请求，服务端回 206 就接着写。
//! 三个必须处理的分支：
//! - 服务端忽略 Range 回 200 → 从头覆写（不能把两段拼在一起）
//! - 回 416 → 说明本地那段已经失效，删掉重来
//! - 回 206 但 Content-Range 起点对不上 → 同样删掉重来

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

const USER_AGENT: &str = concat!("devbox/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// 拿到了 HTTP 状态码但非 2xx
    Status(u16),
    /// 连接层失败：DNS、TLS、超时、被重置
    Transport(String),
    Io(String),
}

impl FetchError {
    /// 映射成 `mirror::should_fallback` 需要的输入。
    pub fn status_code(&self) -> Option<u16> {
        match self {
            FetchError::Status(c) => Some(*c),
            _ => None,
        }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Status(c) => write!(f, "HTTP {c}"),
            FetchError::Transport(e) => write!(f, "连接失败: {e}"),
            FetchError::Io(e) => write!(f, "写入失败: {e}"),
        }
    }
}

impl std::error::Error for FetchError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadOutcome {
    /// 下载完成后文件的总字节数（不是本次新增的）
    pub bytes_total: u64,
    /// 本次从哪个偏移开始写；0 表示从头下的
    pub resumed_from: u64,
}

/// 解析 `Content-Range: bytes 1024-2047/9202456` 里的 (起点, 总长)。
pub fn parse_content_range(value: &str) -> Option<(u64, u64)> {
    let rest = value.trim().strip_prefix("bytes")?.trim();
    let (range, total) = rest.split_once('/')?;
    let (start, _end) = range.split_once('-')?;
    Some((start.trim().parse().ok()?, total.trim().parse().ok()?))
}

pub fn download_with_progress(
    url: &str,
    dest: &Path,
    on_progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<DownloadOutcome, FetchError> {
    if !url.starts_with("https://") {
        // 明文 http 一律拒绝：下载的是可执行内容，不能给中间人机会
        return Err(FetchError::Transport(format!("拒绝非 https 地址: {url}")));
    }

    let existing = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .user_agent(USER_AGENT)
        .build();

    let mut request = agent.get(url);
    if existing > 0 {
        request = request.set("Range", &format!("bytes={existing}-"));
    }

    match request.call() {
        Ok(resp) if resp.status() == 206 => {
            let header = resp.header("content-range").unwrap_or("").to_string();
            match parse_content_range(&header) {
                Some((start, total)) if start == existing => {
                    append_body(resp.into_reader(), dest, existing, Some(total), on_progress)
                }
                _ => {
                    // 起点对不上就拼不了，丢弃重来
                    std::fs::remove_file(dest).ok();
                    Err(FetchError::Transport(format!(
                        "Content-Range 不可用（{header:?}），已丢弃分片"
                    )))
                }
            }
        }
        Ok(resp) => {
            // 200：服务端忽略了 Range，只能从头覆写
            let total = resp
                .header("content-length")
                .and_then(|v| v.parse::<u64>().ok());
            overwrite_body(resp.into_reader(), dest, total, on_progress)
        }
        Err(ureq::Error::Status(416, _)) => {
            std::fs::remove_file(dest).ok();
            Err(FetchError::Transport(
                "本地分片已失效（HTTP 416），已丢弃".to_string(),
            ))
        }
        Err(ureq::Error::Status(code, _)) => Err(FetchError::Status(code)),
        Err(ureq::Error::Transport(t)) => Err(FetchError::Transport(t.to_string())),
    }
}

fn append_body(
    mut reader: impl io::Read,
    dest: &Path,
    existing: u64,
    total: Option<u64>,
    on_progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<DownloadOutcome, FetchError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| FetchError::Io(e.to_string()))?;
    }
    let mut file = OpenOptions::new()
        .append(true)
        .open(dest)
        .map_err(|e| FetchError::Io(e.to_string()))?;
    let written = pump(&mut reader, &mut file, existing, total, on_progress)?;
    Ok(DownloadOutcome {
        bytes_total: existing + written,
        resumed_from: existing,
    })
}

fn overwrite_body(
    mut reader: impl io::Read,
    dest: &Path,
    total: Option<u64>,
    on_progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<DownloadOutcome, FetchError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| FetchError::Io(e.to_string()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(dest)
        .map_err(|e| FetchError::Io(e.to_string()))?;
    let written = pump(&mut reader, &mut file, 0, total, on_progress)?;
    Ok(DownloadOutcome {
        bytes_total: written,
        resumed_from: 0,
    })
}

fn pump(
    reader: &mut impl io::Read,
    file: &mut impl Write,
    base: u64,
    total: Option<u64>,
    on_progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<u64, FetchError> {
    let mut buf = [0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| FetchError::Transport(e.to_string()))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| FetchError::Io(e.to_string()))?;
        written += n as u64;
        on_progress(base + written, total);
    }
    file.flush().map_err(|e| FetchError::Io(e.to_string()))?;
    if written == 0 && base == 0 {
        return Err(FetchError::Transport("响应为空".to_string()));
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_range() {
        assert_eq!(
            parse_content_range("bytes 1024-2047/9202456"),
            Some((1024, 9202456))
        );
        assert_eq!(parse_content_range("bytes 0-99/*"), None, "未知总长不接受");
        assert_eq!(parse_content_range("items 0-99/100"), None);
        assert_eq!(parse_content_range(""), None);
    }

    #[test]
    fn rejects_plain_http() {
        let dest = std::env::temp_dir().join("devbox-http-test.part");
        let err =
            download_with_progress("http://example.com/a.zip", &dest, &mut |_, _| {}).unwrap_err();
        assert!(matches!(err, FetchError::Transport(_)));
    }
}
