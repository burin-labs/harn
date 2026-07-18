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

#[cfg(test)]
mod tests {
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use crate::process_liveness::{process_liveness, ProcessLiveness};

    use super::KillOnCloseJob;

    const WORKER_ENV: &str = "HARN_TEST_KILL_ON_CLOSE_WORKER";
    const WORKER_TEST: &str = "process::windows::tests::kill_on_close_job_worker_fixture";

    #[test]
    fn kill_on_close_job_worker_fixture() {
        if std::env::var_os(WORKER_ENV).is_none() {
            return;
        }

        let _job = KillOnCloseJob::enroll_current_process().expect("enroll worker in Job Object");
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
    fn abruptly_losing_worker_kills_its_job_descendant() {
        let mut worker = Command::new(std::env::current_exe().expect("resolve test binary"))
            .args(["--exact", WORKER_TEST, "--nocapture"])
            .env(WORKER_ENV, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn Job Object worker fixture");
        let stdout = worker.stdout.take().expect("capture worker handshake");
        let mut child_pid = None;
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.expect("read worker handshake");
            if let Some(raw) = line.strip_prefix("HARN_JOB_CHILD_PID=") {
                child_pid = Some(raw.parse::<u32>().expect("parse descendant PID"));
                break;
            }
        }
        let child_pid = child_pid.expect("worker reports its descendant PID");

        worker.kill().expect("abruptly terminate worker");
        worker.wait().expect("reap terminated worker");
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_liveness(child_pid) != ProcessLiveness::Dead && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        if process_liveness(child_pid) != ProcessLiveness::Dead {
            let _ = Command::new("taskkill")
                .args(["/PID", &child_pid.to_string(), "/T", "/F"])
                .status();
            panic!("Job Object descendant {child_pid} survived abrupt worker loss");
        }
    }
}
