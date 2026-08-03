//! Capability migrations for retired `std/testing` helpers.
//!
//! The testing module used to expose ambient mock wrappers and counters. The
//! wrappers are now unnecessary; observable host-call state projects through
//! `HarnessTesting`. Keeping this policy together prevents the generic fix
//! planner from accumulating stdlib-specific import rewrites.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use harn_builtin_meta::CapabilityId;
use harn_lexer::{FixEdit, Span};
use harn_parser::{visit, DiagnosticCode as Code, Node, Repair, RepairSafety};

use super::{
    capability_argument_for_span, root_harness_argument_for_span, RepairCandidate, RepairImpactWire,
};

const RETIRED_UNUSED_HELPERS: &[&str] = &["with_mocks", "with_host_mocks"];

/// Capabilities a retired `std/testing` wrapper needs once it is projected onto
/// its typed replacement.
///
/// The replacements take explicit handles — `with_host_mocks` becomes
/// `with_capability_fixtures(testing, ..)`, and `with_mocks` splits into a
/// `HarnessTesting` scope around a `HarnessLlm` script. The whole-program pass
/// seeds these so the enclosing callable actually receives a carrier; without
/// the seed the rewrite has no handle to name, declines the file, and the plan
/// converges while retired calls remain.
pub(super) fn retired_wrapper_capabilities(callee: &str) -> &'static [CapabilityId] {
    match callee {
        "with_host_mocks" => &[CapabilityId::Testing],
        "with_mocks" => &[CapabilityId::Testing, CapabilityId::Llm],
        _ => &[],
    }
}

#[derive(Clone)]
struct TestingCall {
    span: Span,
    args: Vec<Span>,
}

pub(super) fn repair(file: &Path) -> Option<RepairCandidate> {
    let source = std::fs::read_to_string(file).ok()?;
    let program = harn_parser::parse_source(&source).ok()?;
    let mut value_references = BTreeSet::new();
    let mut calls = BTreeMap::<String, Vec<TestingCall>>::new();
    visit::walk_program(&program, &mut |node| match &node.node {
        Node::Identifier(name) => {
            value_references.insert(name.clone());
        }
        Node::FunctionCall { name, args, .. } => {
            calls.entry(name.clone()).or_default().push(TestingCall {
                span: node.span,
                args: args.iter().map(|arg| arg.span).collect(),
            });
        }
        _ => {}
    });

    let mut edits = Vec::new();
    let mut removed = BTreeSet::new();
    let imports_host_mock_wrapper = program.iter().any(|node| {
        matches!(
            &node.node,
            Node::SelectiveImport { names, path, .. }
                if path == "std/testing" && names.iter().any(|name| name == "with_host_mocks")
        )
    });
    let host_mock_call_edits = if imports_host_mock_wrapper
        && !value_references.contains("with_host_mocks")
    {
        calls
            .get("with_host_mocks")
            .filter(|calls| !calls.is_empty())
            .and_then(|calls| {
                calls
                    .iter()
                    .map(|call| {
                        if call.args.len() != 2 {
                            return None;
                        }
                        let testing =
                            capability_argument_for_span(&program, call.span, "HarnessTesting")?;
                        let name_end = call.span.start.checked_add("with_host_mocks".len())?;
                        let first_arg = call.args.first()?;
                        Some([
                            FixEdit {
                                span: Span::with_offsets(
                                    call.span.start,
                                    name_end,
                                    call.span.line,
                                    call.span.column,
                                ),
                                replacement: "with_capability_fixtures".to_string(),
                            },
                            FixEdit {
                                span: Span::with_offsets(
                                    first_arg.start,
                                    first_arg.start,
                                    first_arg.line,
                                    first_arg.column,
                                ),
                                replacement: format!("{testing}, "),
                            },
                        ])
                    })
                    .collect::<Option<Vec<_>>>()
            })
    } else {
        None
    };
    let migrate_host_mock_wrapper = host_mock_call_edits.is_some();
    if let Some(call_edits) = host_mock_call_edits {
        edits.extend(call_edits.into_iter().flatten());
        let fixture_arguments = calls
            .get("with_host_mocks")
            .into_iter()
            .flatten()
            .filter_map(|call| call.args.first().copied())
            .collect::<Vec<_>>();
        let fixture_scopes = fixture_source_scopes(&program, &fixture_arguments);
        edits.extend(legacy_host_fixture_field_edits(&program, &fixture_scopes));
    }

    // `with_mocks(config, body)` was superseded by `with_scenario(harness,
    // config, body)`, which is the same combinator over an explicit root plus
    // `fs`/`temp_dir` scopes. Only the wrapper name, the leading argument, and
    // two config keys differ, so this is a rename — not a structural split into
    // nested `with_capability_fixtures` / `with_llm_mocks` scopes.
    let imports_mock_wrapper = program.iter().any(|node| {
        matches!(
            &node.node,
            Node::SelectiveImport { names, path, .. }
                if path == "std/testing" && names.iter().any(|name| name == "with_mocks")
        )
    });
    let mock_call_edits = if imports_mock_wrapper && !value_references.contains("with_mocks") {
        calls
            .get("with_mocks")
            .filter(|calls| !calls.is_empty())
            .and_then(|calls| {
                calls
                    .iter()
                    .map(|call| {
                        if call.args.len() != 2 {
                            return None;
                        }
                        let harness = root_harness_argument_for_span(&program, call.span)?;
                        let name_end = call.span.start.checked_add("with_mocks".len())?;
                        let config = call.args.first()?;
                        let mut call_edits = vec![
                            FixEdit {
                                span: Span::with_offsets(
                                    call.span.start,
                                    name_end,
                                    call.span.line,
                                    call.span.column,
                                ),
                                replacement: "with_scenario".to_string(),
                            },
                            FixEdit {
                                span: Span::with_offsets(
                                    config.start,
                                    config.start,
                                    config.line,
                                    config.column,
                                ),
                                replacement: format!("{harness}, "),
                            },
                        ];
                        call_edits.extend(scenario_config_key_edits(&program, *config));
                        Some(call_edits)
                    })
                    .collect::<Option<Vec<_>>>()
            })
    } else {
        None
    };
    let migrate_mock_wrapper = mock_call_edits.is_some();
    if let Some(call_edits) = mock_call_edits {
        edits.extend(call_edits.into_iter().flatten());
        // Legacy host entries still spell the operation `operation`; the
        // fixture scope walk reaches entries built by helper functions too.
        let host_entries = calls
            .get("with_mocks")
            .into_iter()
            .flatten()
            .filter_map(|call| call.args.first().copied())
            .collect::<Vec<_>>();
        let fixture_scopes = fixture_source_scopes(&program, &host_entries);
        edits.extend(legacy_host_fixture_field_edits(&program, &fixture_scopes));
    }
    for node in &program {
        let Node::SelectiveImport {
            names,
            path,
            is_pub,
        } = &node.node
        else {
            continue;
        };
        if path != "std/testing" {
            continue;
        }
        let Some(import_source) = source.get(node.span.start..node.span.end) else {
            continue;
        };
        if import_source.contains("//") || import_source.contains("/*") {
            continue;
        }

        let mut removable = names
            .iter()
            .filter(|name| {
                RETIRED_UNUSED_HELPERS.contains(&name.as_str())
                    && !value_references.contains(*name)
                    && calls.get(*name).is_none_or(Vec::is_empty)
            })
            .cloned()
            .collect::<BTreeSet<_>>();

        if names.iter().any(|name| name == "host_call_count")
            && !value_references.contains("host_call_count")
        {
            let host_count_calls = calls.get("host_call_count").cloned().unwrap_or_default();
            let replacements = host_count_calls
                .iter()
                .map(|call| {
                    if !call.args.is_empty() {
                        return None;
                    }
                    let testing =
                        capability_argument_for_span(&program, call.span, "HarnessTesting")?;
                    Some(FixEdit {
                        span: call.span,
                        replacement: format!("len({testing}.calls())"),
                    })
                })
                .collect::<Option<Vec<_>>>();
            if let Some(replacements) = replacements {
                if !replacements.is_empty() {
                    edits.extend(replacements);
                    removable.insert("host_call_count".to_string());
                }
            }
        }

        let rename_host_mock_wrapper =
            migrate_host_mock_wrapper && names.iter().any(|name| name == "with_host_mocks");
        let rename_mock_wrapper =
            migrate_mock_wrapper && names.iter().any(|name| name == "with_mocks");
        if removable.is_empty() && !rename_host_mock_wrapper && !rename_mock_wrapper {
            continue;
        }
        removed.extend(removable.iter().cloned());
        let mut seen = BTreeSet::new();
        let remaining = names
            .iter()
            .filter(|name| !removable.contains(*name))
            .map(|name| {
                if rename_host_mock_wrapper && name == "with_host_mocks" {
                    "with_capability_fixtures".to_string()
                } else if rename_mock_wrapper && name == "with_mocks" {
                    "with_scenario".to_string()
                } else {
                    name.clone()
                }
            })
            .filter(|name| seen.insert(name.clone()))
            .collect::<Vec<_>>();
        let replacement = if remaining.is_empty() {
            String::new()
        } else {
            format!(
                "{}import {{ {} }} from \"{path}\"",
                if *is_pub { "pub " } else { "" },
                remaining.join(", ")
            )
        };
        edits.push(FixEdit {
            span: node.span,
            replacement,
        });
    }
    if edits.is_empty() {
        return None;
    }

    let mut migrated = removed;
    if migrate_host_mock_wrapper {
        migrated.insert("with_host_mocks".to_string());
    }

    Some(RepairCandidate {
        file: file.to_string_lossy().into_owned(),
        source: "capability-migration",
        severity: "error",
        code: Code::ImportSymbolMissing,
        message: format!(
            "retired std/testing import{} must be removed or projected through typed Harness capabilities: {}",
            if migrated.len() == 1 { "" } else { "s" },
            migrated.into_iter().collect::<Vec<_>>().join(", ")
        ),
        unresolved_name: None,
        expected_type: None,
        span: edits.last().map(|edit| edit.span),
        repair: Repair {
            id: harn_parser::RepairId::from_owned(
                "imports/remove-retired-testing-helper".to_string(),
            ),
            summary: "Remove retired std/testing imports and project supported observations through typed Harness capabilities".to_string(),
            safety: RepairSafety::SurfaceChanging,
        },
        impact: RepairImpactWire::local_ambient("retired-testing-helper"),
        edits,
    })
}

/// Rename `with_mocks` config keys onto their `with_scenario` spellings.
///
/// Only keys of the dict literal passed directly as the config argument are
/// touched; nested dicts (fixture entries, LLM turns) keep their own field
/// names. A non-literal config carries no keys to rewrite and yields nothing,
/// which is correct — `with_scenario` reads the same field names off whatever
/// the expression produces.
fn scenario_config_key_edits(program: &[harn_parser::SNode], config: Span) -> Vec<FixEdit> {
    let mut edits = Vec::new();
    visit::walk_program(program, &mut |node| {
        if node.span.start != config.start || node.span.end != config.end {
            return;
        }
        let Node::DictLiteral(entries) = &node.node else {
            return;
        };
        for entry in entries {
            let (Node::Identifier(key) | Node::StringLiteral(key)) = &entry.key.node else {
                continue;
            };
            let renamed = match key.as_str() {
                "host_mocks" => "capabilities",
                "llm_mocks" => "llm",
                _ => continue,
            };
            edits.push(FixEdit {
                span: entry.key.span,
                replacement: renamed.to_string(),
            });
        }
    });
    edits
}

fn fixture_source_scopes(program: &[harn_parser::SNode], fixture_arguments: &[Span]) -> Vec<Span> {
    let mut scopes = fixture_arguments.to_vec();
    loop {
        let mut called = BTreeSet::new();
        visit::walk_program(program, &mut |node| {
            if scopes
                .iter()
                .any(|scope| scope.start <= node.span.start && node.span.end <= scope.end)
            {
                if let Node::FunctionCall { name, .. } = &node.node {
                    called.insert(name.clone());
                }
            }
        });

        let mut added = false;
        for node in program {
            let Node::FnDecl { name, .. } = &node.node else {
                continue;
            };
            if called.contains(name)
                && !scopes
                    .iter()
                    .any(|scope| scope.start == node.span.start && scope.end == node.span.end)
            {
                scopes.push(node.span);
                added = true;
            }
        }
        if !added {
            return scopes;
        }
    }
}

fn legacy_host_fixture_field_edits(
    program: &[harn_parser::SNode],
    fixture_spans: &[Span],
) -> Vec<FixEdit> {
    fn key_name(entry: &harn_parser::DictEntry) -> Option<&str> {
        match &entry.key.node {
            Node::Identifier(name) | Node::StringLiteral(name) => Some(name.as_str()),
            _ => None,
        }
    }

    let mut edits = Vec::new();
    visit::walk_program(program, &mut |node| {
        let Node::DictLiteral(entries) = &node.node else {
            return;
        };
        if !fixture_spans
            .iter()
            .any(|span| span.start <= node.span.start && node.span.end <= span.end)
        {
            return;
        }
        let keys = entries.iter().filter_map(key_name).collect::<BTreeSet<_>>();
        if !keys.contains("capability")
            || !keys.contains("operation")
            || !keys
                .iter()
                .any(|key| matches!(*key, "result" | "error" | "unregistered_ok"))
        {
            return;
        }
        for entry in entries {
            let replacement = match key_name(entry) {
                Some("operation") => "method",
                Some("params") => "when",
                _ => continue,
            };
            edits.push(FixEdit {
                span: entry.key.span,
                replacement: replacement.to_string(),
            });
        }
    });
    edits
}
