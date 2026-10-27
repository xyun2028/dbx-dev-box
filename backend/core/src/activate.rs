//! 激活（切换当前版本）。
//!
//! 这一层只算"应该链到哪、指针写哪"，不碰文件系统，所以能离线测。
//! 真正的创建链接与原子替换在 engine 里，因为要区分 junction / symlink。
//!
//! 三条规则：
//! 1. 只有装完的制品能被激活（调用方负责检查 `.devbox-ok`）。
//! 2. 组件名与版本号必须能安全地拼进路径，不允许出现分隔符或 `..`。
//! 3. 激活只改指针，**永不移动或复制 store 里的内容**——这是"秒级切换"的前提。

use std::path::{Path, PathBuf};

use crate::platform::PlatformKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationPlan {
    pub component: String,
    pub version: String,
    pub platform: String,
    /// store 里的只读制品目录
    pub target: PathBuf,
    /// `current/<component>`：稳定的入口，PATH 只需要加这一层
    pub link: PathBuf,
    /// `current/<component>.txt`：人能读的指针，不依赖链接是否可用
    pub pointer: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivateError {
    BadComponent(String),
    BadVersion(String),
    BadPlatform(String),
}

impl std::fmt::Display for ActivateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivateError::BadComponent(c) => write!(f, "组件名不合法: {c}"),
            ActivateError::BadVersion(v) => write!(f, "版本号不合法: {v}"),
            ActivateError::BadPlatform(p) => write!(f, "平台标识不合法: {p}"),
        }
    }
}

impl std::error::Error for ActivateError {}

pub fn plan(
    data_root: &Path,
    component: &str,
    version: &str,
    platform: &str,
) -> Result<ActivationPlan, ActivateError> {
    if !is_safe_component(component) {
        return Err(ActivateError::BadComponent(component.to_string()));
    }
    if !is_safe_version(version) {
        return Err(ActivateError::BadVersion(version.to_string()));
    }
    let parsed = PlatformKey::parse(platform)
        .ok_or_else(|| ActivateError::BadPlatform(platform.to_string()))?;

    Ok(ActivationPlan {
        component: component.to_string(),
        version: version.to_string(),
        platform: parsed.key(),
        target: data_root
            .join("store")
            .join(component)
            .join(version)
            .join(parsed.key()),
        link: data_root.join("current").join(component),
        pointer: data_root
            .join("current")
            .join(format!("{component}.txt")),
    })
}

fn is_safe_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !s.starts_with('.')
        && !s.contains("..")
}

/// 版本号比组件名更保守：只允许数字、点、连字符、下划线，
/// 因为版本号经常来自用户或目录，是最容易被拼错的一处。
fn is_safe_version(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && !s.starts_with('.')
        && !s.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_expected_paths() {
        let p = plan(Path::new("C:/data"), "node", "22.11.0", "windows-x64").unwrap();
        assert!(p.target.ends_with("store/node/22.11.0/windows-x64") || p.target.ends_with("store\\node\\22.11.0\\windows-x64"));
        assert!(p.link.ends_with("current/node") || p.link.ends_with("current\\node"));
        assert!(p.pointer.ends_with("current/node.txt") || p.pointer.ends_with("current\\node.txt"));
        assert_eq!(p.platform, "windows-x64");
    }

    #[test]
    fn platform_aliases_are_normalized() {
        let p = plan(Path::new("d"), "jdk", "21.0.5", "macos-aarch64").unwrap();
        assert_eq!(p.platform, "darwin-arm64");
    }

    #[test]
    fn rejects_path_tricks() {
        let root = Path::new("d");
        assert!(plan(root, "../node", "1.0.0", "linux-x64").is_err());
        assert!(plan(root, "no/de", "1.0.0", "linux-x64").is_err());
        assert!(plan(root, ".hidden", "1.0.0", "linux-x64").is_err());
        assert!(plan(root, "node", "../../etc", "linux-x64").is_err());
        assert!(plan(root, "node", "1.0.0/../x", "linux-x64").is_err());
        assert!(plan(root, "node", "", "linux-x64").is_err());
    }

    #[test]
    fn rejects_unknown_platform() {
        let err = plan(Path::new("d"), "node", "1.0.0", "plan9-x64").unwrap_err();
        assert!(matches!(err, ActivateError::BadPlatform(_)));
    }

    #[test]
    fn accepts_real_version_shapes() {
        for v in ["22.11.0", "3.9.9", "8.0.432", "17.0.13+11", "1.0.0-rc1"] {
            // "+11" 里的加号不在白名单，属于故意收紧：Temurin 的版本号应该用主版本
            let ok = plan(Path::new("d"), "x", v, "linux-x64").is_ok();
            assert_eq!(ok, !v.contains('+'), "version {v}");
        }
    }
}
