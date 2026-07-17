/// 候选路径收集器：去重 + 保序
///
/// 用于 JDK / Maven 等工具探测时的候选路径收集，避免重复路径。
/// 内部按小写 + 正斜杠归一化进行去重。
pub struct CandidateCollector {
    seen: std::collections::HashSet<String>,
    candidates: Vec<String>,
}

impl CandidateCollector {
    pub fn new() -> Self {
        Self {
            seen: std::collections::HashSet::new(),
            candidates: Vec::new(),
        }
    }

    pub fn push(&mut self, p: String) {
        let norm = p.to_lowercase().replace('\\', "/");
        if self.seen.insert(norm) {
            self.candidates.push(p);
        }
    }

    pub fn into_candidates(self) -> Vec<String> {
        self.candidates
    }
}

/// 为 `std::process::Command` 和 `tokio::process::Command` 提供统一的
/// `CREATE_NO_WINDOW` 设置，避免在打包后的 GUI 应用中弹出终端窗口。
pub trait NoWindow {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

impl NoWindow for std::process::Command {
    #[cfg(windows)]
    fn creation_flags_no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(CREATE_NO_WINDOW);
        self
    }
    #[cfg(not(windows))]
    fn creation_flags_no_window(&mut self) -> &mut Self {
        self
    }
}

impl NoWindow for tokio::process::Command {
    #[cfg(windows)]
    fn creation_flags_no_window(&mut self) -> &mut Self {
        self.creation_flags(CREATE_NO_WINDOW);
        self
    }
    #[cfg(not(windows))]
    fn creation_flags_no_window(&mut self) -> &mut Self {
        self
    }
}