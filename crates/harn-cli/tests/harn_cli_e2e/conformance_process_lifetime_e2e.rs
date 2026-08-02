//! Public conformance-runner process-lifetime regressions.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::io::{BufRead, Read};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};

use super::test_util::process::harn_e2e_command;

struct HelperCleanup(i32);

impl Drop for HelperCleanup {
    fn drop(&mut self) {
        if self.0 > 0 {
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
            }
        }
    }
}

struct RunnerCleanup(Option<std::process::Child>);

impl RunnerCleanup {
    fn new(child: std::process::Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> &mut std::process::Child {
        self.0.as_mut().expect("runner is present")
    }

    fn take(&mut self) -> std::process::Child {
        self.0.take().expect("runner is present")
    }
}

impl Drop for RunnerCleanup {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        if child.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
        }
    }
}

#[test]
fn completed_case_reaps_a_surviving_helper_before_success() {
    let fixture = LifetimeFixture::new(false);
    let mut runner = RunnerCleanup::new(fixture.spawn_runner(false));
    let helper_pid = fixture.read_helper_pid(runner.child_mut());
    let mut cleanup = HelperCleanup(helper_pid);
    let output = runner
        .take()
        .wait_with_output()
        .expect("wait for completed conformance runner");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed, 0 failed"),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_process_gone(helper_pid);
    cleanup.0 = 0;
}

#[test]
fn sigint_and_sigkill_of_runner_do_not_strand_helpers() {
    assert_signals_do_not_strand_helpers(false);
}

#[test]
fn sigint_and_sigkill_of_parallel_runner_do_not_strand_workers_or_helpers() {
    assert_signals_do_not_strand_helpers(true);
}

fn assert_signals_do_not_strand_helpers(parallel: bool) {
    for signal in [libc::SIGINT, libc::SIGKILL] {
        let fixture = LifetimeFixture::new(true);
        let mut runner = RunnerCleanup::new(fixture.spawn_runner(parallel));
        let helper_pid = fixture.read_helper_pid(runner.child_mut());
        let mut cleanup = HelperCleanup(helper_pid);

        assert_eq!(
            unsafe { libc::kill(runner.child_mut().id() as i32, signal) },
            0
        );
        let status = runner
            .child_mut()
            .wait()
            .expect("reap signaled conformance runner");
        assert_eq!(status.signal(), Some(signal));
        fixture.wait_for_helper_eof();
        assert_process_gone(helper_pid);
        cleanup.0 = 0;
    }
}

struct LifetimeFixture {
    _root: tempfile::TempDir,
    root_path: std::path::PathBuf,
    report: std::sync::Mutex<std::io::BufReader<std::fs::File>>,
    report_write: RawFd,
}

impl LifetimeFixture {
    fn new(block: bool) -> Self {
        let root = tempfile::tempdir().expect("create conformance lifetime fixture");
        let suite = root.path().join("conformance");
        std::fs::create_dir(&suite).expect("create fixture conformance directory");
        let fifo_path = root.path().join("helper.fifo");
        let fifo_cstr =
            std::ffi::CString::new(fifo_path.as_os_str().as_bytes()).expect("encode fixture FIFO");
        assert_eq!(unsafe { libc::mkfifo(fifo_cstr.as_ptr(), 0o600) }, 0);
        let mut report_pipe = [0_i32; 2];
        assert_eq!(unsafe { libc::pipe(report_pipe.as_mut_ptr()) }, 0);
        let report_read = report_pipe[0];
        let report_write = report_pipe[1];
        let background = if block { "" } else { " &" };
        let fifo_shell = fifo_path
            .to_str()
            .filter(|path| !path.bytes().any(|byte| byte.is_ascii_whitespace()))
            .expect("fixture FIFO path is shell-safe");
        let source = format!(
            "import {{ process_shell }} from \"std/runtime\"\n\
             pipeline test(harness: Harness, task) {{\n\
               const start = process_shell(harness.process, \"sh -c 'echo $$ \
                 $HARN_INTERNAL_PROCESS_OWNER_TOKEN >&{report_write}; exec cat \
                 {fifo_shell}' >/dev/null 2>&1{background}\")\n\
               require start?.success, \"helper started\"\n\
               harness.stdio.println(true)\n\
             }}\n"
        );
        std::fs::write(suite.join("lifetime.harn"), source).expect("write lifetime fixture");
        std::fs::write(suite.join("lifetime.expected"), "true\n")
            .expect("write lifetime expectation");
        Self {
            root_path: root.path().to_path_buf(),
            _root: root,
            report: std::sync::Mutex::new(std::io::BufReader::new(unsafe {
                std::fs::File::from_raw_fd(report_read)
            })),
            report_write,
        }
    }

    fn spawn_runner(&self, parallel: bool) -> std::process::Child {
        let mut command = harn_e2e_command();
        command
            .current_dir(&self.root_path)
            .args(["test", "conformance"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0);
        if parallel {
            command.args(["--parallel", "--jobs", "2"]);
        }
        let runner = command.spawn().expect("spawn conformance runner");
        unsafe {
            libc::close(self.report_write);
        }
        runner
    }

    fn read_helper_pid(&self, runner: &mut std::process::Child) -> i32 {
        let mut line = String::new();
        let mut report = self.report.lock().expect("lock helper report");
        wait_for_report_event(report.get_ref().as_raw_fd(), "helper pid");
        report.read_line(&mut line).expect("read helper pid");
        let mut fields = line.split_whitespace();
        let pid = fields.next().unwrap_or_else(|| {
            let status = runner.try_wait().expect("inspect conformance runner");
            let mut stdout = String::new();
            let mut stderr = String::new();
            if let Some(stream) = runner.stdout.as_mut() {
                stream
                    .read_to_string(&mut stdout)
                    .expect("read runner stdout");
            }
            if let Some(stream) = runner.stderr.as_mut() {
                stream
                    .read_to_string(&mut stderr)
                    .expect("read runner stderr");
            }
            panic!(
                "helper report closed before a pid was written; runner status: {status:?}\n\
                 stdout:\n{stdout}\nstderr:\n{stderr}"
            )
        });
        let pid = pid.parse().expect("parse helper pid");
        assert!(
            fields
                .next()
                .is_some_and(|token| token.starts_with("harn-cleanup-")),
            "helper did not inherit the conformance owner token: {line:?}"
        );
        pid
    }

    fn wait_for_helper_eof(&self) {
        let mut report = self.report.lock().expect("lock helper report");
        wait_for_report_event(report.get_ref().as_raw_fd(), "helper report EOF");
        let mut byte = [0_u8; 1];
        assert_eq!(
            report.read(&mut byte).expect("read helper report EOF"),
            0,
            "helper kept its report descriptor open"
        );
    }
}

fn wait_for_report_event(fd: RawFd, description: &str) {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&raw mut poll_fd, 1, 10_000) },
        1,
        "{description} was not observable before the kernel deadline"
    );
}

fn assert_process_gone(pid: i32) {
    wait_for_native_exit(pid);
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "helper {pid} is alive");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH),
        "helper {pid} still exists"
    );
}

#[cfg(target_os = "linux")]
fn wait_for_native_exit(pid: i32) {
    let pid_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    if pid_fd < 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        return;
    }
    let mut poll_fd = libc::pollfd {
        fd: pid_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&raw mut poll_fd, 1, 10_000) }, 1);
    unsafe {
        libc::close(pid_fd);
    }
}

#[cfg(target_os = "macos")]
fn wait_for_native_exit(pid: i32) {
    let queue = unsafe { libc::kqueue() };
    assert!(queue >= 0, "create process kqueue");
    let change = libc::kevent {
        ident: pid as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let timeout = libc::timespec {
        tv_sec: 10,
        tv_nsec: 0,
    };
    let mut event = change;
    let result = unsafe {
        libc::kevent(
            queue,
            &raw const change,
            1,
            &raw mut event,
            1,
            &raw const timeout,
        )
    };
    unsafe {
        libc::close(queue);
    }
    if result < 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    } else {
        assert_eq!(result, 1, "helper did not exit before kernel deadline");
    }
}
