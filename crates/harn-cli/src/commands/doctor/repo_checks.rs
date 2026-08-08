use std::fs;
use std::path::{Path, PathBuf};

use super::{DoctorCheck, DoctorStatus};

/// Checks the checked-in protocol artifacts against what the current binary
/// would generate. Only runs inside the Harn repository.
pub(super) fn check_protocol_artifacts() -> Vec<DoctorCheck> {
    let cwd = std::env::current_dir().unwrap_or_default();
    check_protocol_artifacts_from(&cwd)
}

fn check_protocol_artifacts_from(anchor: &Path) -> Vec<DoctorCheck> {
    let Some(repo) = find_harn_repo_root(anchor) else {
        return vec![DoctorCheck {
            id: "protocol-artifacts".to_string(),
            status: DoctorStatus::Skip,
            label: "protocol-artifacts".to_string(),
            detail: "not running inside the harn repo; skipping artifact drift check".to_string(),
            ..Default::default()
        }];
    };

    let ts_path = repo.join("spec/protocol-artifacts/harn-protocol.ts");
    let Ok(text) = fs::read_to_string(&ts_path) else {
        return vec![DoctorCheck {
            id: "protocol-artifacts".to_string(),
            status: DoctorStatus::Warn,
            label: "protocol-artifacts".to_string(),
            detail: format!("unable to read {}", ts_path.display()),
            fix_command: Some("make gen-protocol-artifacts".to_string()),
            docs_url: Some("https://harnlang.com/docs/protocol-artifacts.html".to_string()),
            blocks: vec!["release"],
        }];
    };

    let pinned_version = text
        .lines()
        .find_map(|line| {
            line.split_once("HARN_PROTOCOL_ARTIFACT_VERSION = \"")
                .map(|(_, rest)| rest)
                .and_then(|rest| rest.split_once('"').map(|(version, _)| version.to_string()))
        })
        .unwrap_or_default();
    let current = env!("CARGO_PKG_VERSION");
    if pinned_version.is_empty() {
        return vec![DoctorCheck {
            id: "protocol-artifacts".to_string(),
            status: DoctorStatus::Warn,
            label: "protocol-artifacts".to_string(),
            detail: format!(
                "could not parse HARN_PROTOCOL_ARTIFACT_VERSION from {}",
                ts_path.display()
            ),
            fix_command: Some("make gen-protocol-artifacts".to_string()),
            docs_url: Some("https://harnlang.com/docs/protocol-artifacts.html".to_string()),
            blocks: vec!["release"],
        }];
    }

    if pinned_version == current {
        vec![DoctorCheck {
            id: "protocol-artifacts".to_string(),
            status: DoctorStatus::Ok,
            label: "protocol-artifacts".to_string(),
            detail: format!("pinned at v{pinned_version}"),
            ..Default::default()
        }]
    } else {
        vec![DoctorCheck {
            id: "protocol-artifacts".to_string(),
            status: DoctorStatus::Fail,
            label: "protocol-artifacts".to_string(),
            detail: format!("stale: pinned v{pinned_version}, current v{current}"),
            fix_command: Some("make gen-protocol-artifacts".to_string()),
            docs_url: Some("https://harnlang.com/docs/protocol-artifacts.html".to_string()),
            blocks: vec!["release"],
        }]
    }
}

/// Walks upward from `start` until both repository identity markers exist.
pub(super) fn find_harn_repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join("spec/protocol-artifacts/manifest.json").is_file()
            && dir.join("crates/harn-cli/Cargo.toml").is_file()
        {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{check_protocol_artifacts_from, find_harn_repo_root};
    use crate::commands::doctor::DoctorStatus;
    use std::path::Path;

    fn tempdir_outside_ambient_repo() -> tempfile::TempDir {
        let ambient = std::env::current_dir().expect("cwd");
        let temp_root = std::env::temp_dir();
        let ambient_repo = find_harn_repo_root(&ambient)
            .or_else(|| {
                ambient
                    .ancestors()
                    .find(|path| path.join(".git").exists())
                    .map(Path::to_path_buf)
            })
            .unwrap_or_else(|| ambient.clone());
        if !temp_root.starts_with(&ambient_repo) {
            return tempfile::tempdir().expect("tempdir");
        }
        let parent = ambient_repo
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .join(".harn-cli-test-tmp");
        std::fs::create_dir_all(&parent).expect("test temp parent");
        tempfile::Builder::new()
            .prefix("harn-cli-doctor-")
            .tempdir_in(parent)
            .expect("tempdir")
    }

    #[test]
    fn find_harn_repo_root_walks_up_from_nested_dir() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("spec/protocol-artifacts")).unwrap();
        std::fs::write(
            root.path().join("spec/protocol-artifacts/manifest.json"),
            "{}",
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("crates/harn-cli")).unwrap();
        std::fs::write(root.path().join("crates/harn-cli/Cargo.toml"), "").unwrap();

        let nested = root.path().join("crates/harn-cli/src/commands");
        std::fs::create_dir_all(&nested).unwrap();

        let found = find_harn_repo_root(&nested).expect("repo root");
        assert_eq!(found, root.path());

        let unrelated = tempdir_outside_ambient_repo();
        assert!(find_harn_repo_root(unrelated.path()).is_none());
    }

    #[test]
    fn protocol_artifacts_check_skipped_outside_repo() {
        let dir = tempdir_outside_ambient_repo();
        let checks = check_protocol_artifacts_from(dir.path());
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, DoctorStatus::Skip);
        assert_eq!(checks[0].id, "protocol-artifacts");
    }
}
