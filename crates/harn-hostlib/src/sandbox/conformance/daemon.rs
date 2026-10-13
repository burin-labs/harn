use std::path::{Path, PathBuf};

use sysinfo::{Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// One case's detached helpers. A private FIFO keeps them alive without a
/// timer; closing the parent's writer ends them even without unwinding.
pub(super) struct DaemonProbe {
    helper: PathBuf,
    started: PathBuf,
    _custody: Option<std::fs::File>,
}

impl DaemonProbe {
    pub(super) fn create(workspace: &Path) -> std::io::Result<Self> {
        let fifo = workspace.join("daemon.fifo");
        make_fifo(&fifo)?;
        let custody = open_custody(&fifo)?;
        let helper = workspace.join("daemon-helper.sh");
        let started = workspace.join("daemon-started.txt");
        // Bootstrap a writer before opening the read end, so a helper that
        // starts after parent death cannot block waiting for a vanished writer.
        // It then drops its own writer and waits only on the parent's custody.
        // The stdout line is a readiness handshake: on macOS `/bin/sh` is a
        // shim that re-execs the real shell, and the helper's argv cannot be
        // read mid-exec. Detached helpers discard it; tests block on it.
        std::fs::write(
            &helper,
            format!(
                "exec 3<> {fifo}\nexec 4< {fifo}\nexec 3>&-\n\
                 printf '%s\\n' \"$$\" >> {started}\n\
                 printf '%s\\n' \"$$\"\n\
                 while IFS= read -r line <&4; do :; done\n",
                fifo = shell_path(&fifo),
                started = shell_path(&started),
            ),
        )?;
        Ok(Self {
            helper,
            started,
            _custody: Some(custody),
        })
    }

    pub(super) fn detach_command(&self) -> String {
        format!(
            "( sh {} </dev/null >/dev/null 2>&1 & )\n",
            shell_path(&self.helper),
        )
    }

    #[cfg(all(test, unix))]
    fn alive(&self) -> bool {
        !self.helper_states().is_empty()
    }

    pub(super) fn helper_states(&self) -> Vec<(sysinfo::Pid, sysinfo::ProcessStatus)> {
        snapshot()
            .processes()
            .values()
            .filter(|process| self.owns(process))
            .map(|process| (process.pid(), process.status()))
            .collect()
    }

    pub(super) fn started(&self) -> bool {
        std::fs::read_to_string(&self.started).is_ok_and(|text| {
            text.lines()
                .any(|line| line.parse::<u32>().is_ok_and(|pid| pid > 0))
        })
    }

    fn owns(&self, process: &Process) -> bool {
        let cmd = process.cmd();
        cmd.first()
            .is_some_and(|arg| Path::new(arg).file_name() == Some(std::ffi::OsStr::new("sh")))
            && cmd.iter().any(|arg| arg == self.helper.as_os_str())
    }

    fn stop(&self) -> Vec<(sysinfo::Pid, bool)> {
        snapshot()
            .processes()
            .values()
            .filter(|process| self.owns(process))
            .map(|process| (process.pid(), process.kill()))
            .collect()
    }
}

impl Drop for DaemonProbe {
    fn drop(&mut self) {
        // Judge before cleanup: a backend leak remains a failed case, while
        // the fixture cannot leave helpers behind after an unconfined control.
        let _ = self.stop();
    }
}

fn snapshot() -> System {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    system
}

pub(super) fn shell_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn open_custody(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    // std opens the file close-on-exec, so children cannot keep custody alive.
    options.open(path)
}

#[cfg(unix)]
fn make_fifo(path: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    // SAFETY: the owned CString is NUL-terminated and lives across the call;
    // mkfifo only reads it and creates a node under the private fixture root.
    if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn make_fifo(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the daemonizing wrapper fixture requires a POSIX FIFO",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct Child(std::process::Child);

    impl Child {
        /// Spawn a helper and block until it reports its pid from inside the
        /// read loop's shell, so its argv is measurable.
        fn spawn(probe: &DaemonProbe) -> Self {
            use std::io::BufRead;
            let mut child = Self(
                std::process::Command::new("sh")
                    .arg(&probe.helper)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("spawn FIFO reader"),
            );
            let mut ready = String::new();
            std::io::BufReader::new(child.0.stdout.take().unwrap())
                .read_line(&mut ready)
                .expect("read helper readiness");
            assert_eq!(ready.trim(), child.0.id().to_string());
            child
        }
    }

    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn each_case_measures_and_stops_only_its_own_live_helper() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let probe_a = DaemonProbe::create(root_a.path()).unwrap();
        let probe_b = DaemonProbe::create(root_b.path()).unwrap();
        let mut child_b = Child::spawn(&probe_b);
        assert!(probe_b.alive(), "the control must measure a live helper");
        assert!(!probe_a.alive(), "a sibling is not this case's helper");
        let mut child_a = Child::spawn(&probe_a);
        assert!(probe_a.alive());
        assert_eq!(
            probe_a.stop(),
            vec![(sysinfo::Pid::from_u32(child_a.0.id()), true)],
        );
        assert!(!child_a.0.wait().unwrap().success());
        assert!(!probe_a.alive());
        assert!(child_b.0.try_wait().unwrap().is_none());
        assert!(
            probe_b.alive(),
            "stopping this case must preserve its sibling"
        );
        drop(probe_b);
        assert!(!child_b.0.wait().unwrap().success());
    }

    #[test]
    fn closing_parent_custody_ends_a_helper_without_probe_drop() {
        let root = tempfile::tempdir().unwrap();
        let mut probe = DaemonProbe::create(root.path()).unwrap();
        let mut child = Child::spawn(&probe);
        assert!(probe.alive(), "the control must measure a live helper");
        drop(probe._custody.take());
        assert!(child.0.wait().unwrap().success());
        assert!(!probe.alive());
    }

    #[test]
    fn a_helper_started_after_custody_closed_does_not_wait_for_a_writer() {
        let root = tempfile::tempdir().unwrap();
        let mut probe = DaemonProbe::create(root.path()).unwrap();
        drop(probe._custody.take());
        let mut child = Child::spawn(&probe);
        assert!(child.0.wait().unwrap().success());
        assert!(!probe.alive());
    }
}
