//! Root-anchored, no-follow effects on regular workspace files.
//!
//! The root directory is opened once. Every later step is relative to a
//! directory descriptor opened with `O_NOFOLLOW | O_DIRECTORY`, so a component
//! swapped for a symlink mid-restore fails the open instead of redirecting the
//! write outside the root.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use sha2::{Digest, Sha256};

/// Marker hash for anything that is not a regular file (symlink, directory,
/// fifo); it can never equal an attributed content hash.
pub(crate) const NOT_A_REGULAR_FILE: &str = "not-a-regular-file";

/// A parent directory reached by walking from the root with no-follow opens.
pub(crate) struct Parent {
    /// Directory descriptors from the root's child down to the parent, with
    /// each directory's name, for pruning emptied directories.
    chain: Vec<(String, OwnedFd)>,
    root: OwnedFd,
    name: CString,
    display: String,
}

pub(crate) struct Root(OwnedFd);

pub(crate) struct FileImage {
    pub bytes: Vec<u8>,
    pub permissions: libc::mode_t,
}

pub(crate) struct PreparedWrite<'a> {
    parent: &'a Parent,
    name: CString,
    permissions: libc::mode_t,
    armed: bool,
}

impl PreparedWrite<'_> {
    pub(crate) fn permissions(&self) -> libc::mode_t {
        self.permissions
    }

    pub(crate) fn publish(mut self, no_replace: bool) -> Result<bool, String> {
        let parent = self.parent;
        let result = if no_replace {
            // SAFETY: both names are NUL-terminated and the directory descriptor is live.
            unsafe {
                libc::linkat(
                    parent.dir(),
                    self.name.as_ptr(),
                    parent.dir(),
                    parent.name.as_ptr(),
                    0,
                )
            }
        } else {
            // SAFETY: both names are NUL-terminated and the directory descriptor is live.
            unsafe {
                libc::renameat(
                    parent.dir(),
                    self.name.as_ptr(),
                    parent.dir(),
                    parent.name.as_ptr(),
                )
            }
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            return if no_replace && error.kind() == io::ErrorKind::AlreadyExists {
                Ok(false)
            } else {
                Err(format!("publish {}: {error}", parent.display))
            };
        }
        if no_replace {
            // SAFETY: the temporary name belongs to this prepared write in a live directory.
            unsafe { libc::unlinkat(parent.dir(), self.name.as_ptr(), 0) };
        }
        self.armed = false;
        Ok(true)
    }
}

impl Drop for PreparedWrite<'_> {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: the temporary name belongs to this prepared write in a live directory.
            unsafe { libc::unlinkat(self.parent.dir(), self.name.as_ptr(), 0) };
        }
    }
}

fn cstr(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn open_at(
    dir: RawFd,
    name: &CString,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<OwnedFd> {
    // SAFETY: `name` is NUL-terminated and `dir` is a live descriptor.
    let fd = unsafe {
        libc::openat(
            dir,
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            libc::c_uint::from(mode),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a fresh descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn is_symlink_or_not_dir(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ELOOP | libc::ENOTDIR | libc::EMLINK)
    )
}

fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `st` is valid for writes and `fd` is a live descriptor.
    if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fstat` succeeded and initialized the struct.
    Ok(unsafe { st.assume_init() })
}

fn hash_fd(fd: OwnedFd) -> io::Result<String> {
    let mut file = std::fs::File::from(fd);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            return Ok(hex::encode(hasher.finalize()));
        }
        hasher.update(&buf[..n]);
    }
}

impl Root {
    pub(crate) fn open(root: &Path) -> Result<Self, String> {
        let name = cstr(root.as_os_str().as_bytes()).map_err(|e| format!("root path: {e}"))?;
        open_at(libc::AT_FDCWD, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
            .map(Self)
            .map_err(|e| format!("open root {}: {e}", root.display()))
    }

    /// Walk to the directory holding `rel`. A missing directory is `Ok(None)`
    /// unless `create`; a symlink or non-directory component is an error.
    pub(crate) fn parent_of(&self, rel: &str, create: bool) -> Result<Option<Parent>, String> {
        let mut names = Vec::new();
        for component in Path::new(rel).components() {
            match component {
                Component::Normal(n) => names.push(n.as_bytes().to_vec()),
                _ => return Err(format!("{rel}: not a plain relative path")),
            }
        }
        let Some(leaf) = names.pop() else {
            return Err(format!("{rel}: empty path"));
        };
        let mut chain: Vec<(String, OwnedFd)> = Vec::new();
        let mut walked = String::new();
        for part in names {
            let dir = chain
                .last()
                .map_or(self.0.as_raw_fd(), |(_, fd)| fd.as_raw_fd());
            let name = cstr(&part).map_err(|e| format!("{rel}: {e}"))?;
            let label = String::from_utf8_lossy(&part).into_owned();
            walked.push_str(&label);
            walked.push('/');
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
            let mut opened = open_at(dir, &name, flags, 0);
            if create && matches!(&opened, Err(e) if e.kind() == io::ErrorKind::NotFound) {
                // SAFETY: `name` is NUL-terminated and `dir` is a live descriptor.
                let made = unsafe { libc::mkdirat(dir, name.as_ptr(), 0o777) };
                if made != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
                    return Err(format!("create {walked}: {}", io::Error::last_os_error()));
                }
                opened = open_at(dir, &name, flags, 0);
            }
            match opened {
                Ok(fd) => chain.push((label, fd)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) if is_symlink_or_not_dir(&e) => {
                    return Err(format!(
                        "{walked} is a symlink or not a directory; refusing to write through it"
                    ))
                }
                Err(e) => return Err(format!("open {walked}: {e}")),
            }
        }
        Ok(Some(Parent {
            chain,
            root: self
                .0
                .try_clone()
                .map_err(|error| format!("retain root: {error}"))?,
            name: cstr(&leaf).map_err(|e| format!("{rel}: {e}"))?,
            display: rel.to_string(),
        }))
    }
}

impl Parent {
    fn dir(&self) -> RawFd {
        self.chain
            .last()
            .map_or_else(|| self.root.as_raw_fd(), |(_, fd)| fd.as_raw_fd())
    }

    /// SHA-256 of the entry; `None` when missing. A symlink, directory, or
    /// other non-regular entry hashes as [`NOT_A_REGULAR_FILE`].
    pub(crate) fn hash(&self) -> Result<Option<String>, String> {
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        let fd = match open_at(self.dir(), &self.name, flags, 0) {
            Ok(fd) => fd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) if is_symlink_or_not_dir(&e) => return Ok(Some(NOT_A_REGULAR_FILE.into())),
            Err(e) => return Err(format!("open {}: {e}", self.display)),
        };
        let st = fstat(fd.as_raw_fd()).map_err(|e| format!("stat {}: {e}", self.display))?;
        if st.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Ok(Some(NOT_A_REGULAR_FILE.into()));
        }
        hash_fd(fd)
            .map(Some)
            .map_err(|e| format!("read {}: {e}", self.display))
    }

    /// Read bytes and permissions from the same no-follow regular-file handle.
    pub(crate) fn image(&self) -> Result<Option<FileImage>, String> {
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        let fd = match open_at(self.dir(), &self.name, flags, 0) {
            Ok(fd) => fd,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("open {}: {error}", self.display)),
        };
        let metadata =
            fstat(fd.as_raw_fd()).map_err(|error| format!("stat {}: {error}", self.display))?;
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(format!("{} is not a regular file", self.display));
        }
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut std::fs::File::from(fd), &mut bytes)
            .map_err(|error| format!("read {}: {error}", self.display))?;
        Ok(Some(FileImage {
            bytes,
            permissions: metadata.st_mode & 0o7777,
        }))
    }

    /// Stage bytes and publish them, optionally requiring an absent destination.
    /// Returns false when a no-replace destination appeared concurrently.
    pub(crate) fn write(
        &self,
        bytes: &[u8],
        exec: bool,
        permissions: Option<libc::mode_t>,
        no_replace: bool,
        recheck: impl FnOnce(&Self) -> Result<(), String>,
    ) -> Result<bool, String> {
        let prepared = self.prepare(bytes, exec, permissions)?;
        recheck(self)?;
        prepared.publish(no_replace)
    }

    /// Flush a private temporary image without changing the destination.
    pub(crate) fn prepare(
        &self,
        bytes: &[u8],
        exec: bool,
        permissions: Option<libc::mode_t>,
    ) -> Result<PreparedWrite<'_>, String> {
        let tmp = cstr(
            harn_vm::atomic_io::temp_sibling_name_os(std::ffi::OsStr::from_bytes(
                self.name.as_bytes(),
            ))
            .as_bytes(),
        )
        .map_err(|e| format!("{}: {e}", self.display))?;
        let fail = |what: &str, e: io::Error| format!("{what} {}: {e}", self.display);

        let existing = open_at(
            self.dir(),
            &self.name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )
        .ok()
        .and_then(|fd| fstat(fd.as_raw_fd()).ok())
        .filter(|st| st.st_mode & libc::S_IFMT == libc::S_IFREG)
        .map(|st| st.st_mode & 0o7777);

        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW;
        let fd = open_at(self.dir(), &tmp, flags, 0o666).map_err(|e| fail("create temp for", e))?;
        let unlink_tmp = || {
            // SAFETY: `tmp` is NUL-terminated and the directory descriptor is live.
            unsafe { libc::unlinkat(self.dir(), tmp.as_ptr(), 0) };
        };
        let staged = (|| {
            let mut file = std::fs::File::from(fd);
            io::Write::write_all(&mut file, bytes).map_err(|e| fail("write", e))?;
            file.sync_all().map_err(|e| fail("sync", e))?;
            let mode = permissions.or(existing).unwrap_or_else(|| {
                // Keep the umask-filtered mode `O_CREAT` gave the temp file.
                fstat(file.as_raw_fd()).map_or(0o644, |st| st.st_mode & 0o7777)
            });
            let mode = if permissions.is_some() {
                mode
            } else if exec {
                mode | 0o111
            } else {
                mode & !0o111
            };
            // SAFETY: `file` owns a live descriptor.
            if unsafe { libc::fchmod(file.as_raw_fd(), mode) } != 0 {
                return Err(fail("chmod", io::Error::last_os_error()));
            }
            Ok(mode)
        })();
        match staged {
            Ok(permissions) => Ok(PreparedWrite {
                parent: self,
                name: tmp,
                permissions,
                armed: true,
            }),
            Err(error) => {
                unlink_tmp();
                Err(error)
            }
        }
    }

    /// Unlink the entry, then remove directories left empty up to the root.
    pub(crate) fn delete(&self) -> Result<(), String> {
        self.unlink()?;
        self.prune_empty_parents();
        Ok(())
    }

    /// Remove a file while retaining its parent directories.
    pub(crate) fn unlink(&self) -> Result<(), String> {
        // SAFETY: `name` is NUL-terminated and the directory descriptor is live.
        if unsafe { libc::unlinkat(self.dir(), self.name.as_ptr(), 0) } != 0 {
            return Err(format!(
                "delete {}: {}",
                self.display,
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn prune_empty_parents(&self) {
        for i in (0..self.chain.len()).rev() {
            let up = if i == 0 {
                self.root.as_raw_fd()
            } else {
                self.chain[i - 1].1.as_raw_fd()
            };
            let Ok(name) = cstr(self.chain[i].0.as_bytes()) else {
                break;
            };
            // SAFETY: `name` is NUL-terminated and `up` is a live descriptor.
            if unsafe { libc::unlinkat(up, name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
                break;
            }
        }
    }
}
