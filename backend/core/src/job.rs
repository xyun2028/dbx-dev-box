//! 安装事务的状态机。
//!
//! 为什么不是一组布尔字段：UI 需要能区分"下载中 / 校验中 / 已装好 / 装坏了"，
//! 用户也需要知道能不能取消。用状态机把非法组合从类型上排除掉。
//!
//! 关键不变量：
//! - 只有 `Ready` 代表真的装好了（对应磁盘上的 `.devbox-ok`）。
//! - `Failed` / `Canceled` 是终态，重装必须新建 Job，不复用旧的。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Queued,
    Plan,
    Download,
    Verify,
    Extract,
    Commit,
    Ready,
    Failed,
    Canceled,
}

impl Phase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Phase::Ready | Phase::Failed | Phase::Canceled)
    }

    pub fn is_running(self) -> bool {
        matches!(
            self,
            Phase::Plan | Phase::Download | Phase::Verify | Phase::Extract | Phase::Commit
        )
    }

    /// 能否在这个阶段接受取消请求。
    ///
    /// 注意 `Commit` 虽然算 running，但**不允许**取消：此时正在做
    /// "写完成标记 + 原子改名"，中途退出只会留下需要 GC 的残骸。
    pub fn cancellable(self) -> bool {
        matches!(
            self,
            Phase::Queued | Phase::Plan | Phase::Download | Phase::Verify | Phase::Extract
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Queued => "queued",
            Phase::Plan => "plan",
            Phase::Download => "download",
            Phase::Verify => "verify",
            Phase::Extract => "extract",
            Phase::Commit => "commit",
            Phase::Ready => "ready",
            Phase::Failed => "failed",
            Phase::Canceled => "canceled",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: Phase,
    pub to: Phase,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "illegal phase transition: {} -> {}", self.from, self.to)
    }
}

impl std::error::Error for IllegalTransition {}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub component: String,
    pub version: String,
    pub phase: Phase,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub error: Option<String>,
    cancel_requested: bool,
}

impl Job {
    pub fn new(id: impl Into<String>, component: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            component: component.into(),
            version: version.into(),
            phase: Phase::Queued,
            bytes_total: 0,
            bytes_done: 0,
            error: None,
            cancel_requested: false,
        }
    }

    /// 唯一的阶段推进入口。非法跳转一律拒绝，不允许"顺手设置一下"。
    pub fn advance(&mut self, to: Phase) -> Result<(), IllegalTransition> {
        if !Self::allowed(self.phase, to) {
            return Err(IllegalTransition {
                from: self.phase,
                to,
            });
        }
        self.phase = to;
        if to.is_terminal() {
            self.cancel_requested = false;
        }
        Ok(())
    }

    pub fn fail(&mut self, reason: impl Into<String>) -> Result<(), IllegalTransition> {
        self.error = Some(reason.into());
        self.advance(Phase::Failed)
    }

    /// 请求取消。只是打个标记，真正的退出发生在引擎的检查点，
    /// 这样不会在写盘中途被打断。
    pub fn request_cancel(&mut self) -> bool {
        if self.phase.cancellable() {
            self.cancel_requested = true;
            true
        } else {
            false
        }
    }

    pub fn cancel_requested(&self) -> bool {
        self.cancel_requested
    }

    pub fn set_progress(&mut self, done: u64, total: u64) {
        self.bytes_done = done.min(total);
        self.bytes_total = total;
    }

    pub fn percent(&self) -> u8 {
        if self.bytes_total == 0 {
            return 0;
        }
        let p = (self.bytes_done as u128 * 100) / self.bytes_total as u128;
        p.min(100) as u8
    }

    fn allowed(from: Phase, to: Phase) -> bool {
        use Phase::*;
        match from {
            Queued => matches!(to, Plan | Canceled),
            Plan => matches!(to, Download | Failed | Canceled),
            Download => matches!(to, Verify | Failed | Canceled),
            Verify => matches!(to, Extract | Failed | Canceled),
            Extract => matches!(to, Commit | Failed | Canceled),
            Commit => matches!(to, Ready | Failed),
            Ready | Failed | Canceled => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_reaches_ready() {
        let mut j = Job::new("j1", "jdk", "21.0.3");
        for p in [Phase::Plan, Phase::Download, Phase::Verify, Phase::Extract, Phase::Commit, Phase::Ready] {
            j.advance(p).expect("legal");
        }
        assert_eq!(j.phase, Phase::Ready);
        assert!(j.phase.is_terminal());
    }

    #[test]
    fn cannot_skip_verification() {
        let mut j = Job::new("j1", "jdk", "21.0.3");
        j.advance(Phase::Plan).unwrap();
        j.advance(Phase::Download).unwrap();
        // 跳过校验直接解压 = 未校验的字节落盘，必须拒绝
        assert!(j.advance(Phase::Extract).is_err());
        assert_eq!(j.phase, Phase::Download);
    }

    #[test]
    fn terminal_states_are_final() {
        let mut j = Job::new("j1", "jdk", "21");
        j.advance(Phase::Plan).unwrap();
        j.advance(Phase::Failed).unwrap();
        assert!(j.advance(Phase::Download).is_err());
    }

    #[test]
    fn cancel_only_before_commit_finishes() {
        let mut j = Job::new("j1", "jdk", "21");
        assert!(j.request_cancel());
        assert!(j.cancel_requested());

        j.advance(Phase::Plan).unwrap();
        assert!(j.request_cancel());

        j.advance(Phase::Download).unwrap();
        j.advance(Phase::Verify).unwrap();
        j.advance(Phase::Extract).unwrap();
        j.advance(Phase::Commit).unwrap();
        // 提交阶段已经不可能干净回退，不再接受取消
        assert!(!j.request_cancel());
    }

    #[test]
    fn percent_is_bounded_and_safe_on_zero() {
        let mut j = Job::new("j1", "jdk", "21");
        assert_eq!(j.percent(), 0);
        j.set_progress(50, 200);
        assert_eq!(j.percent(), 25);
        j.set_progress(999, 200);
        assert_eq!(j.percent(), 100);
        assert_eq!(j.bytes_done, 200, "progress must not exceed total");
    }

    #[test]
    fn fail_records_reason() {
        let mut j = Job::new("j1", "jdk", "21");
        j.advance(Phase::Plan).unwrap();
        j.fail("checksum mismatch").unwrap();
        assert_eq!(j.phase, Phase::Failed);
        assert_eq!(j.error.as_deref(), Some("checksum mismatch"));
    }
}
