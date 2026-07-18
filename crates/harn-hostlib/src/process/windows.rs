//! Windows process-tree lifetime guard for supervised host workloads.

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Owns a Job Object that terminates the enrolled process tree if this process
/// exits before explicitly disarming the guard.
pub struct KillOnCloseJob {
    handle: HANDLE,
}

impl KillOnCloseJob {
    /// Create a kill-on-close job and enroll the current worker process.
    #[must_use = "dropping the guard immediately terminates the enrolled process tree"]
    pub fn enroll_current_process() -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self { handle };
        job.set_kill_on_close(true)?;
        if unsafe { AssignProcessToJobObject(job.handle, GetCurrentProcess()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Clear kill-on-close after every child has exited, then close the job.
    pub fn disarm(mut self) -> io::Result<()> {
        self.set_kill_on_close(false)?;
        self.close();
        Ok(())
    }

    fn set_kill_on_close(&self, enabled: bool) -> io::Result<()> {
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        if enabled {
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        if unsafe {
            SetInformationJobObject(
                self.handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn close(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        self.close();
    }
}
