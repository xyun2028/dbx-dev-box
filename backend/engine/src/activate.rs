//! 激活的落地：创建指向 store 的链接、写指针、失败回滚。
//!
//! Windows 用 junction（不需要管理员、不需要开发者模式），Unix 用 symlink。
//! 只改链接，不动 store 内容——所以切换是毫秒级的，也没有拷贝成本。

use std::path::{Path, PathBuf};

use devbox_core::activate::{plan, ActivateError};
use devbox_core::READY_MARKER;

#[derive(Debug)]
pub enum LinkError {
    Core(ActivateError),
    NotInstalled { component: String, version: String },
    Io(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Core(e) => write!(f, "{e}"),
            LinkError::NotInstalled { component, version } => {
                write!(f, "{component} {version} 还没装好，不能激活")
            }
            LinkError::Io(e) => write!(f, "切换失败: {e}"),
        }
    }
}

impl std::error::Error for LinkError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    pub component: String,
    pub version: String,
    pub link: PathBuf,
    pub target: PathBuf,
    /// 切换前的版本，便于回滚与展示
    pub previous: Option<String>,
}

pub fn activate(
    data_root: &Path,
    component: &str,
    version: &str,
    platform: &str,
) -> Result<Activation, LinkError> {
    let plan = plan(data_root, component, version, platform).map_err(LinkError::Core)?;

    if !plan.target.join(READY_MARKER).is_file() {
        return Err(LinkError::NotInstalled {
            component: component.to_string(),
            version: version.to_string(),
        });
    }

    let previous = current(data_root, component);
    if let Some(parent) = plan.link.parent() {
        std::fs::create_dir_all(parent).map_err(|e| LinkError::Io(e.to_string()))?;
    }

    // 先摘掉旧链接。中间会有一瞬间没有链接，这是可接受的：
    // 正在运行的进程早已打开了文件句柄，不受影响。
    remove_link(&plan.link).map_err(|e| LinkError::Io(e.to_string()))?;

    if let Err(error) = make_link(&plan.target, &plan.link) {
        // 回滚：把之前的版本re接上，别让用户停在"没有当前版本"的状态
        if let Some(prev) = &previous {
            let prev_dir = data_root
                .join("store")
                .join(component)
                .join(prev)
                .join(&plan.platform);
            let _ = make_link(&prev_dir, &plan.link);
        }
        return Err(LinkError::Io(error));
    }

    write_pointer(&plan.pointer, component, version, &plan.platform, &plan.target)
        .map_err(|e| LinkError::Io(e.to_string()))?;

    Ok(Activation {
        component: component.to_string(),
        version: version.to_string(),
        link: plan.link,
        target: plan.target,
        previous,
    })
}

pub fn deactivate(data_root: &Path, component: &str) -> Result<bool, LinkError> {
    let link = data_root.join("current").join(component);
    let pointer = data_root.join("current").join(format!("{component}.txt"));
    let mut removed = false;
    if link.exists() || std::fs::symlink_metadata(&link).is_ok() {
        remove_link(&link).map_err(|e| LinkError::Io(e.to_string()))?;
        removed = true;
    }
    if pointer.is_file() {
        std::fs::remove_file(&pointer).map_err(|e| LinkError::Io(e.to_string()))?;
        removed = true;
    }
    Ok(removed)
}

/// 当前激活的版本。以指针文件为准——链接可能因为权限或外部操作被破坏，
/// 但指针是我们自己写的，且人可以打开看。
pub fn current(data_root: &Path, component: &str) -> Option<String> {
    let pointer = data_root.join("current").join(format!("{component}.txt"));
    let text = std::fs::read_to_string(pointer).ok()?;
    parse_pointer(&text)
}

pub fn parse_pointer(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("version="))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn write_pointer(
    path: &Path,
    component: &str,
    version: &str,
    platform: &str,
    target: &Path,
) -> std::io::Result<()> {
    let body = format!(
        "component={component}\nversion={version}\nplatform={platform}\ntarget={}\n",
        target.display()
    );
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, body)?;
    // 先删再改名：Windows 上 rename 覆盖已存在文件会失败
    std::fs::remove_file(path).ok();
    std::fs::rename(&tmp, path)
}

fn remove_link(path: &Path) -> std::io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    #[cfg(windows)]
    {
        // junction 不能用 std::fs::remove_dir 删（会返回拒绝访问），
        // 必须走 reparse point 的删除路径
        if junction::exists(path).unwrap_or(false) {
            junction::delete(path)?;
            // 实测两点：junction 在 symlink_metadata 里被识别为 symlink（is_dir=false），
            // 且 delete 之后仍会残留一个悬空条目。不按类型清掉的话，
            // 紧接着创建新链接会直接失败。
            if let Ok(meta) = std::fs::symlink_metadata(path) {
                let _ = if meta.is_dir() {
                    std::fs::remove_dir(path)
                } else {
                    std::fs::remove_file(path)
                };
            }
            return Ok(());
        }
    }

    if meta.is_dir() {
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(windows)]
fn make_link(target: &Path, link: &Path) -> Result<(), String> {
    junction::create(target, link).map_err(|e| format!("创建 junction 失败: {e}"))
}

#[cfg(unix)]
fn make_link(target: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link).map_err(|e| format!("创建符号链接失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devbox-act-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_install(root: &Path, component: &str, version: &str, platform: &str) {
        let dir = root.join("store").join(component).join(version).join(platform);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(READY_MARKER), "ok").unwrap();
        std::fs::write(dir.join("payload.txt"), version).unwrap();
    }

    #[test]
    fn activates_and_switches_versions() {
        let root = sandbox("switch");
        fake_install(&root, "maven", "3.9.9", "windows-x64");
        fake_install(&root, "maven", "3.8.8", "windows-x64");

        let a = activate(&root, "maven", "3.9.9", "windows-x64").unwrap();
        assert_eq!(a.previous, None);
        assert_eq!(current(&root, "maven").as_deref(), Some("3.9.9"));
        assert_eq!(
            std::fs::read_to_string(root.join("current").join("maven").join("payload.txt")).unwrap(),
            "3.9.9"
        );

        let b = activate(&root, "maven", "3.8.8", "windows-x64").unwrap();
        assert_eq!(b.previous.as_deref(), Some("3.9.9"));
        assert_eq!(current(&root, "maven").as_deref(), Some("3.8.8"));
        assert_eq!(
            std::fs::read_to_string(root.join("current").join("maven").join("payload.txt")).unwrap(),
            "3.8.8"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn refuses_unfinished_artifact_and_keeps_current() {
        let root = sandbox("unfinished");
        fake_install(&root, "node", "22.11.0", "windows-x64");
        activate(&root, "node", "22.11.0", "windows-x64").unwrap();

        // 目录存在但没有完成标记
        let half = root.join("store").join("node").join("20.0.0").join("windows-x64");
        std::fs::create_dir_all(&half).unwrap();

        let err = activate(&root, "node", "20.0.0", "windows-x64").unwrap_err();
        assert!(matches!(err, LinkError::NotInstalled { .. }));
        assert_eq!(
            current(&root, "node").as_deref(),
            Some("22.11.0"),
            "失败的切换不能把当前版本弄丢"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn deactivate_removes_link_and_pointer() {
        let root = sandbox("deact");
        fake_install(&root, "node", "22.11.0", "windows-x64");
        activate(&root, "node", "22.11.0", "windows-x64").unwrap();

        assert!(deactivate(&root, "node").unwrap());
        assert_eq!(current(&root, "node"), None);
        assert!(!root.join("current").join("node").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn pointer_parsing() {
        assert_eq!(parse_pointer("component=x\nversion=1.2.3\ntarget=/a\n").as_deref(), Some("1.2.3"));
        assert_eq!(parse_pointer("garbage"), None);
        assert_eq!(parse_pointer("version=\n"), None);
    }
}
