//! 容器数据卷规划。
//!
//! 决策（2026-10-27）：**默认 bind mount 到实例目录**，命名卷作为可选项。
//! 理由是数据可见、可直接备份、删除实例时用户看得见自己在删什么；
//! 代价是 Docker Desktop 上的文件共享层比较慢，这一点必须在界面上说清楚，
//! 不能让用户装完才觉得卡。
//!
//! 两条不可退让的约束：
//! 1. 宿主路径必须落在 `$DATA/instances` 之内——否则配方里写个 `/etc` 就能
//!    把系统目录挂进容器，等于把沙箱交给 catalog 的作者。
//! 2. 容器内路径要做黑名单：docker.sock、/proc、/sys、/dev 一律拒绝。

use std::path::Path;

use crate::safe_path::{safe_join, PathError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeMode {
    /// 挂到 `$DATA/instances/<id>/data`，数据可见可备份
    BindMount,
    /// Docker 命名卷 `devbox-<id>`，快但数据在 Docker 目录里
    NamedVolume,
}

impl VolumeMode {
    /// 默认挂载出来。宁可慢一点，也不要让用户找不到自己的数据。
    pub fn default_mode() -> Self {
        VolumeMode::BindMount
    }

    pub fn as_str(self) -> &'static str {
        match self {
            VolumeMode::BindMount => "bind",
            VolumeMode::NamedVolume => "volume",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bind" | "bind-mount" | "mount" => Some(VolumeMode::BindMount),
            "volume" | "named-volume" => Some(VolumeMode::NamedVolume),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountPlan {
    pub mode: VolumeMode,
    /// bind 模式是宿主绝对路径；volume 模式是卷名
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountError {
    BadInstanceId,
    BadContainerPath,
    ProtectedContainerPath,
    Path(PathError),
}

impl std::fmt::Display for MountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountError::BadInstanceId => write!(f, "实例名只能包含字母、数字、-、_、."),
            MountError::BadContainerPath => write!(f, "容器内路径必须是绝对路径且不含 .."),
            MountError::ProtectedContainerPath => write!(f, "不允许挂载受保护的路径"),
            MountError::Path(e) => write!(f, "挂载路径不安全: {e}"),
        }
    }
}

impl std::error::Error for MountError {}

const PROTECTED: &[&str] = &[
    "/",
    "/proc",
    "/sys",
    "/dev",
    "/boot",
    "/etc",
    "/var/run/docker.sock",
    "/run/docker.sock",
    "/var/lib/docker",
];

/// 规划一个实例的数据挂载。
///
/// `data_root` 即插件的 `$DATA`；`instance_id` 必须已经是安全的名字。
pub fn plan_data_mount(
    data_root: &Path,
    instance_id: &str,
    container_path: &str,
    mode: VolumeMode,
) -> Result<MountPlan, MountError> {
    if !is_safe_id(instance_id) {
        return Err(MountError::BadInstanceId);
    }

    let target = normalize_container_path(container_path)?;
    if is_protected(&target) {
        return Err(MountError::ProtectedContainerPath);
    }

    let source = match mode {
        VolumeMode::BindMount => {
            let root = data_root.join("instances");
            // safe_join 会挡掉 ../、绝对路径、设备名等，这里再拼一层 data
            let host = safe_join(&root, instance_id).map_err(MountError::Path)?;
            host.join("data").to_string_lossy().to_string()
        }
        VolumeMode::NamedVolume => format!("devbox-{instance_id}"),
    };

    Ok(MountPlan {
        mode,
        source,
        target,
        read_only: false,
    })
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !id.starts_with('.')
        && !id.contains("..")
}

fn normalize_container_path(p: &str) -> Result<String, MountError> {
    if !p.starts_with('/') || p.is_empty() || p.len() > 512 {
        return Err(MountError::BadContainerPath);
    }
    if p.split('/').any(|seg| seg == "..") {
        return Err(MountError::BadContainerPath);
    }
    if p.chars().any(|c| c.is_control()) {
        return Err(MountError::BadContainerPath);
    }
    // 去掉重复斜杠与结尾斜杠，便于和黑名单比较
    let mut out = String::with_capacity(p.len());
    for seg in p.split('/').filter(|s| !s.is_empty()) {
        out.push('/');
        out.push_str(seg);
    }
    if out.is_empty() {
        out.push('/');
    }
    Ok(out)
}

fn is_protected(path: &str) -> bool {
    PROTECTED
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")) && *p != "/")
        || path == "/"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from("C:/Users/dev/AppData/Roaming/dbx/devbox")
    }

    #[test]
    fn bind_mount_is_the_default() {
        assert_eq!(VolumeMode::default_mode(), VolumeMode::BindMount);
        let p = plan_data_mount(&root(), "mysql-dev", "/var/lib/mysql", VolumeMode::default_mode())
            .expect("should plan");
        assert_eq!(p.mode, VolumeMode::BindMount);
        assert!(p.source.ends_with("instances\\mysql-dev\\data") || p.source.ends_with("instances/mysql-dev/data"));
    }

    #[test]
    fn named_volume_when_opted_in() {
        let p = plan_data_mount(&root(), "redis-dev", "/data", VolumeMode::NamedVolume).unwrap();
        assert_eq!(p.mode, VolumeMode::NamedVolume);
        assert_eq!(p.source, "devbox-redis-dev");
        assert_eq!(p.target, "/data");
    }

    #[test]
    fn instance_id_cannot_escape() {
        for bad in ["../etc", "a/b", "a\\b", "", ".hidden", "a..b", "名字"] {
            assert!(
                plan_data_mount(&root(), bad, "/data", VolumeMode::BindMount).is_err(),
                "{bad} should be rejected"
            );
        }
        assert!(plan_data_mount(&root(), "mysql-dev_2.0", "/data", VolumeMode::BindMount).is_ok());
    }

    #[test]
    fn protected_container_paths_are_rejected() {
        for p in [
            "/",
            "/proc",
            "/proc/self",
            "/sys",
            "/dev",
            "/etc",
            "/etc/passwd",
            "/var/run/docker.sock",
            "/var/lib/docker",
        ] {
            assert_eq!(
                plan_data_mount(&root(), "x", p, VolumeMode::BindMount).unwrap_err(),
                MountError::ProtectedContainerPath,
                "{p} must be rejected"
            );
        }
    }

    #[test]
    fn container_path_must_be_absolute_and_clean() {
        for bad in ["data", "./data", "/data/../etc", "/data\0x"] {
            assert!(
                plan_data_mount(&root(), "x", bad, VolumeMode::BindMount).is_err(),
                "{bad} should be rejected"
            );
        }
        // 重复斜杠和结尾斜杠被规范化，不算错误
        let p = plan_data_mount(&root(), "x", "//var//lib/mysql/", VolumeMode::BindMount).unwrap();
        assert_eq!(p.target, "/var/lib/mysql");
    }

    #[test]
    fn volume_mode_roundtrip() {
        assert_eq!(VolumeMode::parse("bind"), Some(VolumeMode::BindMount));
        assert_eq!(VolumeMode::parse("volume"), Some(VolumeMode::NamedVolume));
        assert_eq!(VolumeMode::parse("tmpfs"), None);
        assert_eq!(VolumeMode::BindMount.as_str(), "bind");
    }
}
