//! Windows process-tree lifetime guard for supervised host workloads.

use std::io;
use std::os::windows::process::CommandExt;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, OpenThread, ResumeThread, CREATE_SUSPENDED,
    PROCESS_SET_QUOTA, PROCESS_TERMINATE, THREAD_SUSPEND_RESUME,
};

/// Owns a Job Object that terminates the enrolled process tree if this process
/// exits before explicitly disarming the guard.
pub struct KillOnCloseJob {
    handle: HANDLE,
}

impl KillOnCloseJob {
    /// Create an empty kill-on-close Job Object owned by this process.
    pub fn new() -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self { handle };
        job.set_kill_on_close(true)?;
        Ok(job)
    }

    /// Create a kill-on-close job and enroll the current worker process.
    #[must_use = "dropping the guard immediately terminates the enrolled process tree"]
    pub fn enroll_current_process() -> io::Result<Self> {
        let job = Self::new()?;
        job.assign_process(unsafe { GetCurrentProcessId() })?;
        Ok(job)
    }

    /// Enroll one process and all descendants it subsequently creates.
    pub fn assign_process(&self, pid: u32) -> io::Result<()> {
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let assigned = unsafe { AssignProcessToJobObject(self.handle, process) };
        unsafe {
            CloseHandle(process);
        }
        if assigned == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Terminate every process currently enrolled in the job.
    pub fn terminate(&self) -> io::Result<()> {
        if unsafe { TerminateJobObject(self.handle, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
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

/// Start a managed worker suspended so it cannot create an uncontained
/// descendant before the supervisor assigns it to the Job Object.
pub(crate) fn configure_suspended(command: &mut std::process::Command) {
    command.creation_flags(CREATE_SUSPENDED);
}

/// Resume the initial thread of a newly created suspended process.
pub(crate) fn resume_process(pid: u32) -> io::Result<()> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    let mut found = unsafe { Thread32First(snapshot, &raw mut entry) } != 0;
    let mut result = Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no initial thread found for process {pid}"),
    ));
    while found {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                result = Err(io::Error::last_os_error());
                break;
            }
            let previous_count = unsafe { ResumeThread(thread) };
            unsafe {
                CloseHandle(thread);
            }
            result = if previous_count == u32::MAX {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            };
            break;
        }
        found = unsafe { Thread32Next(snapshot, &raw mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }
    result
}

// Job handles may be closed or used from the background waiter/canceller
// threads; Windows kernel handles have process-wide thread-safe ownership.
unsafe impl Send for KillOnCloseJob {}
unsafe impl Sync for KillOnCloseJob {}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};

    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    use super::KillOnCloseJob;

    const OWNER_ENV: &str = "HARN_TEST_KILL_ON_CLOSE_OWNER";
    const OWNER_TEST: &str = "process::windows::tests::kill_on_close_job_owner_fixture";

    #[test]
    fn kill_on_close_job_owner_fixture() {
        if std::env::var_os(OWNER_ENV).is_none() {
            return;
        }

        let job = KillOnCloseJob::new().expect("create owner Job Object");
        let mut descendant = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Write-Output $PID; Start-Sleep -Seconds 30",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn Job Object descendant");
        job.assign_process(descendant.id())
            .expect("assign descendant to owner Job Object");
        let stdout = descendant.stdout.take().expect("capture descendant PID");
        let mut lines = std::io::BufReader::new(stdout).lines();
        let pid = lines
            .next()
            .expect("descendant reports its PID")
            .expect("read descendant PID");
        println!("HARN_JOB_CHILD_PID={pid}");
        std::io::stdout().flush().expect("flush descendant PID");
        let _ = descendant.wait();
    }

    #[test]
    fn closing_job_owner_kills_its_managed_descendant() {
        let mut owner = Command::new(std::env::current_exe().expect("resolve test binary"))
            .args(["--exact", OWNER_TEST, "--nocapture"])
            .env(OWNER_ENV, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn Job Object owner fixture");
        let stdout = owner.stdout.take().expect("capture owner handshake");
        let mut child_pid = None;
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.expect("read worker handshake");
            if let Some(raw) = line.strip_prefix("HARN_JOB_CHILD_PID=") {
                child_pid = Some(raw.parse::<u32>().expect("parse descendant PID"));
                break;
            }
        }
        let child_pid = child_pid.expect("worker reports its descendant PID");

        let child = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, child_pid) };
        assert!(!child.is_null(), "open managed descendant");
        owner.kill().expect("abruptly terminate Job Object owner");
        owner.wait().expect("reap terminated Job Object owner");
        assert_eq!(
            unsafe { WaitForSingleObject(child, 5_000) },
            WAIT_OBJECT_0,
            "Job Object descendant {child_pid} survived owner loss"
        );
        unsafe {
            CloseHandle(child);
        }
    }
}
