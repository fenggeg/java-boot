//! Windows Job Object 封装。
//!
//! 职责单一：把子进程整树托管进 daemon 持有的 Job。
//!
//! 关键语义（与 ADR-0001 决策 4/6 对齐）：**不设 `KILL_ON_JOB_CLOSE`**——
//! daemon 自身退出（含崩溃/升级）不会连坐杀子进程，从而保证「崩溃恢复(R3)」能枚举到
//! 存活的 java 并继续跟踪；停止一律由 daemon 显式 `terminate_pid`（优雅终止）完成。

use windows::core::w;
use windows::Win32::Foundation::BOOL;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, PROCESS_ALL_ACCESS, PROCESS_TERMINATE,
};

use crate::error::Result;

pub struct JobObject {
    handle: windows::Win32::Foundation::HANDLE,
}

// HANDLE 由本对象独占拥有；可在线程间移动/共享（daemon 侧久居）。
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}

impl JobObject {
    /// 创建 Job 并把所有子进程挂入。阻塞（win32），须在 spawn_blocking 中调用。
    pub fn create() -> Result<Self> {
        unsafe {
            let job = CreateJobObjectW(None, w!("javaboot-daemon-svc"))?;
            if job.is_invalid() {
                return Err(crate::error::Error::Other("CreateJobObjectW 失败".into()));
            }
            Ok(JobObject { handle: job })
        }
    }

    /// 把某 PID 的进程挂进 Job。阻塞（win32），须在 spawn_blocking 中调用。
    pub fn assign(&self, pid: u32) -> Result<()> {
        unsafe {
            let access = PROCESS_ALL_ACCESS;
            let h = OpenProcess(access, BOOL(0), pid)?;
            let r = AssignProcessToJobObject(self.handle, h);
            let _ = windows::Win32::Foundation::CloseHandle(h);
            r?;
            Ok(())
        }
    }

    /// 终止 Job 内所有进程（整树）。须在 spawn_blocking 中调用。
    pub fn terminate(&self) -> Result<()> {
        unsafe {
            TerminateJobObject(self.handle, 1)?;
            Ok(())
        }
    }

    /// 按 PID 终止单个进程（由 stop 路径调用，job 已托管该进程，即整树终止）。
    pub fn terminate_pid(&self, pid: u32) -> Result<()> {
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, BOOL(0), pid)?;
            let r = TerminateProcess(h, 1);
            let _ = windows::Win32::Foundation::CloseHandle(h);
            r?;
            Ok(())
        }
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}