//! 安全解压。
//!
//! 这是攻击面最大的地方：一个精心构造的压缩包可以写到任意路径、用符号链接
//! 把后续写入引到沙箱外、或者用压缩炸弹把磁盘写满。
//!
//! 铁律：**每一条目都先过 `devbox_core::safe_path::safe_join`，再落盘**。
//! 宁可拒绝一个合法但奇怪的文件名，也不放过一个可疑的。

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use devbox_core::safe_path::safe_join;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    TarGz,
}

impl ArchiveFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "zip" => Some(ArchiveFormat::Zip),
            "tar.gz" | "tgz" => Some(ArchiveFormat::TarGz),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExtractLimits {
    pub max_entries: usize,
    pub max_total_bytes: u64,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_entries: 200_000,
            max_total_bytes: 8 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractStats {
    pub files: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    Io(String),
    Unsafe(String),
    TooManyEntries,
    TooLarge,
    UnsupportedEntry(String),
    Corrupt(String),
    Empty,
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::Io(e) => write!(f, "读写失败: {e}"),
            ExtractError::Unsafe(p) => write!(f, "压缩包内含不安全路径: {p}"),
            ExtractError::TooManyEntries => write!(f, "条目数超出上限"),
            ExtractError::TooLarge => write!(f, "解压后体积超出上限"),
            ExtractError::UnsupportedEntry(k) => write!(f, "不支持的特殊条目: {k}"),
            ExtractError::Corrupt(e) => write!(f, "压缩包损坏: {e}"),
            ExtractError::Empty => write!(f, "压缩包内没有可用的文件"),
        }
    }
}

impl std::error::Error for ExtractError {}

pub fn extract(
    src: &Path,
    dest: &Path,
    format: ArchiveFormat,
    strip_components: usize,
    limits: ExtractLimits,
) -> Result<ExtractStats, ExtractError> {
    std::fs::create_dir_all(dest).map_err(|e| ExtractError::Io(e.to_string()))?;
    match format {
        ArchiveFormat::Zip => extract_zip(src, dest, strip_components, limits),
        ArchiveFormat::TarGz => extract_tar_gz(src, dest, strip_components, limits),
    }
}

fn extract_zip(
    src: &Path,
    dest: &Path,
    strip: usize,
    limits: ExtractLimits,
) -> Result<ExtractStats, ExtractError> {
    let file = File::open(src).map_err(|e| ExtractError::Io(e.to_string()))?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| ExtractError::Corrupt(e.to_string()))?;

    if archive.len() > limits.max_entries {
        return Err(ExtractError::TooManyEntries);
    }

    let mut stats = ExtractStats { files: 0, bytes: 0 };
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| ExtractError::Corrupt(e.to_string()))?;

        if entry.is_dir() {
            continue;
        }
        // 符号链接在 zip 里用 unix mode 标记，一律拒绝：它能把后续写入引到外面
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 {
                return Err(ExtractError::UnsupportedEntry(format!(
                    "symlink {}",
                    entry.name()
                )));
            }
        }

        reject_absolute(entry.name())?;
        let Some(relative) = strip_prefix(entry.name(), strip) else {
            continue;
        };
        let target = safe_join(dest, &relative)
            .map_err(|e| ExtractError::Unsafe(format!("{} ({e})", entry.name())))?;

        stats.bytes += copy_entry(&mut entry, &target, limits.max_total_bytes - stats.bytes)?;
        stats.files += 1;
        apply_mode(&target, &relative, entry.unix_mode());
    }

    if stats.files == 0 {
        return Err(ExtractError::Empty);
    }
    Ok(stats)
}

fn extract_tar_gz(
    src: &Path,
    dest: &Path,
    strip: usize,
    limits: ExtractLimits,
) -> Result<ExtractStats, ExtractError> {
    let file = File::open(src).map_err(|e| ExtractError::Io(e.to_string()))?;
    let decoder = flate2::read::GzDecoder::new(BufReader::new(file));
    let mut archive = tar::Archive::new(decoder);

    let mut stats = ExtractStats { files: 0, bytes: 0 };
    let mut entries = 0usize;

    for entry in archive
        .entries()
        .map_err(|e| ExtractError::Corrupt(e.to_string()))?
    {
        let mut entry = entry.map_err(|e| ExtractError::Corrupt(e.to_string()))?;
        entries += 1;
        if entries > limits.max_entries {
            return Err(ExtractError::TooManyEntries);
        }

        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        if kind.is_symlink() || kind.is_hard_link() {
            return Err(ExtractError::UnsupportedEntry(format!("link {:?}", entry.path())));
        }

        let raw = entry
            .path()
            .map_err(|e| ExtractError::Corrupt(e.to_string()))?
            .to_string_lossy()
            .to_string();

        reject_absolute(&raw)?;
        let Some(relative) = strip_prefix(&raw, strip) else {
            continue;
        };
        let target = safe_join(dest, &relative)
            .map_err(|e| ExtractError::Unsafe(format!("{raw} ({e})")))?;

        let mode = entry.header().mode().ok();
        stats.bytes += copy_entry(&mut entry, &target, limits.max_total_bytes - stats.bytes)?;
        stats.files += 1;
        apply_mode(&target, &relative, mode);
    }

    if stats.files == 0 {
        return Err(ExtractError::Empty);
    }
    Ok(stats)
}

/// 去掉开头的 N 层目录（`apache-maven-3.9.9/bin/mvn` → `bin/mvn`）。
/// 去完之后没有剩余内容（比如顶层目录本身）就跳过。
fn strip_prefix(name: &str, strip: usize) -> Option<String> {
    let parts: Vec<&str> = name
        .split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.len() <= strip {
        return None;
    }
    Some(parts[strip..].join("/"))
}

/// 绝对路径在压缩包里没有正当理由，说明打包方式有问题或者包被人动过。
/// 与其"顺手转成相对路径"放过它，不如直接拒绝并记录下来。
fn reject_absolute(name: &str) -> Result<(), ExtractError> {
    let bytes = name.as_bytes();
    let absolute = matches!(bytes.first(), Some(b'/') | Some(b'\\'))
        || (bytes.len() > 1 && bytes[1] == b':');
    if absolute {
        return Err(ExtractError::Unsafe(format!("绝对路径 {name}")));
    }
    Ok(())
}

fn copy_entry(
    reader: &mut impl Read,
    target: &Path,
    remaining: u64,
) -> Result<u64, ExtractError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ExtractError::Io(e.to_string()))?;
    }
    let mut out = File::create(target).map_err(|e| ExtractError::Io(e.to_string()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| ExtractError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > remaining {
            // 中途发现超标就立刻收手，不等解压完
            drop(out);
            std::fs::remove_file(target).ok();
            return Err(ExtractError::TooLarge);
        }
        out.write_all(&buf[..n])
            .map_err(|e| ExtractError::Io(e.to_string()))?;
    }
    Ok(written)
}

/// 决定解压后应有的权限位。返回 None 表示保持默认（0o644）。
///
/// 纯函数，所以在 Windows 上也能单测——真正 chmod 的那一步才需要 cfg(unix)。
pub fn exec_mode_for(relative: &str, archive_mode: Option<u32>) -> Option<u32> {
    let has_exec = archive_mode.map(|m| m & 0o111 != 0).unwrap_or(false);
    let looks_executable = relative
        .split('/')
        .any(|seg| seg == "bin" || seg == "sbin")
        || relative.ends_with(".sh");
    if has_exec || looks_executable {
        // 一律 0755：去掉 setuid/setgid/sticky，这类位在开发机上是纯粹的隐患
        Some(0o755)
    } else {
        None
    }
}

fn apply_mode(target: &Path, relative: &str, archive_mode: Option<u32>) {
    let Some(mode) = exec_mode_for(relative, archive_mode) else {
        return;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(mode)).ok();
    }
    #[cfg(not(unix))]
    {
        let _ = (target, mode);
    }
}

/// 把一段字节写成 zip，供测试与本地构造样例使用。
pub fn write_zip_for_test(path: &Path, entries: &[(&str, &[u8])]) -> std::io::Result<()> {
    use zip::write::SimpleFileOptions;
    let file = File::create(path)?;
    let mut writer = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    for (name, body) in entries {
        writer
            .start_file(*name, options)
            .map_err(std::io::Error::other)?;
        writer.write_all(body)?;
    }
    writer.finish().map_err(std::io::Error::other)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devbox-extract-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extracts_zip_and_strips_top_dir() {
        let dir = tmp("ok");
        let zip_path = dir.join("pkg.zip");
        write_zip_for_test(
            &zip_path,
            &[
                ("apache-maven-3.9.9/bin/mvn", b"#!/bin/sh\necho mvn\n"),
                ("apache-maven-3.9.9/conf/settings.xml", b"<settings/>"),
            ],
        )
        .unwrap();

        let out = dir.join("out");
        let stats = extract(&zip_path, &out, ArchiveFormat::Zip, 1, ExtractLimits::default()).unwrap();
        assert_eq!(stats.files, 2);
        assert!(out.join("bin").join("mvn").is_file());
        assert!(out.join("conf").join("settings.xml").is_file());
        assert!(!out.join("apache-maven-3.9.9").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_traversal_inside_archive() {
        let dir = tmp("evil");
        let zip_path = dir.join("evil.zip");
        write_zip_for_test(&zip_path, &[("dir/../../evil.txt", b"pwned")]).unwrap();

        let out = dir.join("out");
        let err = extract(&zip_path, &out, ArchiveFormat::Zip, 0, ExtractLimits::default())
            .unwrap_err();
        assert!(matches!(err, ExtractError::Unsafe(_)), "got {err:?}");
        // 关键：外面不能出现文件
        assert!(!dir.join("evil.txt").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_absolute_path_inside_archive() {
        let dir = tmp("abs");
        let zip_path = dir.join("abs.zip");
        write_zip_for_test(&zip_path, &[("/etc/passwd", b"x")]).unwrap();

        let out = dir.join("out");
        let err = extract(&zip_path, &out, ArchiveFormat::Zip, 0, ExtractLimits::default())
            .unwrap_err();
        assert!(matches!(err, ExtractError::Unsafe(_)), "got {err:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn enforces_size_limit_midway() {
        let dir = tmp("bomb");
        let zip_path = dir.join("big.zip");
        let payload = vec![0u8; 2 * 1024 * 1024];
        write_zip_for_test(&zip_path, &[("big.bin", &payload)]).unwrap();

        let out = dir.join("out");
        let limits = ExtractLimits {
            max_entries: 10,
            max_total_bytes: 1024,
        };
        let err = extract(&zip_path, &out, ArchiveFormat::Zip, 0, limits).unwrap_err();
        assert_eq!(err, ExtractError::TooLarge);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn strip_prefix_is_bounded() {
        assert_eq!(strip_prefix("a/b/c", 1), Some("b/c".into()));
        assert_eq!(strip_prefix("a/b/c", 0), Some("a/b/c".into()));
        assert_eq!(strip_prefix("a", 1), None);
        assert_eq!(strip_prefix("./a/b", 1), Some("b".into()));
    }

    #[test]
    fn exec_mode_keeps_bin_and_scripts_runnable() {
        assert_eq!(exec_mode_for("bin/mvn", None), Some(0o755));
        assert_eq!(exec_mode_for("sbin/nginx", None), Some(0o755));
        assert_eq!(exec_mode_for("tools/run.sh", None), Some(0o755));
        assert_eq!(exec_mode_for("lib/jansi.jar", None), None);
        assert_eq!(exec_mode_for("README.txt", None), None);
        // 归档里本来就带执行位 → 保持可执行
        assert_eq!(exec_mode_for("weird-name", Some(0o100755)), Some(0o755));
        // setuid 位不能被保留成高位
        assert_eq!(exec_mode_for("bin/su", Some(0o104755)), Some(0o755));
    }

    #[test]
    fn format_parsing() {
        assert_eq!(ArchiveFormat::parse("zip"), Some(ArchiveFormat::Zip));
        assert_eq!(ArchiveFormat::parse("TAR.GZ"), Some(ArchiveFormat::TarGz));
        assert_eq!(ArchiveFormat::parse("tgz"), Some(ArchiveFormat::TarGz));
        assert_eq!(ArchiveFormat::parse("rar"), None);
    }
}
