use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use fs2::FileExt;

pub struct HarnProcessTestLock {
    _thread_guard: MutexGuard<'static, ()>,
    file: File,
}

impl Drop for HarnProcessTestLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn lock_harn_process_tests() -> HarnProcessTestLock {
    let thread_guard = harn_process_mutex().lock().unwrap();
    let path = harn_process_lock_path();
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap_or_else(|error| panic!("failed to open {}: {error}", path.display()));
    file.lock_exclusive()
        .unwrap_or_else(|error| panic!("failed to lock {}: {error}", path.display()));
    HarnProcessTestLock {
        _thread_guard: thread_guard,
        file,
    }
}

fn harn_process_lock_path() -> PathBuf {
    std::env::temp_dir().join("harn-process-tests.lock")
}

fn harn_process_mutex() -> &'static Mutex<()> {
    static HARN_PROCESS_TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    HARN_PROCESS_TEST_MUTEX.get_or_init(|| Mutex::new(()))
}
