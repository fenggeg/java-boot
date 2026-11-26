//! 软件自身运行数据占用查询与清理。
use std::path::PathBuf;

// ============================ 运行数据清理 ============================
// 软件在**自身数据目录**生成的运行期数据（daemon 日志镜像 + spec 快照）：
// `<data_dir>/javaboot-launcher/run/`。这些不再写入用户项目，但会随使用累积，
// 提供查询占用与一键清除，供用户手动释放。

/// 软件数据目录下运行期数据根目录（与 daemon `run_logs::run_root` 保持一致）。
fn run_data_root() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("javaboot-launcher")
        .join("run")
}

/// 统计 / 清除运行数据的结果。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDataResult {
    /// 数据根目录绝对路径。
    pub path: String,
    /// 数据总文件数。
    pub file_count: usize,
    /// 数据总占用（字节）。
    pub total_bytes: u64,
}

/// 查询运行数据占用的总大小与文件数（不清除）。
#[tauri::command]
pub fn get_run_data_usage() -> RunDataResult {
    let mut res = RunDataResult {
        path: run_data_root().to_string_lossy().to_string(),
        file_count: 0,
        total_bytes: 0,
    };
    walk_usage(&run_data_root(), &mut res);
    res
}

/// 一键清除软件自身运行期数据（`run/` 目录）。返回清除后的剩余占用。
///
/// 正在被 daemon 打开的日志文件（服务运行中）在 Windows 上无法删除，会保留在 `remaining`
/// 中；可停止服务或稍后再清。
#[tauri::command]
pub fn clear_run_data() -> RunDataResult {
    let root = run_data_root();
    let mut before = RunDataResult {
        path: root.to_string_lossy().to_string(),
        file_count: 0,
        total_bytes: 0,
    };
    walk_usage(&root, &mut before);

    let mut cleared_file_count = 0usize;
    let mut cleared_bytes = 0u64;
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() {
                clear_entry(&p, &mut cleared_file_count, &mut cleared_bytes);
            } else if p.is_dir() {
                clear_tree(&p, &mut cleared_file_count, &mut cleared_bytes);
            }
        }
    }
    RunDataResult {
        path: before.path,
        file_count: before.file_count.saturating_sub(cleared_file_count),
        total_bytes: before.total_bytes.saturating_sub(cleared_bytes),
    }
}

/// 递归统计目录下的文件数与总字节。
fn walk_usage(dir: &std::path::Path, res: &mut RunDataResult) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_usage(&p, res);
        } else if p.is_file() {
            res.file_count += 1;
            if let Ok(meta) = p.metadata() {
                res.total_bytes += meta.len();
            }
        }
    }
}

/// 递归删除目录，计数成功清理的文件与字节。
fn clear_tree(dir: &std::path::Path, files: &mut usize, bytes: &mut u64) {
    // 目录可能已被删除 / 无权限访问：静默跳过，绝不 panic。
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            clear_tree(&p, files, bytes);
        } else if p.is_file() {
            clear_entry(&p, files, bytes);
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// 删除单个文件，成功则累加计数（Windows 上被占用文件的删除会失败并静默跳过）。
fn clear_entry(p: &std::path::Path, files: &mut usize, bytes: &mut u64) {
    if let Ok(meta) = p.metadata() {
        if std::fs::remove_file(p).is_ok() {
            *files += 1;
            *bytes += meta.len();
        }
    }
}
