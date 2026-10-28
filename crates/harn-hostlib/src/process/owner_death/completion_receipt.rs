use super::*;
use std::os::fd::{AsRawFd, FromRawFd};

pub(super) const RECEIPT_FD: i32 = 198;
const MAX_RECEIPT_BYTES: u64 = 64 * 1024;
const MAX_CHILDREN: usize = 128;

/// Only the guardian inherits the writer; the payload closes it on exec.
pub(crate) fn prepare(command: &mut Command) -> io::Result<ReceiptReader> {
    let (reader, writer) = io::pipe()?;
    unsafe {
        command.pre_exec(move || {
            let fd = writer.as_raw_fd();
            if fd != RECEIPT_FD && libc::dup2(fd, RECEIPT_FD) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(RECEIPT_FD, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(ReceiptReader(Some(harn_parser::runtime_stack::spawn(
        move || {
            let mut bytes = Vec::new();
            reader.take(MAX_RECEIPT_BYTES + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_RECEIPT_BYTES {
                return Err(io::Error::other("guardian cleanup receipt exceeded limit"));
            }
            serde_json::from_slice(&bytes).map_err(io::Error::other)
        },
    ))))
}

pub(crate) struct ReceiptReader(
    Option<std::thread::JoinHandle<io::Result<harn_vm::op_interrupt::ProcessCleanupReport>>>,
);

impl ReceiptReader {
    pub(crate) fn finish(mut self) -> harn_vm::op_interrupt::ProcessCleanupReport {
        self.0
            .take()
            .and_then(|reader| reader.join().ok())
            .and_then(Result::ok)
            .unwrap_or_default()
    }
}

pub(super) fn inherited_writer() -> io::Result<std::fs::File> {
    if unsafe { libc::fcntl(RECEIPT_FD, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the private parent launch transferred this descriptor exclusively.
    Ok(unsafe { std::fs::File::from_raw_fd(RECEIPT_FD) })
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
