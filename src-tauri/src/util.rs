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