//! 运行期产物落盘位置管理。
//!
//! daemon 为托管服务写两类运行期文件：日志镜像（`<module>.log`）与进程 spec 快照
//! （`.spec-<run_id>.json`）。此前两者都写在**用户项目**的 `<working_dir>/.javaboot/`
//! 下，会污染用户源码树（且 Git 并不默认忽略 `.javaboot`，易被误提交）。
//! 现统一改写到 launcher 数据目录：
//! `<data>/javaboot-launcher/run/<working_dir 稳定哈希>/`
//!
//! 说明：
//! - 用 FNV-1a 稳定哈希分目录（不能用 `DefaultHasher`，其种子每次进程不同，
//!   跨 daemon 重启会散落到不同目录）。
//! - 日志具有累积性，需按保留期清理（见 [`prune_old`]）。

use std::path::PathBuf;
use std::time::SystemTime;

/// launcher 数据目录下运行期产物的根目录。
pub fn run_root() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("javaboot-launcher")
        .join("run")
}

/// 某服务（按 working_dir）的运行期产物目录。
pub fn run_log_dir(working_dir: &str) -> PathBuf {
    run_root().join(format!("{:016x}", fnv1a(working_dir)))
}

/// 稳定哈希：跨进程可复现。
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3); // FNV-1a prime
    }
    h
}

/// 删除 `run_root()` 下超过保留期（天）的日志镜像 / spec 文件；文件删空的目录一并移除。
///
/// 镜像文件持续 append、无限增长，这里按 mtime 兜底回收，避免数据目录无限制膨胀。
pub fn prune_old(retention_days: i64) {
    let root = run_root();
    if retention_days <= 0 {
        return;
    }
    let cutoff = match SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(retention_days as u64 * 86400))
    {
        Some(t) => t,
        None => return,
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for dir in entries.flatten() {
        let p = dir.path();
        if !p.is_dir() {
            if is_older_than(&p, cutoff) {
                let _ = std::fs::remove_file(&p);
            }
            continue;
        }
        let Ok(files) = std::fs::read_dir(&p) else {
            continue;
        };
        let mut removed_any = false;
        for f in files.flatten() {
            if is_older_than(&f.path(), cutoff) && std::fs::remove_file(&f.path()).is_ok() {
                removed_any = true;
            }
        }
        // 目录已空则连空目录一起清掉
        if removed_any
            && std::fs::read_dir(&p)
                .map(|mut it| it.next().is_none())
                .unwrap_or(false)
        {
            let _ = std::fs::remove_dir(&p);
        }
    }
}

fn is_older_than(p: &std::path::Path, cutoff: SystemTime) -> bool {
    p.metadata()
        .and_then(|m| m.modified())
        .map(|t| t < cutoff)
        .unwrap_or(false)
}