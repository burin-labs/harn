use super::*;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::{net::UnixStream, process::CommandExt};

#[test]
fn an_inherited_lease_remains_busy_until_the_child_executes() {
    run_isolated(
        "inherited_lease::an_inherited_lease_remains_busy_until_the_child_executes",
        inherited_lease_case,
    );
}

fn inherited_lease_case() {
    let source = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    fs::write(source.path().join("file"), "initial").unwrap();
    let mut workspace =
        ReplayWorkspace::capture_in(source.path(), storage.path(), SeedLimits::default()).unwrap();
    let saved = workspace.persist();
    let flags = unsafe { libc::fcntl(workspace._lease.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0);
    let (mut ready, child_ready) = UnixStream::pair().unwrap();
    ready
        .set_read_timeout(Some(harn_clock::test_support::HANG_CEILING))
        .unwrap();
    let (mut release, child_release) = UnixStream::pair().unwrap();
    let child = std::thread::spawn(move || {
        let ready_fd = child_ready.as_raw_fd();
        let release_fd = child_release.as_raw_fd();
        let mut command = std::process::Command::new("/usr/bin/true");
        // Only async-signal-safe calls may run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                let mut byte = 1u8;
                if libc::write(ready_fd, (&raw const byte).cast(), 1) != 1
                    || libc::read(release_fd, (&raw mut byte).cast(), 1) != 1
                {
                    libc::_exit(101);
                }
                Ok(())
            });
        }
        command.status()
    });
    let ready_result = ready.read_exact(&mut [0u8]);
    drop(workspace);
    let busy = ReplayWorkspace::restore_saved(&saved);
    // Release and reap before asserting, including when the contention check fails.
    let release_result = release.write_all(&[1]);
    drop(release);
    let status = child.join();
    ready_result.unwrap();
    release_result.unwrap();
    assert!(status.unwrap().unwrap().success());
    assert!(matches!(busy, Err(SeedError::InUse { .. })));
    let restored = ReplayWorkspace::restore_saved(&saved).unwrap();
    assert_eq!(restored.receipt(), saved.receipt());
    assert_eq!(
        fs::read_to_string(restored.path().join("file")).unwrap(),
        "initial"
    );
}
