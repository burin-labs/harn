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
//! - `ambiguous_symbol` — multiple distinct symbols match `symbol_ref`,
//!   or another declaration with the same name is inside the rewrite scope.
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

    // Two modes share the same machinery:
    //   - rename  : `new_name` is a fresh identifier; shadow-checked, identifier
    //     -validated; every identifier-context occurrence becomes `new_name`.
    //   - replace : `replacement_text` is arbitrary text (e.g. `client.fetch`);
    //     no shadow/identifier gate (a literal replacement can't shadow), but the
    //     post-edit file must still re-parse. This turns the one-shot atomic
    //     cross-file primitive into a general symbol-grounded find/replace so an
    //     API migration is one call instead of N `edit`s.
    let replacement_text = optional_string(BUILTIN, dict, "replacement_text")?;
    let scope = Scope::parse(&require_string(BUILTIN, dict, "scope")?)?;
    let session_id = optional_string(BUILTIN, dict, "session_id")?;
    let dry_run = optional_bool(BUILTIN, dict, "dry_run", false)?;
    let validate = optional_bool(BUILTIN, dict, "validate", true)?;

    let new_name = match &replacement_text {
        // In rename mode `new_name` is required and reported as the after-text.
        None => require_string(BUILTIN, dict, "new_name")?,
        // In replace mode `new_name` is irrelevant; carry the literal so response
        // shaping (which echoes the after-text) stays uniform.
        Some(text) => text.clone(),
    };
    let is_rename = replacement_text.is_none();

    if is_rename && new_name == symbol_name {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "new_name",
            message: "new_name must differ from symbol_ref.name".into(),
        });
    }
    if let Some(text) = &replacement_text {
        if text.is_empty() {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "replacement_text",
                message: "replacement_text must be non-empty (to delete a symbol \
                     use `remove_symbol` / `delete_range`)"
                    .into(),
            });
        }
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

    // The index's REFS edges are name-based, not binding-resolved. Pinning a
    // declaration identifies the seed, but cannot establish which same-named
    // declaration each use refers to. The identifier rewrite below would
    // otherwise silently rename both definitions and their unrelated uses.
    let competing_declarations: Vec<_> = state
        .symbols
        .nodes_named(&symbol_name)
        .iter()
        .filter(|id| **id != seed_node_id)
        .filter_map(|id| state.symbols.node(*id))
        .filter(|node| is_rename_declaration(node.kind) && in_scope_files.contains(&node.path))
        .map(|node| (node.path.clone(), node.line, node.kind.as_str()))
        .collect();
    if !competing_declarations.is_empty() {
        let mut candidates = vec![(seed_path, seed_node.line, seed_node.kind.as_str())];
        candidates.extend(competing_declarations);
        return Ok(ambiguous_response_with_details(
            &env,
            &candidates,
            "rename aborted without writes: the scope contains separate declarations with this name. Pinning the seed does not disambiguate their references; use a binding-aware LSP rename or narrow the scope.",
        ));
    }

    // The identifier-validity gate is a rename-only concern: an arbitrary
    // `replacement_text` (e.g. `client.fetch(`) is intentionally not an
    // identifier. Syntax validation (below) is the safety net for replace mode.
    if is_rename && !is_identifier_token(&new_name) {
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
        // Shadow detection is a rename-only concern (the new identifier must not
        // already exist). In replace mode `replacement_text` is arbitrary, so
        // pass an empty shadow target — no source identifier can equal it.
        let shadow_target: &str = if is_rename { &new_name } else { "" };
        collect_identifier_spans(
            tree.root_node(),
            source.as_bytes(),
            &symbol_name,
            shadow_target,
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
        .filter(|node| is_rename_declaration(node.kind))
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

fn is_rename_declaration(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Type
            | NodeKind::Field
            | NodeKind::EnumCase
            | NodeKind::Module
    )
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
        "Field" => Ok(NodeKind::Field),
        "EnumCase" => Ok(NodeKind::EnumCase),
        "Module" => Ok(NodeKind::Module),
        other => Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "symbol_ref.kind",
            message: format!(
                "expected one of [Function, Type, Field, EnumCase, Module], got `{other}`"
            ),
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
    ambiguous_response_with_details(
        env,
        candidates,
        "multiple symbols share `symbol_ref.name`; pass `symbol_ref.line` (and optionally `symbol_ref.kind`) to disambiguate. Candidates surfaced in the `warnings` field.",
    )
}

fn ambiguous_response_with_details(
    env: &ResponseEnv<'_>,
    candidates: &[(String, u32, &'static str)],
    details: &str,
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
            details: details.to_string(),
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
#[path = "rename_tests.rs"]
mod tests;
