//! External roots: host directories outside the workspace that a policy
//! admits, each with the access it grants.
//!
//! This module owns the whole contract. The approval boundary asks it which
//! root covers a declared path and whether that root admits a write; the
//! filesystem builtins and the process sandbox ask it for the read-only roots
//! to add to their read scope. Both answers come from the same entries, so a
//! `read` root cannot be read-only at one layer and writable at another.

use std::fmt;
use std::path::Path;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::stdlib::sandbox::paths::normalize_for_policy;
use crate::tool_annotations::SideEffectLevel;
use crate::workspace_path::WorkspacePathInfo;

/// What a tool call may do under an external root.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalRootAccess {
    /// Reads only. A write-side call under the root is refused at the
    /// approval boundary, the filesystem builtins treat the root as
    /// read-only, and a confined child gets it as a read root.
    #[default]
    Read,
    /// Reads and writes, subject to every other policy check.
    ReadWrite,
}

impl ExternalRootAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadWrite => "read_write",
        }
    }

    /// The narrower of two modes. `Read` orders before `ReadWrite`.
    pub fn most_restrictive(self, other: Self) -> Self {
        self.min(other)
    }
}

impl fmt::Display for ExternalRootAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ExternalRootAccess {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "read" => Ok(Self::Read),
            "read_write" => Ok(Self::ReadWrite),
            other => Err(de::Error::custom(format!(
                "unsupported external root access {other:?}; expected \"read\" or \"read_write\""
            ))),
        }
    }
}

/// One host directory outside the workspace and the access it grants.
///
/// Deserializes from either a bare path string, which means `read`, or an
/// object `{ "path": ..., "access": "read" | "read_write" }` whose `access`
/// defaults to `read`. Always serializes as the object form, so a receipt
/// states the mode instead of leaving the reader to know the default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExternalRoot {
    pub path: String,
    pub access: ExternalRootAccess,
}

impl ExternalRoot {
    pub fn read(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            access: ExternalRootAccess::Read,
        }
    }

    pub fn read_write(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            access: ExternalRootAccess::ReadWrite,
        }
    }

    fn normalized(&self) -> std::path::PathBuf {
        normalize_for_policy(Path::new(&self.path))
    }
}

impl From<String> for ExternalRoot {
    fn from(path: String) -> Self {
        Self::read(path)
    }
}

impl From<&str> for ExternalRoot {
    fn from(path: &str) -> Self {
        Self::read(path)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalRootObject {
    path: String,
    #[serde(default)]
    access: ExternalRootAccess,
}

impl<'de> Deserialize<'de> for ExternalRoot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ExternalRootVisitor;

        impl<'de> Visitor<'de> for ExternalRootVisitor {
            type Value = ExternalRoot;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a path string or an object with `path` and optional `access`")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(ExternalRoot::read(value))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(ExternalRoot::read(value))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let object =
                    ExternalRootObject::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(ExternalRoot {
                    path: object.path,
                    access: object.access,
                })
            }
        }

        deserializer.deserialize_any(ExternalRootVisitor)
    }
}

/// The root that governs `path`: the deepest entry containing it, so a host
/// can carve a read-only subtree out of a writable root or the reverse.
///
/// Both sides resolve the way the OS sandbox resolves its roots: through the
/// longest existing ancestor's real path, so a symlinked spelling such as
/// `/tmp` and `/private/tmp` names one root rather than slipping past it.
pub(crate) fn governing_root<'a>(
    path: &str,
    roots: &'a [ExternalRoot],
) -> Option<&'a ExternalRoot> {
    let path = normalize_for_policy(Path::new(path));
    roots
        .iter()
        .map(|root| (root, root.normalized()))
        .filter(|(_, normalized)| path.starts_with(normalized))
        .max_by_key(|(_, normalized)| normalized.components().count())
        .map(|(root, _)| root)
}

/// The distinct roots governing the declared paths outside the workspace,
/// for the decision receipt.
pub(crate) fn governing_roots(
    roots: &[ExternalRoot],
    entries: &[WorkspacePathInfo],
) -> Vec<ExternalRoot> {
    let mut governing: Vec<ExternalRoot> = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.workspace_path.is_none())
    {
        if let Some(root) = entry
            .host_path
            .as_deref()
            .and_then(|path| governing_root(path, roots))
        {
            if !governing.contains(root) {
                governing.push(root.clone());
            }
        }
    }
    governing
}

/// Whether a call is known to only read the paths it declares.
///
/// Reads the side-effect level first and falls back to the tool kind when the
/// level is unset. A call with neither, such as an unannotated tool, is not
/// known to be read-only and counts as a write, matching how the write-path
/// allowlist treats unannotated tools.
pub(crate) fn is_read_side(side_effect: Option<&str>, tool_kind: Option<&str>) -> bool {
    match side_effect.map(SideEffectLevel::parse) {
        Some(SideEffectLevel::ReadOnly) => true,
        Some(level) if level != SideEffectLevel::None => false,
        _ => tool_kind.is_some_and(|kind| matches!(kind, "read" | "search" | "think" | "fetch")),
    }
}

/// Intersect two policies' roots, most restrictive wins.
///
/// An empty side imposes no bound and yields the other side, matching the
/// rest of `ToolApprovalPolicy::intersect`. Otherwise only roots both sides
/// name survive, and a root both sides name keeps the narrower mode.
pub(crate) fn intersect(left: &[ExternalRoot], right: &[ExternalRoot]) -> Vec<ExternalRoot> {
    if left.is_empty() {
        return right.to_vec();
    }
    if right.is_empty() {
        return left.to_vec();
    }
    left.iter()
        .filter_map(|root| {
            let shared = right.iter().find(|other| other.path == root.path)?;
            Some(ExternalRoot {
                path: root.path.clone(),
                access: root.access.most_restrictive(shared.access),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_string_entry_reads_as_a_read_root() {
        let roots: Vec<ExternalRoot> =
            serde_json::from_value(serde_json::json!(["/opt/fixtures"])).unwrap();
        assert_eq!(roots, vec![ExternalRoot::read("/opt/fixtures")]);
    }

    #[test]
    fn an_object_entry_carries_its_mode_and_defaults_to_read() {
        let roots: Vec<ExternalRoot> = serde_json::from_value(serde_json::json!([
            {"path": "/opt/scratch", "access": "read_write"},
            {"path": "/opt/sdk"},
        ]))
        .unwrap();
        assert_eq!(
            roots,
            vec![
                ExternalRoot::read_write("/opt/scratch"),
                ExternalRoot::read("/opt/sdk"),
            ]
        );
    }

    #[test]
    fn an_unknown_mode_or_field_is_rejected_rather_than_widened() {
        for bad in [
            serde_json::json!([{"path": "/opt/x", "access": "write"}]),
            serde_json::json!([{"path": "/opt/x", "mode": "read_write"}]),
        ] {
            assert!(serde_json::from_value::<Vec<ExternalRoot>>(bad).is_err());
        }
    }

    #[test]
    fn entries_serialize_with_their_mode() {
        assert_eq!(
            serde_json::to_value(ExternalRoot::read("/opt/sdk")).unwrap(),
            serde_json::json!({"path": "/opt/sdk", "access": "read"})
        );
    }

    #[test]
    fn intersect_keeps_the_more_restrictive_mode_for_a_shared_root() {
        let left = vec![
            ExternalRoot::read_write("/opt/shared"),
            ExternalRoot::read_write("/opt/left-only"),
        ];
        let right = vec![
            ExternalRoot::read("/opt/shared"),
            ExternalRoot::read_write("/opt/right-only"),
        ];
        assert_eq!(
            intersect(&left, &right),
            vec![ExternalRoot::read("/opt/shared")]
        );
        assert_eq!(
            intersect(&right, &left),
            vec![ExternalRoot::read("/opt/shared")]
        );
        let both_write = vec![ExternalRoot::read_write("/opt/shared")];
        assert_eq!(intersect(&both_write, &both_write), both_write);
        assert_eq!(intersect(&[], &right), right);
    }

    #[test]
    fn the_deepest_covering_root_governs_a_path() {
        let roots = vec![
            ExternalRoot::read_write("/opt/tree"),
            ExternalRoot::read("/opt/tree/fixtures"),
        ];
        assert_eq!(
            governing_root("/opt/tree/fixtures/a.txt", &roots),
            Some(&ExternalRoot::read("/opt/tree/fixtures"))
        );
        assert_eq!(
            governing_root("/opt/tree/out/a.txt", &roots),
            Some(&ExternalRoot::read_write("/opt/tree"))
        );
        assert_eq!(governing_root("/opt/treehouse/a.txt", &roots), None);
    }

    /// A root and a path that name one directory through different spellings
    /// (a symlink, or macOS `/tmp` against `/private/tmp`) still match, in both
    /// directions, including for a file that does not exist yet.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_spelling_of_a_root_is_the_same_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("real dir");
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&real, &alias).expect("symlink");
        let by_alias = vec![ExternalRoot::read(alias.to_string_lossy())];
        let by_real = vec![ExternalRoot::read(real.to_string_lossy())];
        let real_file = real.join("new.txt");
        let alias_file = alias.join("new.txt");

        assert_eq!(
            governing_root(&real_file.to_string_lossy(), &by_alias),
            Some(&by_alias[0])
        );
        assert_eq!(
            governing_root(&alias_file.to_string_lossy(), &by_real),
            Some(&by_real[0])
        );
    }
}
