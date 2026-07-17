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

/// 安全的 canonicalize：去掉 Windows 扩展路径前缀 `\\?\`。
///
/// `std::fs::canonicalize` 在 Windows 上返回 `\\?\C:\...` 格式，
/// 该前缀会导致 `Command::new` 和前端显示异常。
pub fn canonicalize_clean(path: &std::path::Path) -> Option<std::path::PathBuf> {
    std::fs::canonicalize(path).ok().map(|p| {
        let s = p.to_string_lossy().to_string();
        let cleaned = if let Some(stripped) = s.strip_prefix(r"\\?\") {
            std::path::PathBuf::from(stripped)
        } else {
            p
        };
        cleaned
    })
}

/// 解析 junction / symlink 到真实路径（去掉 `\\?\` 前缀）。
/// 如果路径不是 junction 或解析失败，返回原路径。
pub fn resolve_junction(path: &std::path::Path) -> std::path::PathBuf {
    canonicalize_clean(path).unwrap_or_else(|| path.to_path_buf())
}

/// 解码子进程输出：优先 UTF-8，失败时 fallback 到 GBK（Windows 中文系统默认编码）。
///
/// `mvn -v` / `java -version` 等命令在中文 Windows 上失败时输出 GBK 编码的错误信息，
/// `String::from_utf8_lossy` 会产生乱码。此函数用 `encoding_rs` 正确解码。
pub fn decode_output(bytes: &[u8]) -> String {
    // 先尝试 UTF-8
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    // fallback: GBK 解码
    let (s, _, _) = encoding_rs::GBK.decode(bytes);
    s.into_owned()
}