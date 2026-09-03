//! 监控服务（P3 / ADR-0001 架构目标 MonitorService）。
//!
//! 职责：周期用 sysinfo 采样每个托管进程的 CPU / 内存，回填 `ProcHandle.metrics`
//! （使 `proc.list` / `daemon_reconcile` 拿到真实指标），并把每次采样作为
//! `proc.metrics` 事件推送 UI。
//!
//! 并发模型：采集是阻塞 sysinfo 调用，放到 `spawn_blocking`；事件经 broadcast 发
//! 布到各 session。采样间隔 2s，量级（进程数 × 2s）可忽略。

use std::sync::Arc;
use std::time::Duration;

use once_cell::sync::Lazy;
use parking_lot::Mutex as PMutex;
use sysinfo::System;
use tokio::sync::broadcast;

use jb_core::protocol::{event, Message, Notification, ProcMetrics};

use crate::proc::ProcService;

/// 采样间隔（ms）。
const INTERVAL_MS: u64 = 2000;

pub struct MonitorService {
    procs: Arc<ProcService>,
    bus: broadcast::Sender<Message>,
}

impl MonitorService {
    pub fn new(procs: Arc<ProcService>, bus: broadcast::Sender<Message>) -> Arc<Self> {
        Arc::new(MonitorService { procs, bus })
    }

    /// 启动后台周期采样。
    pub fn spawn(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(INTERVAL_MS)).await;
                let metrics = {
                    let slots = this.procs.metrics_slots();
                    tokio::task::spawn_blocking(move || sample(&slots)).await
                        .unwrap_or_default()
                };
                for m in &metrics {
                    let _ = this.bus.send(Message::Notification(Notification::named(
                        event::PROC_METRICS,
                        m,
                    )));
                }
            }
        });
    }
}

/// 复用的全局 `System` 实例。
///
/// **关键**：cpu_usage() 需要「进程 CPU 时间差分 / 全局 CPU 时间差分」同一采样窗口的
/// 基线。若每次 `new System`，全局差分恒为 0，占比会被放大到 ~100%（CPU 虚高）。
/// 这里跨采样复用同一实例，`cpu_usage()` 才能算出差分。采样是阻塞 sysinfo 调用，
/// 用 Mutex 串行化（间隔 2s，量级可忽略）。
static SYSTEM: Lazy<PMutex<System>> = Lazy::new(|| PMutex::new(System::new()));

/// 对一批采样槽位做一次 sysinfo 采集。返回每个（进程仍存活的）槽位的指标。
fn sample(
    slots: &[(i64, Option<u32>, Arc<parking_lot::Mutex<(Option<f32>, Option<f64>)>>)],
) -> Vec<ProcMetrics> {
    let mut sys = SYSTEM.lock();
    // 先刷新全局 CPU 基线，再刷新托管进程：二者需同窗（本采样），
    // 否则全局时间差分过小会把进程占比放大到近 100%。
    sys.refresh_cpu_usage();
    // 只刷新托管中的 pid，remove_dead=true 清理已退出进程。
    let pids: Vec<sysinfo::Pid> = slots
        .iter()
        .filter_map(|(_, pid, _)| *pid)
        .map(|p| sysinfo::Pid::from_u32(p))
        .collect();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pids), true);

    let mut out = Vec::with_capacity(slots.len());
    for (run_id, pid, slot) in slots {
        let Some(pid) = pid else { continue };
        let Some(proc) = sys.process(sysinfo::Pid::from_u32(*pid)) else {
            // 进程已退出：本次回填 None，等 lifecycle 收尾移除。
            *slot.lock() = (None, None);
            continue;
        };
        let cpu = f32::clamp(proc.cpu_usage(), 0.0, 100.0);
        // sysinfo Process::memory() 返回字节；换算为 MB。
        let mem_mb = proc.memory() as f64 / 1024.0 / 1024.0;
        *slot.lock() = (Some(cpu), Some(mem_mb));
        out.push(ProcMetrics {
            run_id: *run_id,
            cpu_usage: Some(cpu),
            memory_mb: Some(mem_mb),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 采样槽位按 pid 跳过已退出进程，且不回填值。
    #[test]
    fn sample_skips_absent_pid() {
        let slot = Arc::new(parking_lot::Mutex::new((None, None)));
        let absent: Vec<_> = vec![(7, None, Arc::clone(&slot))];
        let out = sample(&absent);
        assert!(out.is_empty());
        assert_eq!(*slot.lock(), (None, None));
    }

    /// 本进程（pid 存在）能被采集并写回内存指标（CPU 首次可能为 0，不在此断言）。
    #[test]
    fn sample_writes_current_process() {
        let myself = std::process::id();
        let slot = Arc::new(parking_lot::Mutex::new((None, None)));
        let list: Vec<_> = vec![(1, Some(myself), Arc::clone(&slot))];
        let out = sample(&list);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].run_id, 1);
        assert!(out[0].memory_mb.unwrap_or(0.0) > 0.0);
        let stored = *slot.lock();
        assert!(stored.1.unwrap_or(0.0) > 0.0);
    }
}