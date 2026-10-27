//! devbox 引擎：把"一份制品从网上安全地变成 store 里的只读目录"这件事做好。
//!
//! 分层：`fetch` 只管单个源，`store` 负责换源与事务，`extract` 负责把不可信的
//! 压缩包安全落盘，`checksum` 负责字节级校验。四块都能单独测。

pub mod activate;
pub mod catalog;
pub mod checksum;
pub mod extract;
pub mod fetch;
pub mod store;

pub use activate::{activate, current, deactivate, Activation};
pub use store::{install, InstallError, InstallOutcome, InstallSpec};
