//! 安装事务：下载 → 校验 → 解压 → 原子提交。
//!
//! 顺序不能变。`.devbox-ok` 是唯一的"成功"标志，它必须在**改名之前**写进
//! staging 目录：先改名再打标记，中间崩一次就会留下"看起来装好了但缺文件"的
//! 假目录，比干脆失败更难排查。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use devbox_core::mirror::{rewrite_origin, should_fallback};
use devbox_core::READY_MARKER;

use crate::checksum::{verify_file, Checksum, VerifyError};
use crate::extract::{extract, ArchiveFormat, ExtractLimits};
use crate::fetch::download_with_progress;

/// 最多试几个源。再多说明问题不在源上，早点失败比慢慢失败好。
const MAX_SOURCE_ATTEMPTS: usize = 3;

#[derive(Debug, Clone)]
pub struct InstallSpec {
    pub component: String,
    pub version: String,
    pub platform: String,
    pub url: String,
    /// 纯 hex，或官方 checksum 文件的内容
    pub checksum: String,
    /// `zip` / `tar.gz`
    pub format: String,
    pub strip_components: usize,
    /// 用户配置的加速器在前，预设在后；原始地址永远作为最后兜底
    pub mirrors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub path: PathBuf,
    pub bytes: u64,
    pub source: String,
    pub files: usize,
    /// 已经装过，直接返回
    pub skipped: bool,
    /// 本次从哪个偏移续传；0 表示从头下的
    pub resumed_from: u64,
}

#[derive(Debug)]
pub enum InstallError {
    BadChecksum(String),
    BadFormat(String),
    NoSource,
    Fetch { url: String, reason: String },
    Verify { url: String, reason: String },
    Extract(String),
    Io(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::BadChecksum(e) => write!(f, "校验值不合法: {e}"),
            InstallError::BadFormat(name) => write!(f, "不支持的包格式: {name}"),
            InstallError::NoSource => write!(f, "所有下载源都不可用"),
            InstallError::Fetch { url, reason } => write!(f, "下载失败 {url}: {reason}"),
            InstallError::Verify { url, reason } => write!(f, "校验失败 {url}: {reason}"),
            InstallError::Extract(e) => write!(f, "解压失败: {e}"),
            InstallError::Io(e) => write!(f, "文件操作失败: {e}"),
        }
    }
}

impl std::error::Error for InstallError {}

/// 组装候选源：加速器在前，原始地址兜底，去重。
pub fn candidate_urls(url: &str, mirrors: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for mirror in mirrors {
        if let Ok(candidate) = rewrite_origin(url, mirror) {
            if candidate != url && !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }
    if !out.iter().any(|c| c == url) {
        out.push(url.to_string());
    }
    out
}

pub fn artifact_dir(data_root: &Path, spec: &InstallSpec) -> PathBuf {    data_root
        .join("store")
        .join(&spec.component)
        .join(&spec.version)
        .join(&spec.platform)
}

pub fn is_installed(dir: &Path) -> bool {
    dir.join(READY_MARKER).is_file()
}

pub fn install(
    spec: &InstallSpec,
    data_root: &Path,
    on_progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<InstallOutcome, InstallError> {
    let checksum = Checksum::parse(&spec.checksum)
        .map_err(|e| InstallError::BadChecksum(e.to_string()))?;
    let format =
        ArchiveFormat::parse(&spec.format).ok_or_else(|| InstallError::BadFormat(spec.format.clone()))?;

    let final_dir = artifact_dir(data_root, spec);
    if is_installed(&final_dir) {
        return Ok(InstallOutcome {
            path: final_dir,
            bytes: 0,
            source: "(already installed)".into(),
            files: 0,
            skipped: true,
            resumed_from: 0,
        });
    }
    // 没有标记的目录都是残骸（上次崩在中途），清掉重来
    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir).map_err(|e| InstallError::Io(e.to_string()))?;
    }

    let staging = final_dir.with_extension(format!("staging-{}", std::process::id()));
    std::fs::remove_dir_all(&staging).ok();

    let tmp_dir = data_root.join("tmp");
    std::fs::create_dir_all(&tmp_dir).map_err(|e| InstallError::Io(e.to_string()))?;
    let tmp_file = tmp_dir.join(format!(
        "{}-{}-{}.part",
        spec.component, spec.version, spec.platform
    ));

    let result = (|| -> Result<InstallOutcome, InstallError> {
        let candidates = candidate_urls(&spec.url, &spec.mirrors);
        if candidates.is_empty() {
            return Err(InstallError::NoSource);
        }

        let mut last_error: Option<String> = None;
        let mut last_verify: Option<(String, String)> = None;
        let mut used: Option<(String, u64, u64)> = None;

        for (index, candidate) in candidates.iter().enumerate() {
            // 换源最多试三次：再多说明问题不在源上
            if index >= MAX_SOURCE_ATTEMPTS {
                break;
            }
            match download_with_progress(candidate, &tmp_file, on_progress) {
                Ok(outcome) => match verify_file(&tmp_file, &checksum) {
                    Ok(()) => {
                        used = Some((candidate.clone(), outcome.bytes_total, outcome.resumed_from));
                        break;
                    }
                    Err(VerifyError::Mismatch { expected, actual }) => {
                        // 内容不对：可能是镜像同步了半截，或真的被篡改。
                        // 记下来换下一个源，但绝不放行。
                        last_verify = Some((
                            candidate.clone(),
                            format!("校验不匹配（期望 {expected} 实际 {actual}）"),
                        ));
                        std::fs::remove_file(&tmp_file).ok();
                        continue;
                    }
                    Err(e) => {
                        return Err(InstallError::Verify {
                            url: candidate.clone(),
                            reason: e.to_string(),
                        })
                    }
                },
                Err(e) => {
                    let fallback = should_fallback(e.status_code());
                    last_error = Some(e.to_string());
                    if fallback {
                        continue;
                    }
                    // 404 / 401：版本或路径本身就错了，换源只会把错误重复三遍
                    return Err(InstallError::Fetch {
                        url: candidate.clone(),
                        reason: e.to_string(),
                    });
                }
            }
        }

        let Some((source, bytes, resumed_from)) = used else {
            // 校验失败和一个都连不上是两件事，报错时不能混成一句"源不可用"
            if let Some((url, reason)) = last_verify {
                return Err(InstallError::Verify { url, reason });
            }
            return Err(InstallError::Fetch {
                url: candidates.join(" | "),
                reason: last_error.unwrap_or_else(|| "未知原因".into()),
            });
        };

        let stats = extract(
            &tmp_file,
            &staging,
            format,
            spec.strip_components,
            ExtractLimits::default(),
        )
        .map_err(|e| InstallError::Extract(e.to_string()))?;

        write_marker(&staging, spec, &checksum, &source, stats.files, bytes)
            .map_err(|e| InstallError::Io(e.to_string()))?;

        if let Some(parent) = final_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| InstallError::Io(e.to_string()))?;
        }
        std::fs::rename(&staging, &final_dir).map_err(|e| InstallError::Io(e.to_string()))?;

        Ok(InstallOutcome {
            path: final_dir,
            bytes,
            source,
            files: stats.files,
            skipped: false,
            resumed_from,
        })
    })();

    // 分片的去留要分清：内容坏了就删，网络断了要留着下次续传。
    // 这是断点续传能不能真正work的关键——一刀切删掉等于白续。
    match &result {
        Ok(_) => {
            std::fs::remove_file(&tmp_file).ok();
        }
        Err(InstallError::Fetch { .. }) => {
            // 保留 .part，下次同源续传；全量哈希最后会兜底，不担心拼错
        }
        Err(_) => {
            std::fs::remove_file(&tmp_file).ok();
        }
    }
    if result.is_err() {
        std::fs::remove_dir_all(&staging).ok();
    }
    result
}

fn write_marker(
    dir: &Path,
    spec: &InstallSpec,
    checksum: &Checksum,
    source: &str,
    files: usize,
    bytes: u64,
) -> std::io::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = format!(
        "component={}\nversion={}\nplatform={}\n{}={}\nsource={}\nfiles={}\nbytes={}\ninstalled_at={}\n",
        spec.component,
        spec.version,
        spec.platform,
        checksum.algorithm.as_str(),
        checksum.hex,
        source,
        files,
        bytes,
        now
    );
    std::fs::write(dir.join(READY_MARKER), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(url: &str, mirrors: &[&str]) -> InstallSpec {
        InstallSpec {
            component: "maven".into(),
            version: "3.9.9".into(),
            platform: "windows-x64".into(),
            url: url.into(),
            checksum: "0".repeat(64),
            format: "zip".into(),
            strip_components: 1,
            mirrors: mirrors.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn mirrors_come_first_and_original_is_last_resort() {
        let s = spec(
            "https://dlcdn.apache.org/maven/maven-3/3.9.9/binaries/apache-maven-3.9.9-bin.zip",
            &["https://mirrors.tuna.tsinghua.edu.cn/apache"],
        );
        let urls = candidate_urls(&s.url, &s.mirrors);
        assert_eq!(urls.len(), 2);
        assert!(urls[0].starts_with("https://mirrors.tuna.tsinghua.edu.cn/"));
        assert!(urls[0].ends_with("/apache/maven/maven-3/3.9.9/binaries/apache-maven-3.9.9-bin.zip"));
        assert_eq!(urls[1], s.url);
    }

    #[test]
    fn insecure_mirror_is_dropped_not_fatal() {
        let s = spec("https://a.com/x.zip", &["http://insecure.local", "not a url"]);
        let urls = candidate_urls(&s.url, &s.mirrors);
        assert_eq!(urls, vec!["https://a.com/x.zip"]);
    }

    #[test]
    fn artifact_path_layout() {
        let s = spec("https://a.com/x.zip", &[]);
        let p = artifact_dir(Path::new("C:/data"), &s);
        assert!(p.ends_with("store/maven/3.9.9/windows-x64") || p.ends_with("store\\maven\\3.9.9\\windows-x64"));
    }
}
