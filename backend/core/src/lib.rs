//! dev-box 引擎核心。
//!
//! 设计约束（见 docs/plugin-architecture.md）：
//! - 这一层不知道 DBX 的存在，也不做任何 IO 编排，因此可以脱离宿主单测。
//! - 所有"能不能"的判断都在这里：状态能否跳转、路径能否落盘、平台是否支持。

pub mod dep;
pub mod job;
pub mod mirror;
pub mod platform;
pub mod runtime;
pub mod safe_path;
pub mod volume;

pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 磁盘布局的格式版本。插件自身升级时用它决定是否要跑迁移。
/// 一旦发布过正式版本，只增不改语义。
pub const DATA_FORMAT_VERSION: u32 = 1;

/// 只读制品的完成标记。写入它之前的一切都视为垃圾。
pub const READY_MARKER: &str = ".devbox-ok";
