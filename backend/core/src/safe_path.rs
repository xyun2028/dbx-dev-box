//! 解压路径校验。
//!
//! 这是整个插件最容易被攻击的一处：一个精心构造的压缩包可以写到任意位置、
//! 覆盖系统文件、或者用符号链接把后续写入引到沙箱外。
//!
//! 所以规则写得比"够用"更严一点：宁可拒绝一个合法但奇怪的文件名，
//! 也不要放过一个可疑的。被拒绝的条目会记进安装日志，用户能看见。

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// 空路径
    Empty,
    /// 绝对路径：`/etc/passwd`、`\\\\server\\share`、`C:\\Windows`
    Absolute,
    /// 含 `..`，会造成目录穿越
    ParentRef,
    /// 控制字符（含 NUL）
    Control,
    /// Windows 备用数据流写法，如 `file.txt:hidden`
    AltStream,
    /// Windows 保留设备名，如 `CON`、`aux.txt`、`com1.log`
    ReservedName(String),
    /// 结尾是点或空格（Windows 会静默截断，造成同名覆盖）
    TrailingDotOrSpace(String),
    /// 超长
    TooLong,
    /// 兜底：拼出来的路径不在根目录下
    EscapesRoot,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::Empty => write!(f, "empty path"),
            PathError::Absolute => write!(f, "absolute path"),
            PathError::ParentRef => write!(f, "parent directory reference"),
            PathError::Control => write!(f, "control character"),
            PathError::AltStream => write!(f, "alternate data stream"),
            PathError::ReservedName(n) => write!(f, "reserved device name: {n}"),
            PathError::TrailingDotOrSpace(n) => write!(f, "trailing dot or space: {n:#?}"),
            PathError::TooLong => write!(f, "path too long"),
            PathError::EscapesRoot => write!(f, "path escapes extraction root"),
        }
    }
}

impl std::error::Error for PathError {}

const MAX_COMPONENT_LEN: usize = 255;
const MAX_TOTAL_LEN: usize = 4096;

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 把压缩包里的条目名安全地拼到 root 下。
///
/// 返回的路径**保证**位于 `root` 之内，且不含 `..`、绝对路径、设备名等危险构造。
pub fn safe_join(root: &Path, entry: &str) -> Result<PathBuf, PathError> {
    if entry.trim().is_empty() {
        return Err(PathError::Empty);
    }
    if entry.len() > MAX_TOTAL_LEN {
        return Err(PathError::TooLong);
    }

    let bytes = entry.as_bytes();
    // 绝对路径：开头就是分隔符
    if bytes[0] == b'/' || bytes[0] == b'\\' {
        return Err(PathError::Absolute);
    }
    // 盘符相对路径（`C:evil`）在 Windows 上同样危险，一并按绝对路径处理
    if bytes.len() > 1 && bytes[1] == b':' {
        return Err(PathError::Absolute);
    }
    if entry.chars().any(|c| c.is_control()) {
        return Err(PathError::Control);
    }

    // 归档里 `/` 和 `\` 都可能当分隔符用，统一拆开
    let mut out = PathBuf::from(root);
    for raw in entry.split(['/', '\\']) {
        if raw.is_empty() {
            continue; // `a//b` 这种重复分隔符无害
        }
        if raw == "." {
            continue;
        }
        if raw == ".." {
            return Err(PathError::ParentRef);
        }
        if raw.contains(':') {
            return Err(PathError::AltStream);
        }
        if raw.len() > MAX_COMPONENT_LEN {
            return Err(PathError::TooLong);
        }
        if raw.ends_with('.') || raw.ends_with(' ') {
            return Err(PathError::TrailingDotOrSpace(raw.to_string()));
        }

        // 保留名判定按第一个点之前的部分，`aux.txt` 同样是危险名
        let stem = raw.split('.').next().unwrap_or(raw);
        let upper = stem.to_ascii_uppercase();
        if RESERVED.contains(&upper.as_str()) {
            return Err(PathError::ReservedName(raw.to_string()));
        }

        out.push(raw);
    }

    if out == root {
        return Err(PathError::Empty);
    }
    // 兜底：即使前面的规则漏了，这里也不允许跑出 root
    if !lexically_under(root, &out) {
        return Err(PathError::EscapesRoot);
    }
    Ok(out)
}

/// 纯词法比较，不碰文件系统（解压时目标可能还不存在）。
fn lexically_under(root: &Path, candidate: &Path) -> bool {
    let mut a = root.components();
    let mut b = candidate.components();
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) => {
                if normalize(x) != normalize(y) {
                    return false;
                }
            }
            (None, _) => return true,
            (Some(_), None) => return false,
        }
    }
}

fn normalize(c: Component<'_>) -> String {
    match c {
        Component::Normal(s) => s.to_string_lossy().to_lowercase(),
        other => other.as_os_str().to_string_lossy().to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("C:/devbox/store/staging")
    }

    fn ok(entry: &str) -> PathBuf {
        safe_join(&root(), entry).unwrap_or_else(|e| panic!("{entry} should pass, got {e}"))
    }

    fn reject(entry: &str) -> PathError {
        safe_join(&root(), entry).expect_err(&format!("{entry} must be rejected"))
    }

    #[test]
    fn normal_nested_paths_join() {
        assert_eq!(ok("bin/java"), root().join("bin").join("java"));
        assert_eq!(ok("./a/./b"), root().join("a").join("b"));
        assert_eq!(ok("sub//file.txt"), root().join("sub").join("file.txt"));
        assert_eq!(ok("a\\b\\c"), root().join("a").join("b").join("c"));
    }

    #[test]
    fn rejects_traversal() {
        assert_eq!(reject("../etc/passwd"), PathError::ParentRef);
        assert_eq!(reject("a/../../b"), PathError::ParentRef);
        assert_eq!(reject(".."), PathError::ParentRef);
        assert_eq!(reject("a\\..\\..\\b"), PathError::ParentRef);
    }

    #[test]
    fn rejects_absolute_paths() {
        assert_eq!(reject("/etc/passwd"), PathError::Absolute);
        assert_eq!(reject("\\\\server\\share\\x"), PathError::Absolute);
        assert_eq!(reject("C:/Windows/System32/evil.dll"), PathError::Absolute);
        assert_eq!(reject("C:evil"), PathError::Absolute, "drive-relative is just as bad");
    }

    #[test]
    fn rejects_control_characters() {
        assert_eq!(reject("file\u{0}.txt"), PathError::Control);
        assert_eq!(reject("bad\u{1}name"), PathError::Control);
        assert_eq!(reject("line\nbreak"), PathError::Control);
    }

    #[test]
    fn rejects_windows_device_names() {
        for name in ["CON", "con", "Aux", "aux.txt", "com1.log", "LPT9", "nul"] {
            assert!(
                matches!(reject(name), PathError::ReservedName(_)),
                "{name} must be rejected"
            );
        }
        // 名字里含 CON 但不是保留名，必须放行
        assert!(safe_join(&root(), "console.txt").is_ok());
        assert!(safe_join(&root(), "auxiliary/readme.md").is_ok());
    }

    #[test]
    fn rejects_alternate_data_streams() {
        assert_eq!(reject("file.txt:hidden"), PathError::AltStream);
        assert_eq!(reject("dir/stream:$DATA"), PathError::AltStream);
    }

    #[test]
    fn rejects_trailing_dot_or_space() {
        assert!(matches!(reject("evil."), PathError::TrailingDotOrSpace(_)));
        assert!(matches!(reject("dir/evil.txt "), PathError::TrailingDotOrSpace(_)));
        // 点/空格在中间是合法的
        assert!(safe_join(&root(), "my.file.txt").is_ok());
        assert!(safe_join(&root(), "my dir/file").is_ok());
    }

    #[test]
    fn rejects_empty_and_blank() {
        assert_eq!(reject(""), PathError::Empty);
        assert_eq!(reject("   "), PathError::Empty);
        assert_eq!(reject("./"), PathError::Empty);
        assert_eq!(reject("a/.."), PathError::ParentRef);
    }

    #[test]
    fn rejects_overlong_names() {
        let long = "a".repeat(300);
        assert_eq!(reject(&long), PathError::TooLong);
    }

    #[test]
    fn case_insensitive_containment_check() {
        // Windows 上 Root 与 root 是同一个目录，兜底判断不能因此误判逃逸
        let mixed = PathBuf::from("C:/DEVBOX/Store/Staging");
        assert!(safe_join(&mixed, "bin/java").is_ok());
    }
}
