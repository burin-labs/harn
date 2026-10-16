//! Source-URI shapes.

use crate::package::*;

#[test]
fn windows_drive_registry_sources_are_file_paths_not_url_schemes() {
    let source = r"C:\Users\RUNNER~1\AppData\Local\Temp\index.toml";
    let path = registry_file_url_or_path(source).unwrap().unwrap();
    assert_eq!(path, PathBuf::from(source));

    let archive_error = normalize_archive_url(source).unwrap_err();
    assert!(
        archive_error
            .to_string()
            .contains("package archive not found"),
        "expected missing Windows path to be treated as a file path, got: {archive_error}"
    );
}
