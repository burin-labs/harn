//! Typed workspace projections within the extensible session metadata.

use std::collections::BTreeMap;

use crate::{CreateSession, SessionMeta, StoreError, StoreResult, UpdateSession};

const DIRECTORIES: &str = "harn.workspace.additionalDirectories";

fn set_directories(attributes: &mut BTreeMap<String, serde_json::Value>, directories: Vec<String>) {
    attributes.insert(
        DIRECTORIES.into(),
        serde_json::Value::Array(
            directories
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
}

impl SessionMeta {
    /// Read the complete ordered path projection. Stored paths do not grant access.
    pub fn additional_directories(&self) -> StoreResult<Vec<String>> {
        match self.attributes.get(DIRECTORIES) {
            None => Ok(Vec::new()),
            Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
                StoreError::Backend(format!(
                    "invalid additional directories for session '{}': {error}",
                    self.id
                ))
            }),
        }
    }
}

impl CreateSession {
    /// Seed the complete ordered path projection without changing launch authority.
    pub fn with_additional_directories(mut self, directories: Vec<String>) -> Self {
        set_directories(&mut self.attributes, directories);
        self
    }
}

impl UpdateSession {
    /// Replace the path projection; an empty vector explicitly clears it.
    pub fn with_additional_directories(mut self, directories: Vec<String>) -> Self {
        set_directories(&mut self.attributes, directories);
        self
    }
}
