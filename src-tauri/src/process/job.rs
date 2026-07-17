#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, TerminateJobObject,
    JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_BREAKAWAY_OK,
};

/// Windows Job Object 包装
///
/// **不设置** `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`：应用退出时 Job 句柄关闭，
/// 子进程不会被自动终止，java 服务继续运行（由 `stop_all_on_exit` 配置控制是否主动停止）。
/// `kill()` 通过 `TerminateJobObject` 显式杀掉整个 Job 的进程树。
pub struct JobObject {
    #[cfg(windows)]
    handle: Option<HANDLE>,
}

// HANDLE 是裸指针，不自动实现 Send/Sync。我们保证访问安全（仅通过 Mutex 串行访问）。
#[cfg(windows)]
unsafe impl Send for JobObject {}
#[cfg(windows)]
unsafe impl Sync for JobObject {}

impl JobObject {
    pub fn new() -> std::io::Result<Self> {
        #[cfg(windows)]
        {
            unsafe {
                let handle = CreateJobObjectW(None, None)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("CreateJobObject failed: {}", e)))?;
                // 不设置 KILL_ON_JOB_CLOSE，允许子进程在应用退出后存活
                // 设置 BREAKAWAY_OK 允许子进程脱离 Job（未来扩展用）
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("SetInformationJobObject failed: {}", e)))?;
                Ok(Self { handle: Some(handle) })
            }
        }
        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }

    /// 将进程加入 Job
    #[cfg(windows)]
    pub fn assign(&self, process_handle: HANDLE) -> std::io::Result<()> {
        unsafe {
            AssignProcessToJobObject(self.handle.ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::Other, "job handle closed")
            })?, process_handle)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("AssignProcessToJobObject failed: {}", e)))
        }
    }

    /// 显式终止 Job 内所有进程（TerminateJobObject）
    pub fn kill(&mut self) {
        #[cfg(windows)]
        {
            if let Some(h) = self.handle.take() {
                unsafe {
                    // TerminateJobObject 杀掉 Job 内所有进程，再关闭句柄
                    let _ = TerminateJobObject(h, 1);
                    let _ = CloseHandle(h);
                }
            }
        }
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        // 只关闭句柄，不杀进程（应用退出时让 java 服务存活）
        #[cfg(windows)]
        {
            if let Some(h) = self.handle.take() {
                unsafe {
                    let _ = CloseHandle(h);
                }
            }
        }
    }
}
