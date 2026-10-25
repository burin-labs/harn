use super::*;
use std::fs::{self, File, FileTimes};
use std::time::Duration;

fn write_at(path: &Path, contents: &[u8], nanos: u32) {
    fs::write(path, contents).unwrap();
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::new(1_600_000_000, nanos)))
        .unwrap();
}

#[test]
fn content_hash_detects_same_second_same_size_edits() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.txt");
    write_at(&path, b"alpha", 100_000_000);
    let before = compute_content_hash_for_dir(directory.path()).unwrap();
    let first = fs::metadata(&path).unwrap().modified().unwrap();
    write_at(&path, b"bravo", 200_000_000);
    let second = fs::metadata(&path).unwrap().modified().unwrap();
    assert_ne!(
        first, second,
        "fixture filesystem must retain subsecond mtimes"
    );
    assert_eq!(
        first.duration_since(UNIX_EPOCH).unwrap().as_secs(),
        second.duration_since(UNIX_EPOCH).unwrap().as_secs()
    );
    assert_ne!(
        before,
        compute_content_hash_for_dir(directory.path()).unwrap()
    );
}

#[test]
fn missing_directory_is_not_an_empty_directory() {
    let directory = tempfile::tempdir().unwrap();
    assert!(!compute_content_hash_for_dir(directory.path())
        .unwrap()
        .is_empty());
    let missing = directory.path().join("missing");
    assert_eq!(
        compute_content_hash_for_dir(&missing).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        compute_structure_hash(&missing).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[cfg(unix)]
#[test]
fn entry_names_cannot_alias_record_separators() {
    let pair = tempfile::tempdir().unwrap();
    for name in ["a", "b"] {
        write_at(&pair.path().join(name), b"alpha", 100_000_000);
    }
    let single = tempfile::tempdir().unwrap();
    let alias = single.path().join("a:5|b");
    write_at(&alias, b"alpha", 100_000_000);
    assert_ne!(
        compute_structure_hash(pair.path()).unwrap(),
        compute_structure_hash(single.path()).unwrap()
    );
    fs::rename(alias, single.path().join("a:5:1600000000|b")).unwrap();
    assert_ne!(
        compute_content_hash_for_dir(pair.path()).unwrap(),
        compute_content_hash_for_dir(single.path()).unwrap()
    );
}

// macOS filesystems reject these names before directory enumeration.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_entry_names_remain_distinct() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let raw = tempfile::tempdir().unwrap();
    let first = raw.path().join(OsString::from_vec(vec![b'a', 0x80]));
    let second = raw.path().join(OsString::from_vec(vec![b'a', 0x81]));
    write_at(&first, b"alpha", 100_000_000);
    let before = compute_structure_hash(raw.path()).unwrap();
    fs::rename(first, second).unwrap();
    assert_ne!(before, compute_structure_hash(raw.path()).unwrap());
}
