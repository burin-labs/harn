//! Capability-ceiling guard for nested Harn invocations.
//!
//! When a script that already runs under a parent execution policy
//! launches another Harn invocation — `harn run`, `harn workflow run`,
//! `harn supervisor fire/replay`, or an embedding host — Harn must scan
//! the target and reject anything that asks for *more* than the parent
//! ceiling. This module hosts the scanner: it accepts a parent
//! [`CapabilityPolicy`] plus a description of the nested target, and
//! returns the list of dimensions where the target widens the parent.
//!
//! The scanner is deliberately conservative: when in doubt about what
//! the target requests, it errs on the side of "ask for more" so the
//! parent rejects rather than silently widens. It is the integration
//! layer's job to wire this into the actual launch points (the `exec`,
//! `shell`, `host_call` builtins, the workflow CLI, the supervisor
//! API).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::workflow_bundle::WorkflowBundle;
use super::workflow_patch::{bundle_capability_ceiling, CapabilityCeilingViolation};
use super::CapabilityPolicy;

/// One Harn invocation that could be launched from inside another.
/// Each variant carries the data the scanner needs to project an
/// effective requested ceiling.
#[derive(Clone, Debug)]
pub enum NestedInvocationTarget<'a> {
    /// A `harn workflow run`-style invocation against a parsed bundle.
    WorkflowBundle(&'a WorkflowBundle),
    /// A `harn run`-style invocation against a Harn script source. The
    /// scanner does a coarse string scan for builtins that require
    /// elevated capabilities; that is enough to catch the obvious
    /// widening attempts without forcing the AST into this layer.
    HarnScript { path: &'a str, source: &'a str },
    /// An embedding-host harness manifest. The scanner reads
    /// `capability_ceiling` and `tools` if present, falling back to
    /// "request everything" so the parent rejects unstructured
    /// manifests rather than rubber-stamping them.
    BurinHarness { manifest: &'a serde_json::Value },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NestedInvocationCeilingReport {
    pub target_kind: String,
    pub target_label: String,
    pub parent: CapabilityPolicy,
    pub requested: CapabilityPolicy,
    pub violations: Vec<CapabilityCeilingViolation>,
}

impl NestedInvocationCeilingReport {
    pub fn allowed(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Compute the capability ceiling that running `target` would request
/// of its parent runtime. This mirrors the projection used inside
/// [`bundle_capability_ceiling`] so workflow bundles and standalone
/// scripts share one comparison axis.
pub fn requested_ceiling_for_target(target: &NestedInvocationTarget<'_>) -> CapabilityPolicy {
    match target {
        NestedInvocationTarget::WorkflowBundle(bundle) => bundle_capability_ceiling(bundle),
        NestedInvocationTarget::HarnScript { source, .. } => scan_harn_script_ceiling(source),
        NestedInvocationTarget::BurinHarness { manifest } => scan_burin_manifest_ceiling(manifest),
    }
}

/// Enforce the parent ceiling against a nested invocation target.
/// Returns a populated report; callers reject the launch if
/// `report.allowed()` is false.
pub fn enforce_nested_invocation_ceiling(
    parent: &CapabilityPolicy,
    target: &NestedInvocationTarget<'_>,
) -> NestedInvocationCeilingReport {
    let requested = requested_ceiling_for_target(target);
    let violations = collect_violations(parent, &requested);
    let (kind, label) = match target {
        NestedInvocationTarget::WorkflowBundle(bundle) => {
            ("workflow_bundle".to_string(), bundle.id.clone())
        }
        NestedInvocationTarget::HarnScript { path, .. } => {
            ("harn_script".to_string(), path.to_string())
        }
        NestedInvocationTarget::BurinHarness { manifest } => {
            let label = manifest
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("<unknown>")
                .to_string();
            ("burin_harness".to_string(), label)
        }
    };
    NestedInvocationCeilingReport {
        target_kind: kind,
        target_label: label,
        parent: parent.clone(),
        requested,
        violations,
    }
}

fn collect_violations(
    parent: &CapabilityPolicy,
    requested: &CapabilityPolicy,
) -> Vec<CapabilityCeilingViolation> {
    let mut violations = Vec::new();
    if parent.tools_are_restricted() {
        if parent.tools_restricted && !requested.tools_are_restricted() {
            violations.push(CapabilityCeilingViolation {
                kind: "tool".to_string(),
                detail: "nested target drops the parent tool ceiling".to_string(),
            });
        }
        for tool in &requested.tools {
            if !parent.tools.contains(tool) {
                violations.push(CapabilityCeilingViolation {
                    kind: "tool".to_string(),
                    detail: format!("nested target requests tool '{tool}' outside parent ceiling"),
                });
            }
        }
    }
    if parent.capabilities_restricted && !requested.capabilities_are_restricted() {
        violations.push(CapabilityCeilingViolation {
            kind: "capability".to_string(),
            detail: "nested target drops the parent capability ceiling".to_string(),
        });
    }
    for (capability, ops) in &requested.capabilities {
        match parent.capabilities.get(capability) {
            Some(parent_ops) => {
                for op in ops {
                    if !parent_ops.contains(op) {
                        violations.push(CapabilityCeilingViolation {
                            kind: "capability".to_string(),
                            detail: format!(
                                "nested target requests '{capability}.{op}' outside parent ceiling"
                            ),
                        });
                    }
                }
            }
            None if parent.capabilities_are_restricted() => {
                violations.push(CapabilityCeilingViolation {
                    kind: "capability".to_string(),
                    detail: format!(
                        "nested target requests capability '{capability}' outside parent ceiling"
                    ),
                });
            }
            _ => {}
        }
    }
    if let (Some(parent_level), Some(requested_level)) = (
        parent.side_effect_level.as_deref(),
        requested.side_effect_level.as_deref(),
    ) {
        if rank(requested_level) > rank(parent_level) {
            violations.push(CapabilityCeilingViolation {
                kind: "side_effect_level".to_string(),
                detail: format!(
                    "nested target requests side_effect_level '{requested_level}' outside parent ceiling '{parent_level}'"
                ),
            });
        }
    }
    if !parent.workspace_roots.is_empty() {
        for root in &requested.workspace_roots {
            if !parent.workspace_roots.contains(root) {
                violations.push(CapabilityCeilingViolation {
                    kind: "workspace_root".to_string(),
                    detail: format!(
                        "nested target requests workspace_root '{root}' outside parent allowlist"
                    ),
                });
            }
        }
    }
    // A read-only root is in scope if the parent could read it — i.e. it
    // is one of the parent's writable or read-only roots. The parent only
    // bounds this dimension once it declares roots of either kind.
    if !parent.workspace_roots.is_empty() || !parent.read_only_roots.is_empty() {
        for root in &requested.read_only_roots {
            if !parent.workspace_roots.contains(root) && !parent.read_only_roots.contains(root) {
                violations.push(CapabilityCeilingViolation {
                    kind: "read_only_root".to_string(),
                    detail: format!(
                        "nested target requests read_only_root '{root}' outside parent allowlist"
                    ),
                });
            }
        }
    }
    violations
}

fn rank(level: &str) -> usize {
    crate::tool_annotations::SideEffectLevel::rank_str(level)
}

/// Coarse capability projection for a Harn script source. We look for
/// stdlib builtin tokens (`exec`, `shell`, `http_*`, `write_file`,
/// `connector_call`, `llm_call`, etc.) and project an `(operation,
/// side_effect_level)` set that the parent must allow. This is not
/// type-aware — it intentionally over-includes rather than miss a
/// widening attempt.
fn scan_harn_script_ceiling(source: &str) -> CapabilityPolicy {
    let stripped = strip_comments(source);
    let mut capabilities: std::collections::BTreeMap<String, BTreeSet<String>> =
        std::collections::BTreeMap::new();
    let mut max_side_effect: Option<&'static str> = None;

    for (token, capability, op, side_effect) in BUILTIN_CAPABILITIES {
        if contains_call(&stripped, token) {
            capabilities
                .entry((*capability).to_string())
                .or_default()
                .insert((*op).to_string());
            max_side_effect = match max_side_effect {
                Some(current) if rank(current) >= rank(side_effect) => Some(current),
                _ => Some(side_effect),
            };
        }
    }

    CapabilityPolicy {
        tools: Vec::new(),
        tools_restricted: false,
        capabilities: capabilities
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect()))
            .collect(),
        capabilities_restricted: false,
        workspace_roots: Vec::new(),
        read_only_roots: Vec::new(),
        side_effect_level: max_side_effect.map(|level| level.to_string()),
        recursion_limit: None,
        tool_arg_constraints: Vec::new(),
        tool_annotations: std::collections::BTreeMap::new(),
        sandbox_profile: crate::orchestration::SandboxProfile::default(),
        process_sandbox: Default::default(),
    }
}

fn scan_burin_manifest_ceiling(manifest: &serde_json::Value) -> CapabilityPolicy {
    if let Some(ceiling) = manifest.get("capability_ceiling") {
        if let Ok(parsed) = serde_json::from_value::<CapabilityPolicy>(ceiling.clone()) {
            return parsed;
        }
    }
    let tools = manifest
        .get("tools")
        .and_then(|value| value.as_array())
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    CapabilityPolicy {
        tools,
        tools_restricted: false,
        capabilities: std::collections::BTreeMap::new(),
        capabilities_restricted: false,
        workspace_roots: Vec::new(),
        read_only_roots: Vec::new(),
        side_effect_level: Some("network".to_string()),
        recursion_limit: None,
        tool_arg_constraints: Vec::new(),
        tool_annotations: std::collections::BTreeMap::new(),
        sandbox_profile: crate::orchestration::SandboxProfile::default(),
        process_sandbox: Default::default(),
    }
}

/// Strip line/block comments and the contents of string literals so the
/// builtin scanner only sees actual source identifiers. Quoted text is
/// replaced with whitespace of the same length to keep byte offsets and
/// line numbers stable for any future diagnostics.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut in_block = false;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if in_block {
            if c == '*' && matches!(chars.peek(), Some('/')) {
                chars.next();
                in_block = false;
            }
            continue;
        }
        if c == '/' {
            match chars.peek() {
                Some('/') => {
                    for next in chars.by_ref() {
                        if next == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    in_block = true;
                    continue;
                }
                _ => {}
            }
        }
        if c == '#' {
            for next in chars.by_ref() {
                if next == '\n' {
                    out.push('\n');
                    break;
                }
            }
            continue;
        }
        if c == '"' || c == '\'' {
            out.push(' ');
            let quote = c;
            while let Some(next) = chars.next() {
                if next == '\\' {
                    chars.next();
                    out.push(' ');
                    out.push(' ');
                    continue;
                }
                if next == quote {
                    out.push(' ');
                    break;
                }
                out.push(if next == '\n' { '\n' } else { ' ' });
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn contains_call(source: &str, token: &str) -> bool {
    let bytes = source.as_bytes();
    let needle = token.as_bytes();
    if bytes.len() < needle.len() + 1 {
        return false;
    }
    let mut start = 0;
    while let Some(pos) = find_subslice(&bytes[start..], needle) {
        let absolute = start + pos;
        let before = if absolute == 0 {
            None
        } else {
            Some(bytes[absolute - 1])
        };
        let after = bytes.get(absolute + needle.len()).copied();
        let valid_before = match before {
            None => true,
            Some(c) => !is_identifier_byte(c),
        };
        let valid_after = matches!(after, Some(b'(') | Some(b' ') | Some(b'\t'))
            || matches!(after, Some(b'\n') | Some(b'\r'));
        if valid_before && valid_after {
            return true;
        }
        start = absolute + needle.len();
    }
    false
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn is_identifier_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

const BUILTIN_CAPABILITIES: &[(&str, &str, &str, &str)] = &[
    ("read_file", "workspace", "read_text", "read_only"),
    ("read_file_result", "workspace", "read_text", "read_only"),
    ("read_file_bytes", "workspace", "read_text", "read_only"),
    (
        "package_snapshot_open",
        "workspace",
        "read_text",
        "read_only",
    ),
    ("render", "workspace", "read_text", "read_only"),
    ("render_prompt", "workspace", "read_text", "read_only"),
    (
        "render_with_provenance",
        "workspace",
        "read_text",
        "read_only",
    ),
    ("list_dir", "workspace", "list", "read_only"),
    ("file_exists", "workspace", "exists", "read_only"),
    ("path_status", "workspace", "exists", "read_only"),
    ("stat", "workspace", "exists", "read_only"),
    ("write_file", "workspace", "write_text", "workspace_write"),
    (
        "write_file_bytes",
        "workspace",
        "write_text",
        "workspace_write",
    ),
    ("append_file", "workspace", "write_text", "workspace_write"),
    (
        "append_file_locked",
        "workspace",
        "write_text",
        "workspace_write",
    ),
    ("mkdir", "workspace", "write_text", "workspace_write"),
    ("copy_file", "workspace", "write_text", "workspace_write"),
    ("delete_file", "workspace", "delete", "workspace_write"),
    ("apply_edit", "workspace", "apply_edit", "workspace_write"),
    ("exec", "process", "exec", "process_exec"),
    ("exec_at", "process", "exec", "process_exec"),
    ("shell", "process", "exec", "process_exec"),
    ("shell_at", "process", "exec", "process_exec"),
    ("http_get", "network", "http", "network"),
    ("http_post", "network", "http", "network"),
    ("http_put", "network", "http", "network"),
    ("http_patch", "network", "http", "network"),
    ("http_delete", "network", "http", "network"),
    ("http_request", "network", "http", "network"),
    ("http_download", "network", "http", "network"),
    ("connector_call", "connector", "call", "network"),
    ("secret_get", "connector", "secret_get", "read_only"),
    ("llm_call", "llm", "call", "network"),
    ("llm_call_safe", "llm", "call", "network"),
    ("llm_completion", "llm", "call", "network"),
    ("llm_stream", "llm", "call", "network"),
    ("agent_loop", "llm", "call", "network"),
    ("vision_ocr", "vision", "ocr", "process_exec"),
    ("mcp_call", "process", "exec", "process_exec"),
    ("mcp_connect", "process", "exec", "process_exec"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn permissive_parent() -> CapabilityPolicy {
        let mut capabilities = BTreeMap::new();
        capabilities.insert(
            "workspace".to_string(),
            vec!["read_text".to_string(), "list".to_string()],
        );
        capabilities.insert("connector".to_string(), vec!["call".to_string()]);
        capabilities.insert("process".to_string(), vec!["exec".to_string()]);
        capabilities.insert("network".to_string(), vec!["http".to_string()]);
        capabilities.insert("llm".to_string(), vec!["call".to_string()]);
        CapabilityPolicy {
            tools: Vec::new(),
            tools_restricted: false,
            capabilities,
            capabilities_restricted: false,
            workspace_roots: Vec::new(),
            read_only_roots: Vec::new(),
            side_effect_level: Some("network".to_string()),
            recursion_limit: None,
            tool_arg_constraints: Vec::new(),
            tool_annotations: BTreeMap::new(),
            sandbox_profile: crate::orchestration::SandboxProfile::default(),
            process_sandbox: Default::default(),
        }
    }

    fn read_only_parent() -> CapabilityPolicy {
        let mut capabilities = BTreeMap::new();
        capabilities.insert(
            "workspace".to_string(),
            vec![
                "read_text".to_string(),
                "list".to_string(),
                "exists".to_string(),
            ],
        );
        CapabilityPolicy {
            tools: Vec::new(),
            tools_restricted: false,
            capabilities,
            capabilities_restricted: false,
            workspace_roots: Vec::new(),
            read_only_roots: Vec::new(),
            side_effect_level: Some("read_only".to_string()),
            recursion_limit: None,
            tool_arg_constraints: Vec::new(),
            tool_annotations: BTreeMap::new(),
            sandbox_profile: crate::orchestration::SandboxProfile::default(),
            process_sandbox: Default::default(),
        }
    }

    #[test]
    fn harn_script_with_only_reads_passes_under_read_only_parent() {
        let source = r#"
            let body = read_file("README.md")
            let exists = file_exists("Cargo.toml")
        "#;
        let report = enforce_nested_invocation_ceiling(
            &read_only_parent(),
            &NestedInvocationTarget::HarnScript {
                path: "test.harn",
                source,
            },
        );
        assert!(report.allowed(), "{report:#?}");
    }

    #[test]
    fn harn_script_with_exec_is_rejected_under_read_only_parent() {
        let source = r#"
            let result = exec("ls", ["-la"])
        "#;
        let report = enforce_nested_invocation_ceiling(
            &read_only_parent(),
            &NestedInvocationTarget::HarnScript {
                path: "exec.harn",
                source,
            },
        );
        assert!(!report.allowed());
        let kinds: Vec<&str> = report.violations.iter().map(|v| v.kind.as_str()).collect();
        assert!(kinds.contains(&"capability"));
        assert!(kinds.contains(&"side_effect_level"));
    }

    #[test]
    fn harn_script_with_http_is_rejected_under_read_only_parent() {
        let source = r#"
            http_get("https://example.com")
        "#;
        let report = enforce_nested_invocation_ceiling(
            &read_only_parent(),
            &NestedInvocationTarget::HarnScript {
                path: "http.harn",
                source,
            },
        );
        assert!(!report.allowed());
    }

    #[test]
    fn harn_script_with_vision_ocr_is_rejected_under_read_only_parent() {
        let source = r#"
            vision_ocr("receipt.png")
        "#;
        let report = enforce_nested_invocation_ceiling(
            &read_only_parent(),
            &NestedInvocationTarget::HarnScript {
                path: "vision.harn",
                source,
            },
        );
        assert!(!report.allowed());
        let kinds: Vec<&str> = report.violations.iter().map(|v| v.kind.as_str()).collect();
        assert!(kinds.contains(&"capability"));
        assert!(kinds.contains(&"side_effect_level"));
    }

    #[test]
    fn harn_script_keyword_inside_string_does_not_trigger() {
        let source = r#"
            let label = "exec is not invoked here"
            let body = read_file("README.md")
        "#;
        let report = enforce_nested_invocation_ceiling(
            &read_only_parent(),
            &NestedInvocationTarget::HarnScript {
                path: "string.harn",
                source,
            },
        );
        assert!(
            report.allowed(),
            "false positive on quoted token: {report:#?}"
        );
    }

    #[test]
    fn harn_script_keyword_in_comment_is_ignored() {
        let source = r#"
            // exec("rm -rf /") is a comment-only token and must not trip policy.
            let x = read_file("README.md")
        "#;
        let report = enforce_nested_invocation_ceiling(
            &read_only_parent(),
            &NestedInvocationTarget::HarnScript {
                path: "comments.harn",
                source,
            },
        );
        assert!(
            report.allowed(),
            "false positive on commented token: {report:#?}"
        );
    }

    #[test]
    fn workflow_bundle_with_act_auto_is_rejected_under_read_only_parent() {
        let mut bundle = super::super::workflow_test_fixtures::pr_monitor_bundle();
        bundle.policy.autonomy_tier = "act_auto".to_string();
        let report = enforce_nested_invocation_ceiling(
            &read_only_parent(),
            &NestedInvocationTarget::WorkflowBundle(&bundle),
        );
        assert!(!report.allowed());
    }

    #[test]
    fn burin_manifest_with_explicit_ceiling_is_used_directly() {
        let manifest = serde_json::json!({
            "id": "burin.harness.repair",
            "capability_ceiling": {
                "capabilities": {
                    "workspace": ["read_text"]
                },
                "side_effect_level": "read_only"
            }
        });
        let report = enforce_nested_invocation_ceiling(
            &read_only_parent(),
            &NestedInvocationTarget::BurinHarness {
                manifest: &manifest,
            },
        );
        assert!(report.allowed(), "{report:#?}");
    }

    #[test]
    fn burin_manifest_without_ceiling_falls_back_to_network_and_is_rejected() {
        let manifest = serde_json::json!({"id": "burin.harness.unknown"});
        let report = enforce_nested_invocation_ceiling(
            &read_only_parent(),
            &NestedInvocationTarget::BurinHarness {
                manifest: &manifest,
            },
        );
        assert!(!report.allowed());
    }

    #[test]
    fn permissive_parent_accepts_workflow_bundle() {
        let bundle = super::super::workflow_test_fixtures::pr_monitor_bundle();
        let report = enforce_nested_invocation_ceiling(
            &permissive_parent(),
            &NestedInvocationTarget::WorkflowBundle(&bundle),
        );
        assert!(report.allowed(), "{report:#?}");
    }
}
