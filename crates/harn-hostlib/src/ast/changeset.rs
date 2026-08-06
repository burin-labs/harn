//! Symbol-level changeset summary over explicit before/after file images.
//!
//! This is the semantic owner for review clients. Hosts supply the bytes they
//! already own; this module applies the registered AST catalog once and returns
//! one typed, honestly degraded projection above ordinary text hunks.

use std::collections::{BTreeMap, BTreeSet};

use harn_vm::VmValue;
use serde::{Deserialize, Serialize};

use crate::code_index::{NodeKind, SharedIndex};
use crate::error::HostlibError;
use crate::tools::args::dict_arg;

use super::{api, structural_diff, Language, Symbol};

const BUILTIN: &str = "hostlib_ast_changeset_summary";
const SCHEMA: &str = "harn.review_changeset.v1";
const MAX_FILES: usize = 100;
const MAX_TOTAL_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Request {
    #[serde(default)]
    files: Vec<FileInput>,
}

#[derive(Debug, Deserialize)]
struct FileInput {
    path: String,
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    after: Option<String>,
}

#[derive(Debug, Serialize)]
struct Summary {
    schema: &'static str,
    status: &'static str,
    files: Vec<FileSummary>,
    changes: Vec<SymbolChange>,
    candidate_callers: Vec<CandidateCallers>,
    counts: Counts,
    warnings: Vec<Warning>,
}

#[derive(Debug, Serialize)]
struct FileSummary {
    path: String,
    classification: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SymbolFact {
    name: String,
    kind: String,
    path: String,
    line: u32,
    signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    access_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exported: Option<bool>,
}

#[derive(Debug, Serialize)]
struct SymbolChange {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    before: Option<SymbolFact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after: Option<SymbolFact>,
}

#[derive(Debug, Serialize)]
struct CandidateCallers {
    symbol_name: String,
    relation: &'static str,
    match_kind: &'static str,
    precision: &'static str,
    callers: Vec<Caller>,
}

#[derive(Debug, Ord, PartialOrd, Eq, PartialEq, Serialize)]
struct Caller {
    path: String,
    line: u32,
    signature: String,
}

#[derive(Debug, Default, Serialize)]
struct Counts {
    files_structural: usize,
    files_reshaped_only: usize,
    files_unchanged: usize,
    files_degraded: usize,
    symbols_added: usize,
    symbols_removed: usize,
    symbols_moved: usize,
    symbols_renamed: usize,
    signatures_changed: usize,
    exported_signatures_changed: usize,
}

#[derive(Debug, Serialize)]
struct Warning {
    path: String,
    reason: String,
}

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
struct Identity {
    kind: String,
    name: String,
    container: Option<String>,
}

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    run_with_code_index(None, args)
}

pub(super) fn run_with_code_index(
    code_index: Option<&SharedIndex>,
    args: &[VmValue],
) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let json = crate::json::vm_dict_to_json(raw.as_ref());
    let request: Request =
        serde_json::from_value(json).map_err(|error| HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "files",
            message: error.to_string(),
        })?;
    if request.files.len() > MAX_FILES {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "files",
            message: format!("at most {MAX_FILES} file images are allowed"),
        });
    }
    if let Some(file) = request
        .files
        .iter()
        .find(|file| file.path.trim().is_empty())
    {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "files.path",
            message: format!("file path must not be empty: {:?}", file.path),
        });
    }
    let summary = summarize(request, code_index);
    let json = serde_json::to_value(summary).expect("changeset summary serializes");
    Ok(harn_vm::bridge::json_result_to_vm_value(&json))
}

fn summarize(request: Request, code_index: Option<&SharedIndex>) -> Summary {
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    let mut before_facts = Vec::new();
    let mut after_facts = Vec::new();
    let mut counts = Counts::default();
    let mut total_bytes = 0usize;

    for input in request.files {
        if input.before.is_none() && input.after.is_none() {
            record_degraded(
                input.path,
                None,
                "missing_comparison_images".to_string(),
                &mut files,
                &mut warnings,
                &mut counts,
            );
            continue;
        }
        let before = input.before.as_deref().unwrap_or("");
        let after = input.after.as_deref().unwrap_or("");
        total_bytes = total_bytes
            .saturating_add(before.len())
            .saturating_add(after.len());
        if total_bytes > MAX_TOTAL_BYTES {
            record_degraded(
                input.path,
                None,
                "changeset_total_byte_limit_exceeded".to_string(),
                &mut files,
                &mut warnings,
                &mut counts,
            );
            continue;
        }
        if let Some(reason) = unsafe_source_reason(before, after) {
            let language = Language::detect(std::path::Path::new(&input.path), None)
                .map(|language| language.name().to_string());
            record_degraded(
                input.path,
                language,
                reason.to_string(),
                &mut files,
                &mut warnings,
                &mut counts,
            );
            continue;
        }
        if before == after {
            counts.files_unchanged += 1;
            let language = Language::detect(std::path::Path::new(&input.path), None)
                .map(|language| language.name().to_string());
            files.push(FileSummary {
                path: input.path,
                classification: "unchanged",
                language,
                reason: None,
            });
            continue;
        }
        let Some(language) = Language::detect(std::path::Path::new(&input.path), None) else {
            record_degraded(
                input.path,
                None,
                "unsupported_language".to_string(),
                &mut files,
                &mut warnings,
                &mut counts,
            );
            continue;
        };
        match structural_diff::sources_have_structural_changes(before, after, language) {
            Ok(false) => {
                counts.files_reshaped_only += 1;
                files.push(FileSummary {
                    path: input.path,
                    classification: "reshaped_only",
                    language: Some(language.name().to_string()),
                    reason: None,
                });
            }
            Ok(true) => {
                counts.files_structural += 1;
                collect_symbols(
                    before,
                    language,
                    &input.path,
                    &mut before_facts,
                    &mut warnings,
                );
                collect_symbols(
                    after,
                    language,
                    &input.path,
                    &mut after_facts,
                    &mut warnings,
                );
                files.push(FileSummary {
                    path: input.path,
                    classification: "structural",
                    language: Some(language.name().to_string()),
                    reason: None,
                });
            }
            Err(reason) => {
                record_degraded(
                    input.path,
                    Some(language.name().to_string()),
                    reason,
                    &mut files,
                    &mut warnings,
                    &mut counts,
                );
            }
        }
    }

    let changes = classify_symbols(before_facts, after_facts, &mut counts);
    let candidate_callers = candidate_callers(code_index, &changes);
    let status = if warnings.is_empty() {
        "complete"
    } else {
        "degraded"
    };
    Summary {
        schema: SCHEMA,
        status,
        files,
        changes,
        candidate_callers,
        counts,
        warnings,
    }
}

fn record_degraded(
    path: String,
    language: Option<String>,
    reason: String,
    files: &mut Vec<FileSummary>,
    warnings: &mut Vec<Warning>,
    counts: &mut Counts,
) {
    counts.files_degraded += 1;
    warnings.push(Warning {
        path: path.clone(),
        reason: reason.clone(),
    });
    files.push(FileSummary {
        path,
        classification: "degraded",
        language,
        reason: Some(reason),
    });
}

fn unsafe_source_reason(before: &str, after: &str) -> Option<&'static str> {
    if before.contains('\0') || after.contains('\0') {
        return Some("binary_input");
    }
    if before
        .chars()
        .chain(after.chars())
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return Some("unsafe_control_character");
    }
    None
}

fn collect_symbols(
    source: &str,
    language: Language,
    path: &str,
    out: &mut Vec<SymbolFact>,
    warnings: &mut Vec<Warning>,
) {
    if !language.supports_symbol_extraction() {
        warnings.push(Warning {
            path: path.to_string(),
            reason: "symbol_projection_unavailable".to_string(),
        });
        return;
    }
    match api::symbols_from_source(source, language) {
        Ok(symbols) => out.extend(symbols.into_iter().map(|symbol| symbol_fact(path, symbol))),
        Err(error) => warnings.push(Warning {
            path: path.to_string(),
            reason: format!("symbol_parse_failed: {error}"),
        }),
    }
}

fn symbol_fact(path: &str, symbol: Symbol) -> SymbolFact {
    let exported = symbol.access_level.as_deref().map(|level| {
        matches!(
            level.trim().to_ascii_lowercase().as_str(),
            "public" | "pub" | "open" | "export" | "exported"
        )
    });
    SymbolFact {
        name: symbol.name,
        kind: symbol.kind.as_str().to_string(),
        path: path.to_string(),
        line: symbol.start_row.saturating_add(1),
        signature: symbol.signature,
        container: symbol.container,
        access_level: symbol.access_level,
        exported,
    }
}

fn classify_symbols(
    before: Vec<SymbolFact>,
    after: Vec<SymbolFact>,
    counts: &mut Counts,
) -> Vec<SymbolChange> {
    let mut before_by_id = group_by_identity(before);
    let mut after_by_id = group_by_identity(after);
    let identities: BTreeSet<Identity> = before_by_id
        .keys()
        .chain(after_by_id.keys())
        .cloned()
        .collect();
    let mut changes = Vec::new();
    let mut removed = Vec::new();
    let mut added = Vec::new();

    for identity in identities {
        let mut old = before_by_id.remove(&identity).unwrap_or_default();
        let mut new = after_by_id.remove(&identity).unwrap_or_default();
        old.sort_by_key(|fact| (fact.path.clone(), fact.line));
        new.sort_by_key(|fact| (fact.path.clone(), fact.line));
        let mut old_used = vec![false; old.len()];
        let mut new_used = vec![false; new.len()];

        // Eliminate exact survivors before reasoning about changes. This keeps
        // repeated names and overloads in other files from shifting positional
        // pairs and creating false signature or move claims.
        for (old_index, before) in old.iter().enumerate() {
            if let Some((new_index, _)) = new.iter().enumerate().find(|(index, after)| {
                !new_used[*index]
                    && before.path == after.path
                    && before.signature == after.signature
            }) {
                old_used[old_index] = true;
                new_used[new_index] = true;
            }
        }

        for old_index in 0..old.len() {
            if old_used[old_index] {
                continue;
            }
            let path = &old[old_index].path;
            let old_at_path = old
                .iter()
                .enumerate()
                .filter(|(index, fact)| !old_used[*index] && fact.path == *path)
                .count();
            let new_at_path: Vec<usize> = new
                .iter()
                .enumerate()
                .filter(|(index, fact)| !new_used[*index] && fact.path == *path)
                .map(|(index, _)| index)
                .collect();
            if old_at_path == 1 && new_at_path.len() == 1 {
                let new_index = new_at_path[0];
                old_used[old_index] = true;
                new_used[new_index] = true;
                counts.signatures_changed += 1;
                if new[new_index].exported == Some(true) {
                    counts.exported_signatures_changed += 1;
                }
                changes.push(SymbolChange {
                    kind: "signature_changed",
                    before: Some(old[old_index].clone()),
                    after: Some(new[new_index].clone()),
                });
            }
        }

        for old_index in 0..old.len() {
            if old_used[old_index] {
                continue;
            }
            let candidates: Vec<usize> = new
                .iter()
                .enumerate()
                .filter(|(index, after)| {
                    !new_used[*index] && old[old_index].signature == after.signature
                })
                .map(|(index, _)| index)
                .collect();
            let matching_old = old
                .iter()
                .enumerate()
                .filter(|(index, before)| {
                    !old_used[*index] && before.signature == old[old_index].signature
                })
                .count();
            if matching_old == 1 && candidates.len() == 1 {
                let new_index = candidates[0];
                old_used[old_index] = true;
                new_used[new_index] = true;
                counts.symbols_moved += 1;
                changes.push(SymbolChange {
                    kind: "moved",
                    before: Some(old[old_index].clone()),
                    after: Some(new[new_index].clone()),
                });
            }
        }
        removed.extend(
            old.into_iter()
                .enumerate()
                .filter_map(|(index, fact)| (!old_used[index]).then_some(fact)),
        );
        added.extend(
            new.into_iter()
                .enumerate()
                .filter_map(|(index, fact)| (!new_used[index]).then_some(fact)),
        );
    }

    let mut used_added = BTreeSet::new();
    for old in removed {
        let old_signature = normalized_renamed_signature(&old);
        let matches: Vec<usize> = added
            .iter()
            .enumerate()
            .filter(|(index, new)| {
                !used_added.contains(index)
                    && old.kind == new.kind
                    && old.container == new.container
                    && old_signature.is_some()
                    && old_signature == normalized_renamed_signature(new)
            })
            .map(|(index, _)| index)
            .collect();
        if matches.len() == 1 {
            let index = matches[0];
            used_added.insert(index);
            counts.symbols_renamed += 1;
            changes.push(SymbolChange {
                kind: "renamed",
                before: Some(old),
                after: Some(added[index].clone()),
            });
        } else {
            counts.symbols_removed += 1;
            changes.push(SymbolChange {
                kind: "removed",
                before: Some(old),
                after: None,
            });
        }
    }
    for (index, new) in added.into_iter().enumerate() {
        if used_added.contains(&index) {
            continue;
        }
        counts.symbols_added += 1;
        changes.push(SymbolChange {
            kind: "added",
            before: None,
            after: Some(new),
        });
    }
    changes.sort_by(|left, right| {
        let l = left.after.as_ref().or(left.before.as_ref());
        let r = right.after.as_ref().or(right.before.as_ref());
        (
            l.map(|fact| fact.path.as_str()).unwrap_or(""),
            l.map(|fact| fact.line).unwrap_or(0),
            left.kind,
        )
            .cmp(&(
                r.map(|fact| fact.path.as_str()).unwrap_or(""),
                r.map(|fact| fact.line).unwrap_or(0),
                right.kind,
            ))
    });
    changes
}

fn group_by_identity(facts: Vec<SymbolFact>) -> BTreeMap<Identity, Vec<SymbolFact>> {
    let mut grouped = BTreeMap::new();
    for fact in facts {
        grouped
            .entry(Identity {
                kind: fact.kind.clone(),
                name: fact.name.clone(),
                container: fact.container.clone(),
            })
            .or_insert_with(Vec::new)
            .push(fact);
    }
    grouped
}

fn normalized_renamed_signature(fact: &SymbolFact) -> Option<String> {
    let is_identifier = |ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '$');
    let start = fact
        .signature
        .match_indices(&fact.name)
        .find_map(|(start, _)| {
            let before = fact.signature[..start].chars().next_back();
            let after = fact.signature[start + fact.name.len()..].chars().next();
            (!before.is_some_and(is_identifier) && !after.is_some_and(is_identifier))
                .then_some(start)
        })?;
    let mut signature = String::with_capacity(fact.signature.len() + 8);
    signature.push_str(&fact.signature[..start]);
    signature.push_str("<symbol>");
    signature.push_str(&fact.signature[start + fact.name.len()..]);
    Some(signature.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn candidate_callers(
    code_index: Option<&SharedIndex>,
    changes: &[SymbolChange],
) -> Vec<CandidateCallers> {
    let Some(index) = code_index else {
        return Vec::new();
    };
    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Vec::new();
    };
    let names: BTreeSet<String> = changes
        .iter()
        .filter(|change| matches!(change.kind, "signature_changed" | "renamed"))
        .filter_map(|change| change.after.as_ref().map(|fact| fact.name.clone()))
        .collect();
    names
        .into_iter()
        .filter_map(|symbol_name| {
            let callers: BTreeSet<Caller> = state
                .symbols
                .nodes_named(&symbol_name)
                .iter()
                .filter_map(|id| state.symbols.node(*id))
                .filter(|node| node.kind == NodeKind::CallSite)
                .map(|node| Caller {
                    path: node.path.clone(),
                    line: node.line,
                    signature: node.signature.clone(),
                })
                .collect();
            (!callers.is_empty()).then(|| CandidateCallers {
                symbol_name,
                relation: "CALLS",
                match_kind: "name_matched_call",
                precision: "heuristic",
                callers: callers.into_iter().collect(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summarize_files(files: Vec<FileInput>) -> Summary {
        summarize(Request { files }, None)
    }

    #[test]
    fn pure_reformat_is_reshaped_only() {
        let summary = summarize_files(vec![FileInput {
            path: "src/lib.rs".to_string(),
            before: Some("pub fn greet(name: &str) -> String { name.to_string() }\n".to_string()),
            after: Some(
                "pub fn greet(\n    name: &str\n) -> String {\n    name.to_string()\n}\n"
                    .to_string(),
            ),
        }]);
        assert_eq!(summary.counts.files_reshaped_only, 1, "{summary:?}");
        assert_eq!(summary.counts.files_structural, 0);
        assert!(summary.changes.is_empty());
    }

    #[test]
    fn signature_change_is_named_and_exported() {
        let summary = summarize_files(vec![FileInput {
            path: "src/lib.rs".to_string(),
            before: Some("pub fn greet(name: &str) -> String { name.to_string() }\n".to_string()),
            after: Some(
                "pub fn greet(name: &str, loud: bool) -> String { name.to_string() }\n".to_string(),
            ),
        }]);
        assert_eq!(summary.counts.signatures_changed, 1, "{summary:?}");
        assert_eq!(summary.counts.exported_signatures_changed, 1);
        assert_eq!(summary.changes[0].kind, "signature_changed");
        assert_eq!(
            summary.changes[0]
                .after
                .as_ref()
                .map(|fact| fact.name.as_str()),
            Some("greet")
        );
    }

    #[test]
    fn signature_change_reports_name_matched_calls_as_heuristic() {
        let root = tempfile::tempdir().expect("temp workspace");
        std::fs::write(
            root.path().join("lib.rs"),
            "pub fn greet(name: &str, loud: bool) -> String { name.to_string() }\n\
             fn main() { let _ = greet(\"Burin\", true); }\n",
        )
        .expect("write indexed source");
        let capability = crate::code_index::CodeIndexCapability::new();
        let shared = capability.shared();
        let (state, _) = crate::code_index::IndexState::build_from_root(root.path());
        *shared.lock().expect("code index mutex") = Some(state);

        let summary = summarize(
            Request {
                files: vec![FileInput {
                    path: "lib.rs".to_string(),
                    before: Some(
                        "pub fn greet(name: &str) -> String { name.to_string() }\n".to_string(),
                    ),
                    after: Some(
                        "pub fn greet(name: &str, loud: bool) -> String { name.to_string() }\n"
                            .to_string(),
                    ),
                }],
            },
            Some(&shared),
        );

        assert_eq!(summary.candidate_callers.len(), 1, "{summary:?}");
        assert_eq!(summary.candidate_callers[0].symbol_name, "greet");
        assert_eq!(summary.candidate_callers[0].relation, "CALLS");
        assert_eq!(summary.candidate_callers[0].precision, "heuristic");
        assert_eq!(summary.candidate_callers[0].callers[0].path, "lib.rs");
    }

    #[test]
    fn declaration_rename_is_coalesced_once() {
        let summary = summarize_files(vec![
            FileInput {
                path: "src/lib.rs".to_string(),
                before: Some("pub fn old_name() {}\n".to_string()),
                after: Some("pub fn new_name() {}\n".to_string()),
            },
            FileInput {
                path: "src/a.rs".to_string(),
                before: Some("fn call() { old_name(); }\n".to_string()),
                after: Some("fn call() { new_name(); }\n".to_string()),
            },
            FileInput {
                path: "src/b.rs".to_string(),
                before: Some("fn call_again() { old_name(); }\n".to_string()),
                after: Some("fn call_again() { new_name(); }\n".to_string()),
            },
        ]);
        assert_eq!(summary.counts.symbols_renamed, 1);
        assert_eq!(
            summary
                .changes
                .iter()
                .filter(|change| change.kind == "renamed")
                .count(),
            1
        );
    }

    #[test]
    fn repeated_names_pair_by_evidence_instead_of_path_order() {
        fn fact(path: &str, signature: &str) -> SymbolFact {
            SymbolFact {
                name: "helper".to_string(),
                kind: "function".to_string(),
                path: path.to_string(),
                line: 1,
                signature: signature.to_string(),
                container: None,
                access_level: None,
                exported: None,
            }
        }

        let mut counts = Counts::default();
        let changes = classify_symbols(
            vec![
                fact("src/a.rs", "fn helper(value: i32)"),
                fact("src/b.rs", "fn helper(value: &str)"),
            ],
            vec![
                fact("src/b.rs", "fn helper(value: &str, strict: bool)"),
                fact("src/c.rs", "fn helper(value: i32)"),
            ],
            &mut counts,
        );

        assert_eq!(counts.signatures_changed, 1, "{changes:?}");
        assert_eq!(counts.symbols_moved, 1, "{changes:?}");
        assert_eq!(counts.symbols_added, 0);
        assert_eq!(counts.symbols_removed, 0);
        assert!(changes.iter().any(|change| {
            change.kind == "signature_changed"
                && change.before.as_ref().map(|fact| fact.path.as_str()) == Some("src/b.rs")
                && change.after.as_ref().map(|fact| fact.path.as_str()) == Some("src/b.rs")
        }));
        assert!(changes.iter().any(|change| {
            change.kind == "moved"
                && change.before.as_ref().map(|fact| fact.path.as_str()) == Some("src/a.rs")
                && change.after.as_ref().map(|fact| fact.path.as_str()) == Some("src/c.rs")
        }));
    }

    #[test]
    fn rename_matching_does_not_replace_partial_identifiers() {
        let mut counts = Counts::default();
        let changes = classify_symbols(
            vec![SymbolFact {
                name: "foo".to_string(),
                kind: "function".to_string(),
                path: "src/lib.rs".to_string(),
                line: 1,
                signature: "fn foo(fooValue: i32)".to_string(),
                container: None,
                access_level: None,
                exported: None,
            }],
            vec![SymbolFact {
                name: "bar".to_string(),
                kind: "function".to_string(),
                path: "src/lib.rs".to_string(),
                line: 1,
                signature: "fn bar(barValue: i32)".to_string(),
                container: None,
                access_level: None,
                exported: None,
            }],
            &mut counts,
        );

        assert_eq!(counts.symbols_renamed, 0, "{changes:?}");
        assert_eq!(counts.symbols_removed, 1);
        assert_eq!(counts.symbols_added, 1);
    }

    #[test]
    fn unsupported_language_degrades_without_semantic_claims() {
        let summary = summarize_files(vec![FileInput {
            path: "notes.unknown".to_string(),
            before: Some("before".to_string()),
            after: Some("after".to_string()),
        }]);
        assert_eq!(summary.status, "degraded");
        assert_eq!(summary.counts.files_degraded, 1);
        assert!(summary.changes.is_empty());
    }

    #[test]
    fn missing_comparison_images_degrade_without_semantic_claims() {
        let summary = summarize_files(vec![FileInput {
            path: "src/lib.rs".to_string(),
            before: None,
            after: None,
        }]);
        assert_eq!(summary.status, "degraded");
        assert_eq!(summary.counts.files_degraded, 1);
        assert_eq!(
            summary.files[0].reason.as_deref(),
            Some("missing_comparison_images")
        );
        assert!(summary.changes.is_empty());
    }

    #[test]
    fn binary_images_degrade_even_when_both_sides_match() {
        let summary = summarize_files(vec![FileInput {
            path: "src/lib.rs".to_string(),
            before: Some("fn main() {}\0".to_string()),
            after: Some("fn main() {}\0".to_string()),
        }]);
        assert_eq!(summary.status, "degraded");
        assert_eq!(summary.counts.files_degraded, 1);
        assert_eq!(summary.files[0].reason.as_deref(), Some("binary_input"));
        assert_eq!(summary.counts.files_unchanged, 0);
        assert!(summary.changes.is_empty());
    }

    #[test]
    fn signature_classifier_names_symbols_from_every_scanner_language() {
        let fixtures =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ast");
        for language in Language::all()
            .iter()
            .copied()
            .filter(|language| language.supports_symbol_extraction())
        {
            let dir = fixtures.join(language.name());
            let source_path = std::fs::read_dir(&dir)
                .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| path.file_stem().is_some_and(|stem| stem == "source"))
                .unwrap_or_else(|| panic!("missing source fixture for {}", language.name()));
            let source = std::fs::read_to_string(&source_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", source_path.display()));
            let symbol = api::symbols_from_source(&source, language)
                .unwrap_or_else(|error| panic!("parse {}: {error}", language.name()))
                .into_iter()
                .find(|symbol| !symbol.signature.is_empty())
                .unwrap_or_else(|| panic!("no signature-bearing symbol for {}", language.name()));
            let before = symbol_fact(source_path.to_string_lossy().as_ref(), symbol);
            let mut after = before.clone();
            after.signature.push_str(" changed");
            let mut counts = Counts::default();
            let changes = classify_symbols(vec![before.clone()], vec![after], &mut counts);
            assert_eq!(
                counts.signatures_changed,
                1,
                "{} did not classify a signature change",
                language.name()
            );
            assert_eq!(
                changes[0].after.as_ref().map(|fact| fact.name.as_str()),
                Some(before.name.as_str()),
                "{} did not preserve the scanner-owned symbol name",
                language.name()
            );
        }
    }
}
