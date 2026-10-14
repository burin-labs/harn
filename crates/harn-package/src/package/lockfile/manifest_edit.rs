//! Formatting-preserving edits to the `[dependencies]` table of a manifest.

use super::materialize::dependency_manifest_item;
use crate::package::*;

pub(crate) fn ensure_manifest_exists(manifest_path: &Path) -> Result<String, PackageError> {
    if manifest_path.exists() {
        return fs::read_to_string(manifest_path).map_err(|error| {
            PackageError::Lockfile(format!(
                "failed to read {}: {error}",
                manifest_path.display()
            ))
        });
    }
    Ok("[package]\nname = \"my-project\"\nversion = \"0.1.0\"\n".to_string())
}

pub(crate) fn upsert_dependency_in_manifest_locked(
    manifest_path: &Path,
    alias: &str,
    dependency: &Dependency,
) -> Result<(), PackageError> {
    let content = ensure_manifest_exists(manifest_path)?;
    let mut document = content.parse::<toml_edit::DocumentMut>().map_err(|error| {
        PackageError::Manifest(format!(
            "failed to parse {} for editing: {error}",
            manifest_path.display()
        ))
    })?;
    if document.get("dependencies").is_none() {
        document["dependencies"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let dependencies = document["dependencies"].as_table_mut().ok_or_else(|| {
        PackageError::Manifest(format!(
            "[dependencies] in {} is not a table",
            manifest_path.display()
        ))
    })?;
    let mut replacement = dependency_manifest_item(alias, dependency)?;
    if let Some((_key, existing)) = dependencies.get_key_value_mut(alias) {
        if let (Some(old), Some(new)) = (existing.as_value(), replacement.as_value_mut()) {
            *new.decor_mut() = old.decor().clone();
        }
        *existing = replacement;
    } else {
        dependencies.insert(alias, replacement);
    }
    write_manifest_content_locked(manifest_path, &document.to_string())
}

pub(crate) fn remove_dependency_from_manifest_locked(
    manifest_path: &Path,
    alias: &str,
) -> Result<bool, PackageError> {
    let content = fs::read_to_string(manifest_path)
        .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()))?;
    let mut document = content.parse::<toml_edit::DocumentMut>().map_err(|error| {
        PackageError::Manifest(format!(
            "failed to parse {} for editing: {error}",
            manifest_path.display()
        ))
    })?;
    let Some(dependencies) = document
        .get_mut("dependencies")
        .and_then(toml_edit::Item::as_table_mut)
    else {
        return Ok(false);
    };
    if dependencies.remove(alias).is_some() {
        write_manifest_content_locked(manifest_path, &document.to_string())?;
        return Ok(true);
    }
    Ok(false)
}
