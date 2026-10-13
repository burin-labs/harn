//! The one registry of paths Harn knows by name.
//!
//! `known_paths.toml` classifies every home path the runtime grants or denies
//! and every file name that marks credential material. The sandbox denylist,
//! the preset read and cache roots, the taint gate's secret-read check, the
//! command risk scan, and the default approval patterns all derive from it, so
//! a path added once is recognized everywhere.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::Deserialize;

static KNOWN_PATHS_TOML: &str = include_str!("known_paths.toml");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    credential: Credential,
    package_config: HomeList,
    toolchain_read: ToolchainRead,
    toolchain_cache: HomeList,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    home: Vec<String>,
    basename_globs: Vec<String>,
    reason: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HomeList {
    home: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolchainRead {
    home: Vec<String>,
    windows_home: Vec<String>,
}

/// The parsed, validated registry.
///
/// A parse failure or a broken invariant is a hard error at first use, not a
/// silent empty default: an empty credential list reads as a working denylist
/// while denying nothing.
fn registry() -> &'static Registry {
    static PARSED: OnceLock<Registry> = OnceLock::new();
    PARSED.get_or_init(|| {
        let registry: Registry = toml::from_str(KNOWN_PATHS_TOML)
            .expect("known_paths.toml must parse; the credential registry is not optional");
        if let Err(problem) = validate(&registry) {
            panic!("known_paths.toml is invalid: {problem}");
        }
        registry
    })
}

fn validate(registry: &Registry) -> Result<(), String> {
    let credential = &registry.credential;
    if credential.home.is_empty() || credential.basename_globs.is_empty() {
        return Err("the credential lists must not be empty".into());
    }
    for entry in &credential.home {
        if !credential.reason.contains_key(entry) {
            return Err(format!("credential `{entry}` has no reason"));
        }
    }
    for key in credential.reason.keys() {
        if !credential.home.contains(key) {
            return Err(format!("reason `{key}` names no credential"));
        }
    }
    let lists: [(&str, &[String]); 6] = [
        ("credential.home", &credential.home),
        ("credential.basename_globs", &credential.basename_globs),
        ("package_config.home", &registry.package_config.home),
        ("toolchain_read.home", &registry.toolchain_read.home),
        (
            "toolchain_read.windows_home",
            &registry.toolchain_read.windows_home,
        ),
        ("toolchain_cache.home", &registry.toolchain_cache.home),
    ];
    for (name, list) in lists {
        let mut seen = BTreeSet::new();
        for entry in list {
            if entry.is_empty() || entry.starts_with('/') || entry.contains('\\') {
                return Err(format!(
                    "{name} entry `{entry}` must be a relative POSIX path"
                ));
            }
            if !seen.insert(entry) {
                return Err(format!("{name} lists `{entry}` twice"));
            }
        }
    }
    // A grant may contain a credential (the denial beats it), but a grant that
    // names a credential or sits inside one would contradict the denial.
    for (name, list) in &lists[2..] {
        for grant in *list {
            if let Some(credential) = credential
                .home
                .iter()
                .find(|credential| within(grant, credential))
            {
                return Err(format!(
                    "{name} grants `{grant}`, which is the credential `{credential}` or inside it"
                ));
            }
        }
    }
    Ok(())
}

/// Whether home-relative `path` equals `root` or sits underneath it.
fn within(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Home-relative credential stores a confined child may never read.
pub fn credential_home_paths() -> &'static [String] {
    &registry().credential.home
}

/// File-name globs that mark credential material wherever it lives.
pub fn credential_basename_globs() -> &'static [String] {
    &registry().credential.basename_globs
}

/// Why a credential home path is on the list, for review and docs.
pub fn credential_reason(path: &str) -> Option<&'static str> {
    registry().credential.reason.get(path).map(String::as_str)
}

// The preset accessors follow their sandbox consumers' platform gates, so a
// target without that backend builds them only for the tests.

/// Home-relative read-only roots of the PackageManagerConfig preset.
#[cfg(any(test, target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn package_config_home_paths() -> &'static [String] {
    &registry().package_config.home
}

/// Home-relative read-only roots of the DeveloperToolchains preset, with the
/// Windows-only entries included when `windows` is set.
#[cfg(any(test, target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn toolchain_read_home_paths(windows: bool) -> impl Iterator<Item = &'static str> {
    let read = &registry().toolchain_read;
    let extra: &'static [String] = if windows { &read.windows_home } else { &[] };
    read.home.iter().chain(extra).map(String::as_str)
}

/// Home-relative read+write cache roots of the DeveloperToolchains preset.
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
pub(crate) fn toolchain_cache_home_paths() -> &'static [String] {
    &registry().toolchain_cache.home
}

/// Whether `path` names credential material.
///
/// Matches a credential home entry anywhere in the path (`~/.ssh/id_rsa`,
/// `/home/u/.docker/config.json`, `.aws` relative to a checkout), and a
/// credential file name in the last component. Case-insensitive, and either
/// slash separates. Surrounding quotes and punctuation from shell text are
/// ignored, so a token cut from a command line matches like a path.
pub fn is_credential_path(path: &str) -> bool {
    let trimmed = path.trim_matches(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '`' | ',' | ';' | ':' | '(' | ')' | '[' | ']'
            )
    });
    let normalized = trimmed.replace('\\', "/").to_ascii_lowercase();
    let components: Vec<&str> = normalized
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect();
    let Some(basename) = components.last() else {
        return false;
    };
    let registry = registry();
    registry.credential.home.iter().any(|entry| {
        let needle: Vec<String> = entry.split('/').map(str::to_ascii_lowercase).collect();
        components
            .windows(needle.len())
            .any(|window| window.iter().zip(&needle).all(|(a, b)| *a == b))
    }) || registry
        .credential
        .basename_globs
        .iter()
        .any(|glob| harn_glob::match_name(&glob.to_ascii_lowercase(), basename))
}

#[cfg(test)]
mod parity_tests;
#[cfg(test)]
mod tests;
