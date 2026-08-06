use std::path::Path;

use super::super::{is_standard_io_device_for_access, path_is_within, FsAccess};

#[test]
fn standard_io_devices_are_narrowly_classified() {
    for (path, access) in [
        ("/dev/stdin", FsAccess::Read),
        ("/dev/stdout", FsAccess::Write),
        ("/dev/stderr", FsAccess::Write),
        ("/dev/null", FsAccess::Write),
        ("/dev/fd/0", FsAccess::Read),
        ("/dev/fd/12", FsAccess::Write),
    ] {
        assert!(is_standard_io_device_for_access(Path::new(path), access));
    }
    for (path, access) in [
        ("/dev/stdin", FsAccess::Write),
        ("/dev/null", FsAccess::Delete),
        ("/dev/fd/", FsAccess::Write),
        ("/dev/fd/1a", FsAccess::Write),
        ("/dev/stdoutx", FsAccess::Write),
        ("/dev/random", FsAccess::Read),
        ("/tmp/dev/null", FsAccess::Write),
    ] {
        assert!(!is_standard_io_device_for_access(Path::new(path), access));
    }
}

#[test]
fn root_membership_accepts_only_the_root_and_its_children() {
    let root = Path::new("/tmp/harn-root");
    assert!(path_is_within(root, root));
    assert!(path_is_within(Path::new("/tmp/harn-root/file"), root));
    assert!(!path_is_within(
        Path::new("/tmp/harn-root-other/file"),
        root
    ));
}
