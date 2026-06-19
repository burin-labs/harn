//! Typed workspace anchor primitives for first-class sessions.
//!
//! A workspace anchor names the primary directory a session operates
//! against plus any additional roots the host has mounted alongside it.
//! Sessions carry it as `SessionState::workspace_anchor`; the bundle
//! exporter serializes it; permission matchers consult it; ACP / TUI
//! flows mutate it through `agent_session_reanchor` and
//! `agent_session_add_root` (filed separately under #2218 / #2220).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::value::VmValue;

/// How a mounted root contributes to the session's effective workspace.
///
/// * `ReadOnly` — visible to file-reading tools, blocked for writes.
/// * `Extend` — fully writable, treated as part of the workspace.
/// * `Sandboxed` — writable inside the mount but isolated from the
///   primary anchor for permission scoping.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountMode {
    #[default]
    ReadOnly,
    Extend,
    Sandboxed,
}

impl MountMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Extend => "extend",
            Self::Sandboxed => "sandboxed",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "read_only" => Ok(Self::ReadOnly),
            "extend" => Ok(Self::Extend),
            "sandboxed" => Ok(Self::Sandboxed),
            other => Err(format!(
                "unknown mount mode '{other}'; expected one of: read_only, extend, sandboxed"
            )),
        }
    }
}

/// Session-level defaults for workspace-anchor behavior.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspacePolicy {
    pub default_mount_mode: MountMode,
}

impl Default for WorkspacePolicy {
    fn default() -> Self {
        Self {
            default_mount_mode: MountMode::ReadOnly,
        }
    }
}

impl WorkspacePolicy {
    pub fn to_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or(JsonValue::Null)
    }

    pub fn to_vm_value(&self) -> VmValue {
        crate::stdlib::json_to_vm_value(&self.to_json())
    }
}

/// Additional workspace root mounted alongside the primary anchor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MountedRoot {
    pub path: PathBuf,
    pub mount_mode: MountMode,
    pub mounted_at: String,
}

/// Typed workspace anchor for a session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceAnchor {
    pub primary: PathBuf,
    #[serde(default)]
    pub additional_roots: Vec<MountedRoot>,
    pub anchored_at: String,
}

impl WorkspaceAnchor {
    pub fn to_json(&self) -> JsonValue {
        serde_json::to_value(self).unwrap_or(JsonValue::Null)
    }

    pub fn from_json(value: &JsonValue) -> Result<Self, String> {
        serde_json::from_value(value.clone())
            .map_err(|err| format!("invalid workspace_anchor: {err}"))
    }

    pub fn to_vm_value(&self) -> VmValue {
        crate::stdlib::json_to_vm_value(&self.to_json())
    }
}

/// Parse a `workspace_anchor` option dict in the shape used by stdlib
/// builtins (`{primary, additional_roots?, anchored_at?}`).
///
/// `anchored_at` defaults to the current RFC3339 timestamp when omitted
/// so callers don't have to wire a clock through for every open call.
pub fn parse_anchor_dict(value: &VmValue) -> Result<WorkspaceAnchor, String> {
    parse_anchor_dict_with_default_mount_mode(value, MountMode::default())
}

pub fn parse_anchor_dict_with_default_mount_mode(
    value: &VmValue,
    default_mount_mode: MountMode,
) -> Result<WorkspaceAnchor, String> {
    let dict = value
        .as_dict()
        .ok_or_else(|| "workspace_anchor must be a dict".to_string())?;

    let primary = match dict.get("primary") {
        Some(VmValue::String(s)) if !s.trim().is_empty() => PathBuf::from(s.to_string()),
        Some(_) => {
            return Err("workspace_anchor.primary must be a non-empty string".to_string());
        }
        None => return Err("workspace_anchor.primary is required".to_string()),
    };

    let anchored_at = match dict.get("anchored_at") {
        Some(VmValue::String(s)) if !s.trim().is_empty() => s.to_string(),
        Some(VmValue::Nil) | None => crate::orchestration::now_rfc3339(),
        _ => return Err("workspace_anchor.anchored_at must be a string".to_string()),
    };

    let additional_roots = match dict.get("additional_roots") {
        None | Some(VmValue::Nil) => Vec::new(),
        Some(VmValue::List(items)) => items
            .iter()
            .map(|value| parse_mounted_root_value(value, default_mount_mode))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("workspace_anchor.additional_roots must be a list of dicts".to_string()),
    };

    Ok(WorkspaceAnchor {
        primary,
        additional_roots,
        anchored_at,
    })
}

fn parse_mounted_root_value(
    value: &VmValue,
    default_mount_mode: MountMode,
) -> Result<MountedRoot, String> {
    let dict = value
        .as_dict()
        .ok_or_else(|| "workspace_anchor.additional_roots entry must be a dict".to_string())?;

    let path = match dict.get("path") {
        Some(VmValue::String(s)) if !s.trim().is_empty() => PathBuf::from(s.to_string()),
        Some(_) => {
            return Err(
                "workspace_anchor.additional_roots[*].path must be a non-empty string".to_string(),
            );
        }
        None => {
            return Err("workspace_anchor.additional_roots[*].path is required".to_string());
        }
    };

    let mount_mode = match dict.get("mount_mode") {
        None | Some(VmValue::Nil) => default_mount_mode,
        Some(VmValue::String(s)) => MountMode::parse(s)?,
        _ => {
            return Err(
                "workspace_anchor.additional_roots[*].mount_mode must be a string".to_string(),
            );
        }
    };

    let mounted_at = match dict.get("mounted_at") {
        Some(VmValue::String(s)) if !s.trim().is_empty() => s.to_string(),
        Some(VmValue::Nil) | None => crate::orchestration::now_rfc3339(),
        _ => {
            return Err(
                "workspace_anchor.additional_roots[*].mounted_at must be a string".to_string(),
            );
        }
    };

    Ok(MountedRoot {
        path,
        mount_mode,
        mounted_at,
    })
}

pub fn parse_workspace_policy_dict(value: &VmValue) -> Result<WorkspacePolicy, String> {
    let dict = value
        .as_dict()
        .ok_or_else(|| "workspace_policy must be a dict".to_string())?;
    let mut policy = WorkspacePolicy::default();
    match dict.get("default_mount_mode") {
        None | Some(VmValue::Nil) => {}
        Some(VmValue::String(value)) => {
            policy.default_mount_mode = MountMode::parse(value)?;
        }
        Some(_) => return Err("workspace_policy.default_mount_mode must be a string".to_string()),
    }
    Ok(policy)
}

/// Canonical key used for the workspace anchor inside transcript
/// metadata dicts and bundle exports.
pub const WORKSPACE_ANCHOR_METADATA_KEY: &str = "workspace_anchor";

/// Read a workspace anchor out of a transcript metadata dict, if any.
pub fn anchor_from_transcript_metadata_json(metadata: &JsonValue) -> Option<WorkspaceAnchor> {
    metadata
        .get(WORKSPACE_ANCHOR_METADATA_KEY)
        .and_then(|value| WorkspaceAnchor::from_json(value).ok())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn mount_mode_round_trips() {
        for mode in [MountMode::ReadOnly, MountMode::Extend, MountMode::Sandboxed] {
            let parsed = MountMode::parse(mode.as_str()).expect("parse");
            assert_eq!(parsed, mode);
        }
        assert!(MountMode::parse("unknown").is_err());
    }

    #[test]
    fn anchor_json_round_trips() {
        let anchor = WorkspaceAnchor {
            primary: PathBuf::from("/workspace/main"),
            additional_roots: vec![MountedRoot {
                path: PathBuf::from("/workspace/lib"),
                mount_mode: MountMode::Extend,
                mounted_at: "2026-05-23T00:00:00Z".to_string(),
            }],
            anchored_at: "2026-05-23T00:00:00Z".to_string(),
        };
        let json = anchor.to_json();
        let back = WorkspaceAnchor::from_json(&json).expect("round trip");
        assert_eq!(anchor, back);
    }

    #[test]
    fn parse_anchor_dict_accepts_minimal_shape() {
        let dict = VmValue::dict(BTreeMap::from([(
            "primary".to_string(),
            VmValue::String(arcstr::ArcStr::from("/workspace/main")),
        )]));
        let anchor = parse_anchor_dict(&dict).expect("parse minimal");
        assert_eq!(anchor.primary, PathBuf::from("/workspace/main"));
        assert!(anchor.additional_roots.is_empty());
        assert!(!anchor.anchored_at.is_empty());
    }

    #[test]
    fn parse_anchor_dict_rejects_missing_primary() {
        let dict = VmValue::dict(BTreeMap::new());
        let err = parse_anchor_dict(&dict).expect_err("missing primary should fail");
        assert!(err.contains("primary"));
    }

    #[test]
    fn parse_anchor_dict_accepts_additional_roots() {
        let roots = vec![VmValue::dict(BTreeMap::from([
            (
                "path".to_string(),
                VmValue::String(arcstr::ArcStr::from("/workspace/lib")),
            ),
            (
                "mount_mode".to_string(),
                VmValue::String(arcstr::ArcStr::from("extend")),
            ),
            (
                "mounted_at".to_string(),
                VmValue::String(arcstr::ArcStr::from("2026-05-23T00:00:00Z")),
            ),
        ]))];
        let dict = VmValue::dict(BTreeMap::from([
            (
                "primary".to_string(),
                VmValue::String(arcstr::ArcStr::from("/workspace/main")),
            ),
            (
                "additional_roots".to_string(),
                VmValue::List(std::sync::Arc::new(roots)),
            ),
        ]));
        let anchor = parse_anchor_dict(&dict).expect("parse with roots");
        assert_eq!(anchor.additional_roots.len(), 1);
        assert_eq!(anchor.additional_roots[0].mount_mode, MountMode::Extend);
    }

    #[test]
    fn parse_anchor_dict_uses_supplied_default_mount_mode() {
        let roots = vec![VmValue::dict(BTreeMap::from([(
            "path".to_string(),
            VmValue::String(arcstr::ArcStr::from("/workspace/lib")),
        )]))];
        let dict = VmValue::dict(BTreeMap::from([
            (
                "primary".to_string(),
                VmValue::String(arcstr::ArcStr::from("/workspace/main")),
            ),
            (
                "additional_roots".to_string(),
                VmValue::List(std::sync::Arc::new(roots)),
            ),
        ]));
        let anchor =
            parse_anchor_dict_with_default_mount_mode(&dict, MountMode::Extend).expect("parse");
        assert_eq!(anchor.additional_roots[0].mount_mode, MountMode::Extend);
    }

    #[test]
    fn parse_workspace_policy_accepts_default_mount_mode() {
        let dict = VmValue::dict(BTreeMap::from([(
            "default_mount_mode".to_string(),
            VmValue::String(arcstr::ArcStr::from("sandboxed")),
        )]));
        let policy = parse_workspace_policy_dict(&dict).expect("parse policy");
        assert_eq!(policy.default_mount_mode, MountMode::Sandboxed);
    }
}
