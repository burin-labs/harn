#![allow(dead_code)]

#[path = "../bootstrap/freshness_manifest.rs"]
mod freshness_manifest;
#[path = "../path_policy.rs"]
mod path_policy;

use freshness_manifest::{
    artifact_stat_id, canonical_path_id, file_content_hash, platform_build_id, verify_manifest,
    Verification,
};
use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

const RECEIPT_FORMAT: &str = "harn-bin-freshness-v5";
const EVIDENCE_FORMAT: &str = "harn-artifact-evidence-v5-cargo-output-dep-info-v1-manifest-3";
const CHECKER_FORMAT: &str = "harn-freshness-check-v3";

fn main() -> ExitCode {
    let arguments = env::args().collect::<Vec<_>>();
    let result = match arguments.as_slice() {
        [_, command, binary, manifest, repo_root] if command == "record-evidence" => {
            record_evidence(
                Path::new(binary),
                Path::new(manifest),
                Path::new(repo_root),
            )
            .map(|evidence| {
                print!("{evidence}");
            })
        }
        [_, command, receipt, manifest, binary, repo_root] if command == "verify" => verify(
            Path::new(receipt),
            Path::new(manifest),
            Path::new(binary),
            Path::new(repo_root),
        ),
        [_, command, binary, repo_root] if command == "verify-worktree" => {
            verify_worktree(Path::new(binary), Path::new(repo_root))
        }
        _ => Err("usage: harn-freshness-check {record-evidence <binary> <manifest> <repo-root>|verify <receipt> <manifest> <binary> <repo-root>|verify-worktree <binary> <repo-root>}".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn verify_worktree(binary: &Path, repo_root: &Path) -> Result<(), String> {
    let receipt = path_with_suffix(binary, ".freshness");
    let manifest = path_with_suffix(binary, ".freshness.manifest");
    verify(&receipt, &manifest, binary, repo_root)
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    value.into()
}

fn record_evidence(binary: &Path, manifest: &Path, repo_root: &Path) -> Result<String, String> {
    artifact_stat_id(binary)?;
    match verify_manifest(manifest)? {
        Verification::Fresh => {}
        Verification::InventoryChanged(path) => {
            return Err(format!(
                "freshness input inventory changed after the manifest snapshot: {}",
                path.display()
            ));
        }
    }
    let executable = env::current_exe()
        .map_err(|error| format!("cannot resolve running freshness checker: {error}"))?;
    // The checker is deliberately tiny, so exact bytes are both cheaper and
    // stronger than Windows ChangeTime, which may settle after process exit.
    Ok(format!(
        "{CHECKER_FORMAT}\nrepo-path={}\nchecker-build-id={}\nchecker-content={}\nchecker-path={}\nmanifest={}\n",
        canonical_path_id(repo_root)?,
        platform_build_id()?,
        file_content_hash(&executable)?,
        canonical_path_id(&executable)?,
        file_content_hash(manifest)?,
    ))
}

fn verify(receipt: &Path, manifest: &Path, binary: &Path, repo_root: &Path) -> Result<(), String> {
    let receipt_text = fs::read_to_string(receipt).map_err(|error| {
        format!(
            "cannot read freshness receipt {}: {error}",
            receipt.display()
        )
    })?;
    let lines = receipt_text.lines().collect::<Vec<_>>();
    if lines.len() != 14
        || lines[0] != RECEIPT_FORMAT
        || !valid_keyed_hash(lines[1], "worktree", &[40, 64])
        || lines[2] != EVIDENCE_FORMAT
        || !valid_keyed_hash(lines[3], "build-freshness", &[40, 64])
        || !valid_keyed_hex_range(lines[4], "build-id", 2, 128)
        || !valid_keyed_hash(lines[5], "artifact-stat", &[64])
        || !valid_keyed_hash(lines[6], "dep-info", &[64])
        || !valid_keyed_hash(lines[7], "dependencies", &[64])
        || lines[8] != CHECKER_FORMAT
        || !valid_keyed_hash(lines[9], "repo-path", &[64])
        || !valid_keyed_hex_range(lines[10], "checker-build-id", 2, 128)
        || !valid_keyed_hash(lines[11], "checker-content", &[64])
        || !valid_keyed_hash(lines[12], "checker-path", &[64])
        || !valid_keyed_hash(lines[13], "manifest", &[64])
    {
        return Err(format!(
            "malformed Harn freshness receipt at {}",
            receipt.display()
        ));
    }

    let current_evidence = record_evidence(binary, manifest, repo_root)?;
    let recorded_evidence = format!("{}\n", lines[8..].join("\n"));
    if current_evidence != recorded_evidence {
        return Err("freshness checker or manifest changed after the build receipt".into());
    }
    let recorded_binary_stat = lines[5]
        .strip_prefix("artifact-stat=")
        .expect("receipt shape was validated");
    if artifact_stat_id(binary)?.to_hex().to_string() != recorded_binary_stat {
        return Err("worktree Harn executable changed after the build receipt".into());
    }
    Ok(())
}

fn valid_keyed_hex_range(line: &str, key: &str, minimum: usize, maximum: usize) -> bool {
    let Some(value) = line
        .strip_prefix(key)
        .and_then(|value| value.strip_prefix('='))
    else {
        return false;
    };
    (minimum..=maximum).contains(&value.len())
        && value.len() % 2 == 0
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_keyed_hash(line: &str, key: &str, lengths: &[usize]) -> bool {
    let Some(value) = line
        .strip_prefix(key)
        .and_then(|value| value.strip_prefix('='))
    else {
        return false;
    };
    lengths.contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_hash_validation_is_exact() {
        assert!(valid_keyed_hash(
            "manifest=0123456789abcdef",
            "manifest",
            &[16]
        ));
        assert!(!valid_keyed_hash(
            "other=0123456789abcdef",
            "manifest",
            &[16]
        ));
        assert!(!valid_keyed_hash("manifest=xyz", "manifest", &[3]));
    }

    #[test]
    fn receipt_paths_append_without_replacing_the_binary_name() {
        let binary = Path::new("target/debug/harn");
        assert_eq!(
            path_with_suffix(binary, ".freshness"),
            Path::new("target/debug/harn.freshness")
        );
    }

    #[cfg(unix)]
    #[test]
    fn receipt_paths_preserve_non_utf8_binary_names() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let binary = PathBuf::from(OsString::from_vec(b"target/debug/harn-\xff".to_vec()));
        assert_eq!(
            path_with_suffix(&binary, ".freshness")
                .as_os_str()
                .as_bytes(),
            b"target/debug/harn-\xff.freshness"
        );
    }
}
