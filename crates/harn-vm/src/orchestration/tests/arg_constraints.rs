//! Path-argument constraints on tool calls.
//!
//! A constraint matches or rejects on the path argument, ignores tools it was
//! not written for, prefers a declared path-param annotation over guessing, and
//! skips with a warning when it cannot find a path key at all. The error must
//! name the path key rather than the action value.

use crate::orchestration::*;
#[test]
fn arg_constraint_allows_matching_pattern() {
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "exec".to_string(),
            arg_patterns: vec!["cargo *".to_string()],
            arg_key: Some("command".to_string()),
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "exec",
        &serde_json::json!({"command": "cargo test"}),
    );
    assert!(result.is_ok());
}

#[test]
fn arg_constraint_rejects_non_matching_pattern() {
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "exec".to_string(),
            arg_patterns: vec!["cargo *".to_string()],
            arg_key: Some("command".to_string()),
        }],
        ..Default::default()
    };
    let result =
        enforce_tool_arg_constraints(&policy, "exec", &serde_json::json!({"command": "rm -rf /"}));
    assert!(result.is_err());
}

#[test]
fn arg_constraint_ignores_unmatched_tool() {
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "exec".to_string(),
            arg_patterns: vec!["cargo *".to_string()],
            arg_key: Some("command".to_string()),
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "read_file",
        &serde_json::json!({"path": "/etc/passwd"}),
    );
    assert!(result.is_ok());
}

#[test]
fn arg_constraint_prefers_declared_path_param_annotations() {
    let mut tool_annotations = std::collections::BTreeMap::new();
    tool_annotations.insert(
        "edit".to_string(),
        crate::tool_annotations::ToolAnnotations {
            kind: crate::tool_annotations::ToolKind::Edit,
            arg_schema: crate::tool_annotations::ToolArgSchema {
                path_params: vec!["path".to_string()],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "edit".to_string(),
            arg_patterns: vec!["tests/**".to_string()],
            arg_key: None,
        }],
        tool_annotations,
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "edit",
        &serde_json::json!({
            "action": "replace_range",
            "path": "tests/unit/test_experiment_service.py",
            "content": "..."
        }),
    );
    assert!(result.is_ok());
}

#[test]
fn arg_constraint_without_arg_key_or_metadata_skips_with_warning() {
    // Regression: a heuristic fallback used to pick the first string arg
    // (often `action`) and blame it for mismatches. Policy authors now must
    // declare `arg_key` or `path_params`; otherwise the constraint is
    // SKIPPED with a structured `log_warn`.
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "edit".to_string(),
            arg_patterns: vec!["tests/unit/test_experiment_service.py".to_string()],
            arg_key: None,
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "edit",
        &serde_json::json!({
            "action": "exact_patch",
            "path": "tests/unit/test_experiment_service.py",
            "old_string": "assert len(items) == 1",
            "new_string": "assert len(items) == 2",
        }),
    );
    assert!(
        result.is_ok(),
        "unresolved constraint must skip (not reject) so a misconfigured policy doesn't silently block work; got: {result:?}"
    );
}

#[test]
fn arg_constraint_with_explicit_arg_key_allows_matching_path() {
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "edit".to_string(),
            arg_patterns: vec!["tests/unit/*".to_string()],
            arg_key: Some("path".to_string()),
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "edit",
        &serde_json::json!({
            "action": "exact_patch",
            "path": "tests/unit/test_experiment_service.py",
        }),
    );
    assert!(
        result.is_ok(),
        "expected allow (path matches), got: {result:?}"
    );
}

#[test]
fn arg_constraint_error_names_the_path_key_not_the_action_value() {
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "edit".to_string(),
            arg_patterns: vec!["src/allowed/*".to_string()],
            arg_key: Some("path".to_string()),
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "edit",
        &serde_json::json!({
            "action": "replace_range",
            "path": "src/forbidden/foo.rs",
            "content": "..."
        }),
    );
    let Err(err) = result else {
        panic!("expected rejection, got Ok");
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("path 'src/forbidden/foo.rs'"),
        "error should name the `path` argument, got: {msg}"
    );
    assert!(
        !msg.contains("argument 'replace_range'"),
        "error must not blame the `action` value, got: {msg}"
    );
}

#[test]
fn arg_constraint_skips_when_no_path_key_present_in_call() {
    // Absence of the declared arg_key is outside the allow-list's scope —
    // skip rather than rejecting an empty string against the patterns.
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "edit".to_string(),
            arg_patterns: vec!["tests/*".to_string()],
            arg_key: Some("path".to_string()),
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "edit",
        &serde_json::json!({
            "action": "noop",
            "content": "...",
        }),
    );
    assert!(
        result.is_ok(),
        "no path arg → constraint should skip, got: {result:?}"
    );
}

fn path_keyed_policy(tool: &str, arg_key: &str, patterns: &[&str]) -> CapabilityPolicy {
    let mut tool_annotations = std::collections::BTreeMap::new();
    tool_annotations.insert(
        tool.to_string(),
        crate::tool_annotations::ToolAnnotations {
            kind: crate::tool_annotations::ToolKind::Edit,
            arg_schema: crate::tool_annotations::ToolArgSchema {
                path_params: vec!["path".to_string(), "paths".to_string()],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: tool.to_string(),
            arg_patterns: patterns.iter().map(|pattern| pattern.to_string()).collect(),
            arg_key: Some(arg_key.to_string()),
        }],
        tool_annotations,
        ..Default::default()
    }
}

fn admits(policy: &CapabilityPolicy, tool: &str, args: serde_json::Value) -> bool {
    enforce_tool_arg_constraints(policy, tool, &args).is_ok()
}

#[test]
fn path_keyed_single_star_stays_within_one_directory() {
    let policy = path_keyed_policy("edit", "path", &["src/*"]);
    assert!(admits(
        &policy,
        "edit",
        serde_json::json!({"path": "src/a.rs"})
    ));
    assert!(
        !admits(
            &policy,
            "edit",
            serde_json::json!({"path": "src/nested/a.rs"})
        ),
        "a path-keyed `*` must not cross `/`"
    );
    let recursive = path_keyed_policy("edit", "path", &["src/**"]);
    assert!(admits(
        &recursive,
        "edit",
        serde_json::json!({"path": "src/nested/a.rs"})
    ));
}

#[test]
fn non_path_key_keeps_flat_name_matching() {
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "exec".to_string(),
            arg_patterns: vec!["cargo *".to_string()],
            arg_key: Some("command".to_string()),
        }],
        ..path_keyed_policy("exec", "command", &[])
    };
    assert!(
        admits(
            &policy,
            "exec",
            serde_json::json!({"command": "cargo test --manifest-path crates/a/Cargo.toml"})
        ),
        "a command pattern's `*` still crosses `/`"
    );
}

#[test]
fn path_keyed_values_are_matched_on_their_workspace_spelling() {
    let policy = path_keyed_policy("edit", "path", &["src/**"]);
    assert!(admits(
        &policy,
        "edit",
        serde_json::json!({"path": "./src/a.rs"})
    ));
    assert!(admits(
        &policy,
        "edit",
        serde_json::json!({"path": "src//nested/./a.rs"})
    ));
    assert!(
        !admits(
            &policy,
            "edit",
            serde_json::json!({"path": "src/../outside.rs"})
        ),
        "`..` must not walk out of the granted subtree"
    );
    assert!(
        !admits(
            &policy,
            "edit",
            serde_json::json!({"path": "src/../../escape.rs"})
        ),
        "a path escaping the workspace is admitted by no pattern"
    );
    let everything = path_keyed_policy("edit", "path", &["**"]);
    assert!(
        !admits(
            &everything,
            "edit",
            serde_json::json!({"path": "../escape.rs"})
        ),
        "`**` means the whole workspace, not the host"
    );
}

#[test]
fn path_keyed_patterns_support_alternates_and_literal_brackets() {
    let policy = path_keyed_policy("edit", "path", &["src/{a,b}.rs", "app/[slug]/page.tsx"]);
    assert!(admits(
        &policy,
        "edit",
        serde_json::json!({"path": "src/b.rs"})
    ));
    assert!(!admits(
        &policy,
        "edit",
        serde_json::json!({"path": "src/c.rs"})
    ));
    assert!(admits(
        &policy,
        "edit",
        serde_json::json!({"path": "app/[slug]/page.tsx"})
    ));
}

#[test]
fn path_keyed_array_is_admitted_only_when_every_entry_is() {
    let policy = path_keyed_policy("run_codemod", "paths", &["src/**"]);
    assert!(admits(
        &policy,
        "run_codemod",
        serde_json::json!({"paths": ["src/a.rs", "src/b/c.rs"]})
    ));
    assert!(
        !admits(
            &policy,
            "run_codemod",
            serde_json::json!({"paths": ["src/a.rs", "lib/b.rs"]})
        ),
        "one out-of-scope entry rejects the whole call"
    );
}
