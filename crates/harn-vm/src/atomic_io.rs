//! Atomic writes in the native execution's filesystem lock namespace.

#[cfg(windows)]
use std::{fs::File, io, io::BufWriter, path::Path};

pub(crate) use harn_vm_core::atomic_io::atomic_write_with_durability_unlocked;
pub use harn_vm_core::atomic_io::{AtomicWriteDurability, AtomicWriteReceipt};

#[cfg(not(windows))]
pub use harn_vm_core::atomic_io::{
    atomic_copy_with_mode, atomic_write, atomic_write_with, atomic_write_with_durability,
    atomic_write_with_mode,
};

#[cfg(windows)]
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let _scope = crate::conditional_replace::scope_native_lock_root();
    harn_vm_core::atomic_io::atomic_write(path, bytes)
}

#[cfg(windows)]
pub fn atomic_write_with_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let _scope = crate::conditional_replace::scope_native_lock_root();
    harn_vm_core::atomic_io::atomic_write_with_mode(path, bytes, mode)
}

#[cfg(windows)]
pub fn atomic_copy_with_mode(
    source: &Path,
    destination: &Path,
    mode: u32,
) -> io::Result<AtomicWriteReceipt> {
    let _scope = crate::conditional_replace::scope_native_lock_root();
    harn_vm_core::atomic_io::atomic_copy_with_mode(source, destination, mode)
}

#[cfg(windows)]
pub fn atomic_write_with_durability(
    path: &Path,
    bytes: &[u8],
    durability: AtomicWriteDurability,
) -> io::Result<AtomicWriteReceipt> {
    let _scope = crate::conditional_replace::scope_native_lock_root();
    harn_vm_core::atomic_io::atomic_write_with_durability(path, bytes, durability)
}

#[cfg(windows)]
pub fn atomic_write_with<F>(path: &Path, write_fn: F) -> io::Result<()>
where
    F: FnOnce(&mut BufWriter<File>) -> io::Result<()>,
{
    let _scope = crate::conditional_replace::scope_native_lock_root();
    harn_vm_core::atomic_io::atomic_write_with(path, write_fn)
}
