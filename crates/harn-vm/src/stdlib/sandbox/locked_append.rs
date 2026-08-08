use std::fs::TryLockError;
use std::io;
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use super::ScopedMutationTarget;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AppendLockOptions {
    pub timeout: Duration,
    pub sync_data: bool,
}

pub(super) fn append_locked_unscoped(
    path: &Path,
    contents: &[u8],
    options: AppendLockOptions,
) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    let _lock = lock_file_exclusive(&file, path, options.timeout)?;
    let mut writer = &file;
    writer.write_all(contents)?;
    if options.sync_data {
        file.sync_data()?;
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn append_locked_scoped_target(
    target: &ScopedMutationTarget,
    contents: &[u8],
    options: AppendLockOptions,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let (parent, file_name) = super::ensure_parent_dirs_scoped(target)?;
    let file = super::openat_file(
        parent.as_raw_fd(),
        &file_name,
        libc::O_RDWR | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o666,
    )?;
    let full = target.root.join(&target.relative);
    let _lock = lock_file_exclusive(&file, &full, options.timeout)?;
    let mut writer = &file;
    writer.write_all(contents)?;
    if options.sync_data {
        file.sync_data()?;
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn append_locked_scoped_target(
    target: &ScopedMutationTarget,
    contents: &[u8],
    options: AppendLockOptions,
) -> io::Result<()> {
    let (parent, file_name) = super::win_scoped_parent(target, true)?;
    let full = parent.join(&file_name);
    super::win_reject_reparse_leaf(&full)?;
    append_locked_unscoped(&full, contents, options)
}

#[cfg(all(not(unix), not(windows)))]
pub(super) fn append_locked_scoped_target(
    target: &ScopedMutationTarget,
    contents: &[u8],
    options: AppendLockOptions,
) -> io::Result<()> {
    let full = target.root.join(&target.relative);
    if let Some(parent) = full.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    append_locked_unscoped(&full, contents, options)
}

struct FileLockGuard<'a>(&'a std::fs::File);

impl Drop for FileLockGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn lock_file_exclusive<'a>(
    file: &'a std::fs::File,
    path: &Path,
    timeout: Duration,
) -> io::Result<FileLockGuard<'a>> {
    let timeout_ms = duration_millis_i64(timeout);
    let deadline_ms = crate::stdlib::clock::now_monotonic_ms().saturating_add(timeout_ms);
    let mut backoff = Duration::from_millis(10);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(FileLockGuard(file)),
            Err(TryLockError::WouldBlock) => {
                let now_ms = crate::stdlib::clock::now_monotonic_ms();
                if timeout.is_zero() || now_ms >= deadline_ms {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "timed out after {}ms waiting for append lock `{}`",
                            timeout.as_millis(),
                            path.display()
                        ),
                    ));
                }
                let remaining = Duration::from_millis(deadline_ms.saturating_sub(now_ms) as u64);
                wait_for_retry(backoff.min(remaining));
                backoff = (backoff * 2).min(Duration::from_millis(100));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

fn duration_millis_i64(duration: Duration) -> i64 {
    duration.as_millis().min(i64::MAX as u128) as i64
}

fn wait_for_retry(duration: Duration) {
    if duration.is_zero() {
        return;
    }
    if crate::stdlib::clock::is_mocked() {
        crate::stdlib::clock::advance(duration_millis_i64(duration));
        return;
    }
    std::thread::sleep(duration);
}
