#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
    JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// Windows Job Object 包装：确保关闭句柄时杀掉整个进程树
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
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
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

    /// 不显式杀进程：关闭 Job 句柄会自动杀掉整个进程树（KILL_ON_JOB_CLOSE）
    pub fn kill(&mut self) {
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

impl Drop for JobObject {
    fn drop(&mut self) {
        self.kill();
    }
}
