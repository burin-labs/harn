//! `code_index.rename_symbol` — cross-file safe rename.
//!
//! Wraps the typed symbol graph from [`super::symbol_graph`] and the
//! staged-fs overlay from [`crate::fs`] (issues #2434 and #1722) so an
//! agent can rename a symbol across the workspace in one shot.
//!
//! # Wire shape
//!
//! See `schemas/code_index/rename_symbol.{request,response}.json`. The
//! request is a single dict; the response is a tagged result with one of:
//!
//! - `applied`     — the rename succeeded (or, with `dry_run`, would).
//! - `conflict`    — `new_name` already exists as an identifier in at
//!   least one file the rename would have touched.
//! - `no_match`    — `symbol_ref` did not resolve to a node in the graph.
//! - `ambiguous_symbol` — multiple distinct symbols match `symbol_ref`
//!   and the caller didn't pin them down (no `line`, multiple files).
//! - `unsupported_language` — at least one in-scope file uses a grammar
//!   that does not have an identifier table here yet.
//! - `syntax_error` — a rewritten file failed re-parse with `validate=true`.
//! - `invalid_identifier` — `new_name` is empty / shaped wrong for any
//!   in-scope language.
//!
//! # Algorithm
//!
//! 1. Resolve `symbol_ref` to a set of seed nodes in [`SymbolGraph`].
//! 2. From the seeds, collect the set of files in scope. `file` and
//!    `module` are aliases in this implementation (the graph emits one
//!    Module node per file); `workspace` adds every file that already
//!    holds a node named `symbol_ref.name`.
//! 3. Read each in-scope file (through staged-fs when a session id is
//!    supplied), tree-sitter parse, then collect:
//!    - byte spans of every identifier-context occurrence of `name`
//!      (those become the rewrite targets), and
//!    - byte spans of every identifier-context occurrence of `new_name`
//!      (those become the shadow sites).
//! 4. If any file holds a shadow site, return `conflict` without
//!    touching disk.
//! 5. Otherwise splice each file in memory. With `validate=true`, every
//!    rewritten body is re-parsed and ERROR/MISSING nodes abort the run
//!    with `syntax_error`.
//! 6. Pre-flight done — persist. Writes route through staged-fs when a
//!    `session_id` is supplied; otherwise we write directly to disk, in
//!    a single pass after every file has passed pre-flight, so a clean
//!    run is all-or-nothing modulo mid-call disk failures.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use harn_vm::VmValue;
use sha2::{Digest, Sha256};
use tree_sitter::Node;

use crate::ast::{api as ast_api, Language, TEXT_PATCH_FALLBACK};
use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_string, require_string, str_value,
};

use super::builtins::SharedIndex;
use super::state::IndexState;
use super::symbol_graph::{NodeKind, SymbolGraph};

pub(super) const BUILTIN: &str = "hostlib_code_index_rename_symbol";

/// Scope sketches how far the rename walks. `File` is just the seed
/// file; `Module` is an alias today (one Module node per file in the
/// graph) but is kept distinct on the wire so future per-package
/// scoping doesn't require a wire break. `Workspace` follows REFS
/// edges across files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    File,
    Module,
    Workspace,
}

impl Scope {
    fn parse(raw: &str) -> Result<Self, HostlibError> {
        match raw {
            "file" => Ok(Self::File),
            "module" => Ok(Self::Module),
            "workspace" => Ok(Self::Workspace),
            other => Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "scope",
                message: format!(
                    "expected one of \"file\" | \"module\" | \"workspace\", got `{other}`"
                ),
            }),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Workspace => "workspace",
        }
    }
}

/// Per-file plan staged in memory before any disk write.
struct FilePlan {
    path: String,
    language: Language,
    source: String,
    patched: String,
    edits: Vec<EditSpan>,
}

#[derive(Clone, Debug)]
struct EditSpan {
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
    before: String,
    after: String,
}

#[derive(Clone, Debug)]
struct ShadowSite {
    path: String,
    row: usize,
    col: usize,
}

pub(super) fn run(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let symbol_ref = match dict.get("symbol_ref") {
        Some(VmValue::Dict(d)) => d.clone(),
        Some(other) => {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "symbol_ref",
                message: format!("expected dict, got {}", other.type_name()),
            });
        }
        None => {
            return Err(HostlibError::MissingParameter {
                builtin: BUILTIN,
                param: "symbol_ref",
            });
        }
    };
    let symbol_dict = symbol_ref.as_ref();
    let symbol_name = require_string(BUILTIN, symbol_dict, "name")?;
    let symbol_path = require_string(BUILTIN, symbol_dict, "path")?;
    let symbol_line = match symbol_dict.get("line") {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::Int(n)) if *n >= 1 => Some(*n as u32),
        Some(VmValue::Int(n)) => {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "symbol_ref.line",
                message: format!("must be >= 1, got {n}"),
            });
        }
        Some(other) => {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "symbol_ref.line",
                message: format!("expected integer, got {}", other.type_name()),
            });
        }
    };
    let symbol_kind_raw = optional_string(BUILTIN, symbol_dict, "kind")?;
    let symbol_kind = symbol_kind_raw.as_deref().map(parse_kind).transpose()?;

    let new_name = require_string(BUILTIN, dict, "new_name")?;
    let scope = Scope::parse(&require_string(BUILTIN, dict, "scope")?)?;
    let session_id = optional_string(BUILTIN, dict, "session_id")?;
    let dry_run = optional_bool(BUILTIN, dict, "dry_run", false)?;
    let validate = optional_bool(BUILTIN, dict, "validate", true)?;

    if new_name == symbol_name {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "new_name",
            message: "new_name must differ from symbol_ref.name".into(),
        });
    }

    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Err(HostlibError::Backend {
            builtin: BUILTIN,
            message: "code index has not been initialised — call \
                 `hostlib_code_index_rebuild` first"
                .into(),
        });
    };

    let env = ResponseEnv {
        symbol_name: &symbol_name,
        new_name: &new_name,
        symbol_path: &symbol_path,
        symbol_line,
        symbol_kind,
        scope,
    };

    let normalized_path = super::builtins::normalize_relative_path_for(state, &symbol_path);
    let seed_node_id = match resolve_seed(
        &state.symbols,
        &normalized_path,
        &symbol_name,
        symbol_line,
        symbol_kind,
    ) {
        SeedLookup::One(id) => id,
        SeedLookup::None => return Ok(no_match_response(&env)),
        SeedLookup::Many(candidates) => return Ok(ambiguous_response(&env, &candidates)),
    };

    let seed_node = state
        .symbols
        .node(seed_node_id)
        .expect("resolve_seed returned a node id present in the graph");
    let seed_path = seed_node.path.clone();

    let in_scope_files = files_in_scope(
        state,
        scope,
        &symbol_name,
        &seed_path,
        session_id.as_deref(),
    );
    if in_scope_files.is_empty() {
        return Ok(no_match_response(&env));
    }

    if !is_identifier_token(&new_name) {
        return Ok(invalid_identifier_response(
            &env,
            "must start with a letter or underscore and consist of identifier characters",
        ));
    }

    let mut plans: Vec<FilePlan> = Vec::new();
    let mut shadows: Vec<ShadowSite> = Vec::new();

    for path in &in_scope_files {
        let abs = state.root.join(path);
        let Some(language) = Language::detect(Path::new(path), None) else {
            return Ok(unsupported_language_response(&env, path, None));
        };
        let source = read_source(&abs, session_id.as_deref())?;
        let tree = match ast_api::parse_tree(&source, language) {
            Ok(tree) => tree,
            Err(err) => {
                return Ok(syntax_error_response(
                    &env,
                    path,
                    &format!("parse failed: {err}"),
                ));
            }
        };

        let Some(identifier_kinds) = language.rename_identifier_kinds() else {
            return Ok(unsupported_language_response(
                &env,
                path,
                Some(language.name()),
            ));
        };

        let mut targets = Vec::new();
        let mut local_shadows: Vec<ShadowSite> = Vec::new();
        collect_identifier_spans(
            tree.root_node(),
            source.as_bytes(),
            &symbol_name,
            &new_name,
            identifier_kinds,
            path,
            &mut targets,
            &mut local_shadows,
        );

        if !local_shadows.is_empty() {
            shadows.extend(local_shadows);
            continue;
        }

        if targets.is_empty() {
            // Workspace REFS hits frequently surface files where the
            // name appears only in comments/strings; skip them silently
            // so the rename still flows.
            continue;
        }

        let edits: Vec<EditSpan> = targets
            .into_iter()
            .map(|(start, end, srow, scol, erow, ecol)| EditSpan {
                start_byte: start,
                end_byte: end,
                start_row: srow,
                start_col: scol,
                end_row: erow,
                end_col: ecol,
                before: symbol_name.clone(),
                after: new_name.clone(),
            })
            .collect();

        let patched = splice(&source, &edits);

        if validate {
            if let Some(detail) = first_syntax_error(&patched, language) {
                return Ok(syntax_error_response(&env, path, &detail));
            }
        }

        plans.push(FilePlan {
            path: path.clone(),
            language,
            source,
            patched,
            edits,
        });
    }

    if !shadows.is_empty() {
        return Ok(conflict_response(&env, &shadows));
    }

    if plans.is_empty() {
        return Ok(no_match_response(&env));
    }

    // Pre-flight done. With `dry_run` we stop here without persisting.
    let mut failed: Vec<(String, String)> = Vec::new();
    if !dry_run {
        for plan in &plans {
            let abs = state.root.join(&plan.path);
            if let Err(err) = write_source(&abs, &plan.patched, session_id.as_deref()) {
                failed.push((plan.path.clone(), err));
            }
        }
    }

    Ok(applied_response(&env, &plans, dry_run, failed))
}

enum SeedLookup {
    One(super::symbol_graph::NodeId),
    None,
    Many(Vec<(String, u32, &'static str)>),
}

fn resolve_seed(
    graph: &SymbolGraph,
    relative_path: &str,
    name: &str,
    line: Option<u32>,
    kind: Option<NodeKind>,
) -> SeedLookup {
    let candidates: Vec<&super::symbol_graph::Node> = graph
        .nodes_named(name)
        .iter()
        .filter_map(|id| graph.node(*id))
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::Function | NodeKind::Type | NodeKind::Module
            )
        })
        .collect();

    let mut narrowed: Vec<&super::symbol_graph::Node> = candidates
        .iter()
        .copied()
        .filter(|node| paths_match(&node.path, relative_path))
        .collect();

    if narrowed.is_empty() {
        // The caller may have passed a workspace-wide hint; allow seeds
        // from any file as long as the name matches and `line`/`kind`
        // can pin one down.
        narrowed = candidates;
    }

    if let Some(line) = line {
        narrowed.retain(|node| node.line == line);
    }
    if let Some(kind) = kind {
        narrowed.retain(|node| node.kind == kind);
    }

    match narrowed.len() {
        0 => SeedLookup::None,
        1 => SeedLookup::One(narrowed[0].id),
        _ => SeedLookup::Many(
            narrowed
                .iter()
                .map(|n| (n.path.clone(), n.line, n.kind.as_str()))
                .collect(),
        ),
    }
}

fn paths_match(a: &str, b: &str) -> bool {
    a == b
        || a.replace('\\', "/") == b.replace('\\', "/")
        || a.ends_with(&format!("/{b}"))
        || b.ends_with(&format!("/{a}"))
}

fn files_in_scope(
    state: &IndexState,
    scope: Scope,
    name: &str,
    seed_path: &str,
    session_id: Option<&str>,
) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    seen.insert(seed_path.to_string());
    if scope == Scope::Workspace {
        for id in state.symbols.nodes_named(name) {
            if let Some(node) = state.symbols.node(*id) {
                seen.insert(node.path.clone());
            }
        }
        // Also include files whose source contains the bare word — the
        // REFS edge catches the common case but the graph only adds
        // edges for *named* nodes, missing call sites and free-text
        // uses. The textual sweep here is cheap (file-bound) and we
        // re-validate via tree-sitter in the rewrite pass. Reads route
        // through staged-fs (#1722) when a session id is supplied so
        // we observe pending writes from the same session.
        for file in state.files.values() {
            let abs = state.root.join(&file.relative_path);
            if file_contains_word(&abs, name, session_id) {
                seen.insert(file.relative_path.clone());
            }
        }
    }
    seen.into_iter().collect()
}

fn file_contains_word(path: &Path, name: &str, session_id: Option<&str>) -> bool {
    let bytes = match crate::fs::read(path, session_id) {
        Some(Ok(bytes)) => bytes,
        Some(Err(_)) => return false,
        None => match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        },
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|tok| tok == name)
}

fn parse_kind(raw: &str) -> Result<NodeKind, HostlibError> {
    match raw {
        "Function" => Ok(NodeKind::Function),
        "Type" => Ok(NodeKind::Type),
        "Module" => Ok(NodeKind::Module),
        other => Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "symbol_ref.kind",
            message: format!("expected one of [Function, Type, Module], got `{other}`"),
        }),
    }
}

fn is_identifier_token(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Node kinds that must terminate identifier descent — strings, comments,
/// and anything else where a matching textual substring is *not* an
/// identifier reference.
fn is_skip_kind(kind: &str) -> bool {
    matches!(
        kind,
        "comment"
            | "line_comment"
            | "block_comment"
            | "doc_comment"
            | "hash_bang_line"
            | "shebang"
            | "string"
            | "string_literal"
            | "string_fragment"
            | "string_content"
            | "raw_string_literal"
            | "interpreted_string_literal"
            | "interpreted_string"
            | "char_literal"
            | "character_literal"
            | "template_string"
            | "template_substitution"
    )
}

fn collect_identifier_spans(
    root: Node<'_>,
    bytes: &[u8],
    target_name: &str,
    new_name: &str,
    identifier_kinds: &[&str],
    path: &str,
    targets: &mut Vec<(usize, usize, usize, usize, usize, usize)>,
    shadows: &mut Vec<ShadowSite>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if is_skip_kind(node.kind()) {
            continue;
        }
        if identifier_kinds.contains(&node.kind()) {
            let text = match std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()]) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if text == target_name {
                let start = node.start_position();
                let end = node.end_position();
                targets.push((
                    node.start_byte(),
                    node.end_byte(),
                    start.row,
                    start.column,
                    end.row,
                    end.column,
                ));
            } else if text == new_name {
                let pos = node.start_position();
                shadows.push(ShadowSite {
                    path: path.to_string(),
                    row: pos.row,
                    col: pos.column,
                });
            }
            // Identifier nodes are leaves — no children to recurse into.
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn splice(source: &str, edits: &[EditSpan]) -> String {
    let mut ordered: Vec<&EditSpan> = edits.iter().collect();
    ordered.sort_by_key(|e| std::cmp::Reverse(e.start_byte));
    let mut out = source.to_string();
    for edit in ordered {
        out.replace_range(edit.start_byte..edit.end_byte, &edit.after);
    }
    out
}

fn first_syntax_error(source: &str, language: Language) -> Option<String> {
    let tree = ast_api::parse_tree(source, language).ok()?;
    let root = tree.root_node();
    if !root.has_error() {
        return None;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_missing() {
            let pos = node.start_position();
            return Some(format!(
                "missing `{}` at line {}, column {}",
                node.kind(),
                pos.row + 1,
                pos.column + 1
            ));
        }
        if node.is_error() {
            let pos = node.start_position();
            return Some(format!(
                "unexpected token at line {}, column {}",
                pos.row + 1,
                pos.column + 1
            ));
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.has_error() || child.is_missing() {
                stack.push(child);
            }
        }
    }
    Some("post-edit source has parse errors".into())
}

fn read_source(path: &Path, session_id: Option<&str>) -> Result<String, HostlibError> {
    let bytes = if let Some(result) = crate::fs::read(path, session_id) {
        result.map_err(|err| HostlibError::Backend {
            builtin: BUILTIN,
            message: format!("read `{}`: {err}", path.display()),
        })?
    } else {
        std::fs::read(path).map_err(|err| HostlibError::Backend {
            builtin: BUILTIN,
            message: format!("read `{}`: {err}", path.display()),
        })?
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn write_source(path: &Path, contents: &str, session_id: Option<&str>) -> Result<(), String> {
    match crate::fs::stage_write_or_none(BUILTIN, path, contents.as_bytes(), true, true, session_id)
    {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => {}
        Err(err) => return Err(err.to_string()),
    }
    crate::fs_snapshot::auto_capture_for_write(BUILTIN, path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("mkdir `{}`: {err}", parent.display()))?;
        }
    }
    std::fs::write(path, contents).map_err(|err| format!("write `{}`: {err}", path.display()))
}

// === Response shaping ===
//
// Every response variant shares the same outer envelope shape (locked
// by `schemas/code_index/rename_symbol.response.json`); only a handful
// of fields actually vary across variants. The helpers below build one
// envelope through [`emit_response`] and let each call site override
// just the fields it cares about.

/// Bundle every field that's invariant across response variants —
/// the seed symbol descriptor, the requested scope, and the new name.
/// Threading this through cuts the per-variant builders from 8–10
/// parameters down to 1 + extras.
struct ResponseEnv<'a> {
    symbol_name: &'a str,
    new_name: &'a str,
    symbol_path: &'a str,
    symbol_line: Option<u32>,
    symbol_kind: Option<NodeKind>,
    scope: Scope,
}

/// Per-variant overrides for the response envelope. Anything left as
/// the default lands as the empty list / zero / etc. so each call site
/// only mentions the fields it actually carries data for.
#[derive(Default)]
struct ResponseExtras {
    applied: bool,
    dry_run: bool,
    touched_files: Vec<VmValue>,
    conflicts: Vec<VmValue>,
    warnings: Vec<VmValue>,
    failed_paths: Vec<VmValue>,
    match_count: usize,
    details: String,
    /// Text-edit degradation path, emitted only on `unsupported_language`
    /// so the agent loop can fall back without a hard-coded language list.
    fallback_suggestion: Option<String>,
}

fn emit_response(env: &ResponseEnv<'_>, tag: &'static str, extras: ResponseExtras) -> VmValue {
    let mut entries: Vec<(&'static str, VmValue)> = vec![
        ("result", str_value(tag)),
        ("applied", VmValue::Bool(extras.applied)),
        ("dry_run", VmValue::Bool(extras.dry_run)),
        ("scope", str_value(env.scope.as_str())),
        ("symbol", symbol_descriptor(env)),
        (
            "touched_files",
            VmValue::List(Arc::new(extras.touched_files)),
        ),
        ("conflicts", VmValue::List(Arc::new(extras.conflicts))),
        ("warnings", VmValue::List(Arc::new(extras.warnings))),
        (
            "failed_paths_with_reasons",
            VmValue::List(Arc::new(extras.failed_paths)),
        ),
        ("match_count", VmValue::Int(extras.match_count as i64)),
        ("details", str_value(&extras.details)),
    ];
    if let Some(fallback) = extras.fallback_suggestion {
        entries.push(("fallback_suggestion", str_value(fallback)));
    }
    build_dict(entries)
}

fn applied_response(
    env: &ResponseEnv<'_>,
    plans: &[FilePlan],
    dry_run: bool,
    failed: Vec<(String, String)>,
) -> VmValue {
    let touched_files: Vec<VmValue> = plans.iter().map(file_plan_to_value).collect();
    let match_count: usize = plans.iter().map(|p| p.edits.len()).sum();
    let details = if dry_run {
        "dry_run — no files were written"
    } else if failed.is_empty() {
        "rename applied"
    } else {
        "rename partially applied; see failed_paths_with_reasons"
    };
    emit_response(
        env,
        "applied",
        ResponseExtras {
            applied: failed.is_empty() && !dry_run,
            dry_run,
            touched_files,
            failed_paths: failed_paths_value(&failed),
            match_count,
            details: details.to_string(),
            ..Default::default()
        },
    )
}

fn no_match_response(env: &ResponseEnv<'_>) -> VmValue {
    emit_response(
        env,
        "no_match",
        ResponseExtras {
            details: format!(
                "no symbol named `{}` resolved against the typed graph; \
                 either the workspace has not been indexed, the file is not tracked, \
                 or `symbol_ref.line`/`symbol_ref.kind` over-narrowed the search",
                env.symbol_name
            ),
            ..Default::default()
        },
    )
}

fn ambiguous_response(
    env: &ResponseEnv<'_>,
    candidates: &[(String, u32, &'static str)],
) -> VmValue {
    let candidate_list: Vec<VmValue> = candidates
        .iter()
        .map(|(path, line, kind)| {
            build_dict([
                ("path", str_value(path)),
                ("line", VmValue::Int(*line as i64)),
                ("kind", str_value(*kind)),
            ])
        })
        .collect();
    emit_response(
        env,
        "ambiguous_symbol",
        ResponseExtras {
            warnings: candidate_list,
            details: "multiple symbols share `symbol_ref.name`; pass `symbol_ref.line` \
                 (and optionally `symbol_ref.kind`) to disambiguate. \
                 Candidates surfaced in the `warnings` field."
                .to_string(),
            ..Default::default()
        },
    )
}

fn conflict_response(env: &ResponseEnv<'_>, shadows: &[ShadowSite]) -> VmValue {
    let conflicts: Vec<VmValue> = shadows
        .iter()
        .map(|s| {
            build_dict([
                ("path", str_value(&s.path)),
                ("row", VmValue::Int(s.row as i64)),
                ("col", VmValue::Int(s.col as i64)),
                ("shadow", str_value(env.new_name)),
            ])
        })
        .collect();
    emit_response(
        env,
        "conflict",
        ResponseExtras {
            conflicts,
            details: format!(
                "rename to `{}` would shadow an existing identifier; \
                 see `conflicts` for site list",
                env.new_name
            ),
            ..Default::default()
        },
    )
}

fn unsupported_language_response(
    env: &ResponseEnv<'_>,
    file_path: &str,
    language: Option<&str>,
) -> VmValue {
    let supported = Language::all()
        .iter()
        .filter(|l| l.supports_rename())
        .map(|l| l.name())
        .collect::<Vec<_>>()
        .join(", ");
    emit_response(
        env,
        "unsupported_language",
        ResponseExtras {
            details: format!(
                "no identifier-kind table for `{}` in `{file_path}`; \
                 rename supports {supported}",
                language.unwrap_or("?")
            ),
            fallback_suggestion: Some(TEXT_PATCH_FALLBACK.to_string()),
            ..Default::default()
        },
    )
}

fn invalid_identifier_response(env: &ResponseEnv<'_>, detail: &str) -> VmValue {
    emit_response(
        env,
        "invalid_identifier",
        ResponseExtras {
            details: format!("`new_name` rejected: {detail}"),
            ..Default::default()
        },
    )
}

fn syntax_error_response(env: &ResponseEnv<'_>, file_path: &str, detail: &str) -> VmValue {
    emit_response(
        env,
        "syntax_error",
        ResponseExtras {
            details: format!("rewriting `{file_path}` produced syntax errors: {detail}"),
            ..Default::default()
        },
    )
}

fn symbol_descriptor(env: &ResponseEnv<'_>) -> VmValue {
    build_dict([
        ("name", str_value(env.symbol_name)),
        ("new_name", str_value(env.new_name)),
        ("path", str_value(env.symbol_path)),
        (
            "line",
            env.symbol_line
                .map(|n| VmValue::Int(n as i64))
                .unwrap_or(VmValue::Nil),
        ),
        (
            "kind",
            env.symbol_kind
                .map(|k| str_value(k.as_str()))
                .unwrap_or(VmValue::Nil),
        ),
    ])
}

fn file_plan_to_value(plan: &FilePlan) -> VmValue {
    let edits: Vec<VmValue> = plan
        .edits
        .iter()
        .map(|edit| {
            build_dict([
                ("start_byte", VmValue::Int(edit.start_byte as i64)),
                ("end_byte", VmValue::Int(edit.end_byte as i64)),
                ("start_row", VmValue::Int(edit.start_row as i64)),
                ("start_col", VmValue::Int(edit.start_col as i64)),
                ("end_row", VmValue::Int(edit.end_row as i64)),
                ("end_col", VmValue::Int(edit.end_col as i64)),
                ("before", str_value(&edit.before)),
                ("after", str_value(&edit.after)),
            ])
        })
        .collect();
    build_dict([
        ("path", str_value(&plan.path)),
        ("language", str_value(plan.language.name())),
        (
            "before_sha256",
            str_value(sha256_hex(plan.source.as_bytes())),
        ),
        (
            "after_sha256",
            str_value(sha256_hex(plan.patched.as_bytes())),
        ),
        ("edits", VmValue::List(Arc::new(edits))),
    ])
}

fn failed_paths_value(failed: &[(String, String)]) -> Vec<VmValue> {
    failed
        .iter()
        .map(|(path, reason)| {
            build_dict([("path", str_value(path)), ("reason", str_value(reason))])
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use tempfile::tempdir;

    use crate::code_index::CodeIndexCapability;

    fn vm_string(s: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(s))
    }

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        let mut map: harn_vm::value::DictMap = Default::default();
        for (k, v) in pairs {
            map.insert(harn_vm::value::intern_key(k), v.clone());
        }
        VmValue::dict(map)
    }

    fn field<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
        match value {
            VmValue::Dict(d) => d.get(key).expect("missing field"),
            _ => panic!("expected dict, got {value:?}"),
        }
    }

    fn s(value: &VmValue) -> String {
        match value {
            VmValue::String(s) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        }
    }

    fn list_len(value: &VmValue) -> usize {
        match value {
            VmValue::List(list) => list.len(),
            other => panic!("expected list, got {other:?}"),
        }
    }

    fn build_index(root: &Path) -> CodeIndexCapability {
        let capability = CodeIndexCapability::new();
        let shared = capability.shared();
        let mut guard = shared.lock().expect("mutex");
        let (state, _outcome) = IndexState::build_from_root(root);
        *guard = Some(state);
        drop(guard);
        capability
    }

    fn rename(
        capability: &CodeIndexCapability,
        symbol_ref: VmValue,
        new_name: &str,
        scope: &str,
    ) -> VmValue {
        let entries = vec![
            ("symbol_ref", symbol_ref),
            ("new_name", vm_string(new_name)),
            ("scope", vm_string(scope)),
        ];
        run(&capability.shared(), &[dict(&entries)]).expect("rename runs")
    }

    #[test]
    fn rust_workspace_rename_rewrites_definitions_and_call_sites() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "pub struct Widget {\n    pub size: u32,\n}\n\nimpl Widget {\n    pub fn new() -> Widget { Widget { size: 0 } }\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/main.rs"),
            "use crate::Widget;\nfn main() {\n    let w: Widget = Widget::new();\n    let _ = w.size;\n}\n",
        )
        .unwrap();

        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("Widget")),
            ("path", vm_string("src/lib.rs")),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol_ref, "Gadget", "workspace");
        assert_eq!(s(field(&result, "result")), "applied");
        assert_eq!(list_len(field(&result, "touched_files")), 2);
        let lib = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        let main = fs::read_to_string(root.join("src/main.rs")).unwrap();
        assert!(lib.contains("struct Gadget"));
        assert!(lib.contains("impl Gadget"));
        assert!(!lib.contains("Widget"));
        assert!(main.contains("use crate::Gadget;"));
        assert!(main.contains("let w: Gadget = Gadget::new();"));
    }

    #[test]
    fn rename_skips_string_literals_and_comments() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "// Widget is the doc word\npub struct Widget { size: u32 }\nfn label() -> &'static str { \"Widget\" }\n",
        )
        .unwrap();
        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("Widget")),
            ("path", vm_string("src/lib.rs")),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol_ref, "Gadget", "workspace");
        assert_eq!(s(field(&result, "result")), "applied");
        let lib = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert!(lib.contains("struct Gadget"));
        // Both the comment word "Widget" and the string literal "Widget"
        // remain because they're not identifier-context tokens.
        assert!(lib.contains("// Widget is the doc word"));
        assert!(lib.contains("\"Widget\""));
    }

    #[test]
    fn rename_detects_shadow_conflict_and_skips_write() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        let original_main = "pub struct Widget {}\npub struct Gadget {}\nfn main() { let _ = Widget {}; let _ = Gadget {}; }\n";
        fs::write(root.join("src/main.rs"), original_main).unwrap();
        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("Widget")),
            ("path", vm_string("src/main.rs")),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol_ref, "Gadget", "workspace");
        assert_eq!(s(field(&result, "result")), "conflict");
        assert!(list_len(field(&result, "conflicts")) >= 1);
        let on_disk = fs::read_to_string(root.join("src/main.rs")).unwrap();
        assert_eq!(on_disk, original_main, "rename must not write on conflict");
    }

    #[test]
    fn dry_run_does_not_modify_disk() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        let original = "fn alpha() { println!(\"hi\"); }\nfn caller() { alpha(); }\n";
        fs::write(root.join("src/lib.rs"), original).unwrap();
        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("alpha")),
            ("path", vm_string("src/lib.rs")),
            ("kind", vm_string("Function")),
        ]);
        let result = run(
            &capability.shared(),
            &[dict(&[
                ("symbol_ref", symbol_ref),
                ("new_name", vm_string("beta")),
                ("scope", vm_string("workspace")),
                ("dry_run", VmValue::Bool(true)),
            ])],
        )
        .expect("rename runs");
        assert_eq!(s(field(&result, "result")), "applied");
        let on_disk = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert_eq!(on_disk, original, "dry_run must leave disk untouched");
    }

    #[test]
    fn typescript_workspace_rename() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/widget.ts"),
            "export class Widget {\n  size = 0;\n  resize(n: number) { this.size = n; }\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/main.ts"),
            "import { Widget } from \"./widget\";\nconst w = new Widget();\nw.resize(7);\nconsole.log(w.size);\n",
        )
        .unwrap();
        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("Widget")),
            ("path", vm_string("src/widget.ts")),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol_ref, "Gadget", "workspace");
        assert_eq!(s(field(&result, "result")), "applied");
        let widget = fs::read_to_string(root.join("src/widget.ts")).unwrap();
        let main = fs::read_to_string(root.join("src/main.ts")).unwrap();
        assert!(widget.contains("class Gadget"));
        assert!(main.contains("import { Gadget }"));
        assert!(main.contains("new Gadget()"));
    }

    #[test]
    fn python_workspace_rename() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(
            root.join("pkg/widget.py"),
            "class Widget:\n    def __init__(self):\n        self.size = 0\n",
        )
        .unwrap();
        fs::write(
            root.join("pkg/main.py"),
            "from .widget import Widget\nw = Widget()\nprint(w.size)\n",
        )
        .unwrap();
        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("Widget")),
            ("path", vm_string("pkg/widget.py")),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol_ref, "Gadget", "workspace");
        assert_eq!(s(field(&result, "result")), "applied");
        let widget = fs::read_to_string(root.join("pkg/widget.py")).unwrap();
        let main = fs::read_to_string(root.join("pkg/main.py")).unwrap();
        assert!(widget.contains("class Gadget"));
        assert!(main.contains("from .widget import Gadget"));
        assert!(main.contains("w = Gadget()"));
    }

    #[test]
    fn go_workspace_rename() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("widget")).unwrap();
        fs::write(
            root.join("widget/widget.go"),
            "package widget\n\ntype Widget struct{ Size int }\n\nfunc New() *Widget { return &Widget{Size: 0} }\n",
        )
        .unwrap();
        fs::write(
            root.join("main.go"),
            "package main\n\nimport \"./widget\"\n\nfunc main() {\n    w := widget.New()\n    _ = w\n}\n",
        )
        .unwrap();
        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("Widget")),
            ("path", vm_string("widget/widget.go")),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol_ref, "Gadget", "workspace");
        assert_eq!(s(field(&result, "result")), "applied");
        let widget = fs::read_to_string(root.join("widget/widget.go")).unwrap();
        assert!(widget.contains("type Gadget struct"));
        assert!(widget.contains("*Gadget"));
        assert!(!widget.contains("Widget"));
    }

    #[test]
    fn swift_file_rename() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("Widget.swift"),
            "class Widget {\n  var size = 0\n  func resize(_ n: Int) { self.size = n }\n}\n\nlet w = Widget()\nw.resize(3)\n",
        )
        .unwrap();
        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("Widget")),
            ("path", vm_string("Widget.swift")),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol_ref, "Gadget", "file");
        assert_eq!(s(field(&result, "result")), "applied");
        let swift = fs::read_to_string(root.join("Widget.swift")).unwrap();
        assert!(swift.contains("class Gadget"));
        assert!(swift.contains("let w = Gadget()"));
    }

    #[test]
    fn harn_workspace_rename() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/task.harn"),
            "pub struct Task {\n  title: string\n}\n\nimpl Task {\n  fn summary(task: Task) -> string {\n    return task.title\n  }\n}\n\nfn summarize(task: Task) -> string {\n  return task.title\n}\n",
        )
        .unwrap();

        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("Task")),
            ("path", vm_string("src/task.harn")),
            ("kind", vm_string("Type")),
        ]);
        let result = rename(&capability, symbol_ref, "Job", "workspace");
        assert_eq!(s(field(&result, "result")), "applied");
        let source = fs::read_to_string(root.join("src/task.harn")).unwrap();
        assert!(source.contains("struct Job"));
        assert!(source.contains("impl Job"));
        assert!(source.contains("task: Job"));
        assert!(source.contains("return task.title"));
        assert!(!source.contains("Task"));
    }

    #[test]
    fn ambiguous_symbol_without_disambiguator() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/a.rs"),
            "fn helper() {}\nfn helper2() { helper(); }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/b.rs"),
            "fn helper() {}\nfn helper3() { helper(); }\n",
        )
        .unwrap();
        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("helper")),
            ("path", vm_string("src/missing.rs")),
            ("kind", vm_string("Function")),
        ]);
        let result = rename(&capability, symbol_ref, "renamed", "workspace");
        assert_eq!(s(field(&result, "result")), "ambiguous_symbol");
    }

    #[test]
    fn rejects_invalid_new_name() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn alpha() {}\n").unwrap();
        let capability = build_index(root);
        let symbol_ref = dict(&[
            ("name", vm_string("alpha")),
            ("path", vm_string("src/lib.rs")),
            ("kind", vm_string("Function")),
        ]);
        let result = rename(&capability, symbol_ref, "1bad", "file");
        assert_eq!(s(field(&result, "result")), "invalid_identifier");
    }

    #[test]
    fn staged_fs_session_routes_writes_through_overlay() {
        // With a staged-fs session in `staged` mode, the rename's
        // touched files must NOT land on the working tree — the
        // session's staging layer holds them until commit. After
        // commit, the files reflect the rename.
        let dir = tempdir().unwrap();
        let root = dir.path().canonicalize().expect("canonicalize");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let original = "pub fn alpha() {}\nfn caller() { alpha(); }\n";
        std::fs::write(root.join("src/lib.rs"), original).unwrap();
        let capability = build_index(&root);
        // Session id needs to be unique across tests so the on-disk
        // manifest in `.harn/state/staged/<id>/` doesn't collide.
        let session_id = format!(
            "rename-test-{:?}-{}",
            std::thread::current().id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );

        crate::fs::configure_session_root(&session_id, &root);
        crate::fs::set_mode(&session_id, crate::fs::FsMode::Staged, Some(&root)).expect("set_mode");

        let symbol_ref = dict(&[
            ("name", vm_string("alpha")),
            ("path", vm_string("src/lib.rs")),
            ("kind", vm_string("Function")),
        ]);
        let result = run(
            &capability.shared(),
            &[dict(&[
                ("symbol_ref", symbol_ref),
                ("new_name", vm_string("beta")),
                ("scope", vm_string("workspace")),
                ("session_id", vm_string(&session_id)),
            ])],
        )
        .expect("rename runs");
        assert_eq!(s(field(&result, "result")), "applied");

        // Working tree must be untouched while changes sit in the
        // staging overlay.
        let on_disk_pre = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert_eq!(
            on_disk_pre, original,
            "staged session must not flush to disk before commit"
        );

        // Status should report one pending write.
        let status = crate::fs::staged_status(&session_id).expect("status");
        assert_eq!(
            status.pending_writes.len(),
            1,
            "expected one pending file; got {status:?}"
        );

        // Commit flushes the rename through.
        let commit = crate::fs::commit_staged(&session_id, &[]).expect("commit");
        assert!(
            commit.failed_paths_with_reasons.is_empty(),
            "commit reported failed paths: {:?}",
            commit.failed_paths_with_reasons
        );
        let on_disk_post = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert!(on_disk_post.contains("fn beta()"));
        assert!(on_disk_post.contains("beta();"));
        assert!(!on_disk_post.contains("alpha"));
    }
}
