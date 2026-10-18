//! `rename_symbol` through the language-server backend.
//!
//! The server owns binding resolution, so the syntax engine's binding proof
//! and competing-declaration refusals do not apply here. What stays is what
//! the server does not check: the new name shadowing an existing identifier,
//! the requested scope, and that every rewritten file still parses.

use std::path::Path;

use harn_vm::VmValue;

use crate::ast::{api as ast_api, Language};
use crate::error::HostlibError;

use super::super::lsp::{self, RenameRequest};
use super::super::refactor_backend::BackendOutcome;
use super::super::refactor_core::{
    collect_identifier_spans, first_syntax_error, plan_file, read_source, write_plans, FilePlan,
    Scope, ShadowSite,
};
use super::super::rename_macro_bodies::Unresolved;
use super::super::state::IndexState;
use super::super::symbol_graph::Node as SymbolNode;
use super::*;

pub(super) enum Answer {
    Response(VmValue),
    /// The server could not answer; carries why, for the response's
    /// `backend.fallback_reason`.
    Fallback(String),
}

pub(super) struct Request<'a> {
    pub state: &'a IndexState,
    pub seed: &'a SymbolNode,
    pub in_scope_files: &'a [String],
    pub session_id: Option<&'a str>,
    pub dry_run: bool,
    pub validate: bool,
}

pub(super) fn rename(
    env: &mut ResponseEnv<'_>,
    request: Request<'_>,
) -> Result<Answer, HostlibError> {
    let seed_path = request.seed.path.as_ref();
    let Some(language) = Language::detect(Path::new(seed_path), None) else {
        return Ok(Answer::Fallback(
            "the declaration's language is unknown".into(),
        ));
    };
    if !lsp::serves(language) {
        return Ok(Answer::Fallback(format!(
            "no language server is registered for {}",
            language.name()
        )));
    }
    let source = read_source(BUILTIN, &request.state.root, seed_path, request.session_id)?;
    let Some(offset) = declaration_offset(&source, language, env.symbol_name, request.seed.line)
    else {
        return Ok(Answer::Fallback(format!(
            "no `{}` identifier on line {}",
            env.symbol_name, request.seed.line
        )));
    };
    let (outcome, run) = lsp::rename(&RenameRequest {
        index_root: &request.state.root,
        path: seed_path,
        language,
        source: &source,
        offset,
        old_name: env.symbol_name,
        new_name: env.new_name,
        session_id: request.session_id,
    });
    env.backend.language_server = run;
    let mut changeset = match outcome {
        BackendOutcome::Changeset(changeset) => changeset,
        BackendOutcome::Declined(details) => {
            return Ok(Answer::Response(emit_response(
                env,
                "declined",
                ResponseExtras {
                    details: format!("rename aborted without writes: {details}"),
                    ..Default::default()
                },
            )))
        }
        BackendOutcome::Unavailable(reason) => return Ok(Answer::Fallback(reason)),
    };
    if language == Language::Rust {
        let completed = super::super::rename_macro_bodies::complete(
            BUILTIN,
            request.state,
            seed_path,
            env.symbol_name,
            env.new_name,
            request.in_scope_files,
            request.session_id,
            &mut changeset,
        )?;
        if let Err(Unresolved(at)) = completed {
            return Ok(Answer::Response(emit_response(
                env,
                "declined",
                ResponseExtras {
                    details: format!(
                        "rename aborted without writes: the `macro_rules!` body at {at} mentions `{}` in a form that resolves only where the macro expands, which the language server does not rename",
                        env.symbol_name
                    ),
                    ..Default::default()
                },
            )));
        }
    }
    if changeset.is_empty() {
        return Ok(Answer::Response(no_match_response(env)));
    }

    if env.scope != Scope::Workspace {
        let outside: Vec<VmValue> = changeset
            .iter()
            .filter(|file| !request.in_scope_files.contains(&file.path))
            .map(|file| str_value(&file.path))
            .collect();
        if !outside.is_empty() {
            return Ok(Answer::Response(emit_response(
                env,
                "scope_exceeded",
                ResponseExtras {
                    warnings: outside,
                    details: format!(
                        "references to `{}` exist outside scope `{}` (listed in `warnings`); renaming only inside the scope would break them. Re-run with scope `workspace`.",
                        env.symbol_name,
                        env.scope.as_str()
                    ),
                    ..Default::default()
                },
            )));
        }
    }

    let mut plans: Vec<FilePlan> = Vec::new();
    let mut shadows: Vec<ShadowSite> = Vec::new();
    for file in changeset {
        let Some(file_language) = Language::detect(Path::new(&file.path), None) else {
            return Ok(Answer::Response(unsupported_language_response(
                env, &file.path, None,
            )));
        };
        let tree = match ast_api::parse_tree(&file.source, file_language) {
            Ok(tree) => tree,
            Err(err) => {
                return Ok(Answer::Response(syntax_error_response(
                    env,
                    format!("`{}` failed to parse: {err}", file.path),
                )))
            }
        };
        if let Some(kinds) = file_language.rename_identifier_kinds() {
            let mut ignored = Vec::new();
            collect_identifier_spans(
                tree.root_node(),
                file.source.as_bytes(),
                "",
                env.new_name,
                kinds,
                &file.path,
                &mut ignored,
                &mut shadows,
            );
        }
        if request.validate {
            if let Some(detail) = first_syntax_error(&file.source, file_language) {
                return Ok(Answer::Response(syntax_error_response(
                    env,
                    format!(
                        "`{}` does not parse before the edit ({detail}); fix it first",
                        file.path
                    ),
                )));
            }
        }
        let path = file.path.clone();
        match plan_file(
            file.path,
            file_language,
            file.source,
            file.edits,
            request.validate,
        ) {
            Ok(plan) => plans.push(plan),
            Err(detail) => {
                return Ok(Answer::Response(syntax_error_response(
                    env,
                    format!("rewriting `{path}` produced syntax errors: {detail}"),
                )))
            }
        }
    }
    if !shadows.is_empty() {
        return Ok(Answer::Response(conflict_response(env, &shadows)));
    }

    let failed = if request.dry_run {
        Vec::new()
    } else {
        let failed = write_plans(BUILTIN, &request.state.root, &plans, request.session_id)?;
        if request.session_id.is_none() {
            let written: Vec<String> = plans.iter().map(|plan| plan.path.clone()).collect();
            lsp::files_written(&request.state.root, &written);
        }
        failed
    };
    Ok(Answer::Response(applied_response(
        env,
        &plans,
        request.dry_run,
        failed,
    )))
}

/// Byte offset of the declaration's name: the identifier spelled `name` on
/// the seed's line.
fn declaration_offset(source: &str, language: Language, name: &str, line: u32) -> Option<usize> {
    let kinds = language.rename_identifier_kinds()?;
    let tree = ast_api::parse_tree(source, language).ok()?;
    let mut targets = Vec::new();
    collect_identifier_spans(
        tree.root_node(),
        source.as_bytes(),
        name,
        "",
        kinds,
        "",
        &mut targets,
        &mut Vec::new(),
    );
    targets
        .into_iter()
        .filter(|span| span.start_row + 1 == line as usize)
        .map(|span| span.start_byte)
        .min()
}
