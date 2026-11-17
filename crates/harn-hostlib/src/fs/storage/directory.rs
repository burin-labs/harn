//! Journal effects stay relative to the retained workspace directory.

use std::collections::BTreeSet;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use cap_std::fs::{Dir, OpenOptions};

pub(super) struct SessionStorage {
    root: Dir,
    session: PathBuf,
}

impl SessionStorage {
    pub(super) fn open(root: &Path, session: &str) -> io::Result<Self> {
        Ok(Self {
            root: Dir::open_ambient_dir(root, cap_std::ambient_authority())?,
            session: super::session_dir(Path::new(""), session),
        })
    }

    fn path(&self, name: &Path) -> PathBuf {
        self.session.join(name)
    }

    pub(super) fn read(&self, name: &Path) -> io::Result<Vec<u8>> {
        let mut options = OpenOptions::new();
        options.read(true);
        regular_options(&mut options);
        let mut file = self.root.open_with(self.path(name), &options)?;
        require_regular(&file)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn parent(&self, name: &Path) -> io::Result<(Dir, std::ffi::OsString)> {
        let path = self.path(name);
        let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        self.root.create_dir_all(parent)?;
        let parent = self.root.open_dir(parent)?;
        let name = path
            .file_name()
            .ok_or(io::ErrorKind::InvalidInput)?
            .to_owned();
        Ok((parent, name))
    }

    pub(super) fn write(&self, name: &Path, bytes: &[u8]) -> io::Result<()> {
        let (parent, name) = self.parent(name)?;
        let permissions = match parent.symlink_metadata(&name) {
            Ok(metadata) if metadata.is_file() => Some(metadata.permissions()),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "staged journal destination is not a regular file",
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let temporary = harn_vm::atomic_io::temp_sibling_name_os(&name);
        let mut file =
            parent.open_with(&temporary, OpenOptions::new().write(true).create_new(true))?;
        let result = (|| {
            file.write_all(bytes)?;
            if let Some(permissions) = permissions {
                file.set_permissions(permissions)?;
            }
            file.sync_all()?;
            parent.rename(&temporary, &parent, &name)
        })();
        if result.is_err() {
            let _ = parent.remove_file(&temporary);
        }
        result
    }

    pub(super) fn append(&self, name: &Path, bytes: &[u8]) -> io::Result<()> {
        let (parent, name) = self.parent(name)?;
        let mut options = OpenOptions::new();
        options.append(true).create(true);
        regular_options(&mut options);
        let mut file = parent.open_with(name, &options)?;
        require_regular(&file)?;
        file.write_all(bytes)?;
        file.flush()
    }

    pub(super) fn prune_bodies(&self, keep: &BTreeSet<String>) -> io::Result<()> {
        let directory = self.root.open_dir(self.path(Path::new("bodies")))?;
        for entry in directory.entries()? {
            let entry = entry?;
            let name = entry.file_name();
            if !keep.contains(&name.to_string_lossy().into_owned()) {
                let _ = directory.remove_file(name);
            }
        }
        Ok(())
    }

    pub(super) fn remove(&self) -> io::Result<()> {
        self.root.remove_dir_all(&self.session)
    }
}

fn require_regular(file: &cap_std::fs::File) -> io::Result<()> {
    if file.metadata()?.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "staged journal entry is not a regular file",
        ))
    }
}

fn regular_options(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = options;
}
