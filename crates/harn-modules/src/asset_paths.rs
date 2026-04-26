//! Package-root prompt asset addressing (issue #742).
//!
//! Pipelines that move files around want stable, refactor-safe paths to
//! `.harn.prompt` assets. Source-relative `../../partials/foo.harn.prompt`
//! paths break the moment a caller is renamed or relocated. This module
//! resolves a small URI scheme that anchors prompt assets at the
//! project root or a project-defined alias instead:
//!
//! - `@/<rel>` → resolved from the calling module's project root
//!   (the nearest `harn.toml` ancestor of the *calling file*, not the
//!   workspace cwd).
//! - `@<alias>/<rel>` → resolved from a `[asset_roots]` entry in the
//!   project's `harn.toml`, e.g. `[asset_roots] partials = "..."`.
//!
//! Plain (non-`@`) paths fall through to the caller's existing
//! source-relative resolver — back-compat is exact.
//!
//! Lives in `harn-modules` so the VM, the LSP, and the CLI's preflight
//! checker can share one resolver and produce identical errors.

use std::path::{Component, Path, PathBuf};

const ASSET_PREFIX: char = '@';

/// A parsed `@`-prefixed asset reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetRef<'a> {
    /// `@/<rel>` — anchored at the project root.
    ProjectRoot { rel: &'a str },
    /// `@<alias>/<rel>` — anchored at a `[asset_roots]` alias.
    Alias { alias: &'a str, rel: &'a str },
}

/// Returns true when `path` starts with the `@`-asset prefix.
pub fn is_asset_path(path: &str) -> bool {
    path.starts_with(ASSET_PREFIX)
}

/// Parse an `@`-prefixed path. Returns `None` for plain paths so callers
/// can fall back to source-relative resolution. Malformed `@`-paths
/// (e.g. `@foo` with no slash) also return `None`; the resolver wraps
/// `None` cases into a parse-time error when the caller knows it has
/// an `@` prefix.
pub fn parse(path: &str) -> Option<AssetRef<'_>> {
    let stripped = path.strip_prefix(ASSET_PREFIX)?;
    if let Some(rel) = stripped.strip_prefix('/') {
        return Some(AssetRef::ProjectRoot { rel });
    }
    let (alias, rel) = stripped.split_once('/')?;
    Some(AssetRef::Alias { alias, rel })
}

/// Walk up from `base` looking for the nearest ancestor containing
/// `harn.toml`. Mirrors `harn-vm`'s in-VM walker so the resolver can
/// run from the LSP/CLI without dragging in the VM crate.
pub fn find_project_root(base: &Path) -> Option<PathBuf> {
    let mut dir = base.to_path_buf();
    loop {
        if dir.join("harn.toml").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Resolve an `@`-prefixed asset path to an absolute filesystem path.
///
/// `anchor` is the directory the project-root walk starts from — the
/// caller's choice depends on context:
///
/// - **VM runtime**: pass the thread-local source dir
///   (`source_root_path()`), which always reflects the file currently
///   executing.
/// - **LSP / preflight**: pass the calling source file's parent, so the
///   project root is derived from the file under analysis.
///
/// In every case the project root is derived from the *call site*, not
/// the user's cwd, so an imported pipeline resolves prompts the same
/// way regardless of who called it.
pub fn resolve(asset_ref: &AssetRef<'_>, anchor: &Path) -> Result<PathBuf, String> {
    let project_root = find_project_root(anchor).ok_or_else(|| {
        format!(
            "package-root prompt path '{}' has no project root: no harn.toml found above {}",
            display_asset(asset_ref),
            anchor.display()
        )
    })?;
    match asset_ref {
        AssetRef::ProjectRoot { rel } => {
            let safe = safe_relative(rel)
                .ok_or_else(|| format!("invalid project-root asset path '@/{rel}'"))?;
            Ok(project_root.join(safe))
        }
        AssetRef::Alias { alias, rel } => {
            let safe =
                safe_relative(rel).ok_or_else(|| format!("invalid asset path '@{alias}/{rel}'"))?;
            let asset_root = lookup_alias(&project_root, alias).ok_or_else(|| {
                format!(
                    "asset alias '{alias}' is not defined in [asset_roots] of {}",
                    project_root.join("harn.toml").display()
                )
            })?;
            let safe_root = safe_relative(&asset_root).ok_or_else(|| {
                format!(
                    "asset alias '{alias}' resolves to an unsafe path '{asset_root}' \
                     (must be a project-relative directory without `..` segments)"
                )
            })?;
            Ok(project_root.join(safe_root).join(safe))
        }
    }
}

/// Convenience for the common case in `render_prompt(path, ...)`:
/// resolve `@`-prefixed paths against the project root, otherwise apply
/// the caller's source-relative fallback.
pub fn resolve_or<F>(path: &str, anchor: &Path, fallback: F) -> Result<PathBuf, String>
where
    F: FnOnce(&str) -> PathBuf,
{
    if let Some(asset_ref) = parse(path) {
        return resolve(&asset_ref, anchor);
    }
    Ok(fallback(path))
}

/// Reject paths that would escape the anchor or contain shell-relative
/// shenanigans. Mirrors the safety check in
/// `safe_package_relative_path` so package-rooted prompts can't reach
/// outside the project root via `..` traversal.
fn safe_relative(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() || raw.contains('\\') {
        return None;
    }
    let mut out = PathBuf::new();
    let mut saw_component = false;
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => {
                saw_component = true;
                out.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    saw_component.then_some(out)
}

fn display_asset(asset_ref: &AssetRef<'_>) -> String {
    match asset_ref {
        AssetRef::ProjectRoot { rel } => format!("@/{rel}"),
        AssetRef::Alias { alias, rel } => format!("@{alias}/{rel}"),
    }
}

/// Look up `[asset_roots] <alias> = "..."` in the project's harn.toml.
/// Missing manifest, missing table, or missing key all return `None`
/// so the caller can produce one uniform diagnostic.
fn lookup_alias(project_root: &Path, alias: &str) -> Option<String> {
    let manifest = std::fs::read_to_string(project_root.join("harn.toml")).ok()?;
    let parsed: toml::Value = toml::from_str(&manifest).ok()?;
    let table = parsed.get("asset_roots")?.as_table()?;
    table.get(alias)?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parses_project_root_form() {
        assert_eq!(
            parse("@/partials/foo.harn.prompt"),
            Some(AssetRef::ProjectRoot {
                rel: "partials/foo.harn.prompt"
            })
        );
    }

    #[test]
    fn parses_alias_form() {
        assert_eq!(
            parse("@partials/foo.harn.prompt"),
            Some(AssetRef::Alias {
                alias: "partials",
                rel: "foo.harn.prompt"
            })
        );
    }

    #[test]
    fn plain_paths_pass_through() {
        assert!(parse("relative/path").is_none());
        assert!(parse("/absolute/path").is_none());
        assert!(parse("../sibling").is_none());
    }

    #[test]
    fn parent_traversal_rejected() {
        assert!(safe_relative("foo/../bar").is_none());
        assert!(safe_relative("/abs").is_none());
        assert!(safe_relative("").is_none());
    }

    #[test]
    fn resolves_project_root_path_anchored_at_caller_root() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::write(root.join("harn.toml"), "[package]\nname = \"x\"\n").unwrap();
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        let resolved = resolve(
            &parse("@/prompts/foo.harn.prompt").unwrap(),
            &root.join("a/b/c"),
        )
        .unwrap();
        assert_eq!(resolved, root.join("prompts/foo.harn.prompt"));
    }

    #[test]
    fn resolves_alias_path_via_asset_roots() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::write(
            root.join("harn.toml"),
            "[package]\nname = \"x\"\n[asset_roots]\npartials = \"src/prompts\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("a/b")).unwrap();
        let resolved = resolve(
            &parse("@partials/foo.harn.prompt").unwrap(),
            &root.join("a/b"),
        )
        .unwrap();
        assert_eq!(resolved, root.join("src/prompts/foo.harn.prompt"));
    }

    #[test]
    fn missing_alias_produces_clear_error() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        fs::write(root.join("harn.toml"), "[package]\nname = \"x\"\n").unwrap();
        let err = resolve(&parse("@unknown/foo.harn.prompt").unwrap(), root).unwrap_err();
        assert!(err.contains("[asset_roots]"));
        assert!(err.contains("unknown"));
    }

    #[test]
    fn no_project_root_produces_error() {
        let temp = TempDir::new().unwrap();
        let err = resolve(&parse("@/foo.harn.prompt").unwrap(), temp.path()).unwrap_err();
        assert!(err.contains("no harn.toml"));
    }
}
