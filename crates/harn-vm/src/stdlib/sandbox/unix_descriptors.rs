//! Pinned capabilities handed across an exec, with one mapping for direct
//! launches and the owner-death guardian. Keep their actual descriptor numbers
//! occupied until spawn, so Rust's IO/error pipes cannot reuse a target slot.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Arc;

pub struct DescriptorTransfer {
    descriptors: Vec<OwnedFd>,
    shared_files: Vec<Arc<std::fs::File>>,
}

impl DescriptorTransfer {
    pub fn new(descriptors: Vec<OwnedFd>) -> Self {
        Self {
            descriptors,
            shared_files: Vec::new(),
        }
    }

    /// Retain already-open output handles at their allocated descriptor numbers.
    /// The caller can read the same files after the child and its drains settle.
    pub fn shared_files(files: Vec<Arc<std::fs::File>>) -> Self {
        Self {
            descriptors: Vec::new(),
            shared_files: files,
        }
    }

    pub fn count(&self) -> usize {
        self.descriptors.len() + self.shared_files.len()
    }

    pub fn numbers(&self) -> Vec<RawFd> {
        self.descriptors
            .iter()
            .map(AsRawFd::as_raw_fd)
            .chain(self.shared_files.iter().map(|file| file.as_raw_fd()))
            .collect()
    }

    /// Move an exclusively owned subset to another IO role, preserving order.
    /// Invalid or repeated numbers leave the transfer unchanged.
    pub fn take_owned(&mut self, numbers: &[RawFd]) -> io::Result<Vec<OwnedFd>> {
        let selected: std::collections::BTreeSet<_> = numbers.iter().copied().collect();
        if selected.len() != numbers.len()
            || numbers
                .iter()
                .any(|number| !self.descriptors.iter().any(|fd| fd.as_raw_fd() == *number))
        {
            return Err(io::Error::other("invalid owned descriptor subset"));
        }
        Ok(numbers
            .iter()
            .map(|number| {
                let index = self
                    .descriptors
                    .iter()
                    .position(|fd| fd.as_raw_fd() == *number)
                    .expect("validated owned descriptor");
                self.descriptors.remove(index)
            })
            .collect())
    }

    /// Retain newly created handles in the same transfer owner.
    pub fn extend_owned(&mut self, descriptors: Vec<OwnedFd>) {
        self.descriptors.extend(descriptors);
    }

    /// Retain portable slots and duplicate out-of-range handles into unused
    /// slots. Other descriptors are never overwritten. A failed reservation
    /// leaves every original handle unchanged.
    pub fn reserve_owned_slots(
        &mut self,
        numbers: &[RawFd],
        slots: std::ops::RangeInclusive<RawFd>,
    ) -> io::Result<Option<Vec<RawFd>>> {
        if numbers.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let selected: std::collections::BTreeSet<_> = numbers.iter().copied().collect();
        if selected.len() != numbers.len()
            || *slots.start() < 3
            || slots.is_empty()
            || numbers
                .iter()
                .any(|number| !self.descriptors.iter().any(|fd| fd.as_raw_fd() == *number))
        {
            return Err(io::Error::other("invalid owned descriptor reservation"));
        }
        let mut targets = Vec::new();
        let mut reserved = Vec::new();
        for number in numbers {
            if slots.contains(number) {
                targets.push(*number);
                continue;
            }
            // SAFETY: fcntl duplicates a live descriptor into a fresh owned slot.
            let fd = unsafe { libc::fcntl(*number, libc::F_DUPFD_CLOEXEC, *slots.start()) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful F_DUPFD_CLOEXEC returns a newly owned descriptor.
            let descriptor = unsafe { OwnedFd::from_raw_fd(fd) };
            if !slots.contains(&fd) {
                return Ok(None);
            }
            targets.push(fd);
            reserved.push(descriptor);
        }
        self.descriptors
            .retain(|fd| !selected.contains(&fd.as_raw_fd()) || targets.contains(&fd.as_raw_fd()));
        self.descriptors.extend(reserved);
        Ok(Some(targets))
    }

    /// Close an exclusively owned subset that will not reach the payload.
    pub fn discard_owned(&mut self, numbers: &[RawFd]) {
        self.descriptors
            .retain(|fd| !numbers.contains(&fd.as_raw_fd()));
    }

    /// Adopt inherited handles while preserving separately owned startup roles.
    /// Reserved handles must be open and cannot overlap the transferred set.
    ///
    /// # Safety
    /// The caller must own every transferred descriptor exclusively. Reserved
    /// descriptors must remain in the custody of their separate startup owner.
    pub unsafe fn inherited(numbers: Vec<RawFd>, reserved: &[RawFd]) -> io::Result<Self> {
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
            for file in &self.shared_files {
                if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) } < 0 {
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
    fn owned_slot_reservation_retains_portable_and_foreign_handles() {
        use std::io::{Read, Seek};
        let mut first = tempfile::tempfile().unwrap();
        let mut foreign = tempfile::tempfile().unwrap();
        let mut second = tempfile::tempfile().unwrap();
        first.write_all(b"first").unwrap();
        second.write_all(b"second").unwrap();
        foreign.write_all(b"foreign").unwrap();
        let first_number = first.as_raw_fd();
        let second_number = second.as_raw_fd();
        let mut transfer = DescriptorTransfer::new(vec![first.into(), second.into()]);
        let original = transfer.numbers();
        assert!(transfer
            .reserve_owned_slots(&original, foreign.as_raw_fd()..=foreign.as_raw_fd())
            .unwrap()
            .is_none());
        assert_eq!(transfer.numbers(), original);
        let targets = transfer
            .reserve_owned_slots(
                &[second_number, first_number],
                first_number.min(second_number)..=first_number.max(second_number),
            )
            .unwrap()
            .unwrap();
        for (target, expected) in targets.into_iter().zip(["second", "first"]) {
            let fd = transfer
                .descriptors
                .iter()
                .find(|fd| fd.as_raw_fd() == target)
                .unwrap()
                .try_clone()
                .unwrap();
            let mut file = std::fs::File::from(fd);
            file.rewind().unwrap();
            let mut content = String::new();
            file.read_to_string(&mut content).unwrap();
            assert_eq!(content, expected);
        }
        foreign.rewind().unwrap();
        let mut content = String::new();
        foreign.read_to_string(&mut content).unwrap();
        assert_eq!(content, "foreign");
    }

    #[test]
    fn reserved_descriptors_are_checked_without_taking_their_custody() {
        let retained = tempfile::tempfile().unwrap();
        let descriptor = retained.as_raw_fd();
        // SAFETY: the empty transferred set owns nothing; the reserved handle
        // stays exclusively owned by retained throughout validation.
        let transfer = unsafe { DescriptorTransfer::inherited(Vec::new(), &[descriptor]) }.unwrap();
        assert_eq!(transfer.count(), 0);
        drop(transfer);
        retained.metadata().unwrap();
        // SAFETY: overlap is refused before any adoption; reserved custody stays put.
        let error = unsafe { DescriptorTransfer::inherited(vec![descriptor], &[descriptor]) }
            .err()
            .expect("startup roles cannot own the same descriptor");
        assert!(error
            .to_string()
            .contains("repeated confinement descriptor"));
        retained.metadata().unwrap();
        // SAFETY: the closed reserved number must be rejected, with no adoption.
        let error = unsafe { DescriptorTransfer::inherited(Vec::new(), &[i32::MAX]) }
            .err()
            .expect("closed reserved handles must be refused");
        assert_eq!(error.raw_os_error(), Some(libc::EBADF));
    }

    #[test]
    fn shared_output_handle_survives_exec_and_remains_readable_by_its_owner() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let retained = Arc::new(file.reopen().unwrap());
        let transfer = DescriptorTransfer::shared_files(vec![retained.clone()]);
        let number = retained.as_raw_fd().to_string();
        assert_eq!(transfer.numbers(), [retained.as_raw_fd()]);
        assert_eq!(transfer.count(), 1);
        let mut command = Command::new("/usr/bin/tee");
        command
            .arg(format!("/dev/fd/{number}"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let write_in_child = |command: &mut Command| {
            let mut child = command.spawn().unwrap();
            child.stdin.take().unwrap().write_all(b"child").unwrap();
            child.wait_with_output().unwrap()
        };
        let before = write_in_child(&mut command);
        assert!(!before.status.success());
        assert!(std::fs::read(file.path()).unwrap().is_empty());
        transfer.attach(&mut command);
        let after = write_in_child(&mut command);
        assert!(after.status.success(), "{after:?}");
        assert_eq!(std::fs::read(file.path()).unwrap(), b"child");
        retained.metadata().unwrap();
    }

    #[cfg(target_os = "linux")]
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
