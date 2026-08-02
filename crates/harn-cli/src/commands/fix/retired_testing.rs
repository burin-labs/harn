//! Capability migrations for retired `std/testing` helpers.
//!
//! The testing module used to expose ambient mock wrappers and counters. The
//! wrappers are now unnecessary; observable host-call state projects through
//! `HarnessTesting`. Keeping this policy together prevents the generic fix
//! planner from accumulating stdlib-specific import rewrites.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use harn_lexer::{FixEdit, Span};
use harn_parser::{visit, DiagnosticCode as Code, Node, Repair, RepairSafety};

use super::{capability_argument_for_span, RepairCandidate, RepairImpactWire};

const RETIRED_UNUSED_HELPERS: &[&str] = &["with_mocks", "with_host_mocks"];

pub(super) fn repair(file: &Path) -> Option<RepairCandidate> {
    let source = std::fs::read_to_string(file).ok()?;
    let program = harn_parser::parse_source(&source).ok()?;
    let mut value_references = BTreeSet::new();
    let mut calls = BTreeMap::<String, Vec<(Span, usize)>>::new();
    visit::walk_program(&program, &mut |node| match &node.node {
        Node::Identifier(name) => {
            value_references.insert(name.clone());
        }
        Node::FunctionCall { name, args, .. } => {
            calls
                .entry(name.clone())
                .or_default()
                .push((node.span, args.len()));
        }
        _ => {}
    });

    let mut edits = Vec::new();
    let mut removed = BTreeSet::new();
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
                .map(|(span, arg_count)| {
                    if *arg_count != 0 {
                        return None;
                    }
                    let testing = capability_argument_for_span(&program, *span, "HarnessTesting")?;
                    Some(FixEdit {
                        span: *span,
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

        if removable.is_empty() {
            continue;
        }
        removed.extend(removable.iter().cloned());
        let remaining = names
            .iter()
            .filter(|name| !removable.contains(*name))
            .cloned()
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

    Some(RepairCandidate {
        file: file.to_string_lossy().into_owned(),
        source: "capability-migration",
        severity: "error",
        code: Code::ImportSymbolMissing,
        message: format!(
            "retired std/testing import{} must be removed or projected through typed Harness capabilities: {}",
            if removed.len() == 1 { "" } else { "s" },
            removed.into_iter().collect::<Vec<_>>().join(", ")
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
