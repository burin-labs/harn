//! Data-driven test-file discovery for projects without a test manifest.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Language facts supplied by the embedding product's catalog.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestLayoutProfile {
    /// Stable language identifier supplied by the host.
    pub language: String,
    /// File extensions without a leading dot.
    pub extensions: Vec<String>,
    /// Filename prefixes or suffixes, or path fragments containing a slash.
    pub patterns: Vec<String>,
}

/// A complete layout search distinguishes an empty result from ambiguity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TestLayoutDiscovery {
    /// The host supplied no layout conventions.
    Unconfigured,
    /// The configured conventions matched no test files.
    Empty,
    /// Every matching file belongs to one language.
    Single {
        /// The matched language identifier.
        language: String,
        /// Sorted paths relative to the scanned root.
        files: Vec<String>,
    },
    /// More than one language matched, so no single runner is implied.
    Ambiguous {
        /// Sorted matched language identifiers.
        languages: Vec<String>,
    },
}

pub(super) struct LayoutCollector<'a> {
    profiles: &'a [TestLayoutProfile],
    matches: BTreeMap<String, BTreeSet<String>>,
}

impl<'a> LayoutCollector<'a> {
    pub(super) fn new(profiles: &'a [TestLayoutProfile]) -> Self {
        Self {
            profiles,
            matches: BTreeMap::new(),
        }
    }

    pub(super) fn record(&mut self, path: &str) -> bool {
        let normalized = path.replace('\\', "/").to_lowercase();
        let filename = normalized.rsplit('/').next().unwrap_or_default();
        let extension = filename
            .rsplit_once('.')
            .map(|(_, ext)| ext)
            .unwrap_or_default();
        let mut matched = false;
        for profile in self.profiles {
            // Python's scanner checks actual collected symbols and owns that decision.
            if profile.language == "python" {
                continue;
            }
            if !profile
                .extensions
                .iter()
                .any(|item| item.eq_ignore_ascii_case(extension))
            {
                continue;
            }
            if profile
                .patterns
                .iter()
                .any(|pattern| matches_pattern(&normalized, filename, pattern))
            {
                self.matches
                    .entry(profile.language.clone())
                    .or_default()
                    .insert(path.to_owned());
                matched = true;
            }
        }
        matched
    }

    pub(super) fn record_python(&mut self, path: &str) {
        if self
            .profiles
            .iter()
            .any(|profile| profile.language == "python")
        {
            self.matches
                .entry("python".to_owned())
                .or_default()
                .insert(path.to_owned());
        }
    }

    pub(super) fn finish(self) -> TestLayoutDiscovery {
        if self.profiles.is_empty() {
            return TestLayoutDiscovery::Unconfigured;
        }
        match self.matches.len() {
            0 => TestLayoutDiscovery::Empty,
            1 => {
                let (language, files) = self.matches.into_iter().next().expect("one layout");
                TestLayoutDiscovery::Single {
                    language,
                    files: files.into_iter().collect(),
                }
            }
            _ => TestLayoutDiscovery::Ambiguous {
                languages: self.matches.into_keys().collect(),
            },
        }
    }
}

fn matches_pattern(path: &str, filename: &str, pattern: &str) -> bool {
    let pattern = pattern.trim().to_lowercase();
    if pattern.is_empty() {
        return false;
    }
    if pattern.starts_with('/') {
        return path.contains(&pattern);
    }
    if pattern.contains('/') {
        return path.ends_with(&pattern) || path.contains(&pattern);
    }
    filename.starts_with(&pattern) || filename.ends_with(&pattern)
}
