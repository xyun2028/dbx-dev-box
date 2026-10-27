//! 运行后端抽象。
//!
//! dev-box 有两套完全不同的"把东西跑起来"的方式：
//! - Native：解压制品 + spawn 子进程 + 自己监管
//! - Docker：拉镜像 + 建容器 + 交给引擎的重启策略
//!
//! 这两者的差异必须收敛在很小的一块里，否则业务层会被 `if runtime == docker`
//! 渗透，最后两边都不敢改。这个模块只放"两端都需要知道的共同事实"。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    Native,
    Docker,
}

impl RuntimeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeKind::Native => "native",
            RuntimeKind::Docker => "docker",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "native" | "host" => Some(RuntimeKind::Native),
            "docker" | "container" => Some(RuntimeKind::Docker),
            _ => None,
        }
    }

    /// 容器模式下没有"解压"这一步，镜像拉完直接创建容器。
    /// 状态机据此决定 Verify 之后是进 Extract 还是直接 Commit。
    pub fn needs_extraction(self) -> bool {
        matches!(self, RuntimeKind::Native)
    }

    /// 工具链只能装本机：容器里的 JDK 对宿主机的 `mvn` / IDE 无效。
    pub fn supports_toolchain(self) -> bool {
        matches!(self, RuntimeKind::Native)
    }

    /// 本机后端需要宿主上的运行时依赖（如 JDK）；容器镜像自带 JRE，不需要。
    /// 注意服务间依赖（xxl-job 需要 MySQL）与后端无关，两种模式都要满足。
    pub fn needs_host_runtime_deps(self) -> bool {
        matches!(self, RuntimeKind::Native)
    }

    /// 容器模式不需要我们注册开机项（重启策略交给引擎），
    /// 但依赖 Docker 引擎自己开机启动。
    pub fn needs_host_autostart(self) -> bool {
        matches!(self, RuntimeKind::Native)
    }

    pub fn depends_on_engine_autostart(self) -> bool {
        matches!(self, RuntimeKind::Docker)
    }
}

impl fmt::Display for RuntimeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 后端可用性。不可用时必须带**人能看懂的原因**，
/// UI 直接展示这句话，不给一个点了会失败的按钮。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStatus {
    pub kind: RuntimeKind,
    pub available: bool,
    pub version: Option<String>,
    pub reason: Option<String>,
}

impl BackendStatus {
    pub fn ready(kind: RuntimeKind, version: impl Into<String>) -> Self {
        Self {
            kind,
            available: true,
            version: Some(version.into()),
            reason: None,
        }
    }

    pub fn unavailable(kind: RuntimeKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            available: false,
            version: None,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistMode {
    Session,
    Detached,
    Autostart,
}

impl PersistMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PersistMode::Session => "session",
            PersistMode::Detached => "detached",
            PersistMode::Autostart => "autostart",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "session" => Some(PersistMode::Session),
            "detached" => Some(PersistMode::Detached),
            "autostart" => Some(PersistMode::Autostart),
            _ => None,
        }
    }

    /// 容器后端直接映射到引擎的重启策略，不用我们自己写守护。
    pub fn docker_restart_policy(self) -> &'static str {
        match self {
            PersistMode::Session => "no",
            PersistMode::Detached => "unless-stopped",
            PersistMode::Autostart => "always",
        }
    }
}

/// 端口发布地址校验。
///
/// 只允许回环地址：`-p 3306:3306` 等于把数据库发布到 0.0.0.0，
/// 同网段任何人都能连上来。这是同类工具最常见的安全事故。
pub fn validate_port_bind(bind: &str) -> Result<(), &'static str> {
    match bind {
        "127.0.0.1" | "::1" | "localhost" => Ok(()),
        "" => Err("必须显式指定绑定地址，空值等于发布到所有网卡"),
        "0.0.0.0" | "::" | "*" => Err("不允许绑定所有网卡，请用 127.0.0.1 或显式 IP"),
        other => {
            if other.parse::<std::net::IpAddr>().is_ok() {
                // 具体网卡地址是允许的（用户明确要暴露给某块网卡），
                // 但调用方应该让用户二次确认。
                Ok(())
            } else {
                Err("绑定地址既不是回环也不是合法 IP")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_roundtrip_and_aliases() {
        assert_eq!(RuntimeKind::parse("native"), Some(RuntimeKind::Native));
        assert_eq!(RuntimeKind::parse("host"), Some(RuntimeKind::Native));
        assert_eq!(RuntimeKind::parse("container"), Some(RuntimeKind::Docker));
        assert_eq!(RuntimeKind::parse("podman"), None);
        assert_eq!(RuntimeKind::Docker.as_str(), "docker");
    }

    #[test]
    fn toolchain_is_native_only() {
        assert!(RuntimeKind::Native.supports_toolchain());
        assert!(
            !RuntimeKind::Docker.supports_toolchain(),
            "容器里的 JDK 对宿主 CLI 无效，不能提供这个选项"
        );
    }

    #[test]
    fn extraction_only_for_native() {
        assert!(RuntimeKind::Native.needs_extraction());
        assert!(!RuntimeKind::Docker.needs_extraction());
    }

    #[test]
    fn autostart_responsibility_differs() {
        // 本机要我们自己注册开机项；容器交给引擎
        assert!(RuntimeKind::Native.needs_host_autostart());
        assert!(!RuntimeKind::Native.depends_on_engine_autostart());
        assert!(!RuntimeKind::Docker.needs_host_autostart());
        assert!(RuntimeKind::Docker.depends_on_engine_autostart());
    }

    #[test]
    fn persist_mode_maps_to_docker_restart_policy() {
        assert_eq!(PersistMode::Session.docker_restart_policy(), "no");
        assert_eq!(PersistMode::Detached.docker_restart_policy(), "unless-stopped");
        assert_eq!(PersistMode::Autostart.docker_restart_policy(), "always");
        for m in [PersistMode::Session, PersistMode::Detached, PersistMode::Autostart] {
            assert_eq!(PersistMode::parse(m.as_str()), Some(m));
        }
    }

    #[test]
    fn port_bind_rejects_public_interfaces() {
        assert!(validate_port_bind("127.0.0.1").is_ok());
        assert!(validate_port_bind("::1").is_ok());
        assert!(validate_port_bind("192.168.1.10").is_ok());

        assert!(validate_port_bind("0.0.0.0").is_err());
        assert!(validate_port_bind("::").is_err());
        assert!(validate_port_bind("").is_err());
        assert!(validate_port_bind("*").is_err());
        assert!(validate_port_bind("myhost").is_err());
    }

    #[test]
    fn backend_status_carries_human_reason() {
        let s = BackendStatus::unavailable(RuntimeKind::Docker, "未检测到 Docker 引擎");
        assert!(!s.available);
        assert!(s.reason.unwrap().contains("Docker"));
        let ok = BackendStatus::ready(RuntimeKind::Docker, "27.3.1");
        assert!(ok.available);
    }
}
