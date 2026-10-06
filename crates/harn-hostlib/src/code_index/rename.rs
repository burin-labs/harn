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

use std::path::Path;

use harn_vm::VmValue;

use crate::ast::{api as ast_api, Language, TEXT_PATCH_FALLBACK};
use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_string, require_string, str_value,
};

use super::builtins::SharedIndex;
use super::refactor_core::{
    candidates_value, collect_identifier_spans, competing_declarations, edit_envelope,
    failed_paths_value, file_plan_value, files_in_scope, first_syntax_error, is_identifier_token,
    parse_kind, plan_file, read_source, resolve_seed, write_plans, EditEnvelope, EditSpan,
    EditSymbol, FilePlan, Scope, SeedLookup, ShadowSite,
};
#[cfg(test)]
use super::state::IndexState;
use super::symbol_graph::NodeKind;

pub(super) const BUILTIN: &str = "hostlib_code_index_rename_symbol";

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
    let symbol_kind = symbol_kind_raw
        .as_deref()
        .map(|raw| parse_kind(BUILTIN, raw))
        .transpose()?;

    // Two modes share the same machinery:
    //   - rename  : `new_name` is a fresh identifier; shadow-checked, identifier
    //     -validated; every identifier-context occurrence becomes `new_name`.
    //   - replace : `replacement_text` is arbitrary text (e.g. `client.fetch`);
    //     no shadow/identifier gate (a literal replacement can't shadow), but the
    //     post-edit file must still re-parse. This turns the one-shot atomic
    //     cross-file primitive into a general symbol-grounded find/replace so an
    //     API migration is one call instead of N `edit`s.
    let replacement_text = optional_string(BUILTIN, dict, "replacement_text")?;
    let scope = Scope::parse(BUILTIN, &require_string(BUILTIN, dict, "scope")?)?;
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

    // The identifier rewrite below would otherwise silently rename both
    // definitions and their unrelated uses.
    let competing =
        competing_declarations(&state.symbols, seed_node_id, &symbol_name, &in_scope_files);
    if !competing.is_empty() {
        let mut candidates = vec![(seed_path, seed_node.line, seed_node.kind.as_str())];
        candidates.extend(competing);
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
        let source = read_source(BUILTIN, &abs, session_id.as_deref())?;
        let tree = match ast_api::parse_tree(&source, language) {
            Ok(tree) => tree,
            Err(err) => {
                return Ok(syntax_error_response(
                    &env,
                    format!("`{path}` failed to parse: {err}"),
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
        if validate {
            if let Some(detail) = first_syntax_error(&source, language) {
                return Ok(syntax_error_response(
                    &env,
                    format!("`{path}` does not parse before the edit ({detail}); fix it first"),
                ));
            }
        }

        let edits: Vec<EditSpan> = targets
            .into_iter()
            .map(|span| EditSpan {
                span,
                before: symbol_name.clone(),
                after: new_name.clone(),
            })
            .collect();

        match plan_file(path.clone(), language, source, edits, validate) {
            Ok(plan) => plans.push(plan),
            Err(detail) => {
                return Ok(syntax_error_response(
                    &env,
                    format!("rewriting `{path}` produced syntax errors: {detail}"),
                ));
            }
        }
    }

    if !shadows.is_empty() {
        return Ok(conflict_response(&env, &shadows));
    }

    if plans.is_empty() {
        return Ok(no_match_response(&env));
    }

    // Pre-flight done. With `dry_run` we stop here without persisting.
    let failed = if dry_run {
        Vec::new()
    } else {
        write_plans(BUILTIN, &state.root, &plans, session_id.as_deref())
    };

    Ok(applied_response(&env, &plans, dry_run, failed))
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
    edit_envelope(
        tag,
        env.scope,
        &EditSymbol {
            name: env.symbol_name,
            new_name: Some(env.new_name),
            path: env.symbol_path,
            line: env.symbol_line,
            kind: env.symbol_kind,
        },
        EditEnvelope {
            applied: extras.applied,
            dry_run: extras.dry_run,
            touched_files: extras.touched_files,
            conflicts: extras.conflicts,
            warnings: extras.warnings,
            failed_paths: extras.failed_paths,
            match_count: extras.match_count,
            details: extras.details,
            fallback_suggestion: extras.fallback_suggestion,
            extra: Vec::new(),
        },
    )
}

fn applied_response(
    env: &ResponseEnv<'_>,
    plans: &[FilePlan],
    dry_run: bool,
    failed: Vec<(String, String)>,
) -> VmValue {
    let touched_files: Vec<VmValue> = plans.iter().map(file_plan_value).collect();
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
    let candidate_list = candidates_value(candidates);
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

fn syntax_error_response(env: &ResponseEnv<'_>, details: String) -> VmValue {
    emit_response(
        env,
        "syntax_error",
        ResponseExtras {
            details,
            ..Default::default()
        },
    )
}

#[cfg(test)]
#[path = "rename_tests.rs"]
mod tests;
