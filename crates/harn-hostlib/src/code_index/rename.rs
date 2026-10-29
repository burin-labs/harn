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
//!   least one file the rename would have touched, or in a Go sibling
//!   whose package scope may be affected by the new declaration.
//! - `no_match`    — `symbol_ref` did not resolve to a node in the graph.
//! - `ambiguous_symbol` — multiple distinct symbols match `symbol_ref`,
//!   or an occurrence cannot be proved to refer to the seed binding.
//! - `unsupported_language` — the requested language-specific rename is unsupported,
//!   or at least one in-scope file uses a grammar
//!   that does not have an identifier table here yet.
//! - `syntax_error` — a rewritten file failed re-parse with `validate=true`.
//! - `invalid_identifier` — `new_name` is empty / shaped wrong for any
//!   in-scope language.
//! - `cancelled` — the call was stopped before it wrote anything.
//! - `refused` — `deadline_ms` or the caller's deadline passed first; it
//!   carries `retry_after_ms` and an `alternative` request when a language
//!   server was the wait.
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
//! 4. Prove every target's binding before planning a write. Imports resolve
//!    through the shared module model; local bindings, unresolved receivers
//!    and unknown syntax return `ambiguous_symbol`. A new-name shadow returns
//!    `conflict`. Both leave every file unchanged.
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
use crate::host_call::{HostCallContext, Interrupt};
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_string, require_string, str_value,
};

use super::builtins::SharedIndex;
use super::refactor_backend::{BackendChoice, BackendReport};
use super::refactor_core::{
    collect_identifier_spans, competing_declarations, edit_envelope, failed_paths_value,
    file_plan_value, files_in_scope, first_syntax_error, is_identifier_token, parse_kind,
    plan_file, read_source, resolve_seed_selected, write_plans, EditEnvelope, EditSpan, EditSymbol,
    FilePlan, Scope, SeedLookup, ShadowSite,
};
#[cfg(test)]
use super::state::IndexState;
use super::symbol_graph::{NodeId, NodeKind, SymbolGraph};

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
    let symbol_query = require_string(BUILTIN, symbol_dict, "name")?;
    let symbol_name = symbol_query
        .rsplit('.')
        .next()
        .unwrap_or(&symbol_query)
        .to_string();
    let symbol_path = require_string(BUILTIN, symbol_dict, "path")?;
    let symbol_line = super::builtin_args::optional_positive_u32(BUILTIN, symbol_dict, "line")?;
    let symbol_kind_raw = optional_string(BUILTIN, symbol_dict, "kind")?;
    let symbol_col = super::builtin_args::optional_positive_u32(BUILTIN, symbol_dict, "col")?;
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
    let proof = optional_bool(BUILTIN, dict, "proof", false)?;
    let backend = BackendChoice::resolve(
        BUILTIN,
        optional_string(BUILTIN, dict, "backend")?.as_deref(),
    )?;
    let call = HostCallContext::current();
    let call = match super::builtin_args::optional_positive_u64(BUILTIN, dict, "deadline_ms")? {
        Some(ms) => call.with_budget(std::time::Duration::from_millis(ms)),
        None => call,
    };

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

    // A cold language server can take its whole startup budget; start it
    // before taking the index lock so other code-index calls are not held.
    let warmed = (is_rename && backend != BackendChoice::SyntaxTree)
        .then(|| language_server::warm(index, &symbol_path, &call))
        .flatten();

    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Err(HostlibError::Backend {
            builtin: BUILTIN,
            message: "code index has not been initialised — call \
                 `hostlib_code_index_rebuild` first"
                .into(),
        });
    };

    let mut env = ResponseEnv {
        symbol_name: &symbol_name,
        new_name: &new_name,
        symbol_path: &symbol_path,
        symbol_line,
        symbol_kind,
        scope,
        backend: BackendReport::default(),
        stale: Vec::new(),
        proof,
        preserved: None,
        graph: &state.symbols,
    };

    let normalized_path = super::builtins::normalize_relative_path_for(state, &symbol_path);
    let seed = resolve_seed_selected(
        &state.symbols,
        &normalized_path,
        &symbol_query,
        symbol_line,
        symbol_col,
        symbol_kind,
    );
    let seed_node_id = match seed {
        SeedLookup::One(id) => id,
        SeedLookup::None => return Ok(no_match_response(&env)),
        SeedLookup::Many(candidates) => return Ok(ambiguous_response(&env, &candidates)),
    };

    let seed_node = state
        .symbols
        .node(seed_node_id)
        .expect("resolve_seed returned a node id present in the graph");
    let seed_path = seed_node.path.to_string();

    if is_rename && !is_identifier_token(&new_name) {
        return Ok(invalid_identifier_response(
            &env,
            "must start with a letter or underscore and consist of identifier characters",
        ));
    }

    // Constructors are type-renaming seeds, not independent method renames.
    let java_constructor = if seed_node.language == "java"
        && seed_node.kind == NodeKind::Function
        && state.symbols.nodes_named(&seed_node.name).iter().any(|id| {
            state
                .symbols
                .node(*id)
                .is_some_and(|node| node.kind == NodeKind::Type && node.path == seed_node.path)
        }) {
        let source = read_source(BUILTIN, &state.root, &seed_path, session_id.as_deref())?;
        let tree = match ast_api::parse_tree(&source, Language::Java) {
            Ok(tree) => tree,
            Err(err) => {
                return Ok(syntax_error_response(
                    &env,
                    format!("`{seed_path}` failed to parse: {err}"),
                ))
            }
        };
        if tree.root_node().has_error() {
            return Ok(syntax_error_response(
                &env,
                format!("`{seed_path}` does not parse before the edit"),
            ));
        }
        super::rename_java::is_constructor(&tree, &source, seed_node)
    } else {
        false
    };
    if seed_node.language == "java" && (seed_node.kind == NodeKind::Type || java_constructor) {
        return Ok(emit_response(
            &env,
            "unsupported_language",
            ResponseExtras {
                details: "Java type and constructor renames are not supported: references and public-type file moves cannot yet be proven; no files changed".into(),
                fallback_suggestion: Some(TEXT_PATCH_FALLBACK.to_string()),
                ..Default::default()
            },
        ));
    }

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

    // Replace mode rewrites arbitrary text, which a language server's rename
    // cannot express, so it always uses the syntax-tree engine.
    if is_rename && backend != BackendChoice::SyntaxTree {
        let request = language_server::Request {
            state,
            seed: seed_node,
            in_scope_files: &in_scope_files,
            session_id: session_id.as_deref(),
            dry_run,
            validate,
            warmed,
            call: &call,
        };
        match language_server::rename(&mut env, request)? {
            language_server::Answer::Response(response) => return Ok(response),
            language_server::Answer::Interrupted(interrupt) => {
                return Ok(interrupted_response(&env, interrupt, true))
            }
            language_server::Answer::Fallback(reason)
                if backend == BackendChoice::LanguageServer =>
            {
                return Ok(emit_response(
                    &env,
                    "backend_unavailable",
                    ResponseExtras {
                        details: format!("the language server could not answer: {reason}"),
                        ..Default::default()
                    },
                ));
            }
            language_server::Answer::Fallback(reason) => {
                env.backend.fallback_reason = Some(reason);
                // The syntax-tree engine reports its own result.
                env.stale.clear();
            }
        }
    }
    env.backend.syntax_tree = true;

    // The identifier rewrite below would otherwise silently rename both
    // definitions and their unrelated uses.
    let competing =
        competing_declarations(&state.symbols, seed_node_id, &symbol_name, &in_scope_files);
    let java_method = seed_node.language == "java"
        && seed_node.kind == NodeKind::Function
        && seed_node.owner_type.is_some();
    if !java_method && !competing.is_empty() {
        return Ok(ambiguous_response_with_details(
            &env,
            "rename aborted without writes: the scope contains separate declarations with this name. Pinning the seed does not disambiguate their references; use a binding-aware LSP rename or narrow the scope.",
        ));
    }

    let mut plans: Vec<FilePlan> = Vec::new();
    let mut shadows: Vec<ShadowSite> = Vec::new();

    if java_method {
        if let Err(detail) = super::rename_java::check_new_name(&new_name) {
            return Ok(ambiguous_response_with_details(&env, &detail));
        }
    }

    for path in &in_scope_files {
        let Some(language) = Language::detect(Path::new(path), None) else {
            return Ok(unsupported_language_response(&env, path, None));
        };
        // Prose and data files only mention the name; they hold no binding.
        if !language.is_code() {
            env.stale.push(format!(
                "{path} ({} file left unchanged; it may still reference `{symbol_name}`, so review it)",
                language.name()
            ));
            continue;
        }
        if java_method && language != Language::Java {
            return Ok(unsupported_language_response(
                &env,
                path,
                Some(language.name()),
            ));
        }
        let source = read_source(BUILTIN, &state.root, path, session_id.as_deref())?;
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

        if java_method && language == Language::Java {
            match super::rename_java::sites(
                state,
                seed_node,
                path,
                &source,
                &tree,
                targets,
                local_shadows,
            ) {
                Ok(sites) => {
                    targets = sites.targets;
                    local_shadows = sites.shadows;
                }
                Err(detail) => {
                    return Ok(ambiguous_response_with_details(
                        &env,
                        &format!("rename aborted without writes: {detail}"),
                    ))
                }
            }
        }

        if !local_shadows.is_empty() {
            shadows.extend(local_shadows);
            continue;
        }

        if targets.is_empty() {
            if path == &seed_path {
                return Ok(ambiguous_response_with_details(
                    &env,
                    "rename aborted without writes: the indexed declaration is absent from the current source; rebuild the code index",
                ));
            }
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

        if !java_method {
            if let Err(detail) = super::rename_binding::check(
                super::rename_binding::Seed {
                    state,
                    node: seed_node,
                    name: &symbol_name,
                },
                path,
                language,
                &source,
                &tree,
                &targets,
            ) {
                return Ok(ambiguous_response_with_details(
                    &env,
                    &format!(
                        "rename aborted without writes: {detail}; use a binding-aware LSP rename"
                    ),
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

    let check_go_package = is_rename
        && seed_node.language == "go"
        && matches!(seed_node.kind, NodeKind::Function | NodeKind::Type);
    let go_proof = if check_go_package {
        match super::rename_go::inspect(&state.root, &seed_path, &new_name, session_id.as_deref()) {
            Ok((package_shadows, _)) if !package_shadows.is_empty() => {
                return Ok(conflict_response(&env, &package_shadows));
            }
            Ok((_, proof)) => Some(proof),
            Err(detail) => {
                return Ok(emit_response(
                    &env,
                    "refused",
                    ResponseExtras {
                        details: format!("Go package could not be inspected; rename aborted without writes: {detail}"),
                        ..Default::default()
                    },
                ));
            }
        }
    } else {
        None
    };

    // Pre-flight done. With `dry_run` we stop here without persisting.
    let failed = if dry_run {
        Vec::new()
    } else {
        match commit(
            &env,
            &call,
            &state.root,
            &plans,
            session_id.as_deref(),
            go_proof.as_ref(),
        )? {
            Ok(failed) => failed,
            Err(stopped) => return Ok(stopped),
        }
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
    /// Which engine produced the result; reported on every response.
    backend: BackendReport,
    /// Mentions an applied rename left as they were, reported in `warnings`.
    stale: Vec<String>,
    proof: bool,
    preserved: Option<Vec<crate::edit_proof::PreservedSite>>,
    graph: &'a SymbolGraph,
}

/// Per-variant overrides for the response envelope. Anything left as
/// the default lands as the empty list / zero / etc. so each call site
/// only mentions the fields it actually carries data for.
#[derive(Default)]
struct ResponseExtras {
    ambiguity_reason: Option<AmbiguityReason>,
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
    proof: Option<VmValue>,
    /// Variant-specific fields beside the shared envelope.
    extra: Vec<(&'static str, VmValue)>,
}

#[derive(Clone, Copy)]
enum AmbiguityReason {
    MultipleDefinitionSeeds,
    UnresolvedReferenceBinding,
}

impl AmbiguityReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::MultipleDefinitionSeeds => "multiple_definition_seeds",
            Self::UnresolvedReferenceBinding => "unresolved_reference_binding",
        }
    }
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
            extra: [
                ("backend", env.backend.value()),
                (
                    "ambiguity_reason",
                    extras
                        .ambiguity_reason
                        .map_or(VmValue::Nil, |reason| str_value(reason.as_str())),
                ),
            ]
            .into_iter()
            .chain(extras.extra)
            .collect(),
            proof: extras.proof,
        },
    )
}

/// Write the plans past the call's commit point: a stop before it writes
/// nothing, and once it passes every file is written. The plans were read
/// without the write lease, so a file another builtin changed since then
/// refuses the whole rename.
fn commit(
    env: &ResponseEnv<'_>,
    call: &HostCallContext,
    root: &Path,
    plans: &[FilePlan],
    session_id: Option<&str>,
    go_proof: Option<&super::rename_go::PackageProof>,
) -> Result<Result<Vec<(String, String)>, VmValue>, HostlibError> {
    let committed = call.commit(BUILTIN, |writes| {
        if go_proof.is_some_and(|proof| !proof.unchanged(root, session_id)) {
            return Ok(Err(emit_response(
                env,
                "refused",
                ResponseExtras {
                    details: "rename stopped without writes: the Go package changed after the rename read it; run it again".into(),
                    ..Default::default()
                },
            )));
        }
        let changed =
            super::refactor_core::changed_since_planned(BUILTIN, root, plans, session_id);
        if changed.is_empty() {
            return write_plans(BUILTIN, root, plans, session_id, writes).map(Ok);
        }
        Ok(Err(emit_response(
            env,
            "refused",
            ResponseExtras {
                details: format!(
                    "rename stopped without writes: {} changed after the rename read them (listed in `warnings`); run it again",
                    changed.join(", ")
                ),
                warnings: changed.into_iter().map(str_value).collect(),
                ..Default::default()
            },
        )))
    });
    let waited_on_server = env.backend.language_server.is_some();
    committed
        .unwrap_or_else(|interrupt| Ok(Err(interrupted_response(env, interrupt, waited_on_server))))
}

/// The caller stopped the rename before it wrote anything.
/// `waited_on_server` is whether a language server was what the call was
/// waiting on, which makes the server-free request a useful alternative.
fn interrupted_response(
    env: &ResponseEnv<'_>,
    interrupt: Interrupt,
    waited_on_server: bool,
) -> VmValue {
    if interrupt == Interrupt::Cancelled {
        return emit_response(
            env,
            "cancelled",
            ResponseExtras {
                details: "rename stopped without writes: the call was cancelled".into(),
                ..Default::default()
            },
        );
    }
    if !waited_on_server {
        return emit_response(
            env,
            "refused",
            ResponseExtras {
                details: "rename stopped without writes: the call's deadline passed".into(),
                ..Default::default()
            },
        );
    }
    let run = env.backend.language_server.as_ref();
    let retry_after = super::lsp::retry_after(run);
    emit_response(
        env,
        "refused",
        ResponseExtras {
            details: format!(
                "rename stopped without writes: the call's deadline passed while waiting on {}, which keeps starting in the background. Retry after `retry_after_ms`, or send `alternative`, which never waits on a language server.",
                run.map_or("the language server", |run| run.server)
            ),
            extra: vec![
                (
                    "retry_after_ms",
                    VmValue::Int(retry_after.as_millis() as i64),
                ),
                (
                    "alternative",
                    build_dict([("backend", str_value("syntax_tree"))]),
                ),
            ],
            ..Default::default()
        },
    )
}

fn applied_response(
    env: &ResponseEnv<'_>,
    plans: &[FilePlan],
    dry_run: bool,
    failed: Vec<(String, String)>,
) -> VmValue {
    let proof = (env.proof && failed.is_empty()).then(|| {
        let mut proof = super::refactor_core::edit_proof(plans, &failed);
        if let Some(sites) = &env.preserved {
            proof.preserved(
                sites
                    .iter()
                    .cloned()
                    .map(|mut site| {
                        if let Some(plan) = plans.iter().find(|plan| plan.path == site.path) {
                            site.text = plan
                                .patched
                                .lines()
                                .nth(site.line.saturating_sub(1))
                                .unwrap_or("")
                                .to_owned();
                        }
                        site
                    })
                    .collect(),
            );
        }
        proof.value()
    });
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
            warnings: env.stale.iter().map(str_value).collect(),
            proof,
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

fn ambiguous_response(env: &ResponseEnv<'_>, candidates: &[NodeId]) -> VmValue {
    ambiguous_response_with_reason(
        env,
        candidates,
        AmbiguityReason::MultipleDefinitionSeeds,
        "multiple symbols share `symbol_ref.name`; pass `symbol_ref.line` (and optionally `symbol_ref.kind`) to disambiguate. Candidates surfaced in the `warnings` field.",
    )
}

fn ambiguous_response_with_details(env: &ResponseEnv<'_>, details: &str) -> VmValue {
    let candidates: Vec<_> = env
        .graph
        .nodes_named(env.symbol_name)
        .iter()
        .filter_map(|id| env.graph.node(*id))
        .filter(|node| super::refactor_core::is_refactor_declaration(node))
        .map(|node| node.id)
        .collect();
    ambiguous_response_with_reason(
        env,
        &candidates,
        AmbiguityReason::UnresolvedReferenceBinding,
        details,
    )
}

fn ambiguous_response_with_reason(
    env: &ResponseEnv<'_>,
    candidates: &[NodeId],
    reason: AmbiguityReason,
    details: &str,
) -> VmValue {
    let candidate_list = candidates
        .iter()
        .filter_map(|id| env.graph.node(*id))
        .map(|node| {
            build_dict([
                ("symbol", str_value(node.qualified_name())),
                ("qualified_name", str_value(node.qualified_name())),
                ("path", str_value(node.path.as_ref())),
                ("line", VmValue::Int(node.line as i64)),
                (
                    "col",
                    node.col
                        .map_or(VmValue::Nil, |col| VmValue::Int(col as i64)),
                ),
                ("kind", str_value(node.kind.as_str())),
            ])
        })
        .collect();
    emit_response(
        env,
        "ambiguous_symbol",
        ResponseExtras {
            warnings: candidate_list,
            ambiguity_reason: Some(reason),
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

#[path = "rename_lsp.rs"]
mod language_server;

#[cfg(test)]
#[path = "rename_tests.rs"]
mod tests;
