use super::*;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

/// A private pipe addresses the guardian by descriptor custody, never a PID.
#[derive(Clone)]
pub(crate) struct CancelRequest(Arc<Mutex<io::PipeWriter>>);

impl CancelRequest {
    pub(crate) fn request(&self) -> io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .write_all(&[1])
    }
}

pub(super) fn prepare(command: &mut Command) -> io::Result<(CancelRequest, i32)> {
    let (reader, writer) = io::pipe()?;
    let fd = reader.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(reader.as_raw_fd(), libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok((CancelRequest(Arc::new(Mutex::new(writer))), fd))
}

pub(super) fn inherited_reader(fd: Option<i32>) -> io::Result<std::fs::File> {
    let fd = fd
        .filter(|fd| *fd >= 3)
        .ok_or_else(|| io::Error::other("guardian cancellation descriptor missing"))?;
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the validated private launch transferred this descriptor exclusively.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// The kernel handle keeps delayed signals bound to the original child.
#[derive(Clone)]
pub(crate) struct ChildIdentity(Arc<OwnedFd>);

impl ChildIdentity {
    pub(crate) fn open(pid: u32) -> io::Result<Self> {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: pidfd_open returned a new owning descriptor.
        Ok(Self(Arc::new(unsafe { OwnedFd::from_raw_fd(fd as i32) })))
    }

    pub(crate) fn signal(&self, signal: i32) -> io::Result<()> {
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

pub(super) fn watch(mut reader: std::fs::File, events: std::sync::mpsc::Sender<GuardianEvent>) {
    harn_parser::runtime_stack::spawn(move || {
        let mut request = [0_u8; 1];
        if reader.read_exact(&mut request).is_ok() && request == [1] {
            let _ = events.send(GuardianEvent::CancelRequested);
        }
    });
}

pub(super) fn terminate(
    identity: Option<ChildIdentity>,
    pid: u32,
) -> Option<harn_vm::op_interrupt::ProcessCleanupReport> {
    let identity = identity?;
    let report = identity.signal(libc::SIGTERM).ok().map(|()| {
        harn_vm::op_interrupt::ProcessCleanupReport::for_signal(Some(pid), libc::SIGTERM)
    });
    harn_parser::runtime_stack::spawn(move || {
        std::thread::sleep(harn_vm::op_interrupt::SUBPROCESS_TERM_GRACE);
        let _ = identity.signal(libc::SIGKILL);
    });
    report
}
