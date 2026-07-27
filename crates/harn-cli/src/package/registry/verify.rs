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
    if remote {
        for package in &index.packages {
            for version in &package.versions {
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
                format!(", {} Git tag identities", receipt.resolved_git_versions)
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
