//! Tool annotations — the single source of truth for tool semantics.
//!
//! These types describe what a tool does at a semantic level. The VM
//! consumes them to make policy decisions (read-only vs mutating, which
//! argument holds the workspace path, which aliases to normalize, etc.)
//! without hardcoding tool names or file-extension lists. Pipeline
//! authors declare a `ToolAnnotations` value per tool in their
//! `CapabilityPolicy.tool_annotations` registry; everything downstream
//! is driven by that declaration.
//!
//! This alignment is ACP-compliant: `ToolKind` matches the canonical
//! tool-kind vocabulary from the [Agent Client Protocol schema]
//! (https://agentclientprotocol.com/protocol/schema) one-for-one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Canonical tool-kind vocabulary. Matches the ACP `ToolKind` enum so
/// harn-cli's ACP server can forward the value unchanged in
/// `sessionUpdate` variants.
///
/// The VM treats `Read`, `Search`, `Think`, and `Fetch` as read-only
/// for concurrent-dispatch purposes. `Other` is intentionally NOT
/// treated as read-only — unannotated tools should not slip through
/// as auto-approved by default (fail-safe).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Reads file/workspace content without mutation.
    Read,
    /// Mutates workspace content (write, patch, edit).
    Edit,
    /// Removes content irreversibly.
    Delete,
    /// Relocates or renames content.
    Move,
    /// Queries indexes or directories; no mutation.
    Search,
    /// Runs a subprocess or a shell command.
    Execute,
    /// Pure reasoning/thought invocation, no side effects.
    Think,
    /// Retrieves remote content (HTTP, MCP fetch, etc.).
    Fetch,
    /// Anything that doesn't map cleanly into the canonical kinds.
    /// Not treated as read-only — the fail-safe default.
    #[default]
    Other,
}

impl ToolKind {
    /// Read-only tools can dispatch concurrently without risking
    /// conflicting state mutations. `Other` is excluded by design —
    /// unannotated tools must not auto-approve as read-only.
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::Read | Self::Search | Self::Think | Self::Fetch)
    }

    /// Coarse mutation-classification string used in tool-call
    /// telemetry and pre/post bridge payloads. Derived directly from
    /// the kind — the VM no longer guesses from tool names.
    pub fn mutation_class(&self) -> &'static str {
        match self {
            Self::Read | Self::Search | Self::Think | Self::Fetch => "read_only",
            Self::Edit => "workspace_write",
            Self::Delete | Self::Move => "destructive",
            Self::Execute => "ambient_side_effect",
            Self::Other => "other",
        }
    }
}

/// Rough side-effect taxonomy for the capability-ceiling check.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectLevel {
    /// No side effect declared (conservative default; permission logic
    /// treats this as "unknown → deny unless explicitly allowed").
    #[default]
    None,
    /// Pure reads only.
    ReadOnly,
    /// Writes to workspace files.
    WorkspaceWrite,
    /// Runs subprocesses.
    ProcessExec,
    /// Reaches external services over the network.
    Network,
}

impl SideEffectLevel {
    /// Numeric rank used by the policy intersector and side-effect
    /// ceiling check. Higher rank ⇒ more invasive.
    pub fn rank(&self) -> usize {
        match self {
            Self::None => 0,
            Self::ReadOnly => 1,
            Self::WorkspaceWrite => 2,
            Self::ProcessExec => 3,
            Self::Network => 4,
        }
    }

    /// Short string used in policy documents, bridge payloads, and
    /// error messages. Stable wire identifier.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::ProcessExec => "process_exec",
            Self::Network => "network",
        }
    }

    /// Parse from the stable string used in policy documents. Unknown
    /// values deserialize to `None` (the conservative default).
    pub fn parse(value: &str) -> Self {
        match value {
            "none" => Self::None,
            "read_only" => Self::ReadOnly,
            "workspace_write" => Self::WorkspaceWrite,
            "process_exec" => Self::ProcessExec,
            "network" => Self::Network,
            _ => Self::None,
        }
    }
}

/// Declarative description of a tool's argument shape. The VM uses
/// this to:
///
/// - resolve `ToolArgConstraint` lookups (`path_params`),
/// - rewrite high-level aliases to canonical keys without any
///   per-tool hardcoded branches (`arg_aliases`),
/// - validate presence of required arguments at the dispatch boundary
///   (`required`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolArgSchema {
    /// Argument keys whose values are workspace-relative paths.
    /// First matching key whose value is a string wins.
    pub path_params: Vec<String>,
    /// Alias → canonical key. When a tool call arrives with an alias
    /// in its argument object, the VM rewrites the key to the canonical
    /// form before dispatch (generic; no tool-name branches).
    pub arg_aliases: BTreeMap<String, String>,
    /// Argument keys that must be present (non-null) on every call.
    pub required: Vec<String>,
}

/// Full annotations for one tool. Pipelines populate one of these per
/// tool in the capability-policy registry; the VM consults the registry
/// on every tool call.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolAnnotations {
    /// ACP-aligned tool-kind classification.
    pub kind: ToolKind,
    /// Required side-effect level for the capability ceiling check.
    pub side_effect_level: SideEffectLevel,
    /// Argument shape declarations.
    pub arg_schema: ToolArgSchema,
    /// Capability operations requested by this tool (e.g.
    /// `"workspace": ["read_text", "list"]`).
    pub capabilities: BTreeMap<String, Vec<String>>,
    /// True when the tool may return only a handle/reference to a large
    /// output artifact instead of inline output. Execute tools with this
    /// flag must also declare an inspection route.
    pub emits_artifacts: bool,
    /// Tool names that can inspect artifacts/results emitted by this tool.
    pub result_readers: Vec<String>,
    /// Explicit escape hatch for tools whose results are always complete
    /// inline, even though they are execute-like.
    pub inline_result: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_kind_serde_roundtrip() {
        for (kind, expected) in [
            (ToolKind::Read, "\"read\""),
            (ToolKind::Edit, "\"edit\""),
            (ToolKind::Delete, "\"delete\""),
            (ToolKind::Move, "\"move\""),
            (ToolKind::Search, "\"search\""),
            (ToolKind::Execute, "\"execute\""),
            (ToolKind::Think, "\"think\""),
            (ToolKind::Fetch, "\"fetch\""),
            (ToolKind::Other, "\"other\""),
        ] {
            let encoded = serde_json::to_string(&kind).unwrap();
            assert_eq!(encoded, expected);
            let decoded: ToolKind = serde_json::from_str(expected).unwrap();
            assert_eq!(decoded, kind);
        }
    }

    #[test]
    fn only_read_search_think_fetch_are_read_only() {
        assert!(ToolKind::Read.is_read_only());
        assert!(ToolKind::Search.is_read_only());
        assert!(ToolKind::Think.is_read_only());
        assert!(ToolKind::Fetch.is_read_only());
        // Fail-safe: Other is NOT read-only.
        assert!(!ToolKind::Other.is_read_only());
        assert!(!ToolKind::Edit.is_read_only());
        assert!(!ToolKind::Delete.is_read_only());
        assert!(!ToolKind::Move.is_read_only());
        assert!(!ToolKind::Execute.is_read_only());
    }

    #[test]
    fn mutation_class_derived_from_kind() {
        assert_eq!(ToolKind::Read.mutation_class(), "read_only");
        assert_eq!(ToolKind::Search.mutation_class(), "read_only");
        assert_eq!(ToolKind::Edit.mutation_class(), "workspace_write");
        assert_eq!(ToolKind::Delete.mutation_class(), "destructive");
        assert_eq!(ToolKind::Move.mutation_class(), "destructive");
        assert_eq!(ToolKind::Execute.mutation_class(), "ambient_side_effect");
        assert_eq!(ToolKind::Other.mutation_class(), "other");
    }

    #[test]
    fn side_effect_level_round_trip() {
        for level in [
            SideEffectLevel::None,
            SideEffectLevel::ReadOnly,
            SideEffectLevel::WorkspaceWrite,
            SideEffectLevel::ProcessExec,
            SideEffectLevel::Network,
        ] {
            assert_eq!(SideEffectLevel::parse(level.as_str()), level);
            let encoded = serde_json::to_string(&level).unwrap();
            let decoded: SideEffectLevel = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, level);
        }
    }

    #[test]
    fn side_effect_level_rank_orders() {
        assert!(SideEffectLevel::None.rank() < SideEffectLevel::ReadOnly.rank());
        assert!(SideEffectLevel::ReadOnly.rank() < SideEffectLevel::WorkspaceWrite.rank());
        assert!(SideEffectLevel::WorkspaceWrite.rank() < SideEffectLevel::ProcessExec.rank());
        assert!(SideEffectLevel::ProcessExec.rank() < SideEffectLevel::Network.rank());
    }

    #[test]
    fn arg_schema_defaults_empty() {
        let schema = ToolArgSchema::default();
        assert!(schema.path_params.is_empty());
        assert!(schema.arg_aliases.is_empty());
        assert!(schema.required.is_empty());
    }

    #[test]
    fn annotations_default_result_routes_empty() {
        let annotations = ToolAnnotations::default();
        assert!(!annotations.emits_artifacts);
        assert!(annotations.result_readers.is_empty());
        assert!(!annotations.inline_result);
    }
}
