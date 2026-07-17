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

/// 检查路径是否存在，跟随 junction / symlink。
///
/// `Path::exists()` 在某些情况下（如安装器以 elevated 权限启动时）无法解析
/// scoop 的 `current` junction，返回 false。此函数用 `std::fs::metadata`
///（跟随链接）替代，并在失败时尝试 `canonicalize` 解析真实路径。
pub fn path_exists_follow_junction(path: &std::path::Path) -> bool {
    // 1. 直接 metadata（跟随 junction/symlink）
    if std::fs::metadata(path).is_ok() {
        return true;
    }
    // 2. canonicalize 解析完整路径
    if std::fs::canonicalize(path).is_ok() {
        return true;
    }
    // 3. fallback: Path::exists
    path.exists()
}

/// 解析 junction / symlink 到真实路径。
/// 如果路径不是 junction 或解析失败，返回原路径。
pub fn resolve_junction(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}