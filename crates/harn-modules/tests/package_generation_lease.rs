use harn_modules::package_snapshot::{
    generation_root, package_current_path, package_generations_dir, package_lock_digest,
    package_publication_lock_path, PackageGenerationManifest, PackageGenerationPointer,
    GENERATION_LEASE_FILE, GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

const CHILD_ENV: &str = "HARN_PACKAGE_LEASE_TEST_CHILD";

fn publish(root: &Path, generation: &str, source: &str) {
    let generation_root = generation_root(root, generation);
    let packages_root = generation_root.join(GENERATION_PACKAGES_DIR);
    fs::create_dir_all(packages_root.join("acme")).unwrap();
    fs::write(packages_root.join("acme/module.harn"), source).unwrap();
    let lock = b"version = 4\n\n[[package]]\nname = \"acme\"\nsource = \"path+fixture\"\n";
    fs::write(generation_root.join(GENERATION_LOCK_FILE), lock).unwrap();
    fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
    let manifest = PackageGenerationManifest::new(generation, package_lock_digest(lock)).unwrap();
    fs::write(
        generation_root.join(GENERATION_MANIFEST_FILE),
        toml::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let publication_path = package_publication_lock_path(root);
    let publication = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&publication_path)
        .unwrap();
    publication.lock().unwrap();
    let pointer = PackageGenerationPointer::new(generation).unwrap();
    fs::write(
        package_current_path(root),
        toml::to_string_pretty(&pointer).unwrap(),
    )
    .unwrap();
    publication.unlock().unwrap();
}

fn collect(root: &Path, current: &str) {
    for entry in fs::read_dir(package_generations_dir(root)).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == current || !entry.path().is_dir() {
            continue;
        }
        let lease = match File::options()
            .read(true)
            .write(true)
            .open(entry.path().join(GENERATION_LEASE_FILE))
        {
            Ok(lease) => lease,
            Err(error) if open_is_contended(&error) => continue,
            Err(error) => panic!("failed to inspect generation lease: {error}"),
        };
        match lease.try_lock() {
            Ok(()) => {
                lease.unlock().unwrap();
                drop(lease);
                fs::remove_dir_all(entry.path()).unwrap();
            }
            Err(TryLockError::WouldBlock) => {}
            Err(error) => panic!("failed to inspect generation lease: {error}"),
        }
    }
}

/// Windows `ERROR_LOCK_VIOLATION`; see `open_is_contended`.
#[cfg(windows)]
const ERROR_LOCK_VIOLATION: i32 = 33;

/// Whether an `open` failed because another process holds the lease lock.
///
/// `std` folds this code into `TryLockError::WouldBlock` inside `try_lock`, but
/// an `open` rejected by a live byte-range lock surfaces it raw.
fn open_is_contended(error: &io::Error) -> bool {
    #[cfg(windows)]
    let lock_violation = error.raw_os_error() == Some(ERROR_LOCK_VIOLATION);
    #[cfg(not(windows))]
    let lock_violation = false;
    error.kind() == io::ErrorKind::WouldBlock || lock_violation
}

fn child(root: &Path) {
    let entry = root.join("main.harn");
    let resolved = harn_modules::resolve_import_path(&entry, "acme/module")
        .expect("package import should resolve");
    println!("READY");
    io::stdout().flush().unwrap();

    let mut release = [0_u8; 1];
    io::stdin().read_exact(&mut release).unwrap();
    print!("{}", fs::read_to_string(resolved).unwrap());
}

#[test]
fn lazy_import_path_keeps_generation_leased_until_reader_exits() {
    if let Some(root) = std::env::var_os(CHILD_ENV) {
        child(Path::new(&root));
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("main.harn"), "import \"acme/module\"\n").unwrap();
    fs::create_dir(root.join(".harn")).unwrap();
    publish(root, "generation-old", "old generation\n");
    let old_root = generation_root(root, "generation-old");

    let mut reader = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("lazy_import_path_keeps_generation_leased_until_reader_exits")
        .arg("--nocapture")
        .env(CHILD_ENV, root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = io::BufReader::new(reader.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(stdout.read_line(&mut line).unwrap(), 0);
        if line == "READY\n" {
            break;
        }
    }

    publish(root, "generation-new", "new generation\n");
    collect(root, "generation-new");
    assert!(old_root.is_dir());

    reader.stdin.take().unwrap().write_all(b"x").unwrap();
    let mut output = String::new();
    stdout.read_to_string(&mut output).unwrap();
    assert!(reader.wait().unwrap().success());
    assert!(output.contains("old generation"));

    collect(root, "generation-new");
    assert!(!old_root.exists());
}
