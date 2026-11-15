use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

struct Fixture {
    root: tempfile::TempDir,
    repository: PathBuf,
    destination: PathBuf,
    marker: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("private/repository");
        let destination = root.path().join("checkout");
        std::fs::create_dir_all(repository.join("objects")).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(repository.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        std::fs::write(repository.join("config"), b"[core]\n\tbare = false\n").unwrap();
        std::fs::write(
            repository.parent().unwrap().join("global-config"),
            b"controller-only",
        )
        .unwrap();
        let marker = format!("gitdir: {}\n", repository.display()).into_bytes();
        std::fs::write(destination.join(".git"), &marker).unwrap();
        Self {
            root,
            repository,
            destination,
            marker,
        }
    }

    fn completion(&self) -> CloneCompletion {
        CloneCompletion {
            repository: self.repository.clone(),
            target: ScopedMutationTarget {
                root: self.root.path().to_path_buf(),
                relative: PathBuf::from("checkout/.git"),
            },
            marker: self.marker.clone(),
        }
    }
}

#[test]
fn completed_clone_installs_only_its_git_directory() {
    let fixture = Fixture::new();
    fixture.completion().finish(true).unwrap();
    let installed = fixture.destination.join(".git");
    assert!(installed.is_dir());
    assert_eq!(
        std::fs::read(installed.join("HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
    assert!(installed.join("objects").is_dir());
    assert!(!installed.join("global-config").exists());
    assert!(!fixture.repository.exists());
    assert_eq!(
        std::fs::read(fixture.repository.parent().unwrap().join("global-config")).unwrap(),
        b"controller-only"
    );
}

#[test]
fn native_clone_remains_an_ordinary_worktree_after_private_directory_handoff() {
    let root = tempfile::tempdir().unwrap();
    let canonical_root = root.path().canonicalize().unwrap();
    let source = canonical_root.join("source");
    let private = canonical_root.join("private");
    let destination = canonical_root.join("checkout");
    let repository = private.join("repository");
    let template = private.join("empty-template");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir_all(&template).unwrap();
    let program = PathBuf::from(crate::stdlib::process::resolve_program_path_for_spawn(
        "git",
    ))
    .canonicalize()
    .expect("native Git on PATH");
    let run = |cwd: &Path, args: &[&str]| {
        let output = std::process::Command::new(&program)
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    run(&source, &["init", "--quiet"]);
    std::fs::write(source.join("tracked.txt"), b"known clone payload\n").unwrap();
    run(&source, &["add", "tracked.txt"]);
    run(
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
        .arg(&destination)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_DIR", &repository)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(destination.join(".git").is_file());
    CloneCompletion {
        repository: repository.clone(),
        target: ScopedMutationTarget {
            root: canonical_root,
            relative: "checkout/.git".into(),
        },
        marker: format!("gitdir: {}\n", repository.display()).into_bytes(),
    }
    .finish(true)
    .unwrap();
    assert!(destination.join(".git").is_dir());
    assert_eq!(
        run(&destination, &["rev-parse", "HEAD"]),
        run(&source, &["rev-parse", "HEAD"])
    );
    assert!(run(&destination, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        std::fs::read(destination.join("tracked.txt")).unwrap(),
        b"known clone payload\n"
    );
    let config = String::from_utf8(run(&destination, &["config", "--local", "--list"])).unwrap();
    assert!(!config.contains("credential."));
    assert!(!config.contains("http."));
    assert!(!config.contains(private.to_str().unwrap()));
    assert!(!repository.exists());
}

#[test]
fn successful_child_cannot_replace_another_git_marker() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.destination.join(".git"),
        b"gitdir: another-repository\n",
    )
    .unwrap();
    let error = fixture.completion().finish(true).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        std::fs::read(fixture.destination.join(".git")).unwrap(),
        b"gitdir: another-repository\n"
    );
    assert!(fixture.repository.join("HEAD").is_file());
}

#[test]
fn exclusive_publication_preserves_a_new_destination_and_reaches_a_free_one() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("stage")).unwrap();
    std::fs::create_dir(root.path().join("occupied")).unwrap();
    let original = std::fs::metadata(root.path().join("occupied"))
        .unwrap()
        .ino();
    let parent = scoped_fs::open_dir_absolute(root.path()).unwrap();
    let error = scoped_fs::renameat_name_noreplace(
        parent.as_raw_fd(),
        "stage",
        parent.as_raw_fd(),
        "occupied",
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::metadata(root.path().join("occupied"))
            .unwrap()
            .ino(),
        original
    );
    assert!(root.path().join("stage").is_dir());
    scoped_fs::renameat_name_noreplace(
        parent.as_raw_fd(),
        "stage",
        parent.as_raw_fd(),
        "available",
    )
    .unwrap();
    assert!(root.path().join("available").is_dir());
    assert!(!root.path().join("stage").exists());
}

#[test]
fn failed_child_removes_only_the_marker_it_created() {
    let fixture = Fixture::new();
    fixture.completion().finish(false).unwrap();
    assert!(!fixture.destination.join(".git").exists());
    assert!(fixture.repository.join("HEAD").is_file());

    std::fs::write(fixture.destination.join(".git"), b"unrelated").unwrap();
    fixture.completion().finish(false).unwrap();
    assert_eq!(
        std::fs::read(fixture.destination.join(".git")).unwrap(),
        b"unrelated"
    );
}

#[test]
fn marker_symlink_and_replaced_parent_cannot_reach_outside_files() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join(".git"), &fixture.marker).unwrap();
    std::fs::remove_file(fixture.destination.join(".git")).unwrap();
    symlink(
        outside.path().join(".git"),
        fixture.destination.join(".git"),
    )
    .unwrap();
    assert!(fixture.completion().finish(true).is_err());
    assert_eq!(
        std::fs::read(outside.path().join(".git")).unwrap(),
        fixture.marker
    );
    std::fs::remove_file(fixture.destination.join(".git")).unwrap();
    std::fs::remove_dir(&fixture.destination).unwrap();
    symlink(outside.path(), &fixture.destination).unwrap();
    assert!(fixture.completion().finish(true).is_err());
    assert_eq!(
        std::fs::read(outside.path().join(".git")).unwrap(),
        fixture.marker
    );
    assert!(fixture.repository.join("HEAD").is_file());
}

#[test]
fn fifo_marker_is_refused_without_opening_a_blocking_reader() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new();
    let marker = fixture.destination.join(".git");
    std::fs::remove_file(&marker).unwrap();
    let path = std::ffi::CString::new(marker.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let error = fixture.completion().finish(true).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(fixture.repository.join("HEAD").is_file());
}

#[test]
fn cross_filesystem_copy_preserves_native_names_contents_and_permissions() {
    #[cfg(not(target_os = "macos"))]
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    #[cfg(not(target_os = "macos"))]
    let name = std::ffi::OsString::from_vec(b"ref-\xff".to_vec());
    #[cfg(target_os = "macos")]
    let name = std::ffi::OsString::from("ref-é");
    let file = fixture.repository.join("objects").join(&name);
    std::fs::write(&file, b"git-object\0payload").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    let destination = fixture.root.path().join("copy");
    std::fs::create_dir(&destination).unwrap();
    copy_tree(
        &fixture.repository,
        scoped_fs::open_dir_absolute(&destination).unwrap(),
    )
    .unwrap();
    let copied = destination.join("objects").join(name);
    assert_eq!(std::fs::read(&copied).unwrap(), b"git-object\0payload");
    assert_eq!(
        std::fs::metadata(copied).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        std::fs::read(destination.join("HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
}

#[test]
fn cross_filesystem_copy_refuses_symlinks() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("canary"), b"outside").unwrap();
    symlink(
        outside.path().join("canary"),
        fixture.repository.join("link"),
    )
    .unwrap();
    let destination = fixture.root.path().join("copy");
    std::fs::create_dir(&destination).unwrap();
    let error = copy_tree(
        &fixture.repository,
        scoped_fs::open_dir_absolute(&destination).unwrap(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!destination.join("link").exists());
    assert_eq!(
        std::fs::read(outside.path().join("canary")).unwrap(),
        b"outside"
    );
}
