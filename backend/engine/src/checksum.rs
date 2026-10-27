//! 校验和解析与流式计算。

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

use sha2::{Digest, Sha256, Sha512};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha256,
    Sha512,
}

impl Algorithm {
    /// 按十六进制长度判断算法。64 位是 sha256，128 位是 sha512。
    pub fn from_hex_len(len: usize) -> Option<Self> {
        match len {
            64 => Some(Algorithm::Sha256),
            128 => Some(Algorithm::Sha512),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha512 => "sha512",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checksum {
    pub algorithm: Algorithm,
    pub hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChecksumError {
    Empty,
    NotHex,
    BadLength(usize),
}

impl std::fmt::Display for ChecksumError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChecksumError::Empty => write!(f, "校验值为空"),
            ChecksumError::NotHex => write!(f, "校验值含非十六进制字符"),
            ChecksumError::BadLength(n) => write!(f, "校验值长度 {n} 不合法（应为 64 或 128）"),
        }
    }
}

impl std::error::Error for ChecksumError {}

impl Checksum {
    /// 接受纯 hex，也接受 `sha512:abc...` / `abc...  filename` 这类粘贴形式。
    pub fn parse(raw: &str) -> Result<Self, ChecksumError> {
        let cleaned = raw.trim();
        if cleaned.is_empty() {
            return Err(ChecksumError::Empty);
        }
        // 去掉 "sha512:" 前缀
        let after_algo = cleaned
            .split_once(':')
            .map(|(_, rest)| rest)
            .unwrap_or(cleaned);
        // 官方 checksum 文件常常是 "hash  filename"
        let hex = after_algo.split_whitespace().next().unwrap_or("");
        let hex = hex.trim().to_ascii_lowercase();

        if hex.is_empty() {
            return Err(ChecksumError::Empty);
        }
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ChecksumError::NotHex);
        }
        let algorithm =
            Algorithm::from_hex_len(hex.len()).ok_or(ChecksumError::BadLength(hex.len()))?;
        Ok(Self { algorithm, hex })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    Io(String),
    Mismatch { expected: String, actual: String },
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::Io(e) => write!(f, "读取文件失败: {e}"),
            VerifyError::Mismatch { expected, actual } => {
                write!(f, "校验不匹配: 期望 {expected}，实际 {actual}")
            }
        }
    }
}

impl std::error::Error for VerifyError {}

/// 流式校验，不把整个包读进内存（MySQL 那种 290MB 的包必须这样）。
pub fn verify_file(path: &Path, expected: &Checksum) -> Result<(), VerifyError> {
    let file = File::open(path).map_err(|e| VerifyError::Io(e.to_string()))?;
    let mut reader = BufReader::new(file);
    let actual = match expected.algorithm {
        Algorithm::Sha256 => {
            let mut hasher = Sha256::new();
            copy_into(&mut reader, &mut hasher)?;
            to_hex(&hasher.finalize())
        }
        Algorithm::Sha512 => {
            let mut hasher = Sha512::new();
            copy_into(&mut reader, &mut hasher)?;
            to_hex(&hasher.finalize())
        }
    };
    if actual != expected.hex {
        return Err(VerifyError::Mismatch {
            expected: expected.hex.clone(),
            actual,
        });
    }
    Ok(())
}

fn copy_into<R: Read, D: Digest>(reader: &mut R, hasher: &mut D) -> Result<(), VerifyError> {
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| VerifyError::Io(e.to_string()))?;
        if n == 0 {
            return Ok(());
        }
        hasher.update(&buf[..n]);
    }
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 只用于测试与本地计算：给出文件的 hex 摘要。
pub fn hash_file(path: &Path, algorithm: Algorithm) -> io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    Ok(match algorithm {
        Algorithm::Sha256 => {
            let mut h = Sha256::new();
            copy_into(&mut reader, &mut h).map_err(|e| io::Error::other(e.to_string()))?;
            to_hex(&h.finalize())
        }
        Algorithm::Sha512 => {
            let mut h = Sha512::new();
            copy_into(&mut reader, &mut h).map_err(|e| io::Error::other(e.to_string()))?;
            to_hex(&h.finalize())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_bare_hex_and_pastes() {
        let sha256 = "a".repeat(64);
        assert_eq!(Checksum::parse(&sha256).unwrap().algorithm, Algorithm::Sha256);

        let sha512 = "b".repeat(128);
        let c = Checksum::parse(&format!("{sha512}  apache-maven.zip")).unwrap();
        assert_eq!(c.algorithm, Algorithm::Sha512);
        assert_eq!(c.hex, sha512);

        let c = Checksum::parse(&format!("sha512:{}", "c".repeat(128))).unwrap();
        assert_eq!(c.algorithm, Algorithm::Sha512);
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(Checksum::parse(""), Err(ChecksumError::Empty));
        assert_eq!(Checksum::parse("zzz"), Err(ChecksumError::NotHex));
        assert!(matches!(
            Checksum::parse("abc123"),
            Err(ChecksumError::BadLength(6))
        ));
    }

    #[test]
    fn verifies_real_content_and_detects_mismatch() {
        let dir = std::env::temp_dir().join("devbox-verify-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("blob.bin");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"hello devbox").unwrap();
        drop(f);

        let sha256 = hash_file(&path, Algorithm::Sha256).unwrap();
        let expected = Checksum::parse(&sha256).unwrap();
        verify_file(&path, &expected).expect("should match");

        let wrong = Checksum::parse(&"0".repeat(64)).unwrap();
        let err = verify_file(&path, &wrong).unwrap_err();
        assert!(matches!(err, VerifyError::Mismatch { .. }));

        std::fs::remove_dir_all(&dir).ok();
    }
}
