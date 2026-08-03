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
use harn_parser::{visit, DiagnosticCode as Code, DictEntry, Node, Repair, RepairSafety, SNode};

use super::{capability_argument_for_span, RepairCandidate, RepairImpactWire};

const RETIRED_UNUSED_HELPERS: &[&str] = &["with_mocks", "with_host_mocks"];

/// Capabilities a retired `std/testing` wrapper needs once it is projected onto
/// its typed replacement.
///
/// The replacements take explicit handles — `with_host_mocks` becomes
/// `with_capability_fixtures(testing, ..)`, and `with_mocks` resolves to
/// whichever scopes its config literal actually declared. The whole-program
/// pass seeds these so the enclosing callable actually receives a carrier;
/// without the seed the rewrite has no handle to name, declines the file, and
/// the plan converges while retired calls remain.
///
/// Demand is read per call site, not per callee name. Seeding both handles for
/// every `with_mocks` would hand a host-only site an LLM handle it never uses,
/// which the attenuation pass would immediately try to narrow away again.
pub(super) fn retired_wrapper_capabilities(
    program: &[SNode],
    source: &str,
    call: &super::CallSite,
) -> Vec<CapabilityId> {
    match call.callee.as_str() {
        "with_host_mocks" => vec![CapabilityId::Testing],
        // A config this recipe cannot read is a site it will not rewrite, so it
        // demands nothing: threading a carrier for a call that stays put is
        // churn a later attenuation pass has to undo.
        "with_mocks" => call
            .args
            .first()
            .and_then(|config| mock_config_scopes(program, source, *config))
            .map(|scopes| scopes.capabilities())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[derive(Clone)]
struct TestingCall {
    span: Span,
    args: Vec<Span>,
}

/// The scopes one retired `with_mocks(config, body)` call declared, as the
/// value spans of its config literal.
struct MockScopes {
    host: Option<Span>,
    llm: Option<Span>,
}

impl MockScopes {
    fn capabilities(&self) -> Vec<CapabilityId> {
        let mut capabilities = Vec::new();
        if self.host.is_some() {
            capabilities.push(CapabilityId::Testing);
        }
        if self.llm.is_some() {
            capabilities.push(CapabilityId::Llm);
        }
        capabilities
    }
}

fn dict_key_name(entry: &DictEntry) -> Option<&str> {
    match &entry.key.node {
        Node::Identifier(name) | Node::StringLiteral(name) => Some(name.as_str()),
        _ => None,
    }
}

/// Read the scopes a `with_mocks` config literal declares.
///
/// `with_mocks` hid two scopes behind one untyped dict, so the recipe reads the
/// keys the site actually wrote rather than assuming a shape. The split is
/// performed by splicing around the value spans, which keeps each site's own
/// formatting but can only preserve what lies inside those spans. Configs it
/// cannot read that way keep their call site inert:
///
/// * anything that is not a dict literal — a helper call or a forwarded
///   parameter has no keys to read, and reading it twice to split it would
///   evaluate the caller's expression twice;
/// * a key outside the two-scope contract, or the same key twice, which means
///   the site is doing something this recipe has not been taught;
/// * `llm_mocks` written before `host_mocks`, because the fixture scope must
///   stay outside the LLM scope to preserve teardown order, and splicing
///   cannot reorder the two values;
/// * a config carrying a comment, because the spliced-away text between the
///   braces and the values is discarded, and silently dropping a comment
///   during a bulk migration is not a trade this recipe makes.
fn mock_config_scopes(program: &[SNode], source: &str, config: Span) -> Option<MockScopes> {
    let config_source = source.get(config.start..config.end)?;
    if config_source.contains("//") || config_source.contains("/*") {
        return None;
    }
    let mut scopes = None;
    visit::walk_program(program, &mut |node| {
        if node.span.start != config.start || node.span.end != config.end {
            return;
        }
        let Node::DictLiteral(entries) = &node.node else {
            return;
        };
        let mut host = None;
        let mut llm = None;
        for entry in entries {
            match dict_key_name(entry) {
                Some("host_mocks") if host.is_none() => host = Some(entry.value.span),
                Some("llm_mocks") if llm.is_none() => {
                    if host.is_none() && entries.len() > 1 {
                        return;
                    }
                    llm = Some(entry.value.span);
                }
                _ => return,
            }
        }
        if host.is_none() && llm.is_none() {
            return;
        }
        scopes = Some(MockScopes { host, llm });
    });
    scopes
}

fn span_between(start: usize, end: usize, anchor: Span) -> Span {
    Span::with_offsets(start, end, anchor.line, anchor.column)
}

/// Project one `with_mocks` call onto the typed helper(s) its config declared.
///
/// Splitting is by splicing rather than reprinting so the site keeps its own
/// formatting, comments, and multi-line fixture lists. The combined case nests
/// the LLM scope inside the fixture scope, which is exactly what `with_mocks`
/// did and what `with_scenario` still does — reusing the stdlib's own ordering
/// rather than restating it. Nesting also keeps the body's argument contract:
/// both typed helpers pass the same context value the retired wrapper passed.
fn mock_wrapper_call_edits(
    program: &[SNode],
    source: &str,
    call: &TestingCall,
) -> Option<(Vec<FixEdit>, Vec<&'static str>)> {
    if call.args.len() != 2 {
        return None;
    }
    let config = *call.args.first()?;
    let body = *call.args.get(1)?;
    let scopes = mock_config_scopes(program, source, config)?;
    let name_end = call.span.start.checked_add("with_mocks".len())?;
    let name_span = span_between(call.span.start, name_end, call.span);
    let handle = |expected| capability_argument_for_span(program, call.span, expected);

    match (scopes.host, scopes.llm) {
        (Some(host), None) => {
            let testing = handle("HarnessTesting")?;
            Some((
                vec![
                    FixEdit {
                        span: name_span,
                        replacement: "with_capability_fixtures".to_string(),
                    },
                    FixEdit {
                        span: span_between(config.start, host.start, config),
                        replacement: format!("{testing}, "),
                    },
                    FixEdit {
                        span: span_between(host.end, config.end, host),
                        replacement: String::new(),
                    },
                ],
                vec!["with_capability_fixtures"],
            ))
        }
        (None, Some(mocks)) => {
            let llm = handle("HarnessLlm")?;
            Some((
                vec![
                    FixEdit {
                        span: name_span,
                        replacement: "with_llm_mocks".to_string(),
                    },
                    FixEdit {
                        span: span_between(config.start, mocks.start, config),
                        replacement: format!("{llm}, "),
                    },
                    FixEdit {
                        span: span_between(mocks.end, config.end, mocks),
                        replacement: String::new(),
                    },
                ],
                vec!["with_llm_mocks"],
            ))
        }
        (Some(host), Some(mocks)) => {
            let testing = handle("HarnessTesting")?;
            let llm = handle("HarnessLlm")?;
            Some((
                vec![
                    FixEdit {
                        span: name_span,
                        replacement: "with_capability_fixtures".to_string(),
                    },
                    FixEdit {
                        span: span_between(config.start, host.start, config),
                        replacement: format!("{testing}, "),
                    },
                    FixEdit {
                        span: span_between(host.end, mocks.start, host),
                        replacement: format!(", {{ _ -> return with_llm_mocks({llm}, "),
                    },
                    FixEdit {
                        span: span_between(mocks.end, config.end, mocks),
                        replacement: String::new(),
                    },
                    FixEdit {
                        span: span_between(body.end, body.end, body),
                        replacement: ") }".to_string(),
                    },
                ],
                vec!["with_capability_fixtures", "with_llm_mocks"],
            ))
        }
        (None, None) => None,
    }
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
    // Both wrappers carry the same legacy fixture-entry field names, so the
    // renames are collected once over every migrated host scope. Emitting them
    // per wrapper would produce two edits for the same span in files that use
    // both.
    let mut fixture_arguments = Vec::new();
    if let Some(call_edits) = host_mock_call_edits {
        edits.extend(call_edits.into_iter().flatten());
        fixture_arguments.extend(
            calls
                .get("with_host_mocks")
                .into_iter()
                .flatten()
                .filter_map(|call| call.args.first().copied()),
        );
    }

    let imports_mock_wrapper = program.iter().any(|node| {
        matches!(
            &node.node,
            Node::SelectiveImport { names, path, .. }
                if path == "std/testing" && names.iter().any(|name| name == "with_mocks")
        )
    });
    let mock_wrapper_edits = if imports_mock_wrapper && !value_references.contains("with_mocks") {
        calls
            .get("with_mocks")
            .filter(|calls| !calls.is_empty())
            .and_then(|calls| {
                calls
                    .iter()
                    .map(|call| mock_wrapper_call_edits(&program, &source, call))
                    .collect::<Option<Vec<_>>>()
            })
    } else {
        None
    };
    let migrate_mock_wrapper = mock_wrapper_edits.is_some();
    let mut mock_wrapper_names = BTreeSet::new();
    if let Some(per_call) = mock_wrapper_edits {
        for (call_edits, names) in per_call {
            edits.extend(call_edits);
            mock_wrapper_names.extend(names.iter().map(|name| (*name).to_string()));
        }
        fixture_arguments.extend(
            calls
                .get("with_mocks")
                .into_iter()
                .flatten()
                .filter_map(|call| call.args.first().copied())
                .filter_map(|config| mock_config_scopes(&program, &source, config)?.host),
        );
    }

    if !fixture_arguments.is_empty() {
        let fixture_scopes = fixture_source_scopes(&program, &fixture_arguments);
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
            // One retired name can resolve to more than one typed helper: a
            // `with_mocks` file that declares both scopes imports both.
            .flat_map(|name| {
                if rename_host_mock_wrapper && name == "with_host_mocks" {
                    vec!["with_capability_fixtures".to_string()]
                } else if rename_mock_wrapper && name == "with_mocks" {
                    mock_wrapper_names.iter().cloned().collect::<Vec<_>>()
                } else {
                    vec![name.clone()]
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
    if migrate_mock_wrapper {
        migrated.insert("with_mocks".to_string());
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
    let key_name = dict_key_name;
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
