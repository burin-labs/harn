//! Keep the transfer Git directory private until the child has exited.

use std::io;

use super::{Invocation, SeedRequest, Seeded};

#[cfg(unix)]
use crate::stdlib::sandbox::scoped_fs::{self, ScopedMutationTarget};
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub(crate) struct CloneCompletion {
    repository: PathBuf,
    target: ScopedMutationTarget,
    marker: Vec<u8>,
}

#[cfg(not(unix))]
pub(crate) struct CloneCompletion;

#[cfg(unix)]
pub(super) fn seed(
    request: &SeedRequest<'_>,
    invocation: &Invocation<'_>,
) -> io::Result<Option<Seeded>> {
    use crate::stdlib::sandbox::{paths::normalize_for_policy, FsAccess};

    let url = invocation.repositories[0];
    let default_name = super::granted_url_path(url)
        .and_then(|path| path.rsplit('/').next())
        .expect("validated clone URL");
    let default_name = default_name.strip_suffix(".git").unwrap_or(default_name);
    let destination = request
        .cwd
        .join(invocation.destination.unwrap_or(default_name));
    let destination = normalize_for_policy(&destination.join(".git"));
    let target = match scoped_fs::scoped_mutation_target(
        "granted git clone",
        &destination,
        FsAccess::Write,
    )? {
        Some(target) => target,
        None => {
            let root = destination
                .ancestors()
                .last()
                .expect("absolute clone target");
            ScopedMutationTarget {
                root: root.to_path_buf(),
                relative: destination
                    .strip_prefix(root)
                    .expect("absolute clone target")
                    .to_path_buf(),
            }
        }
    };
    let repository = request.dir.join("repository");
    let config = request.dir.join(super::GLOBAL_CONFIG);
    super::super::grant_spawn::write_private_file(
        &config,
        &super::render_config(request.dir, &super::Projection::new()),
    )?;
    let mut args = vec![
        "clone".into(),
        format!("--separate-git-dir={}", repository.display()),
        format!("--template={}", request.dir.join("hooks").display()),
    ];
    args.extend_from_slice(&request.args[1..]);
    Ok(Some(Seeded {
        args: Some(args),
        env: vec![
            ("GIT_DIR".into(), repository.display().to_string()),
            ("GIT_CONFIG_GLOBAL".into(), config.display().to_string()),
        ],
        hold: Some(super::serve_login(
            request.dir,
            &[url.to_string()],
            request.token,
        )?),
        write_roots: vec![repository.clone()],
        completion: Some(CloneCompletion {
            marker: format!("gitdir: {}\n", repository.display()).into_bytes(),
            repository,
            target,
        }),
    }))
}

#[cfg(not(unix))]
pub(super) fn seed(
    _request: &SeedRequest<'_>,
    _invocation: &Invocation<'_>,
) -> io::Result<Option<Seeded>> {
    Ok(None)
}

impl CloneCompletion {
    #[cfg(unix)]
    pub(crate) fn finish(self, succeeded: bool) -> io::Result<()> {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;

        let (parent, name) = match scoped_fs::open_parent_dir_scoped(&self.target) {
            Ok(target) => target,
            Err(error) if !succeeded && error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let marker = match scoped_fs::openat_file(
            parent.as_raw_fd(),
            &name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0,
        ) {
            Ok(marker) => marker,
            Err(error) if !succeeded && error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let metadata = marker.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "clone Git directory marker is not a regular file",
            ));
        }
        let mut contents = Vec::new();
        marker
            .take(self.marker.len() as u64 + 1)
            .read_to_end(&mut contents)?;
        if contents != self.marker {
            return if succeeded {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "clone Git directory marker changed",
                ))
            } else {
                Ok(())
            };
        }
        let unchanged = || {
            let current = scoped_fs::openat_file(
                parent.as_raw_fd(),
                &name,
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0,
            )?
            .metadata()?;
            if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "clone Git directory marker changed",
                ));
            }
            Ok(())
        };
        if !succeeded {
            unchanged()?;
            return scoped_fs::unlinkat_name(parent.as_raw_fd(), &name, 0);
        }
        let stage = format!(".harn-git-{}", uuid::Uuid::now_v7());
        scoped_fs::mkdirat_name(parent.as_raw_fd(), &stage)?;
        let source_parent =
            scoped_fs::open_dir_absolute(self.repository.parent().expect("private Git directory"))?;
        match scoped_fs::renameat_name(
            source_parent.as_raw_fd(),
            "repository",
            parent.as_raw_fd(),
            &stage,
        ) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                let stage_dir = scoped_fs::open_dir_at(parent.as_raw_fd(), &stage)?;
                copy_tree(&self.repository, stage_dir)?;
            }
            Err(error) => return Err(error),
        }
        unchanged()?;
        scoped_fs::unlinkat_name(parent.as_raw_fd(), &name, 0)?;
        scoped_fs::renameat_name_noreplace(parent.as_raw_fd(), &stage, parent.as_raw_fd(), &name)?;
        scoped_fs::sync_dir_fd(parent.as_raw_fd());
        Ok(())
    }

    #[cfg(not(unix))]
    pub(crate) fn finish(self, _succeeded: bool) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "granted Git clone needs a supported Unix host",
        ))
    }
}

#[cfg(unix)]
fn copy_tree(source: &Path, destination: std::os::fd::OwnedFd) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    struct Frame {
        entries: std::fs::ReadDir,
        source: std::os::fd::OwnedFd,
        destination: std::os::fd::OwnedFd,
    }
    impl Frame {
        fn new(source: &Path, destination: std::os::fd::OwnedFd) -> io::Result<Self> {
            Ok(Self {
                entries: std::fs::read_dir(source)?,
                source: scoped_fs::open_dir_absolute(source)?,
                destination,
            })
        }
    }
    let mut frames = vec![Frame::new(source, destination)?];
    while !frames.is_empty() {
        let Some(entry) = frames.last_mut().unwrap().entries.next() else {
            scoped_fs::sync_dir_fd(frames.pop().unwrap().destination.as_raw_fd());
            continue;
        };
        let entry = entry?;
        let name = entry.file_name();
        let kind = entry.file_type()?;
        let frame = frames.last().unwrap();
        if kind.is_dir() {
            scoped_fs::mkdirat_name(frame.destination.as_raw_fd(), &name)?;
            let child = scoped_fs::open_dir_at(frame.destination.as_raw_fd(), &name)?;
            frames.push(Frame::new(&entry.path(), child)?);
        } else if kind.is_file() {
            let mut input = scoped_fs::openat_file(
                frame.source.as_raw_fd(),
                &name,
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )?;
            let mut output = scoped_fs::openat_file(
                frame.destination.as_raw_fd(),
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )?;
            io::copy(&mut input, &mut output)?;
            output.set_permissions(std::fs::Permissions::from_mode(
                input.metadata()?.mode() & 0o777,
            ))?;
            output.sync_all()?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "clone Git directory contains a non-regular entry",
            ));
        }
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "clone/tests.rs"]
mod tests;
