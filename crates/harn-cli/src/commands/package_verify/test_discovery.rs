use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::package;

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct PackageTestDiscovery {
    pub selected_file_count: usize,
    pub discovered_test_count: usize,
    pub files_without_tests: Vec<PackageTestFileIdentity>,
    pub files_with_errors: Vec<PackageTestFileError>,
    pub allow_empty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_empty_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct PackageTestFileDiscovery {
    path: String,
    sha256: String,
    tests: Vec<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PackageTestFileIdentity {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PackageTestFileError {
    pub path: String,
    pub sha256: String,
    pub error: String,
}

pub(super) fn package_test_discovery(package_dir: &Path) -> PackageTestDiscovery {
    let tests_dir = package_dir.join("tests");
    let mut files = Vec::new();
    super::collect_package_harn_files(package_dir, &tests_dir, &mut files);
    files.sort();

    let discovered = files
        .into_iter()
        .map(|path| discover_file(package_dir, &path))
        .collect::<Vec<_>>();
    let files_without_tests = discovered
        .iter()
        .filter(|file| file.tests.is_empty() && file.error.is_none())
        .map(|file| PackageTestFileIdentity {
            path: file.path.clone(),
            sha256: file.sha256.clone(),
        })
        .collect();
    let files_with_errors = discovered
        .iter()
        .filter_map(|file| {
            file.error.as_ref().map(|error| PackageTestFileError {
                path: file.path.clone(),
                sha256: file.sha256.clone(),
                error: error.clone(),
            })
        })
        .collect();
    PackageTestDiscovery {
        selected_file_count: discovered.len(),
        discovered_test_count: discovered.iter().map(|file| file.tests.len()).sum(),
        files_without_tests,
        files_with_errors,
        ..PackageTestDiscovery::default()
    }
}

fn discover_file(package_dir: &Path, path: &Path) -> PackageTestFileDiscovery {
    let relative = path
        .strip_prefix(package_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let (sha256, tests, error) = match fs::read(path) {
        Ok(bytes) => {
            let sha256 = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
            match String::from_utf8(bytes) {
                Ok(source) => match harn_test_runner::parse_program(&source) {
                    Ok(program) => {
                        let source = Arc::new(source);
                        let program = Arc::new(program);
                        match harn_test_runner::extract_cases_from_program(
                            path, &source, &program, None, 1,
                        ) {
                            Ok(cases) => {
                                let tests = cases
                                    .into_iter()
                                    .map(|case| case.pipeline_name)
                                    .collect::<BTreeSet<_>>()
                                    .into_iter()
                                    .collect();
                                (sha256, tests, None)
                            }
                            Err(error) => (sha256, Vec::new(), Some(error)),
                        }
                    }
                    Err(error) => (sha256, Vec::new(), Some(error)),
                },
                Err(error) => (sha256, Vec::new(), Some(error.to_string())),
            }
        }
        Err(error) => (
            "unreadable".to_string(),
            Vec::new(),
            Some(error.to_string()),
        ),
    };
    PackageTestFileDiscovery {
        path: relative,
        sha256,
        tests,
        error,
    }
}

pub(super) fn inspect_package_test_discovery(
    package_dir: &Path,
) -> (PackageTestDiscovery, Vec<String>) {
    let (config, manifest_error) =
        match package::load_manifest_context_for_anchor(Some(package_dir)) {
            Ok(context) => (context.manifest.tests, None),
            Err(error) => (Default::default(), Some(error.to_string())),
        };
    let mut inventory = package_test_discovery(package_dir);
    inventory.allow_empty = config.allow_empty;
    inventory.allow_empty_reason = config
        .reason
        .map(|reason| reason.trim().to_string())
        .filter(|reason| !reason.is_empty());

    let mut problems = Vec::new();
    if let Some(error) = manifest_error {
        problems.push(format!("harn.toml could not be read: {error}"));
    }
    match (
        inventory.allow_empty,
        inventory.allow_empty_reason.as_deref(),
    ) {
        (true, None) => problems
            .push("[tests].allow_empty = true requires a non-empty [tests].reason".to_string()),
        (false, Some(_)) => {
            problems.push("[tests].reason requires [tests].allow_empty = true".to_string())
        }
        _ => {}
    }
    if inventory.selected_file_count == 0 && !inventory.allow_empty {
        problems.push("package has no selected tests/*.harn files; declare [tests].allow_empty = true with a reason only when the package intentionally ships without tests".to_string());
    }
    problems.extend(inventory.files_without_tests.iter().map(|file| {
        format!(
            "{} ({}) contains no discoverable test pipeline; name a pipeline test_* or annotate it with @test",
            file.path, file.sha256
        )
    }));
    problems.extend(
        inventory
            .files_with_errors
            .iter()
            .map(|file| format!("{} ({}): {}", file.path, file.sha256, file.error)),
    );
    (inventory, problems)
}
