//! 镜像源 / 加速器解析。
//!
//! 决策（2026-10-27）：**用户自己配优先，其次用内置预设；用户配的不通才降级**。
//!
//! 这里最容易做错的一件事，是把"换源"和"换校验"混在一起。必须分开：
//! - 换源只替换 scheme + host（+端口，可选路径前缀），**path 和 query 一个字节都不改**。
//!   所以 sha256 / digest 的校验依然成立，镜像站拿不到任何改写内容的机会。
//! - 换源判定要分清"源坏了"和"东西不存在"：
//!   超时、连不上、403、408、429、5xx → 换下一个源；
//!   404、401、400 → 直接失败，因为那说明版本或路径写错了，换源只会把错误掩盖三次。

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorError {
    /// 原始地址必须是 https
    NotHttps,
    /// 镜像地址必须是 https 的纯 origin（允许路径前缀）
    BadMirror,
    /// 地址里带了账号密码，不接受
    Credentials,
    Empty,
}

impl std::fmt::Display for MirrorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MirrorError::NotHttps => write!(f, "只允许 https 地址"),
            MirrorError::BadMirror => write!(f, "镜像地址格式不正确"),
            MirrorError::Credentials => write!(f, "镜像地址不能包含账号密码"),
            MirrorError::Empty => write!(f, "地址为空"),
        }
    }
}

impl std::error::Error for MirrorError {}

/// 用镜像源替换原始地址的 origin，保留 path 与 query。
///
/// 这是"只换域名不换字节"的实现位置：调用方拿到结果后仍然用同一个
/// sha256 / digest 校验，镜像站无法影响内容。
pub fn rewrite_origin(original: &str, mirror: &str) -> Result<String, MirrorError> {
    let original = original.trim();
    let mirror = mirror.trim().trim_end_matches('/');

    if original.is_empty() || mirror.is_empty() {
        return Err(MirrorError::Empty);
    }
    if !original.starts_with("https://") {
        return Err(MirrorError::NotHttps);
    }
    if !mirror.starts_with("https://") {
        return Err(MirrorError::BadMirror);
    }
    if mirror.contains('@') {
        return Err(MirrorError::Credentials);
    }
    if mirror.contains('?') || mirror.contains('#') || mirror.contains("..") {
        return Err(MirrorError::BadMirror);
    }

    let rest = match original[8..].find('/') {
        Some(idx) => &original[8 + idx..],
        None => "",
    };
    Ok(format!("{mirror}{rest}"))
}

/// 候选源顺序：用户配置优先，其后是预设，去重并丢掉格式不合法的。
///
/// 预设的价值不是"更好"，而是兜底——用户填的加速器过期了，安装不应该直接失败。
pub fn candidate_order(user: Option<&str>, presets: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for candidate in user.into_iter().chain(presets.iter().copied()) {
        let c = candidate.trim().trim_end_matches('/');
        if c.is_empty()
            || !c.starts_with("https://")
            || c.contains('@')
            || c.contains('?')
            || c.contains('#')
        {
            continue;
        }
        if !out.iter().any(|existing| existing == c) {
            out.push(c.to_string());
        }
    }
    out
}

/// 这个 HTTP 结果是否值得换下一个源重试。
///
/// `None` 表示连接层失败（超时、DNS、TLS、被重置）。
pub fn should_fallback(status: Option<u16>) -> bool {
    match status {
        None => true,
        Some(403) | Some(408) | Some(429) => true,
        Some(code) if (500..600).contains(&code) => true,
        // 404 说明这个版本在源上本来就没有；401/400 说明请求本身有问题。
        // 换源重试只会把同一个错误重复三遍，还拖慢失败反馈。
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_keeps_path_byte_identical() {
        let original = "https://registry-1.docker.io/v2/library/mysql/blobs/sha256:8f2c1a";
        let out = rewrite_origin(original, "https://docker.mirrors.ustc.edu.cn").unwrap();
        assert_eq!(
            out,
            "https://docker.mirrors.ustc.edu.cn/v2/library/mysql/blobs/sha256:8f2c1a"
        );
        // 核心保证：path 部分完全一致，校验才有意义
        assert_eq!(&original["https://registry-1.docker.io".len()..], &out["https://docker.mirrors.ustc.edu.cn".len()..]);
    }

    #[test]
    fn rewrite_supports_mirror_with_path_prefix() {
        let out = rewrite_origin("https://cdn.example.com/a/b.tar.gz", "https://mirror.local/proxy/").unwrap();
        assert_eq!(out, "https://mirror.local/proxy/a/b.tar.gz");
    }

    #[test]
    fn rewrite_rejects_insecure_or_dirty_mirrors() {
        assert_eq!(
            rewrite_origin("https://a.com/x", "http://mirror.local"),
            Err(MirrorError::BadMirror)
        );
        assert_eq!(
            rewrite_origin("http://a.com/x", "https://mirror.local"),
            Err(MirrorError::NotHttps)
        );
        assert_eq!(
            rewrite_origin("https://a.com/x", "https://user:pass@mirror.local"),
            Err(MirrorError::Credentials)
        );
        assert_eq!(
            rewrite_origin("https://a.com/x", "https://mirror.local/?q=1"),
            Err(MirrorError::BadMirror)
        );
        assert_eq!(rewrite_origin("", "https://m.local"), Err(MirrorError::Empty));
    }

    #[test]
    fn candidate_order_prefers_user_then_presets() {
        let list = candidate_order(
            Some("https://mine.local"),
            &["https://preset-a.local", "https://mine.local", "http://insecure.local", ""],
        );
        assert_eq!(list, vec!["https://mine.local", "https://preset-a.local"]);
    }

    #[test]
    fn candidate_order_without_user_config() {
        let list = candidate_order(None, &["https://preset-a.local", "https://preset-b.local"]);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn fallback_only_on_source_failures() {
        assert!(should_fallback(None), "连接失败要换源");
        assert!(should_fallback(Some(403)));
        assert!(should_fallback(Some(429)));
        assert!(should_fallback(Some(500)));
        assert!(should_fallback(Some(503)));

        assert!(!should_fallback(Some(404)), "版本不存在，换源没用");
        assert!(!should_fallback(Some(401)));
        assert!(!should_fallback(Some(400)));
        assert!(!should_fallback(Some(200)));
    }
}
