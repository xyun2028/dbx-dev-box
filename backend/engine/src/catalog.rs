//! 组件目录（catalog）解析。
//!
//! 这是"加组件只写 YAML"的兑现处：引擎不认识任何具体组件，只认这份结构。
//!
//! 三条硬规则在这里落地：
//! 1. 校验值必须是真实 hex。`<REPLACE_ME>` 一律拒绝——占位符绝不能溜进正式安装。
//! 2. 平台缺失就是缺失，不拿别的平台的包凑数。
//! 3. 目录随插件发版（走商店审核），所以这里不做远程拉取、不验签——
//!    整包已被商店的 Ed25519 签名覆盖。

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::store::InstallSpec;

#[derive(Debug, Deserialize)]
pub struct Catalog {
    pub schema: u32,
    pub revision: String,
    pub components: Vec<Component>,
}

#[derive(Debug, Deserialize)]
pub struct Component {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub versions: BTreeMap<String, VersionEntry>,
}

#[derive(Debug, Deserialize)]
pub struct VersionEntry {
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub artifacts: BTreeMap<String, Artifact>,
    #[serde(default)]
    pub requires: Vec<RequirementEntry>,
    #[serde(default)]
    pub provides: Option<Provides>,
}

#[derive(Debug, Deserialize)]
pub struct RequirementEntry {
    pub component: String,
    pub range: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct Provides {
    #[serde(default)]
    pub binaries: Vec<String>,
    #[serde(default)]
    pub default_port: Option<u16>,
}

#[derive(Debug, Deserialize)]
pub struct Artifact {
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub sha512: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    pub format: String,
    #[serde(default)]
    pub strip_components: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    Io(String),
    Parse(String),
    UnsupportedSchema(u32),
    UnknownComponent(String),
    UnknownVersion { component: String, version: String },
    PlatformUnavailable { component: String, version: String, platform: String, available: Vec<String> },
    MissingChecksum { component: String, version: String, platform: String },
    PlaceholderChecksum { component: String, version: String, platform: String },
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogError::Io(e) => write!(f, "读取目录失败: {e}"),
            CatalogError::Parse(e) => write!(f, "目录格式错误: {e}"),
            CatalogError::UnsupportedSchema(v) => write!(f, "不支持的目录 schema 版本: {v}"),
            CatalogError::UnknownComponent(c) => write!(f, "目录里没有组件 {c}"),
            CatalogError::UnknownVersion { component, version } => {
                write!(f, "{component} 没有版本 {version}")
            }
            CatalogError::PlatformUnavailable { component, version, platform, available } => write!(
                f,
                "{component} {version} 在当前平台 {platform} 不可用；可用平台: {}",
                available.join(", ")
            ),
            CatalogError::MissingChecksum { component, version, platform } => write!(
                f,
                "{component} {version} ({platform}) 既没有 sha256 也没有 sha512，拒绝安装"
            ),
            CatalogError::PlaceholderChecksum { component, version, platform } => write!(
                f,
                "{component} {version} ({platform}) 的校验值仍是占位符，说明这份配方还没被真正验过"
            ),
        }
    }
}

impl std::error::Error for CatalogError {}

pub const PLACEHOLDER_MARKERS: &[&str] = &["REPLACE_ME", "TODO", "PLACEHOLDER", "XXX"];

impl Catalog {
    pub fn from_str(text: &str) -> Result<Self, CatalogError> {
        let catalog: Catalog =
            serde_yaml::from_str(text).map_err(|e| CatalogError::Parse(e.to_string()))?;
        if catalog.schema != 1 {
            return Err(CatalogError::UnsupportedSchema(catalog.schema));
        }
        Ok(catalog)
    }

    pub fn from_file(path: &Path) -> Result<Self, CatalogError> {
        let text = std::fs::read_to_string(path).map_err(|e| CatalogError::Io(e.to_string()))?;
        Self::from_str(&text)
    }

    pub fn component(&self, id: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.id == id)
    }

    /// 把"组件 + 版本 + 平台"翻译成一次安装需要的全部信息。
    pub fn install_spec(
        &self,
        component_id: &str,
        version: &str,
        platform: &str,
    ) -> Result<InstallSpec, CatalogError> {
        let component = self
            .component(component_id)
            .ok_or_else(|| CatalogError::UnknownComponent(component_id.to_string()))?;

        let entry = component
            .versions
            .get(version)
            .ok_or_else(|| CatalogError::UnknownVersion {
                component: component_id.to_string(),
                version: version.to_string(),
            })?;

        let artifact = entry.artifacts.get(platform).ok_or_else(|| {
            CatalogError::PlatformUnavailable {
                component: component_id.to_string(),
                version: version.to_string(),
                platform: platform.to_string(),
                available: entry.artifacts.keys().cloned().collect(),
            }
        })?;

        let checksum = match (&artifact.sha512, &artifact.sha256) {
            (Some(v), _) if !v.trim().is_empty() => v.trim().to_string(),
            (_, Some(v)) if !v.trim().is_empty() => v.trim().to_string(),
            _ => {
                return Err(CatalogError::MissingChecksum {
                    component: component_id.to_string(),
                    version: version.to_string(),
                    platform: platform.to_string(),
                })
            }
        };

        // 占位符必须在解析期就拦住，不能等到下载完才发现
        let upper = checksum.to_ascii_uppercase();
        if PLACEHOLDER_MARKERS.iter().any(|m| upper.contains(m))
            || checksum.trim_start().starts_with('<')
        {
            return Err(CatalogError::PlaceholderChecksum {
                component: component_id.to_string(),
                version: version.to_string(),
                platform: platform.to_string(),
            });
        }

        Ok(InstallSpec {
            component: component.id.clone(),
            version: version.to_string(),
            platform: platform.to_string(),
            url: artifact.url.clone(),
            checksum,
            format: artifact.format.clone(),
            strip_components: artifact.strip_components.unwrap_or(0),
            mirrors: Vec::new(),
        })
    }

    /// 给 UI / 诊断用：某个组件在当前平台可安装的版本列表。
    pub fn installable_versions(&self, component_id: &str, platform: &str) -> Vec<String> {
        self.component(component_id)
            .map(|c| {
                c.versions
                    .iter()
                    .filter(|(_, v)| v.artifacts.contains_key(platform))
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 直接解析仓库里真实发布的那份 YAML——防止 schema 漂移，
    /// 也保证"改配方不用改代码"这件事一直成立。
    const SHIPPED: &str = include_str!("../../../catalog/jdk.yaml");

    #[test]
    fn shipped_catalog_parses() {
        let c = Catalog::from_str(SHIPPED).expect("shipped catalog must parse");
        assert_eq!(c.schema, 1);
        assert!(!c.revision.is_empty());
        assert!(c.component("maven").is_some());
        assert!(c.component("mysql").is_some());
    }

    #[test]
    fn resolves_maven_spec_from_shipped_catalog() {
        let c = Catalog::from_str(SHIPPED).unwrap();
        let spec = c
            .install_spec("maven", "3.9.9", "windows-x64")
            .expect("maven 3.9.9 should be installable");
        assert_eq!(spec.format, "zip");
        assert_eq!(spec.strip_components, 1);
        assert!(spec.url.ends_with("apache-maven-3.9.9-bin.zip"));
        assert_eq!(spec.checksum.len(), 128, "应该是 sha512");
    }

    #[test]
    fn placeholder_checksum_is_rejected() {
        let c = Catalog::from_str(SHIPPED).unwrap();
        // jdk 还没被真正验过，必须拒绝，不能"试试看"
        let err = c.install_spec("jdk", "21.0.5", "windows-x64").unwrap_err();
        assert!(
            matches!(err, CatalogError::PlaceholderChecksum { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains("占位符"));
    }

    #[test]
    fn missing_platform_is_not_silently_substituted() {
        let c = Catalog::from_str(SHIPPED).unwrap();
        let err = c.install_spec("maven", "3.9.9", "plan9-x64").unwrap_err();
        match err {
            CatalogError::PlatformUnavailable { available, .. } => {
                assert!(available.contains(&"windows-x64".to_string()));
            }
            other => panic!("expected PlatformUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn unknown_version_and_component() {
        let c = Catalog::from_str(SHIPPED).unwrap();
        assert!(matches!(
            c.install_spec("maven", "0.0.1", "windows-x64").unwrap_err(),
            CatalogError::UnknownVersion { .. }
        ));
        assert!(matches!(
            c.install_spec("nope", "1.0.0", "windows-x64").unwrap_err(),
            CatalogError::UnknownComponent(_)
        ));
    }

    #[test]
    fn installable_versions_filters_by_platform() {
        let c = Catalog::from_str(SHIPPED).unwrap();
        let v = c.installable_versions("maven", "windows-x64");
        assert!(v.contains(&"3.9.9".to_string()));
        assert!(c.installable_versions("maven", "plan9-x64").is_empty());
    }

    #[test]
    fn bad_yaml_and_bad_schema_are_reported() {
        assert!(matches!(
            Catalog::from_str("schema: [").unwrap_err(),
            CatalogError::Parse(_)
        ));
        let err = Catalog::from_str("schema: 2\nrevision: x\ncomponents: []\n").unwrap_err();
        assert!(matches!(err, CatalogError::UnsupportedSchema(2)));
    }
}
