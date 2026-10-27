//! 依赖解析与安装顺序。
//!
//! 决策（2026-10-27）：**dev-box 管依赖**——Nacos 要 JDK 17+、xxl-job-admin 还要一个
//! 初始化好的 MySQL 库，这些都由我们负责，而不是丢给用户"你先自己装个 JDK"。
//!
//! 三个关键规则：
//! 1. **本机后端才需要宿主运行时依赖**。容器镜像自带 JRE，所以容器模式下
//!    `jdk` 这类依赖不成立；但 `mysql` 这类服务依赖依然成立，且可以是另一个
//!    容器的实例。依赖图按后端不同，不是一张固定的图。
//! 2. **实例创建时把依赖版本钉死**，不跟随全局激活指针。全局切到 JDK 8，
//!    已经跑着的 Nacos 不能因此挂掉。
//! 3. 冲突要给出**人能读的冲突链**，而不是一句 "version conflict"。

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let mut parts = s.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().map(|p| p.parse().ok()).unwrap_or(Some(0))?;
        let patch = parts.next().map(|p| p.parse().ok()).unwrap_or(Some(0))?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Gte,
    Gt,
    Lte,
    Lt,
    Eq,
    /// `^1.2.3` → >=1.2.3 <2.0.0
    Caret,
    /// `~1.2.3` → >=1.2.3 <1.3.0
    Tilde,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparator {
    op: Op,
    version: Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRange {
    comparators: Vec<Comparator>,
    raw: String,
}

impl VersionRange {
    /// 空格分隔表示"同时满足"。不支持 `||`——真需要时应该拆成两个依赖声明，
    /// 而不是把复杂度藏进一行字符串。
    pub fn parse(s: &str) -> Result<Self, RangeError> {
        let raw = s.trim().to_string();
        if raw.is_empty() {
            return Err(RangeError::Empty);
        }
        let mut comparators = Vec::new();
        for token in raw.split_whitespace() {
            if let Some(rest) = token.strip_prefix(">=") {
                comparators.push(Comparator { op: Op::Gte, version: parse_ver(rest)? });
            } else if let Some(rest) = token.strip_prefix("<=") {
                comparators.push(Comparator { op: Op::Lte, version: parse_ver(rest)? });
            } else if let Some(rest) = token.strip_prefix('>') {
                comparators.push(Comparator { op: Op::Gt, version: parse_ver(rest)? });
            } else if let Some(rest) = token.strip_prefix('<') {
                comparators.push(Comparator { op: Op::Lt, version: parse_ver(rest)? });
            } else if let Some(rest) = token.strip_prefix('^') {
                comparators.push(Comparator { op: Op::Caret, version: parse_ver(rest)? });
            } else if let Some(rest) = token.strip_prefix('~') {
                comparators.push(Comparator { op: Op::Tilde, version: parse_ver(rest)? });
            } else if let Some(rest) = token.strip_prefix('=') {
                comparators.push(Comparator { op: Op::Eq, version: parse_ver(rest)? });
            } else {
                return Err(RangeError::MissingOperator(token.to_string()));
            }
        }
        if comparators.is_empty() {
            return Err(RangeError::Empty);
        }
        Ok(Self { comparators, raw })
    }

    pub fn matches(&self, v: &Version) -> bool {
        self.comparators.iter().all(|c| match c.op {
            Op::Gte => *v >= c.version,
            Op::Gt => *v > c.version,
            Op::Lte => *v <= c.version,
            Op::Lt => *v < c.version,
            Op::Eq => *v == c.version,
            Op::Caret => {
                *v >= c.version
                    && v.major == c.version.major
            }
            Op::Tilde => {
                *v >= c.version && v.major == c.version.major && v.minor == c.version.minor
            }
        })
    }
}

impl std::fmt::Display for VersionRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

fn parse_ver(s: &str) -> Result<Version, RangeError> {
    Version::parse(s).ok_or_else(|| RangeError::BadVersion(s.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeError {
    Empty,
    MissingOperator(String),
    BadVersion(String),
}

impl std::fmt::Display for RangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RangeError::Empty => write!(f, "版本范围为空"),
            RangeError::MissingOperator(t) => write!(f, "缺少比较符: {t}（应为 >=17 <22 这种写法）"),
            RangeError::BadVersion(v) => write!(f, "版本号无法解析: {v}"),
        }
    }
}

impl std::error::Error for RangeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// 谁提出的（组件 id），用于冲突时打印链路
    pub by: String,
    pub component: String,
    pub range: VersionRange,
}

impl Requirement {
    pub fn new(by: &str, component: &str, range: &str) -> Result<Self, RangeError> {
        Ok(Self {
            by: by.to_string(),
            component: component.to_string(),
            range: VersionRange::parse(range)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    NoVersion { component: String, range: String },
    /// 多个依赖对同一个组件的范围没有交集
    Conflict {
        component: String,
        demands: Vec<(String, String)>,
        available: Vec<String>,
    },
    Cycle { path: Vec<String> },
    UnknownComponent(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::NoVersion { component, range } => {
                write!(f, "{component} 没有满足 {range} 的版本")
            }
            ResolveError::Conflict {
                component,
                demands,
                available,
            } => {
                let chain = demands
                    .iter()
                    .map(|(who, range)| format!("{who} 需要 {component} {range}"))
                    .collect::<Vec<_>>()
                    .join("，");
                write!(
                    f,
                    "{chain}，三者无交集；可选版本：{}",
                    available.join(", ")
                )
            }
            ResolveError::Cycle { path } => write!(f, "依赖成环: {}", path.join(" -> ")),
            ResolveError::UnknownComponent(c) => write!(f, "未知组件: {c}"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// 给定一组依赖需求，为每个组件选出一个同时满足所有约束的**最高**版本。
///
/// 返回的 map 可以直接冻结进实例：创建后不再随全局激活版本变化。
pub fn resolve_set(
    under: &Requirement,
    requirements: &[Requirement],
    available: &BTreeMap<String, Vec<Version>>,
) -> Result<BTreeMap<String, Version>, ResolveError> {
    let mut grouped: BTreeMap<&str, Vec<&Requirement>> = BTreeMap::new();
    grouped.entry(under.component.as_str()).or_default().push(under);
    for req in requirements {
        grouped.entry(req.component.as_str()).or_default().push(req);
    }

    let mut chosen = BTreeMap::new();
    for (component, demands) in grouped {
        let versions = available
            .get(component)
            .ok_or_else(|| ResolveError::UnknownComponent(component.to_string()))?;

        let pick = versions
            .iter()
            .filter(|v| demands.iter().all(|d| d.range.matches(v)))
            .max();

        match pick {
            Some(v) => {
                chosen.insert(component.to_string(), *v);
            }
            None => {
                // 只有一个需求时是"没有可用版本"，多个需求时是"约束打架"——
                // 这两件事给用户的提示应该不一样。
                if demands.len() == 1 {
                    return Err(ResolveError::NoVersion {
                        component: component.to_string(),
                        range: demands[0].range.to_string(),
                    });
                }
                return Err(ResolveError::Conflict {
                    component: component.to_string(),
                    demands: demands
                        .iter()
                        .map(|d| (d.by.clone(), d.range.to_string()))
                        .collect(),
                    available: versions.iter().map(|v| v.to_string()).collect(),
                });
            }
        }
    }
    Ok(chosen)
}

/// 按依赖关系给出安装顺序（被依赖的在前）。
///
/// `graph` 里每一项是 (组件, 它依赖的组件列表)。
pub fn install_order(graph: &[(String, Vec<String>)]) -> Result<Vec<String>, ResolveError> {
    let mut order: Vec<String> = Vec::new();
    let mut done: Vec<String> = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let lookup: BTreeMap<&str, &Vec<String>> =
        graph.iter().map(|(k, v)| (k.as_str(), v)).collect();

    fn visit(
        node: &str,
        lookup: &BTreeMap<&str, &Vec<String>>,
        done: &mut Vec<String>,
        stack: &mut Vec<String>,
        order: &mut Vec<String>,
    ) -> Result<(), ResolveError> {
        if done.iter().any(|d| d == node) {
            return Ok(());
        }
        if stack.iter().any(|s| s == node) {
            let mut path = stack.clone();
            path.push(node.to_string());
            return Err(ResolveError::Cycle { path });
        }
        stack.push(node.to_string());
        if let Some(deps) = lookup.get(node) {
            for dep in deps.iter() {
                if lookup.contains_key(dep.as_str()) {
                    visit(dep, lookup, done, stack, order)?;
                }
            }
        }
        stack.pop();
        done.push(node.to_string());
        order.push(node.to_string());
        Ok(())
    }

    for (name, _) in graph {
        visit(name, &lookup, &mut done, &mut stack, &mut order)?;
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(list: &[&str]) -> Vec<Version> {
        list.iter().map(|s| Version::parse(s).unwrap()).collect()
    }

    fn available() -> BTreeMap<String, Vec<Version>> {
        let mut m = BTreeMap::new();
        m.insert("jdk".to_string(), versions(&["8.0.432", "17.0.13", "21.0.5"]));
        m.insert("mysql".to_string(), versions(&["5.7.44", "8.0.40", "8.4.3"]));
        m
    }

    #[test]
    fn version_parse_cases() {
        assert_eq!(Version::parse("21"), Some(Version { major: 21, minor: 0, patch: 0 }));
        assert_eq!(Version::parse("17.0"), Some(Version { major: 17, minor: 0, patch: 0 }));
        assert_eq!(
            Version::parse("8.0.432"),
            Some(Version { major: 8, minor: 0, patch: 432 })
        );
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("21.x"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
    }

    #[test]
    fn range_parsing_and_matching() {
        let r = VersionRange::parse(">=17 <22").unwrap();
        assert!(r.matches(&Version::parse("17.0.0").unwrap()));
        assert!(r.matches(&Version::parse("21.0.5").unwrap()));
        assert!(!r.matches(&Version::parse("8.0.432").unwrap()));
        assert!(!r.matches(&Version::parse("22.0.0").unwrap()));

        let caret = VersionRange::parse("^1.2").unwrap();
        assert!(caret.matches(&Version::parse("1.9.0").unwrap()));
        assert!(!caret.matches(&Version::parse("2.0.0").unwrap()));

        assert!(VersionRange::parse("17").is_err(), "缺比较符要报错，不能猜");
        assert!(VersionRange::parse("").is_err());
    }

    #[test]
    fn picks_highest_satisfying_version() {
        let under = Requirement::new("nacos", "jdk", ">=17 <22").unwrap();
        let chosen = resolve_set(&under, &[], &available()).unwrap();
        assert_eq!(chosen["jdk"], Version::parse("21.0.5").unwrap());
    }

    #[test]
    fn intersection_of_two_requirements() {
        // nacos 要 17~21，另一个要 <21 → 取 17
        let under = Requirement::new("nacos", "jdk", ">=17 <22").unwrap();
        let extra = vec![Requirement::new("legacy-app", "jdk", ">=8 <21").unwrap()];
        let chosen = resolve_set(&under, &extra, &available()).unwrap();
        assert_eq!(chosen["jdk"], Version::parse("17.0.13").unwrap());
    }

    #[test]
    fn conflict_prints_readable_chain() {
        let under = Requirement::new("nacos", "jdk", ">=17").unwrap();
        let extra = vec![Requirement::new("legacy-app", "jdk", "<17").unwrap()];
        let err = resolve_set(&under, &extra, &available()).unwrap_err();
        match &err {
            ResolveError::Conflict { component, demands, .. } => {
                assert_eq!(component, "jdk");
                assert_eq!(demands.len(), 2);
            }
            other => panic!("expected conflict, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("nacos") && msg.contains("legacy-app"), "{msg}");
        assert!(msg.contains("21.0.5"), "冲突提示要带上可选版本: {msg}");
    }

    #[test]
    fn missing_version_is_not_a_conflict() {
        let under = Requirement::new("app", "jdk", ">=23").unwrap();
        let err = resolve_set(&under, &[], &available()).unwrap_err();
        assert!(matches!(err, ResolveError::NoVersion { .. }));
    }

    #[test]
    fn install_order_puts_dependencies_first() {
        let graph = vec![
            ("nacos".to_string(), vec!["jdk".to_string()]),
            ("xxl-job-admin".to_string(), vec!["jdk".to_string(), "mysql".to_string()]),
            ("jdk".to_string(), vec![]),
            ("mysql".to_string(), vec![]),
        ];
        let order = install_order(&graph).unwrap();
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("jdk") < pos("nacos"));
        assert!(pos("jdk") < pos("xxl-job-admin"));
        assert!(pos("mysql") < pos("xxl-job-admin"));
    }

    #[test]
    fn cycle_is_detected_with_path() {
        let graph = vec![
            ("a".to_string(), vec!["b".to_string()]),
            ("b".to_string(), vec!["a".to_string()]),
        ];
        let err = install_order(&graph).unwrap_err();
        match err {
            ResolveError::Cycle { path } => assert!(path.len() >= 2),
            other => panic!("expected cycle, got {other:?}"),
        }
    }
}
