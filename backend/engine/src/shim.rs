//! 命令行入口编排（shim）。
//!
//! 问题：jdk / maven / node 各自在 `current/<component>/bin` 下，用户不可能把三条路径
//! 都手动加进 PATH，更不可能每次切版本都改一遍。
//!
//! 解法：`$DATA/bin` 这一层**固定不变**，里面每个文件都通过 `current/<component>/`
//! 间接指向真实程序。切换版本只动 `current` 链接，shim 一个字节都不用重写。
//!
//! 这里刻意**不修改用户的 PATH**——那是宿主环境，属于"不做"清单里的第一条。
//! 我们只负责把目录准备好，并把该加的那一行原样打印出来，由用户决定加不加。

use std::path::{Path, PathBuf};

use devbox_core::platform::PlatformKey;

use crate::activate::current;
use crate::catalog::Catalog;

#[derive(Debug)]
pub struct ShimReport {
    pub dir: PathBuf,
    pub created: Vec<String>,
    /// 配方声明了但找不到实际可执行文件的项，附带原因
    pub skipped: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum ShimError {
    Io(String),
}

impl std::fmt::Display for ShimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShimError::Io(e) => write!(f, "生成命令行入口失败: {e}"),
        }
    }
}

impl std::error::Error for ShimError {}

/// 重建整个 `$DATA/bin`。
///
/// 采取"清空重建"而不是增量维护：这个目录完全归我们所有，重建是幂等的，
/// 而且能顺手清掉已经失效的旧 shim（比如卸载了某个组件）。
pub fn refresh(
    data_root: &Path,
    catalog: &Catalog,
    platform: PlatformKey,
) -> Result<ShimReport, ShimError> {
    let dir = data_root.join("bin");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| ShimError::Io(e.to_string()))?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| ShimError::Io(e.to_string()))?;

    let mut created = Vec::new();
    let mut skipped = Vec::new();

    for component in &catalog.components {
        let Some(active) = current(data_root, &component.id) else {
            continue; // 没有激活版本就没有入口
        };
        let Some(entry) = component.versions.get(&active) else {
            continue;
        };
        let Some(provides) = &entry.provides else {
            continue;
        };

        for binary in &provides.binaries {
            // 解析实际位置：maven 在 bin/ 下且是 mvn.cmd，node 在包根目录且是 node.exe
            match resolve_binary(data_root, &component.id, binary, platform) {
                Some((subdir, actual)) => {
                    write_shim(&dir, binary, &component.id, &subdir, &actual, platform)
                        .map_err(|e| ShimError::Io(e.to_string()))?;
                    created.push(binary.clone());
                }
                None => skipped.push((
                    format!("{}/{}", component.id, binary),
                    format!(
                        "在 current/{} 下找不到 {binary}（查过 bin/ 与包根目录）",
                        component.id
                    ),
                )),
            }
        }
    }

    created.sort();
    Ok(ShimReport {
        dir,
        created,
        skipped,
    })
}

/// 找出真实可执行文件，返回 (相对子目录, 文件名)。
///
/// 要查两处：多数组件放在 `bin/`，但 Node 与 Nginx 的 Windows 包把可执行文件
/// 直接放在包根目录。只查 bin/ 会让 node 在这台机器上"装好了但没有入口"。
fn resolve_binary(
    data_root: &Path,
    component: &str,
    name: &str,
    platform: PlatformKey,
) -> Option<(String, String)> {
    let base = data_root.join("current").join(component);
    let candidates: Vec<String> = if platform.needs_exec_bit() {
        vec![name.to_string()]
    } else {
        // Windows 上可能是 .exe（node/java）也可能是 .cmd（mvn/npm）
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            format!("{name}.bat"),
            name.to_string(),
        ]
    };

    for subdir in ["bin", ""] {
        let dir = if subdir.is_empty() { base.clone() } else { base.join(subdir) };
        for candidate in &candidates {
            if dir.join(candidate).is_file() {
                return Some((subdir.to_string(), candidate.clone()));
            }
        }
    }
    None
}

#[cfg(windows)]
fn write_shim(
    dir: &Path,
    name: &str,
    component: &str,
    subdir: &str,
    actual: &str,
    _platform: PlatformKey,
) -> std::io::Result<()> {
    let rel = if subdir.is_empty() {
        format!("current\\{component}\\{actual}")
    } else {
        format!("current\\{component}\\{subdir}\\{actual}")
    };
    // call + exit /b：既能调用 .exe 也能调用 .cmd，并且把退出码透传出去
    let body = format!(
        "@echo off\r\nrem dev-box shim: {rel}\r\nrem 切换版本不需要重新生成，这个文件只做转发\r\ncall \"%~dp0..\\{rel}\" %*\r\nexit /b %errorlevel%\r\n"
    );
    std::fs::write(dir.join(format!("{name}.cmd")), body)
}

#[cfg(unix)]
fn write_shim(
    dir: &Path,
    name: &str,
    component: &str,
    subdir: &str,
    actual: &str,
    _platform: PlatformKey,
) -> std::io::Result<()> {
    // 相对符号链接：整棵树搬走也不会断
    let mut target = PathBuf::from("..").join("current").join(component);
    if !subdir.is_empty() {
        target = target.join(subdir);
    }
    let link = dir.join(name);
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(target.join(actual), link)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOG: &str = r#"
schema: 1
revision: test
components:
  - id: tool
    kind: toolchain
    label: Tool
    versions:
      "1.0":
        artifacts:
          windows-x64:
            url: https://example.com/x.zip
            sha256: "0000000000000000000000000000000000000000000000000000000000000000"
            format: zip
          linux-x64:
            url: https://example.com/x.tar.gz
            sha256: "0000000000000000000000000000000000000000000000000000000000000000"
            format: tar.gz
        provides:
          binaries: [tool, missing-one]
"#;

    fn sandbox(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devbox-shim-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(windows)]
    fn install_fake(root: &Path, version: &str) -> String {
        let dir = root.join("store").join("tool").join(version).join("windows-x64");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join(".devbox-ok"), "ok").unwrap();
        std::fs::write(
            dir.join("bin").join("tool.cmd"),
            format!("@echo off\r\necho TOOL-{version}\r\n"),
        )
        .unwrap();
        version.to_string()
    }

    #[cfg(unix)]
    fn install_fake(root: &Path, version: &str) -> String {
        let dir = root.join("store").join("tool").join(version).join("linux-x64");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join(".devbox-ok"), "ok").unwrap();
        let script = dir.join("bin").join("tool");
        std::fs::write(&script, format!("#!/bin/sh\necho TOOL-{version}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        version.to_string()
    }

    fn platform() -> PlatformKey {
        PlatformKey::current()
    }

    #[test]
    fn generates_shim_that_follows_the_active_version() {
        let root = sandbox("follow");
        let catalog = Catalog::from_str(CATALOG).unwrap();
        install_fake(&root, "1.0");
        install_fake(&root, "2.0");

        crate::activate(&root, "tool", "1.0", &platform().key()).unwrap();
        let report = refresh(&root, &catalog, platform()).unwrap();
        assert!(report.created.contains(&"tool".to_string()));
        // missing-one 在配方里声明了但没有实际文件，必须如实报告而不是静默跳过
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].0.ends_with("missing-one"));

        let shim = report.dir.join(if cfg!(windows) { "tool.cmd" } else { "tool" });
        let out = run(&shim);
        assert!(out.contains("TOOL-1.0"), "got {out}");

        // 切版本，但**不重新生成 shim**
        crate::activate(&root, "tool", "2.0", &platform().key()).unwrap();
        let out = run(&shim);
        assert!(out.contains("TOOL-2.0"), "shim 应该跟着 current 走，got {out}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn refresh_removes_stale_shims() {
        let root = sandbox("stale");
        let catalog = Catalog::from_str(CATALOG).unwrap();
        install_fake(&root, "1.0");
        crate::activate(&root, "tool", "1.0", &platform().key()).unwrap();
        refresh(&root, &catalog, platform()).unwrap();

        // 塞一个不属于我们的旧文件
        std::fs::write(root.join("bin").join("ghost"), "x").unwrap();
        refresh(&root, &catalog, platform()).unwrap();
        assert!(!root.join("bin").join("ghost").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_active_version_means_no_shims() {
        let root = sandbox("inactive");
        let catalog = Catalog::from_str(CATALOG).unwrap();
        install_fake(&root, "1.0"); // 装了但没激活
        let report = refresh(&root, &catalog, platform()).unwrap();
        assert!(report.created.is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(windows)]
    fn run(shim: &Path) -> String {
        let out = std::process::Command::new("cmd")
            .arg("/c")
            .arg(shim)
            .output()
            .expect("run shim");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    #[cfg(unix)]
    fn run(shim: &Path) -> String {
        let out = std::process::Command::new(shim).output().expect("run shim");
        String::from_utf8_lossy(&out.stdout).to_string()
    }
}
