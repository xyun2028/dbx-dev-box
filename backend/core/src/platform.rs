//! 平台抽象。
//!
//! 三平台并进的核心不是"写三份代码"，而是把差异收敛到这一层：
//! 制品 key、二进制后缀、目录链接方式、可执行位、长路径前缀。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X64,
    Arm64,
}

impl Os {
    fn as_str(self) -> &'static str {
        match self {
            Os::Windows => "windows",
            Os::MacOs => "darwin",
            Os::Linux => "linux",
        }
    }
}

impl Arch {
    fn as_str(self) -> &'static str {
        match self {
            Arch::X64 => "x64",
            Arch::Arm64 => "arm64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformKey {
    pub os: Os,
    pub arch: Arch,
}

impl PlatformKey {
    pub fn new(os: Os, arch: Arch) -> Self {
        Self { os, arch }
    }

    /// 当前进程所在平台。未知组合一律 panic 在编译期之外的地方暴露，
    /// 宁可构建失败，也不要静默按 x64 处理 arm64 机器。
    pub fn current() -> Self {
        let os = if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            panic!("unsupported target os")
        };

        let arch = if cfg!(target_arch = "x86_64") {
            Arch::X64
        } else if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else {
            panic!("unsupported target arch")
        };

        Self { os, arch }
    }

    /// catalog 里声明制品时用的 key，例如 `windows-x64` / `darwin-arm64`。
    pub fn key(&self) -> String {
        format!("{}-{}", self.os.as_str(), self.arch.as_str())
    }

    pub fn parse(s: &str) -> Option<Self> {
        let (os, arch) = s.rsplit_once('-')?;
        let os = match os {
            "windows" => Os::Windows,
            "darwin" | "macos" | "mac" => Os::MacOs,
            "linux" => Os::Linux,
            _ => return None,
        };
        let arch = match arch {
            "x64" | "x86_64" | "amd64" => Arch::X64,
            "arm64" | "aarch64" => Arch::Arm64,
            _ => return None,
        };
        Some(Self { os, arch })
    }

    pub fn binary_name(&self, stem: &str) -> String {
        match self.os {
            Os::Windows => format!("{stem}.exe"),
            _ => stem.to_string(),
        }
    }

    /// 是否需要给解压出来的 bin/* 补可执行位。
    pub fn needs_exec_bit(&self) -> bool {
        self.os != Os::Windows
    }

    /// Windows 的 symlink 需要管理员或开发者模式，所以目录激活用 junction；
    /// 这里返回"是否需要走 junction 语义"。
    pub fn uses_junction_for_dirs(&self) -> bool {
        self.os == Os::Windows
    }

    /// Windows 单条路径超过 260 字符需要 `\\?\` 前缀。其他平台原样返回。
    pub fn native_path(&self, p: &str) -> String {
        if self.os == Os::Windows && p.len() > 240 && !p.starts_with(r"\\?\") {
            let p = p.replace('/', r"\");
            if p.starts_with(r"\\") {
                format!(r"\\?\UNC\{}", p.trim_start_matches(r"\\"))
            } else {
                format!(r"\\?\{p}")
            }
        } else {
            p.to_string()
        }
    }

    /// 大小写敏感的只有 Linux。文件名比较时用它决定是否降小写。
    pub fn case_sensitive(&self) -> bool {
        self.os == Os::Linux
    }
}

impl fmt::Display for PlatformKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_roundtrip() {
        for os in ["windows", "darwin", "linux"] {
            for arch in ["x64", "arm64"] {
                let s = format!("{os}-{arch}");
                let k = PlatformKey::parse(&s).expect("should parse");
                assert_eq!(k.key(), s);
            }
        }
    }

    #[test]
    fn parse_rejects_unknown() {
        assert!(PlatformKey::parse("win32-x64").is_none());
        assert!(PlatformKey::parse("linux-riscv64").is_none());
        assert!(PlatformKey::parse("linux").is_none());
        assert!(PlatformKey::parse("").is_none());
    }

    #[test]
    fn parse_accepts_aliases() {
        assert_eq!(
            PlatformKey::parse("macos-aarch64"),
            Some(PlatformKey::new(Os::MacOs, Arch::Arm64))
        );
        assert_eq!(
            PlatformKey::parse("linux-amd64"),
            Some(PlatformKey::new(Os::Linux, Arch::X64))
        );
    }

    #[test]
    fn binary_name_only_windows_gets_exe() {
        assert_eq!(
            PlatformKey::new(Os::Windows, Arch::X64).binary_name("mysqld"),
            "mysqld.exe"
        );
        assert_eq!(
            PlatformKey::new(Os::Linux, Arch::X64).binary_name("mysqld"),
            "mysqld"
        );
    }

    #[test]
    fn platform_capability_flags() {
        let win = PlatformKey::new(Os::Windows, Arch::X64);
        let mac = PlatformKey::new(Os::MacOs, Arch::Arm64);
        let linux = PlatformKey::new(Os::Linux, Arch::X64);

        assert!(!win.needs_exec_bit());
        assert!(mac.needs_exec_bit());
        assert!(win.uses_junction_for_dirs());
        assert!(!linux.uses_junction_for_dirs());
        assert!(linux.case_sensitive());
        assert!(!mac.case_sensitive());
    }

    #[test]
    fn long_path_prefix_only_on_windows() {
        let long = format!("C:/{}", "a".repeat(300));
        let win = PlatformKey::new(Os::Windows, Arch::X64);
        assert!(win.native_path(&long).starts_with(r"\\?\C:\"));

        let linux = PlatformKey::new(Os::Linux, Arch::X64);
        assert_eq!(linux.native_path(&long), long);

        // 短路径不该被加前缀
        assert_eq!(win.native_path("C:/x/y"), "C:/x/y");
    }

    #[test]
    fn current_platform_key_is_known() {
        let k = PlatformKey::current().key();
        assert!(PlatformKey::parse(&k).is_some(), "{k}");
    }
}
