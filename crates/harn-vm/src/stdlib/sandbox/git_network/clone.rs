//! Stage a clone privately, then publish a new ordinary checkout after reap.

use std::io;

use super::{Invocation, SeedRequest, Seeded};

#[cfg(unix)]
use crate::stdlib::sandbox::scoped_fs::{self, ScopedMutationTarget};
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub(crate) struct CloneCompletion {
    repository: PathBuf,
    worktree: PathBuf,
    target: ScopedMutationTarget,
    marker: Vec<u8>,
}

#[cfg(not(unix))]
pub(crate) struct CloneCompletion;

#[cfg(all(test, unix))]
thread_local! {
    static BEFORE_PUBLICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(unix)]
pub(super) fn seed(
    request: &SeedRequest<'_>,
    invocation: &Invocation<'_>,
) -> io::Result<Option<Seeded>> {
    use crate::stdlib::sandbox::{paths::normalize_for_policy, FsAccess};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let url = invocation.repositories[0];
    let default_name = super::granted_url_path(url)
        .and_then(|path| path.rsplit('/').next())
        .expect("validated clone URL");
    let default_name = default_name.strip_suffix(".git").unwrap_or(default_name);
    let destination = normalize_for_policy(
        &request
            .cwd
            .join(invocation.destination.unwrap_or(default_name)),
    );
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
    let (parent, name) = scoped_fs::open_parent_dir_scoped(&target)?;
    if scoped_fs::entry_exists_at(parent.as_raw_fd(), &name)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a granted clone requires a new destination",
        ));
    }
    let destination_device = std::fs::File::from(parent.try_clone()?).metadata()?.dev();
    if destination_device != std::fs::metadata(request.dir)?.dev() {
        return Err(cross_filesystem_refusal());
    }
    let repository = request.dir.join("repository");
    let worktree = request.dir.join("worktree");
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
    args.extend(
        request
            .args
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(index, _)| Some(*index) != invocation.destination_index)
            .map(|(_, word)| word.clone()),
    );
    args.push(worktree.display().to_string());
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
        write_roots: vec![repository.clone(), worktree.clone()],
        completion: Some(CloneCompletion {
            marker: format!("gitdir: {}\n", repository.display()).into_bytes(),
            repository,
            worktree,
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
        use std::os::fd::AsRawFd;

        // Failed and cancelled children never acquire or mutate a public entry.
        // GrantedSpawn retires the complete private directory after this returns.
        if !succeeded {
            return Ok(());
        }
        self.prepare_private_checkout()?;
        let (parent, name) = scoped_fs::open_parent_dir_scoped(&self.target)?;
        let private =
            scoped_fs::open_dir_absolute(self.worktree.parent().expect("private clone worktree"))?;
        #[cfg(test)]
        BEFORE_PUBLICATION.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        scoped_fs::renameat_name_noreplace(
            private.as_raw_fd(),
            "worktree",
            parent.as_raw_fd(),
            &name,
        )
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::EXDEV) {
                cross_filesystem_refusal()
            } else {
                error
            }
        })?;
        scoped_fs::sync_dir_fd(parent.as_raw_fd());
        Ok(())
    }

    #[cfg(unix)]
    fn prepare_private_checkout(&self) -> io::Result<()> {
        use std::io::Read;
        use std::os::fd::AsRawFd;

        let worktree = scoped_fs::open_dir_absolute(&self.worktree)?;
        let marker = scoped_fs::openat_file(
            worktree.as_raw_fd(),
            ".git",
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0,
        )?;
        if !marker.metadata()?.is_file() {
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
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "clone Git directory marker changed",
            ));
        }
        validate_git_directory(&self.repository)?;
        let private =
            scoped_fs::open_dir_absolute(self.repository.parent().expect("private Git directory"))?;
        // This marker and its parent remain inside the grant's protected scope.
        // No public workspace name is checked and subsequently unlinked.
        scoped_fs::unlinkat_name(worktree.as_raw_fd(), ".git", 0)?;
        scoped_fs::renameat_name_noreplace(
            private.as_raw_fd(),
            "repository",
            worktree.as_raw_fd(),
            ".git",
        )
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
fn cross_filesystem_refusal() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "granted clone destination must share a filesystem with Harn's private transfer directory; choose a destination on that filesystem or place HARN_HOME in a trusted directory on the destination filesystem",
    )
}

/// Git metadata must contain only regular files and directories. Worktree
/// symlinks remain untouched when the complete private checkout is renamed.
#[cfg(unix)]
fn validate_git_directory(source: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    struct Frame {
        entries: std::fs::ReadDir,
        source: std::os::fd::OwnedFd,
    }
    impl Frame {
        fn new(source: &Path) -> io::Result<Self> {
            Ok(Self {
                entries: std::fs::read_dir(source)?,
                source: scoped_fs::open_dir_absolute(source)?,
            })
        }
    }
    let mut frames = vec![Frame::new(source)?];
    while !frames.is_empty() {
        let Some(entry) = frames.last_mut().unwrap().entries.next() else {
            frames.pop();
            continue;
        };
        let entry = entry?;
        let name = entry.file_name();
        let kind = entry.file_type()?;
        let frame = frames.last().unwrap();
        if kind.is_dir() {
            frames.push(Frame::new(&entry.path())?);
        } else if kind.is_file() {
            let input = scoped_fs::openat_file(
                frame.source.as_raw_fd(),
                &name,
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0,
            )?;
            if !input.metadata()?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "clone tree entry is no longer a regular file",
                ));
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "clone tree contains a non-regular entry",
            ));
        }
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "clone/tests.rs"]
mod tests;
