//! Lock-file document round-tripping.

use crate::package::*;

#[test]
fn lock_file_round_trips_typed_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(LOCK_FILE);
    let lock = LockFile {
        version: LOCK_FILE_VERSION,
        generator_version: current_generator_version(),
        protocol_artifact_version: current_protocol_artifact_version(),
        packages: vec![LockEntry {
            name: "acme-lib".to_string(),
            source: "git+https://github.com/acme/acme-lib".to_string(),
            tag: Some("v1.0.0".to_string()),
            rev_request: Some("v1.0.0".to_string()),
            commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            content_hash: Some("sha256:deadbeef".to_string()),
            package_version: Some("1.0.0".to_string()),
            harn_compat: Some(">=0.8,<0.9".to_string()),
            provenance: Some("https://github.com/acme/acme-lib/releases/tag/v1.0.0".to_string()),
            manifest_digest: Some("sha256:cafebabe".to_string()),
            registry: None,
            exports: PackageLockExports {
                modules: vec![PackageLockExport {
                    name: "lib".to_string(),
                    path: Some("lib/main.harn".to_string()),
                    symbol: None,
                }],
                tools: vec![PackageLockExport {
                    name: "echo".to_string(),
                    path: Some("lib/tools.harn".to_string()),
                    symbol: Some("tools".to_string()),
                }],
                skills: Vec::new(),
                personas: Vec::new(),
            },
            permissions: vec!["tool:read_only".to_string()],
            host_requirements: vec!["workspace.read_text".to_string()],
        }],
    };
    lock.save(&path).unwrap();
    let loaded = LockFile::load(&path).unwrap().unwrap();
    assert_eq!(loaded, lock);
}
