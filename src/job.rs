//! Windows Job Object wrapper.
//!
//! Every process the launcher spawns is assigned to a job configured with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Because job membership is inherited,
//! Laragon's children (`mysqld.exe`, `nginx.exe`, `httpd.exe`, `php-cgi.exe`)
//! land in the job too, so nothing survives the launcher - even if the
//! launcher is killed outright.

#[cfg(windows)]
mod imp {
    use anyhow::{anyhow, Result};
    use std::ffi::c_void;
    use std::sync::Arc;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    pub struct Job {
        handle: Arc<HANDLE>,
    }

    impl Clone for Job {
        fn clone(&self) -> Self {
            Job {
                handle: self.handle.clone(),
            }
        }
    }

    // The handle is a kernel object; moving/sharing it across threads is safe.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> Result<Job> {
            unsafe {
                let handle = CreateJobObjectW(None, PCWSTR::null())
                    .map_err(|e| anyhow!("CreateJobObjectW failed: {e}"))?;
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
                .map_err(|e| anyhow!("SetInformationJobObject failed: {e}"))?;
                Ok(Job {
                    handle: Arc::new(handle),
                })
            }
        }

        /// Adds an already-running process to the job.
        /// Called immediately after `spawn`, so the (sub-millisecond) window in
        /// which a grandchild could escape the job is as small as std allows.
        pub fn assign(&self, pid: u32) -> Result<()> {
            unsafe {
                let h = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid)
                    .map_err(|e| anyhow!("OpenProcess({pid}) failed: {e}"))?;
                let _guard = HandleGuard(h);
                AssignProcessToJobObject(*self.handle, h)
                    .map_err(|e| anyhow!("AssignProcessToJobObject({pid}) failed: {e}"))?;
                Ok(())
            }
        }

        /// Kills every process in the job. Used as the shutdown catch-all for
        /// processes we never tracked (MariaDB, nginx, Apache, ...).
        pub fn terminate_all(&self) -> Result<()> {
            unsafe {
                TerminateJobObject(*self.handle, 1)
                    .map_err(|e| anyhow!("TerminateJobObject failed: {e}"))
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                // Kill-on-close fires here for the last reference, killing
                // anything still in the job (mysqld, nginx, apache, ...).
                let _ = CloseHandle(*self.handle);
            }
        }
    }

    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use anyhow::Result;

    #[derive(Clone)]
    pub struct Job;

    impl Job {
        pub fn new() -> Result<Job> {
            Ok(Job)
        }
        pub fn assign(&self, _pid: u32) -> Result<()> {
            Ok(())
        }
        pub fn terminate_all(&self) -> Result<()> {
            Ok(())
        }
    }
}

pub use imp::Job;