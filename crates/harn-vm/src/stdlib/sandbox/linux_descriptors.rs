//! Pinned capabilities handed across an exec, with one mapping for direct
//! launches and the owner-death guardian. Keep their actual descriptor numbers
//! occupied until spawn, so Rust's IO/error pipes cannot reuse a target slot.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;

pub struct DescriptorTransfer {
    descriptors: Vec<OwnedFd>,
}

impl DescriptorTransfer {
    pub fn new(descriptors: Vec<OwnedFd>) -> Self {
        Self { descriptors }
    }

    pub fn count(&self) -> usize {
        self.descriptors.len()
    }

    pub fn numbers(&self) -> Vec<RawFd> {
        self.descriptors.iter().map(AsRawFd::as_raw_fd).collect()
    }

    /// Take ownership of the descriptors a trusted launch protocol inherited.
    ///
    /// # Safety
    /// The caller must own every named descriptor exclusively.
    pub unsafe fn inherited(numbers: Vec<RawFd>) -> io::Result<Self> {
        // SAFETY: the caller supplies exclusive custody; no other role is reserved.
        unsafe { Self::inherited_with_reserved(numbers, &[]) }
    }

    /// Adopt inherited handles while preserving separately owned startup roles.
    /// Reserved handles must be open and cannot overlap the transferred set.
    ///
    /// # Safety
    /// The caller must own every transferred descriptor exclusively. Reserved
    /// descriptors must remain in the custody of their separate startup owner.
    pub unsafe fn inherited_with_reserved(
        numbers: Vec<RawFd>,
        reserved: &[RawFd],
    ) -> io::Result<Self> {
        let mut seen = std::collections::BTreeSet::new();
        for fd in numbers.iter().chain(reserved) {
            if *fd < 3 || !seen.insert(*fd) {
                return Err(io::Error::other(
                    "invalid or repeated confinement descriptor",
                ));
            }
        }
        for fd in numbers.iter().chain(reserved) {
            if unsafe { libc::fcntl(*fd, libc::F_GETFD) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let descriptors = numbers
            .into_iter()
            .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
            .collect();
        Ok(Self::new(descriptors))
    }

    pub fn attach(self, command: &mut Command) {
        // SAFETY: the callback only calls fcntl on already owned descriptors.
        unsafe { command.pre_exec(self.hook()) };
    }

    pub fn attach_tokio(self, command: &mut tokio::process::Command) {
        // SAFETY: same callback and ownership as the synchronous constructor.
        unsafe { command.pre_exec(self.hook()) };
    }

    fn hook(self) -> impl FnMut() -> io::Result<()> + Send + Sync + 'static {
        move || {
            for descriptor in &self.descriptors {
                if unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFD, 0) } < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reserved_descriptors_are_checked_without_taking_their_custody() {
        let retained = tempfile::tempfile().unwrap();
        let descriptor = retained.as_raw_fd();
        // SAFETY: the empty transferred set owns nothing; the reserved handle
        // stays exclusively owned by retained throughout validation.
        let transfer =
            unsafe { DescriptorTransfer::inherited_with_reserved(Vec::new(), &[descriptor]) }
                .unwrap();
        assert_eq!(transfer.count(), 0);
        drop(transfer);
        retained.metadata().unwrap();
        // SAFETY: overlap is refused before any adoption; reserved custody stays put.
        let error =
            unsafe { DescriptorTransfer::inherited_with_reserved(vec![descriptor], &[descriptor]) }
                .err()
                .expect("startup roles cannot own the same descriptor");
        assert!(error
            .to_string()
            .contains("repeated confinement descriptor"));
        retained.metadata().unwrap();
        // SAFETY: the closed reserved number must be rejected, with no adoption.
        let error = unsafe { DescriptorTransfer::inherited_with_reserved(Vec::new(), &[i32::MAX]) }
            .err()
            .expect("closed reserved handles must be refused");
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
    }

    #[test]
    fn pinned_descriptors_survive_actual_exec_without_reusing_io_slots() {
        let mut first = tempfile::tempfile().unwrap();
        let mut second = tempfile::tempfile().unwrap();
        first.write_all(b"first").unwrap();
        second.write_all(b"second").unwrap();
        let transfer = DescriptorTransfer::new(vec![first.into(), second.into()]);
        let numbers = transfer.numbers();
        let mut command = Command::new("/usr/bin/sh");
        command.args([
            "-c",
            "cat /proc/self/fd/$1; cat /proc/self/fd/$2",
            "probe",
            &numbers[0].to_string(),
            &numbers[1].to_string(),
        ]);
        let control = command.output().unwrap();
        assert!(
            !control.status.success(),
            "omitting the handover unexpectedly retained the grants"
        );
        assert!(control.stdout.is_empty());
        transfer.attach(&mut command);
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"firstsecond");
    }
}
