use super::*;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

struct Fixture {
    root: tempfile::TempDir,
    repository: PathBuf,
    worktree: PathBuf,
    destination: PathBuf,
    marker: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        Self::at(root, &private)
    }

    fn at(root: tempfile::TempDir, private: &Path) -> Self {
        let repository = private.join("repository");
        let worktree = private.join("worktree");
        let destination = root.path().join("checkout");
        std::fs::create_dir_all(repository.join("objects")).unwrap();
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(repository.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        std::fs::write(repository.join("config"), b"[core]\n\tbare = false\n").unwrap();
        std::fs::write(
            repository.parent().unwrap().join("global-config"),
            b"controller-only",
        )
        .unwrap();
        let marker = format!("gitdir: {}\n", repository.display()).into_bytes();
        std::fs::write(worktree.join(".git"), &marker).unwrap();
        std::fs::write(worktree.join("tracked.txt"), b"checkout payload\n").unwrap();
        Self {
            root,
            repository,
            worktree,
            destination,
            marker,
        }
    }

    fn completion(&self) -> CloneCompletion {
        CloneCompletion {
            repository: self.repository.clone(),
            worktree: self.worktree.clone(),
            target: ScopedMutationTarget {
                root: self.root.path().to_path_buf(),
                relative: PathBuf::from("checkout"),
            },
            marker: self.marker.clone(),
        }
    }
}

#[test]
fn completed_clone_installs_only_its_ordinary_checkout() {
    let fixture = Fixture::new();
    assert!(
        !fixture.destination.exists(),
        "transfer never exposes a public marker"
    );
    fixture.completion().finish(true).unwrap();
    let git = fixture.destination.join(".git");
    assert!(git.is_dir());
    assert_eq!(
        std::fs::read(git.join("HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
    assert!(git.join("objects").is_dir());
    assert_eq!(
        std::fs::read(fixture.destination.join("tracked.txt")).unwrap(),
        b"checkout payload\n"
    );
    assert!(!git.join("global-config").exists());
    assert!(!fixture.repository.exists());
    assert!(!fixture.worktree.exists());
    assert_eq!(
        std::fs::read(fixture.repository.parent().unwrap().join("global-config")).unwrap(),
        b"controller-only"
    );
}

#[test]
fn a_destination_created_immediately_before_publication_is_preserved() {
    let fixture = Fixture::new();
    let destination = fixture.destination.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let (created_tx, created_rx) = std::sync::mpsc::sync_channel(1);
    let peer = std::thread::spawn(move || {
        harn_clock::test_support::recv_within("clone ready to publish", &ready_rx);
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join(".git"), b"foreign marker\n").unwrap();
        created_tx.send(()).unwrap();
    });
    BEFORE_PUBLICATION.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            ready_tx.send(()).unwrap();
            harn_clock::test_support::recv_within("peer created destination", &created_rx);
        }));
    });
    let error = fixture.completion().finish(true).unwrap_err();
    BEFORE_PUBLICATION.with(|hook| {
        hook.borrow_mut().take();
    });
    peer.join().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(fixture.destination.join(".git")).unwrap(),
        b"foreign marker\n"
    );
    assert!(fixture.worktree.join(".git/HEAD").is_file());
    assert!(!fixture.destination.join("tracked.txt").exists());
}

#[test]
fn failed_child_never_touches_a_public_destination() {
    let fixture = Fixture::new();
    fixture.completion().finish(false).unwrap();
    assert!(!fixture.destination.exists());
    assert_eq!(
        std::fs::read(fixture.worktree.join(".git")).unwrap(),
        fixture.marker
    );
    std::fs::create_dir(&fixture.destination).unwrap();
    std::fs::write(fixture.destination.join(".git"), b"foreign marker\n").unwrap();
    fixture.completion().finish(false).unwrap();
    assert_eq!(
        std::fs::read(fixture.destination.join(".git")).unwrap(),
        b"foreign marker\n"
    );
}

#[test]
fn private_marker_corruption_refuses_publication() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.worktree.join(".git"),
        b"gitdir: another-repository\n",
    )
    .unwrap();
    assert_eq!(
        fixture.completion().finish(true).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(!fixture.destination.exists());
    assert!(fixture.repository.join("HEAD").is_file());
}

#[test]
fn private_marker_symlink_and_replaced_parent_never_read_outside_files() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join(".git"), &fixture.marker).unwrap();
    std::fs::remove_file(fixture.worktree.join(".git")).unwrap();
    symlink(outside.path().join(".git"), fixture.worktree.join(".git")).unwrap();
    assert!(fixture.completion().finish(true).is_err());
    assert_eq!(
        std::fs::read(outside.path().join(".git")).unwrap(),
        fixture.marker
    );
    std::fs::rename(
        &fixture.worktree,
        fixture.worktree.with_file_name("original-worktree"),
    )
    .unwrap();
    symlink(outside.path(), &fixture.worktree).unwrap();
    assert!(fixture.completion().finish(true).is_err());
    assert_eq!(
        std::fs::read(outside.path().join(".git")).unwrap(),
        fixture.marker
    );
    assert!(!fixture.destination.exists());
}

#[test]
fn fifo_private_marker_is_refused_without_a_blocking_reader() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new();
    let marker = fixture.worktree.join(".git");
    std::fs::remove_file(&marker).unwrap();
    let path = std::ffi::CString::new(marker.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert_eq!(
        fixture.completion().finish(true).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(!fixture.destination.exists());
}

#[test]
fn metadata_symlinks_are_rejected_even_without_a_cross_filesystem_copy() {
    let fixture = Fixture::new();
    let outside = fixture.root.path().join("outside");
    std::fs::write(&outside, b"outside payload").unwrap();
    symlink(&outside, fixture.repository.join("objects/link")).unwrap();
    assert!(fixture.completion().finish(true).is_err());
    assert!(!fixture.destination.exists());
    assert_eq!(std::fs::read(outside).unwrap(), b"outside payload");
}

#[test]
fn checkout_copy_preserves_native_names_modes_and_symlink_text() {
    #[cfg(not(target_os = "macos"))]
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    fixture.completion().prepare_private_checkout().unwrap();
    #[cfg(not(target_os = "macos"))]
    let name = std::ffi::OsString::from_vec(b"ref-\xff".to_vec());
    #[cfg(target_os = "macos")]
    let name = std::ffi::OsString::from("ref-é");
    let file = fixture.worktree.join(&name);
    std::fs::write(&file, b"payload\0bytes").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o751)).unwrap();
    std::fs::set_permissions(&fixture.worktree, std::fs::Permissions::from_mode(0o750)).unwrap();
    let outside = fixture.root.path().join("outside");
    std::fs::write(&outside, b"do not follow").unwrap();
    symlink(&outside, fixture.worktree.join("outside-link")).unwrap();
    std::fs::create_dir(&fixture.destination).unwrap();
    walk_tree(
        &fixture.worktree,
        Some(scoped_fs::open_dir_absolute(&fixture.destination).unwrap()),
        TreeContents::Worktree,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(fixture.destination.join(&name)).unwrap(),
        b"payload\0bytes"
    );
    assert_eq!(
        std::fs::metadata(fixture.destination.join(name))
            .unwrap()
            .mode()
            & 0o777,
        0o751
    );
    assert_eq!(
        std::fs::metadata(&fixture.destination).unwrap().mode() & 0o777,
        0o750
    );
    let link = fixture.destination.join("outside-link");
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read_link(link).unwrap(), outside);
    assert_eq!(std::fs::read(&outside).unwrap(), b"do not follow");
}

#[test]
fn checkout_copy_keeps_git_metadata_strict() {
    let fixture = Fixture::new();
    fixture.completion().prepare_private_checkout().unwrap();
    symlink(
        "../../tracked.txt",
        fixture.worktree.join(".git/objects/link"),
    )
    .unwrap();
    std::fs::create_dir(&fixture.destination).unwrap();
    assert!(walk_tree(
        &fixture.worktree,
        Some(scoped_fs::open_dir_absolute(&fixture.destination).unwrap()),
        TreeContents::Worktree
    )
    .is_err());
}

fn git(program: &Path, cwd: &Path, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn native_checkout(private: &Path, root: &Path) {
    let root = root.canonicalize().unwrap();
    let private = private.canonicalize().unwrap();
    let source = root.join("source");
    let destination = root.join("checkout");
    let repository = private.join("repository");
    let worktree = private.join("worktree");
    let template = private.join("empty-template");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&template).unwrap();
    let program = PathBuf::from(crate::stdlib::process::resolve_program_path_for_spawn(
        "git",
    ))
    .canonicalize()
    .expect("native Git on PATH");
    git(&program, &source, &["init", "--quiet"]);
    std::fs::write(source.join("tracked.txt"), b"known clone payload\n").unwrap();
    std::fs::write(source.join("executable"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        source.join("executable"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    symlink("tracked.txt", source.join("relative-link")).unwrap();
    git(&program, &source, &["add", "."]);
    git(
        &program,
        &source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "commit",
            "--quiet",
            "-m",
            "Fixture",
        ],
    );
    let output = std::process::Command::new(&program)
        .args(["-c", "protocol.file.allow=always", "clone", "--quiet"])
        .arg(format!("--separate-git-dir={}", repository.display()))
        .arg(format!("--template={}", template.display()))
        .arg(url::Url::from_directory_path(&source).unwrap().as_str())
        .arg(&worktree)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_DIR", &repository)
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(worktree.join(".git").is_file());
    assert!(!destination.exists());
    let executable_mode = std::fs::metadata(worktree.join("executable"))
        .unwrap()
        .mode()
        & 0o777;
    assert_ne!(
        executable_mode & 0o111,
        0,
        "Git checked out the executable bit"
    );
    CloneCompletion {
        repository: repository.clone(),
        worktree,
        target: ScopedMutationTarget {
            root,
            relative: "checkout".into(),
        },
        marker: format!("gitdir: {}\n", repository.display()).into_bytes(),
    }
    .finish(true)
    .unwrap();
    assert!(destination.join(".git").is_dir());
    assert_eq!(
        git(&program, &destination, &["rev-parse", "HEAD"]),
        git(&program, &source, &["rev-parse", "HEAD"])
    );
    assert!(git(&program, &destination, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        std::fs::read(destination.join("tracked.txt")).unwrap(),
        b"known clone payload\n"
    );
    assert_eq!(
        std::fs::read_link(destination.join("relative-link")).unwrap(),
        Path::new("tracked.txt")
    );
    assert_eq!(
        std::fs::metadata(destination.join("executable"))
            .unwrap()
            .mode()
            & 0o777,
        executable_mode
    );
    let config = String::from_utf8(git(
        &program,
        &destination,
        &["config", "--local", "--list"],
    ))
    .unwrap();
    assert!(!config.contains("credential."));
    assert!(!config.contains("http."));
    assert!(!config.contains(private.to_str().unwrap()));
}

#[test]
fn native_clone_remains_an_ordinary_worktree_after_private_checkout_publication() {
    let root = tempfile::tempdir().unwrap();
    let private = tempfile::tempdir().unwrap();
    native_checkout(private.path(), root.path());
}

#[cfg(target_os = "linux")]
#[test]
fn actual_cross_device_clone_publication_preserves_clean_git_status_and_symlinks() {
    use std::os::fd::AsRawFd;
    let root = tempfile::tempdir().unwrap();
    let private = tempfile::tempdir_in("/dev/shm").expect("Linux EXDEV fixture needs /dev/shm");
    assert_ne!(
        std::fs::metadata(root.path()).unwrap().dev(),
        std::fs::metadata(private.path()).unwrap().dev(),
        "EXDEV control needs distinct filesystem devices"
    );
    std::fs::create_dir(private.path().join("probe")).unwrap();
    let from = scoped_fs::open_dir_absolute(private.path()).unwrap();
    let to = scoped_fs::open_dir_absolute(root.path()).unwrap();
    assert_eq!(
        scoped_fs::renameat_name_noreplace(from.as_raw_fd(), "probe", to.as_raw_fd(), "probe")
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EXDEV)
    );
    native_checkout(private.path(), root.path());
}

#[cfg(target_os = "linux")]
fn cross_device_fixture() -> (Fixture, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let private = tempfile::tempdir_in("/dev/shm").expect("Linux EXDEV fixture needs /dev/shm");
    assert_ne!(
        std::fs::metadata(root.path()).unwrap().dev(),
        std::fs::metadata(private.path()).unwrap().dev()
    );
    (Fixture::at(root, private.path()), private)
}

#[cfg(target_os = "linux")]
#[test]
fn cross_device_publication_refuses_a_replaced_copy_directory() {
    let (fixture, _private) = cross_device_fixture();
    let root = fixture.root.path().to_path_buf();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<PathBuf>(1);
    let (replaced_tx, replaced_rx) = std::sync::mpsc::sync_channel(1);
    let peer = std::thread::spawn(move || {
        let copy = harn_clock::test_support::recv_within("cross-device copy complete", &ready_rx);
        std::fs::rename(&copy, copy.with_file_name("original-copy")).unwrap();
        std::fs::create_dir(&copy).unwrap();
        std::fs::write(copy.join("foreign"), b"foreign copy directory\n").unwrap();
        replaced_tx.send(()).unwrap();
    });
    AFTER_COPY.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |name| {
            ready_tx.send(root.join(name)).unwrap();
            harn_clock::test_support::recv_within("peer replaced copy directory", &replaced_rx);
        }));
    });
    let result = fixture.completion().finish(true);
    AFTER_COPY.with(|hook| {
        hook.borrow_mut().take();
    });
    peer.join().unwrap();
    assert!(
        result.is_err(),
        "a foreign copied directory cannot establish clone success"
    );
    assert_eq!(
        std::fs::read(fixture.destination.join("foreign")).unwrap(),
        b"foreign copy directory\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cross_device_copy_failure_preserves_peer_data_and_reports_partial_output() {
    use std::os::unix::ffi::OsStrExt;
    let (fixture, _private) = cross_device_fixture();
    let root = fixture.root.path().to_path_buf();
    let worktree = fixture.worktree.clone();
    let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
    let reached = observed.clone();
    BEFORE_COPY.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |name| {
            let copy = root.join(name);
            std::fs::write(copy.join("peer-canary"), b"preserve peer data\n").unwrap();
            let fifo =
                std::ffi::CString::new(worktree.join("unreadable-node").as_os_str().as_bytes())
                    .unwrap();
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            *reached.lock().unwrap() = Some(copy);
        }));
    });
    let error = fixture.completion().finish(true).unwrap_err();
    BEFORE_COPY.with(|hook| {
        hook.borrow_mut().take();
    });
    let observed = observed.lock().unwrap();
    let copy = observed
        .as_ref()
        .expect("actual cross-device copy was reached");
    assert_eq!(
        std::fs::read(copy.join("peer-canary")).unwrap(),
        b"preserve peer data\n"
    );
    assert!(error
        .to_string()
        .contains("partial clone destination retained"));
}
