use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{
    chrono_now_iso, merge_namespace_shard, namespace_path_component, parse_legacy_entries,
    serialize_namespace_fields, PathMetadata, LEGACY_SHARD_NAME, NAMESPACE_ENTRIES_FILE,
};

/// Loaded form of a namespace shard: directory entries plus file entries
/// keyed by normalized relative path. File entries do not inherit.
#[derive(Default)]
pub(super) struct LoadedEntries {
    pub(super) dirs: BTreeMap<String, PathMetadata>,
    pub(super) files: BTreeMap<String, PathMetadata>,
}

pub(super) trait MetadataBackend {
    fn backend_name(&self) -> &'static str;
    fn load(&self, root: &Path) -> Result<LoadedEntries, String>;
    fn save(
        &self,
        root: &Path,
        dirs: &BTreeMap<String, PathMetadata>,
        files: &BTreeMap<String, PathMetadata>,
    ) -> Result<(), String>;
}

#[derive(Default)]
pub(super) struct FilesystemMetadataBackend;

impl FilesystemMetadataBackend {
    pub(super) fn new() -> Self {
        Self
    }
}

impl MetadataBackend for FilesystemMetadataBackend {
    fn backend_name(&self) -> &'static str {
        "filesystem"
    }

    fn load(&self, root: &Path) -> Result<LoadedEntries, String> {
        let mut loaded = LoadedEntries::default();
        let legacy_path = root.join(LEGACY_SHARD_NAME);
        if let Ok(contents) = std::fs::read_to_string(&legacy_path) {
            loaded.dirs = parse_legacy_entries(&contents);
        }

        let namespace_dirs = match std::fs::read_dir(root) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(loaded),
            Err(error) => return Err(format!("metadata load: {error}")),
        };

        let mut dirs = namespace_dirs
            .flatten()
            .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
            .collect::<Vec<_>>();
        dirs.sort_by_key(|entry| entry.file_name());

        for dir in dirs {
            let shard_path = dir.path().join(NAMESPACE_ENTRIES_FILE);
            let Ok(contents) = std::fs::read_to_string(&shard_path) else {
                continue;
            };
            merge_namespace_shard(&mut loaded, &contents);
        }

        Ok(loaded)
    }

    fn save(
        &self,
        root: &Path,
        dirs: &BTreeMap<String, PathMetadata>,
        files: &BTreeMap<String, PathMetadata>,
    ) -> Result<(), String> {
        std::fs::create_dir_all(root).map_err(|error| format!("metadata mkdir: {error}"))?;

        let mut dir_namespaces: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
            BTreeMap::new();
        for (dir, meta) in dirs {
            for (namespace, fields) in &meta.namespaces {
                dir_namespaces
                    .entry(namespace.clone())
                    .or_default()
                    .insert(dir.clone(), serialize_namespace_fields(fields));
            }
        }
        let mut file_namespaces: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
            BTreeMap::new();
        for (path, meta) in files {
            for (namespace, fields) in &meta.namespaces {
                file_namespaces
                    .entry(namespace.clone())
                    .or_default()
                    .insert(path.clone(), serialize_namespace_fields(fields));
            }
        }

        let mut all_namespaces: BTreeSet<String> = dir_namespaces.keys().cloned().collect();
        all_namespaces.extend(file_namespaces.keys().cloned());

        for namespace in all_namespaces {
            let dir_entries = dir_namespaces.remove(&namespace).unwrap_or_default();
            let file_entries = file_namespaces.remove(&namespace).unwrap_or_default();
            let namespace_dir = root.join(namespace_path_component(&namespace));
            std::fs::create_dir_all(&namespace_dir)
                .map_err(|error| format!("metadata mkdir: {error}"))?;
            let mut shard = serde_json::Map::new();
            shard.insert("version".to_string(), serde_json::json!(1));
            shard.insert(
                "namespace".to_string(),
                serde_json::Value::String(namespace.clone()),
            );
            shard.insert(
                "backend".to_string(),
                serde_json::Value::String(self.backend_name().to_string()),
            );
            shard.insert(
                "generatedAt".to_string(),
                serde_json::Value::String(chrono_now_iso()),
            );
            shard.insert(
                "entries".to_string(),
                serde_json::Value::Object(dir_entries),
            );
            if !file_entries.is_empty() {
                shard.insert("files".to_string(), serde_json::Value::Object(file_entries));
            }
            let json = serde_json::to_string_pretty(&serde_json::Value::Object(shard))
                .map_err(|error| format!("metadata json: {error}"))?;
            std::fs::write(namespace_dir.join(NAMESPACE_ENTRIES_FILE), json)
                .map_err(|error| format!("metadata write: {error}"))?;
        }

        Ok(())
    }
}
