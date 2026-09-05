//! Bounded diagnostic subprocesses. Capture files avoid pipe-drain deadlocks
//! when a tool fills stdout or leaves an inherited pipe open in a descendant.

use std::io::{self, Read, Seek};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub(super) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const TARGET_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const CAPTURE_LIMIT: u64 = 64 * 1024;

pub(super) fn output(mut command: Command, timeout: Duration) -> io::Result<Output> {
    let started = Instant::now();
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command.stdin(Stdio::null());
    command.stdout(stdout.try_clone()?);
    command.stderr(stderr.try_clone()?);
    harn_vm::op_interrupt::configure_kill_group(&mut command);
    let cleanup_token = harn_vm::op_interrupt::new_process_cleanup_token();
    command.env(
        harn_vm::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV,
        &cleanup_token,
    );
    let mut child = command.spawn()?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // A diagnostic may exit after starting a descendant that still
                // owns its capture files. End the owned group before reading.
                harn_vm::op_interrupt::terminate_child_group_with_cleanup_token_report(
                    &mut child,
                    Some(&cleanup_token),
                );
                break status;
            }
            Ok(None) => {}
            Err(error) => {
                harn_vm::op_interrupt::terminate_child_group_with_cleanup_token_report(
                    &mut child,
                    Some(&cleanup_token),
                );
                return Err(error);
            }
        }
        if started.elapsed() >= timeout {
            harn_vm::op_interrupt::terminate_child_group_with_cleanup_token_report(
                &mut child,
                Some(&cleanup_token),
            );
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("probe timed out after {} ms", timeout.as_millis()),
            ));
        }
        let capture_size = stdout
            .metadata()
            .and_then(|out| stderr.metadata().map(|err| (out.len(), err.len())));
        let (stdout_size, stderr_size) = match capture_size {
            Ok(sizes) => sizes,
            Err(error) => {
                harn_vm::op_interrupt::terminate_child_group_with_cleanup_token_report(
                    &mut child,
                    Some(&cleanup_token),
                );
                return Err(error);
            }
        };
        if stdout_size > CAPTURE_LIMIT || stderr_size > CAPTURE_LIMIT {
            harn_vm::op_interrupt::terminate_child_group_with_cleanup_token_report(
                &mut child,
                Some(&cleanup_token),
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "probe output exceeded 64 KiB",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(Output {
        status,
        stdout: read_capture(&mut stdout)?,
        stderr: read_capture(&mut stderr)?,
    })
}

pub(super) async fn output_async(command: Command, timeout: Duration) -> io::Result<Output> {
    tokio::task::spawn_blocking(move || output(command, timeout))
        .await
        .map_err(|error| io::Error::other(format!("probe worker failed: {error}")))?
}

fn read_capture(file: &mut std::fs::File) -> io::Result<Vec<u8>> {
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(CAPTURE_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > CAPTURE_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "probe output exceeded 64 KiB",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn probe_child() {
        let Ok(mode) = std::env::var("HARN_TEST_COMMAND_PROBE") else {
            return;
        };
        if let Ok(marker) = std::env::var("HARN_TEST_COMMAND_PROBE_MARKER") {
            std::fs::write(marker, b"entered").unwrap();
        }
        match mode.as_str() {
            "hang" => std::thread::sleep(Duration::from_secs(30)),
            "large" => std::io::stdout()
                .write_all(&vec![b'x'; 128 * 1024])
                .unwrap(),
            _ => println!("probe-ok"),
        }
    }

    fn child(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "commands::command_probe::tests::probe_child",
            "--nocapture",
        ]);
        command.env("HARN_TEST_COMMAND_PROBE", mode);
        command
    }

    #[test]
    fn synchronous_probe_owns_its_deadline_and_output_bound() {
        let marker = tempfile::NamedTempFile::new().unwrap();
        let mut command = child("hang");
        command.env("HARN_TEST_COMMAND_PROBE_MARKER", marker.path());
        let started = Instant::now();
        let error = output(command, Duration::from_secs(2)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(std::fs::read(marker.path()).unwrap(), b"entered");
        assert!(started.elapsed() < Duration::from_secs(8));
        let error = output(child("large"), Duration::from_secs(5)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let passed = output(child("pass"), Duration::from_secs(5)).unwrap();
        assert!(passed.status.success());
        assert!(String::from_utf8_lossy(&passed.stdout).contains("probe-ok"));
    }

    #[tokio::test]
    async fn asynchronous_probe_uses_the_same_deadline_owner() {
        let marker = tempfile::NamedTempFile::new().unwrap();
        let mut command = child("hang");
        command.env("HARN_TEST_COMMAND_PROBE_MARKER", marker.path());
        let error = output_async(command, Duration::from_secs(2))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(std::fs::read(marker.path()).unwrap(), b"entered");
    }

    #[derive(Default)]
    struct DirectSpawns(Vec<String>);

    impl<'ast> syn::visit::Visit<'ast> for DirectSpawns {
        fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
            let method = node.method.to_string();
            if ["spawn", "output", "status"].contains(&method.as_str()) {
                self.0.push(method);
            }
            syn::visit::visit_expr_method_call(self, node);
        }
    }

    fn direct_spawns(source: &str) -> Vec<String> {
        let mut visitor = DirectSpawns::default();
        syn::visit::Visit::visit_file(&mut visitor, &syn::parse_file(source).unwrap());
        visitor.0
    }

    #[test]
    fn every_doctor_probe_uses_a_timeout_owner() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
        let mut paths = vec![root.join("doctor.rs"), root.join("hardware.rs")];
        let mut directories = vec![root.join("doctor")];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    directories.push(path)
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    paths.push(path)
                }
            }
        }
        assert!(
            paths.len() >= 4,
            "doctor probe census did not reach its declared modules"
        );
        for path in paths {
            assert!(
                direct_spawns(&std::fs::read_to_string(&path).unwrap()).is_empty(),
                "{} bypasses the diagnostic timeout owner",
                path.file_name().unwrap().to_string_lossy()
            );
        }
        assert_eq!(
            direct_spawns("fn probe() { Command::new(\"tool\").output(); }"),
            vec!["output"]
        );
    }
}
