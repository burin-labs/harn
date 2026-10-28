use super::*;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

const MAX_RECEIPT_BYTES: u64 = 64 * 1024;
const MAX_CHILDREN: usize = 128;

/// Only the guardian inherits the writer; the payload closes it on exec.
pub(crate) fn prepare(command: &mut Command) -> io::Result<(ReceiptReader, i32)> {
    let (reader, writer) = io::pipe()?;
    let receipt_fd = writer.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            let fd = writer.as_raw_fd();
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    harn_parser::runtime_stack::spawn(move || {
        let result = (|| {
            let mut bytes = Vec::new();
            reader.take(MAX_RECEIPT_BYTES + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_RECEIPT_BYTES {
                return Err(io::Error::other("guardian cleanup receipt exceeded limit"));
            }
            serde_json::from_slice(&bytes).map_err(io::Error::other)
        })();
        let _ = sender.send(result);
    });
    Ok((
        ReceiptReader(Arc::new(Mutex::new(ReceiptState {
            receiver,
            report: None,
        }))),
        receipt_fd,
    ))
}

struct ReceiptState {
    receiver: mpsc::Receiver<io::Result<harn_vm::op_interrupt::ProcessCleanupReport>>,
    report: Option<harn_vm::op_interrupt::ProcessCleanupReport>,
}

#[derive(Clone)]
pub(crate) struct ReceiptReader(Arc<Mutex<ReceiptState>>);

impl ReceiptReader {
    pub(crate) fn finish(&self) -> Option<harn_vm::op_interrupt::ProcessCleanupReport> {
        let mut state = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        if state.report.is_none() {
            if let Ok(Ok(report)) = state.receiver.recv_timeout(Duration::from_secs(15)) {
                state.report = Some(report);
            }
        }
        state.report.clone()
    }
}

pub(super) fn inherited_writer(fd: Option<i32>) -> io::Result<std::fs::File> {
    let fd = fd
        .filter(|fd| *fd >= 3)
        .ok_or_else(|| io::Error::other("guardian cleanup receipt descriptor missing"))?;
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the private parent launch transferred this descriptor exclusively.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

pub(super) fn write(
    writer: &mut std::fs::File,
    report: &harn_vm::op_interrupt::ProcessCleanupReport,
) -> io::Result<()> {
    let mut report = report.clone();
    report.omitted_children += report.children.len().saturating_sub(MAX_CHILDREN);
    report.children.truncate(MAX_CHILDREN);
    for child in &mut report.children {
        if let Some(name) = child.command_name.as_mut() {
            *name = name.chars().take(64).collect();
        }
    }
    let bytes = serde_json::to_vec(&report).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return Err(io::Error::other("guardian cleanup receipt exceeded limit"));
    }
    writer.write_all(&bytes)
}

pub(super) fn refuse_survivors(
    report: &harn_vm::op_interrupt::ProcessCleanupReport,
) -> io::Result<()> {
    let survivors = report
        .survivors()
        .map(|child| format!("pid={} alive={:?}", child.pid, child.alive_after_cleanup))
        .collect::<Vec<_>>();
    if survivors.is_empty() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "owner-death guardian pid={} left helper processes alive or unmeasured after cleanup: {}",
        std::process::id(),
        survivors.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_receipt_is_unavailable_and_descriptor_is_reserved_until_spawn() {
        let mut command = Command::new("unspawned");
        let (reader, fd) = prepare(&mut command).unwrap();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        drop(command);
        assert!(reader.finish().is_none());
    }

    #[test]
    fn bounded_receipt_retains_omission_evidence() {
        let mut report = harn_vm::op_interrupt::ProcessCleanupReport::default();
        for pid in 1..=MAX_CHILDREN + 1 {
            report
                .children
                .push(harn_vm::op_interrupt::ProcessCleanupChild::new(
                    pid as u32,
                    None,
                    1,
                    Some("x".repeat(4096)),
                ));
        }
        let mut file = tempfile::tempfile().unwrap();
        write(&mut file, &report).unwrap();
        assert!(file.metadata().unwrap().len() <= MAX_RECEIPT_BYTES);
        use std::io::Seek;
        file.rewind().unwrap();
        let receipt: harn_vm::op_interrupt::ProcessCleanupReport =
            serde_json::from_reader(file).unwrap();
        assert_eq!(receipt.children.len(), MAX_CHILDREN);
        assert_eq!(receipt.omitted_children, 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn receipt_alias_is_refused_before_any_descriptor_is_adopted() {
        let (_reader, writer) = io::pipe().unwrap();
        let fd = writer.as_raw_fd();
        let mut request = GuardianCommandRequest::from_command(
            &Command::new("unused"),
            false,
            "token".into(),
            None,
            false,
        );
        request.cleanup_receipt_fd = Some(fd);
        request.pinned_verifier_descriptors.push(fd);
        // SAFETY: the invalid alias must be rejected before taking ownership.
        assert!(unsafe { PreparedCommand::from_inherited_request(request) }.is_err());
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cancellation_cannot_alias_the_receipt_descriptor() {
        let (_reader, writer) = io::pipe().unwrap();
        let fd = writer.as_raw_fd();
        let mut request = GuardianCommandRequest::from_command(
            &Command::new("unused"),
            false,
            "token".into(),
            None,
            false,
        );
        request.cleanup_receipt_fd = Some(fd);
        request.cancel_request_fd = Some(fd);
        // SAFETY: admission must reject the alias before adopting a handle.
        assert!(unsafe { PreparedCommand::from_inherited_request(request) }.is_err());
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
    }
}
