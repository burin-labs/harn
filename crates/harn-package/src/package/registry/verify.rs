//! Registry-v2 verification receipts and optional remote tag identity proof.

use crate::package::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RegistryVerificationReceipt {
    pub schema_version: &'static str,
    pub source: String,
    pub index_version: u32,
    pub package_count: usize,
    pub version_count: usize,
    pub remote_resolution: bool,
    pub resolved_git_versions: usize,
    pub verified_manifests: usize,
    pub ok: bool,
    pub errors: Vec<String>,
}

fn failed_receipt(source: &str, remote: bool, error: impl ToString) -> RegistryVerificationReceipt {
    RegistryVerificationReceipt {
        schema_version: "harn.package_registry_verification.v1",
        source: source.to_string(),
        index_version: REGISTRY_INDEX_VERSION,
        package_count: 0,
        version_count: 0,
        remote_resolution: remote,
        resolved_git_versions: 0,
        verified_manifests: 0,
        ok: false,
        errors: vec![error.to_string()],
    }
}

fn resolve_git_tag(git: &str, tag: &str) -> Result<String, PackageError> {
    let output = git_output(
        [
            "ls-remote".to_string(),
            "--exit-code".to_string(),
            "--tags".to_string(),
            git.to_string(),
            format!("refs/tags/{tag}"),
            format!("refs/tags/{tag}^{{}}"),
        ],
        Cwd::Detached,
    )?;
    if !output.status.success() {
        return Err(format!(
            "{git} tag {tag} does not resolve: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("git ls-remote returned non-UTF-8 output: {error}"))?;
    let mut direct = None;
    let mut peeled = None;
    for line in stdout.lines() {
        let mut fields = line.split_whitespace();
        let Some(commit) = fields.next() else {
            continue;
        };
        match fields.next() {
            Some(reference) if reference.ends_with("^{}") => peeled = Some(commit.to_owned()),
            Some(_) => direct = Some(commit.to_owned()),
            None => {}
        }
    }
    peeled
        .or(direct)
        .ok_or_else(|| format!("{git} tag {tag} did not return a commit").into())
}

/// Prove the pinned revision's manifest declares the identity the index sells.
///
/// A registry entry makes two separate claims: that a package version exists,
/// and that its bytes live at some commit. Nothing structural ties them
/// together. An entry can advertise `0.1.0` while the manifest at its own
/// `rev` still says `0.0.0`, and uniqueness, provenance shape, and even
/// remote tag identity all still pass — the tag really does resolve to that
/// commit; the commit just isn't the version being sold. Reading the manifest
/// is the only check that closes the gap, so remote verification reads it.
fn verify_manifest_identity(
    git: &str,
    commit: &str,
    expected_package: Option<&str>,
    expected_version: &str,
) -> Result<(), PackageError> {
    let checkout = unique_temp_dir(&std::env::temp_dir(), "harn-registry-verify")?;
    let verified = (|| -> Result<(), PackageError> {
        clone_git_commit_to(git, commit, &checkout)?;
        let Some(manifest) = read_package_manifest_from_dir(&checkout)? else {
            return Err(format!("{commit} has no {MANIFEST}").into());
        };
        let Some(package) = manifest.package else {
            return Err(format!("{MANIFEST} at {commit} has no [package] section").into());
        };
        if let Some(expected) = expected_package {
            match package.name.as_deref() {
                Some(name) if name == expected => {}
                Some(name) => {
                    return Err(format!(
                        "{MANIFEST} at {commit} declares package {name}, expected {expected}"
                    )
                    .into())
                }
                None => {
                    return Err(format!(
                        "{MANIFEST} at {commit} declares no package name, expected {expected}"
                    )
                    .into())
                }
            }
        }
        match package.version.as_deref() {
            Some(version) if version == expected_version => Ok(()),
            Some(version) => Err(format!(
                "{MANIFEST} at {commit} declares version {version}, expected {expected_version}"
            )
            .into()),
            None => Err(format!(
                "{MANIFEST} at {commit} declares no version, expected {expected_version}"
            )
            .into()),
        }
    })();
    // A verification checkout is scratch space; leaving it behind would grow
    // the temp directory by one tree per registry version per run.
    let _ = fs::remove_dir_all(&checkout);
    verified
}

pub(crate) fn verify_package_registry_impl(
    source: &str,
    remote: bool,
) -> RegistryVerificationReceipt {
    let content = match read_registry_source(source) {
        Ok(content) => content,
        Err(error) => return failed_receipt(source, remote, error),
    };
    let index = match parse_package_registry_index(source, &content) {
        Ok(index) => index,
        Err(error) => return failed_receipt(source, remote, error),
    };
    let package_count = index.packages.len();
    let version_count = index
        .packages
        .iter()
        .map(|package| package.versions.len())
        .sum();
    let mut errors = Vec::new();
    let mut resolved_git_versions = 0;
    let mut verified_manifests = 0;
    if remote {
        for package in &index.packages {
            for version in &package.versions {
                // Remote verification proves what the registry can resolve. A
                // yanked version is unresolvable by construction — resolution
                // filters it out of range selection and refuses it by name —
                // so the registry makes no live claim about its source and
                // there is nothing left to prove. This is also the only way to
                // retire a record whose upstream history can no longer be
                // corrected, without deleting the record or restating it as
                // something it never was.
                if version.yanked {
                    continue;
                }
                let (Some(git), Some(tag), Some(expected)) = (
                    version.git.as_deref(),
                    version.tag.as_deref(),
                    version.rev.as_deref(),
                ) else {
                    continue;
                };
                match resolve_git_tag(git, tag) {
                    Ok(actual) if actual.eq_ignore_ascii_case(expected) => {
                        resolved_git_versions += 1;
                        match verify_manifest_identity(
                            git,
                            expected,
                            version.package.as_deref(),
                            &version.version,
                        ) {
                            Ok(()) => verified_manifests += 1,
                            Err(error) => errors.push(format!(
                                "{}@{} manifest identity failed: {error}",
                                package.name, version.version
                            )),
                        }
                    }
                    Ok(actual) => errors.push(format!(
                        "{}@{} tag {} resolves to {}, expected {}",
                        package.name, version.version, tag, actual, expected
                    )),
                    Err(error) => errors.push(format!(
                        "{}@{} remote identity failed: {error}",
                        package.name, version.version
                    )),
                }
            }
        }
    }
    RegistryVerificationReceipt {
        schema_version: "harn.package_registry_verification.v1",
        source: source.to_string(),
        index_version: index.version,
        package_count,
        version_count,
        remote_resolution: remote,
        resolved_git_versions,
        verified_manifests,
        ok: errors.is_empty(),
        errors,
    }
}

pub fn verify_package_registry(source: &str, remote: bool, json: bool, receipt_out: Option<&Path>) {
    let receipt = verify_package_registry_impl(source, remote);
    let rendered = serde_json::to_string_pretty(&receipt)
        .unwrap_or_else(|error| format!(r#"{{"ok":false,"errors":["{error}"]}}"#));
    if let Some(path) = receipt_out {
        if let Err(error) = fs::write(path, format!("{rendered}\n")) {
            eprintln!(
                "error: failed to write registry verification receipt {}: {error}",
                path.display()
            );
            process::exit(1);
        }
    }
    if json {
        println!("{rendered}");
    } else if receipt.ok {
        println!(
            "Verified registry v{}: {} packages, {} versions{}.",
            receipt.index_version,
            receipt.package_count,
            receipt.version_count,
            if remote {
                format!(
                    ", {} Git tag identities, {} manifest identities",
                    receipt.resolved_git_versions, receipt.verified_manifests
                )
            } else {
                String::new()
            }
        );
    } else {
        for error in &receipt.errors {
            eprintln!("error: {error}");
        }
    }
    if !receipt.ok {
        process::exit(1);
    }
}
