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
use crate::host_call::{HostCallContext, Interrupt};

use super::super::builtins::SharedIndex;
use super::super::lsp::{self, RenameRequest, Resolution};
use super::super::refactor_backend::{BackendOutcome, FileEdits, ServerRun, Unanswered};
use super::super::refactor_core::{
    collect_identifier_spans, files_in_scope, first_syntax_error, plan_file, read_source, EditSpan,
    FilePlan, Scope, ShadowSite,
};
use super::super::rename_reach::{Exclusions, Reach};
use super::super::rename_rust_residue::{doc_link_mentions, span_at, Residue};
use super::super::state::IndexState;
use super::super::symbol_graph::Node as SymbolNode;
use super::*;

pub(super) enum Answer {
    Response(VmValue),
    /// The server could not answer; carries why, for the response's
    /// `backend.fallback_reason`.
    Fallback(String),
    /// The caller stopped the rename; nothing was written and no other
    /// engine may run.
    Interrupted(Interrupt),
}

impl From<Unanswered> for Answer {
    fn from(unanswered: Unanswered) -> Self {
        match unanswered {
            Unanswered::Unavailable(reason) => Self::Fallback(reason),
            Unanswered::Interrupted(interrupt) => Self::Interrupted(interrupt),
        }
    }
}

pub(super) type WarmResult = Result<lsp::Warmed, (Unanswered, Option<ServerRun>)>;

/// Warm the server for `symbol_path` without holding the code-index lock.
/// `None` when no server could serve the file's language.
pub(super) fn warm(
    index: &SharedIndex,
    symbol_path: &str,
    call: &HostCallContext,
) -> Option<WarmResult> {
    let (root, path) = {
        let guard = index.lock().expect("code_index mutex poisoned");
        let state = guard.as_ref()?;
        (
            state.root.clone(),
            super::super::builtins::normalize_relative_path_for(state, symbol_path),
        )
    };
    let language = Language::detect(Path::new(&path), None)?;
    lsp::serves(language).then(|| lsp::warm(&root, &path, language, call))
}

pub(super) struct Request<'a> {
    pub state: &'a IndexState,
    pub seed: &'a SymbolNode,
    pub in_scope_files: &'a [String],
    pub session_id: Option<&'a str>,
    pub dry_run: bool,
    pub validate: bool,
    pub warmed: Option<WarmResult>,
    pub call: &'a HostCallContext,
}

pub(super) fn rename(
    env: &mut ResponseEnv<'_>,
    mut request: Request<'_>,
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
    let Some(col) = request.seed.col else {
        return Ok(Answer::Fallback(
            "the indexed declaration name position is unavailable".into(),
        ));
    };
    let Some(offset) = declaration_offset(
        &source,
        language,
        env.symbol_name,
        request.seed.line,
        col,
        request.seed.container.as_deref(),
    ) else {
        return Ok(Answer::Fallback(format!(
            "the current source has no matching `{}` declaration at {}:{}",
            env.symbol_name, request.seed.line, col
        )));
    };
    // Every workspace file that mentions the name, whatever the scope: the
    // server sees them as Harn does, and the scope check below covers them.
    let mentions = if env.scope == Scope::Workspace {
        request.in_scope_files.to_vec()
    } else {
        files_in_scope(
            request.state,
            Scope::Workspace,
            env.symbol_name,
            seed_path,
            request.session_id,
        )
    };
    let warmed = match request.warmed.take() {
        Some(Ok(warmed)) => warmed,
        Some(Err((unanswered, run))) => {
            env.backend.language_server = run;
            return Ok(unanswered.into());
        }
        None => {
            return Ok(Answer::Fallback(format!(
                "no language server could start for {}",
                language.name()
            )))
        }
    };
    let lsp_request = RenameRequest {
        index_root: &request.state.root,
        path: seed_path,
        language,
        source: &source,
        offset,
        old_name: env.symbol_name,
        new_name: env.new_name,
        session_id: request.session_id,
        mentions: &mentions,
        call: request.call,
    };
    let (outcome, run) = lsp::rename(&lsp_request, &warmed);
    env.backend.language_server = Some(run);
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
        BackendOutcome::Interrupted(interrupt) => return Ok(Answer::Interrupted(interrupt)),
    };
    // The server's edit proves where references are; check the scope before
    // any fallback, which would rename only inside the scope.
    let changed: Vec<&str> = changeset.iter().map(|file| file.path.as_str()).collect();
    if let Some(response) = scope_exceeded(env, request.in_scope_files, &changed) {
        return Ok(Answer::Response(response));
    }
    if language == Language::Rust {
        super::super::rename_rust_residue::complete(
            Residue {
                builtin: BUILTIN,
                state: request.state,
                seed_path,
                name: env.symbol_name,
                new_name: env.new_name,
                files: &mentions,
                session_id: request.session_id,
                module_level: request.seed.container.is_none(),
                is_type: request.seed.kind == NodeKind::Type,
            },
            &mut changeset,
        )?;
    }
    // Files the server edited but the index does not list are checked too.
    let mut checked: Vec<String> = mentions.clone();
    checked.extend(changeset.iter().map(|file| file.path.clone()));
    checked.sort();
    checked.dedup();
    if language == Language::Rust {
        // rust-analyzer resolves intra-doc links but does not rename them;
        // rename the ones it resolves to the declaration.
        let links = doc_link_sites(
            &request,
            env.symbol_name,
            &checked,
            &changeset,
            &mut env.stale,
        )?;
        if !links.is_empty() {
            let queries: Vec<(String, usize)> = links
                .iter()
                .map(|site| (site.path.clone(), site.offset))
                .collect();
            let resolutions = match lsp::resolve(&lsp_request, &warmed, &queries, false) {
                Ok(resolutions) => resolutions,
                Err(unanswered) => return Ok(unanswered.into()),
            };
            let seed_file = request
                .state
                .root
                .join(seed_path)
                .canonicalize()
                .unwrap_or_else(|_| request.state.root.join(seed_path));
            let seed_modules = module_path(&source, language, offset);
            // A second type of the name in the file (one local to a
            // function, say) makes a bare `Name::` ambiguous there.
            let sole_type =
                type_declarations(&source, language, env.symbol_name, &seed_modules) == 1;
            for (site, resolution) in links.iter().zip(resolutions) {
                // The server resolves a link as a whole, so `[`Span::new`]`
                // answers with `new`. The `Span` segment names the
                // declaration when that answer lies in an impl, enum or trait
                // body for the name in the declaration's own file and module:
                // one module cannot hold two types with one name.
                let own_member = site.qualifies
                    && sole_type
                    && site.path == seed_path
                    && matches!(&resolution, Resolution::Elsewhere(Some(target))
                        if target.path == seed_file
                            && body_owner_is(&source, language, target.offset, env.symbol_name)
                            && module_path(&source, language, target.offset) == seed_modules);
                if site.renameable && (resolution == Resolution::Seed || own_member) {
                    add_edit(
                        &request,
                        &mut changeset,
                        site,
                        env.symbol_name,
                        env.new_name,
                    )?;
                } else {
                    env.stale.push(format!(
                        "{}:{} (doc link left unchanged)",
                        site.path, site.line
                    ));
                }
            }
        }
    }
    if changeset.is_empty() {
        return Ok(Answer::Response(no_match_response(env)));
    }
    let changed: Vec<&str> = changeset.iter().map(|file| file.path.as_str()).collect();
    if let Some(response) = scope_exceeded(env, request.in_scope_files, &changed) {
        return Ok(Answer::Response(response));
    }

    // Every mention the edit leaves must be something else, by the server's
    // own resolution. One it cannot resolve sits in code it did not analyze
    // (an unloaded module, an inactive `cfg`, an unexpanded macro), unless a
    // language rule already puts it out of the declaration's reach. The
    // resolve pass has one time budget; past it the rename is refused.
    let reach = Reach::of(
        seed_path,
        language,
        &source,
        offset,
        env.symbol_name,
        request.seed.kind,
    );
    let mut exclusions = Exclusions::new(reach, request.state, BUILTIN, request.session_id);
    let Leftovers { sites, unreachable } = remaining_mentions(
        &request,
        env.symbol_name,
        language,
        &checked,
        &changeset,
        &mut exclusions,
    )?;
    let queries: Vec<(String, usize)> = sites
        .iter()
        .map(|site| (site.path.clone(), site.offset))
        .collect();
    let stop_at_miss = env.scope == Scope::Workspace;
    let resolutions = match lsp::resolve(&lsp_request, &warmed, &queries, stop_at_miss) {
        Ok(resolutions) => resolutions,
        Err(unanswered) => return Ok(unanswered.into()),
    };
    let mut preserved_sites: Vec<(&Site, &str)> = unreachable
        .iter()
        .map(|(site, reason)| (site, *reason))
        .collect();
    let missed: Vec<(&Site, Resolution)> = sites
        .iter()
        .zip(resolutions)
        .filter(|(site, resolution)| {
            let elsewhere = resolution.is_elsewhere();
            if env.proof && elsewhere {
                preserved_sites.push((
                    *site,
                    "language server resolved this name to another binding",
                ));
            }
            !elsewhere
        })
        .collect();
    if let Some((site, resolution)) = missed.first() {
        let missed_paths: Vec<&str> = missed.iter().map(|(site, _)| site.path.as_str()).collect();
        if let Some(response) = scope_exceeded(env, request.in_scope_files, &missed_paths) {
            return Ok(Answer::Response(response));
        }
        let what = match resolution {
            Resolution::Seed => "left a reference to",
            Resolution::Encloses => "could not tell what this refers to:",
            _ => "could not resolve",
        };
        return Ok(Answer::Fallback(format!(
            "the language server {what} `{}` at {}:{}",
            env.symbol_name, site.path, site.line
        )));
    }

    if env.proof && env.scope == Scope::Workspace {
        let mut preserved = Vec::new();
        for (site, reason) in preserved_sites {
            let source = read_source(BUILTIN, &request.state.root, &site.path, request.session_id)?;
            preserved.push(crate::edit_proof::PreservedSite {
                path: site.path.clone(),
                line: site.line,
                text: source
                    .lines()
                    .nth(site.line.saturating_sub(1))
                    .unwrap_or("")
                    .to_owned(),
                reason: reason.into(),
            });
        }
        env.preserved = Some(preserved);
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
        let failed = match commit(
            env,
            request.call,
            &request.state.root,
            &plans,
            request.session_id,
            None,
        )? {
            Ok(failed) => failed,
            Err(stopped) => return Ok(Answer::Response(stopped)),
        };
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

/// A mention of the old name the edit leaves in place.
struct Site {
    path: String,
    offset: usize,
    line: usize,
    /// An unqualified doc-link segment followed by `::`.
    qualifies: bool,
    /// False for a doc link whose comment block also defines a reference
    /// (`[Span]: path`): renaming the label alone would break the link.
    renameable: bool,
}

/// `scope_exceeded` when the scope is narrower than the workspace and
/// `paths` reach outside it.
fn scope_exceeded(env: &ResponseEnv<'_>, in_scope: &[String], paths: &[&str]) -> Option<VmValue> {
    if env.scope == Scope::Workspace {
        return None;
    }
    let mut outside: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|path| !in_scope.iter().any(|kept| kept == path))
        .collect();
    outside.sort_unstable();
    outside.dedup();
    if outside.is_empty() {
        return None;
    }
    Some(emit_response(
        env,
        "scope_exceeded",
        ResponseExtras {
            warnings: outside.into_iter().map(str_value).collect(),
            details: format!(
                "references to `{}` exist outside scope `{}` (listed in `warnings`); renaming only inside the scope would break them. Re-run with scope `workspace`.",
                env.symbol_name,
                env.scope.as_str()
            ),
            ..Default::default()
        },
    ))
}

/// Identifier-position mentions of `name`, outside strings and comments, in
/// files the same server serves, that `changeset` does not rewrite: the ones
/// to resolve, and the ones `exclusions` puts out of reach, with why.
fn remaining_mentions(
    request: &Request<'_>,
    name: &str,
    language: Language,
    mentions: &[String],
    changeset: &[FileEdits],
    exclusions: &mut Exclusions<'_>,
) -> Result<Leftovers, HostlibError> {
    let mut sites = Vec::new();
    let mut unreachable = Vec::new();
    for path in mentions {
        let Some(file_language) = Language::detect(Path::new(path), None) else {
            continue;
        };
        if !lsp::same_server(language, file_language) {
            continue;
        }
        let Some(kinds) = file_language.rename_identifier_kinds() else {
            continue;
        };
        let edited = changeset.iter().find(|file| &file.path == path);
        let source = match edited {
            Some(file) => file.source.clone(),
            None => read_source(BUILTIN, &request.state.root, path, request.session_id)?,
        };
        let Ok(tree) = ast_api::parse_tree(&source, file_language) else {
            continue;
        };
        let mut found = Vec::new();
        // `r#Span` is the same Rust identifier as `Span`.
        let raw = format!("r#{name}");
        let spellings: &[&str] = if file_language == Language::Rust {
            &[name, &raw]
        } else {
            &[name]
        };
        for spelling in spellings {
            collect_identifier_spans(
                tree.root_node(),
                source.as_bytes(),
                spelling,
                "",
                kinds,
                path,
                &mut found,
                &mut Vec::new(),
            );
        }
        for span in found {
            let covered = edited.is_some_and(|file| {
                file.edits
                    .iter()
                    .any(|edit| edit.span.start_byte == span.start_byte)
            });
            if covered {
                continue;
            }
            let site = Site {
                path: path.clone(),
                offset: span.start_byte,
                line: span.start_row + 1,
                qualifies: false,
                renameable: true,
            };
            match exclusions.reason(path, &source, &tree, span.start_byte)? {
                Some(reason) => unreachable.push((site, reason)),
                None => sites.push(site),
            }
        }
    }
    sites.sort_by(|left, right| (&left.path, left.offset).cmp(&(&right.path, right.offset)));
    Ok(Leftovers { sites, unreachable })
}

/// Mentions an edit left: ones the server must resolve, and ones a language
/// rule puts out of the declaration's reach, each with its reason.
struct Leftovers {
    sites: Vec<Site>,
    unreachable: Vec<(Site, &'static str)>,
}

/// Intra-doc-link mentions of `name` in Rust `paths` that `changeset` does
/// not rewrite. Reference-definition lines naming it go to `stale`.
fn doc_link_sites(
    request: &Request<'_>,
    name: &str,
    paths: &[String],
    changeset: &[FileEdits],
    stale: &mut Vec<String>,
) -> Result<Vec<Site>, HostlibError> {
    let mut sites = Vec::new();
    for path in paths {
        if Language::detect(Path::new(path), None) != Some(Language::Rust) {
            continue;
        }
        let edited = changeset.iter().find(|file| &file.path == path);
        let source = match edited {
            Some(file) => file.source.clone(),
            None => read_source(BUILTIN, &request.state.root, path, request.session_id)?,
        };
        let Ok(tree) = ast_api::parse_tree(&source, Language::Rust) else {
            continue;
        };
        let reference_definitions = has_reference_definition(&source, name);
        if reference_definitions {
            // A `[label]: path` target is not a link occurrence; report the
            // definition lines themselves.
            for (index, line) in source.lines().enumerate() {
                if is_reference_definition(line, name) {
                    stale.push(format!("{path}:{} (doc link left unchanged)", index + 1));
                }
            }
        }
        for offset in doc_link_mentions(tree.root_node(), &source, name) {
            let covered = edited
                .is_some_and(|file| file.edits.iter().any(|edit| edit.span.start_byte == offset));
            if !covered {
                let before = source.get(..offset).unwrap_or_default();
                let after = source.get(offset + name.len()..).unwrap_or_default();
                sites.push(Site {
                    path: path.clone(),
                    offset,
                    line: before.matches('\n').count() + 1,
                    qualifies: after.starts_with("::") && !before.ends_with("::"),
                    renameable: !reference_definitions,
                });
            }
        }
    }
    Ok(sites)
}

/// Rename the mention at `site`, reading the file into `changeset` if the
/// server's edit did not touch it.
fn add_edit(
    request: &Request<'_>,
    changeset: &mut Vec<FileEdits>,
    site: &Site,
    name: &str,
    new_name: &str,
) -> Result<(), HostlibError> {
    let at = match changeset.iter().position(|file| file.path == site.path) {
        Some(at) => at,
        None => {
            let source = read_source(BUILTIN, &request.state.root, &site.path, request.session_id)?;
            changeset.push(FileEdits {
                path: site.path.clone(),
                source,
                edits: Vec::new(),
            });
            changeset.len() - 1
        }
    };
    let file = &mut changeset[at];
    let edit = EditSpan {
        span: span_at(&file.source, site.offset, site.offset + name.len()),
        before: name.to_string(),
        after: new_name.to_string(),
    };
    file.edits.push(edit);
    file.edits.sort_by_key(|edit| edit.span.start_byte);
    Ok(())
}

/// Whether a doc comment defines a reference link (`[label]: target`) that
/// mentions `name` in its label or target.
fn has_reference_definition(source: &str, name: &str) -> bool {
    source
        .lines()
        .any(|line| is_reference_definition(line, name))
}

fn is_reference_definition(line: &str, name: &str) -> bool {
    let doc = line.trim_start();
    let body = doc
        .strip_prefix("///")
        .or_else(|| doc.strip_prefix("//!"))
        .unwrap_or_default()
        .trim_start();
    body.starts_with('[') && body.contains("]:") && body.contains(name)
}

/// Whether byte `offset` of `source` lies in an `impl`, `enum` or `trait`
/// body for the type `name`.
fn body_owner_is(source: &str, language: Language, offset: usize, name: &str) -> bool {
    let Ok(tree) = ast_api::parse_tree(source, language) else {
        return false;
    };
    let text = |node: tree_sitter::Node<'_>| node.utf8_text(source.as_bytes()).unwrap_or_default();
    let mut node = tree.root_node().descendant_for_byte_range(offset, offset);
    while let Some(current) = node {
        let owner = match current.kind() {
            "impl_item" => current
                .child_by_field_name("type")
                .map(|ty| ty.child_by_field_name("type").unwrap_or(ty)),
            "enum_item" | "trait_item" => current.child_by_field_name("name"),
            _ => None,
        };
        if let Some(owner) = owner {
            return text(owner) == name;
        }
        node = current.parent();
    }
    false
}

/// How many type-like items (struct, enum, union, trait, alias) named
/// `name` the file declares under the inline-module path `modules`,
/// including ones local to a function there.
fn type_declarations(source: &str, language: Language, name: &str, modules: &[String]) -> usize {
    let Ok(tree) = ast_api::parse_tree(source, language) else {
        return 0;
    };
    let mut count = 0;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let declares = matches!(
            node.kind(),
            "struct_item" | "enum_item" | "union_item" | "trait_item" | "type_item"
        ) && node
            .child_by_field_name("name")
            .is_some_and(|ident| ident.utf8_text(source.as_bytes()) == Ok(name));
        count +=
            usize::from(declares && module_path(source, language, node.start_byte()) == modules);
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    count
}

/// The names of the inline `mod` items enclosing byte `offset`, outermost
/// first.
fn module_path(source: &str, language: Language, offset: usize) -> Vec<String> {
    let Ok(tree) = ast_api::parse_tree(source, language) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut node = tree.root_node().descendant_for_byte_range(offset, offset);
    while let Some(current) = node {
        if current.kind() == "mod_item" {
            if let Some(name) = current.child_by_field_name("name") {
                names.push(
                    name.utf8_text(source.as_bytes())
                        .unwrap_or_default()
                        .to_string(),
                );
            }
        }
        node = current.parent();
    }
    names.reverse();
    names
}

/// Re-identify the selected declaration in current source before asking the
/// server. The index stores a 1-based UTF-16 name position, not a wrapper's
/// start or an arbitrary same-line occurrence.
fn declaration_offset(
    source: &str,
    language: Language,
    name: &str,
    line: u32,
    col: u32,
    container: Option<&str>,
) -> Option<usize> {
    use super::super::source_positions::{Encoding, LineIndex, Position};

    let kinds = language.rename_identifier_kinds()?;
    let offset = LineIndex::new(source).offset(
        Position {
            line: line.checked_sub(1)?,
            character: col.checked_sub(1)?,
        },
        Encoding::Utf16,
    )?;
    let (tree, symbols) = ast_api::parse_with_symbols(source, language).ok()?;
    let identifier = tree
        .root_node()
        .named_descendant_for_byte_range(offset, offset.checked_add(name.len())?)?;
    if identifier.start_byte() != offset
        || !kinds.contains(&identifier.kind())
        || identifier.utf8_text(source.as_bytes()).ok()? != name
    {
        return None;
    }
    let declares = symbols.iter().any(|symbol| {
        if symbol.name != name || symbol.container.as_deref() != container {
            return false;
        }
        let starts_at = |node: tree_sitter::Node<'_>| {
            (symbol.start_row as usize, symbol.start_col as usize)
                == (node.start_position().row, node.start_position().column)
        };
        // Some enum members are name nodes themselves rather than a parent
        // with a `name` field. The symbol extractor owns that distinction.
        if symbol.kind == crate::ast::SymbolKind::EnumCase && starts_at(identifier) {
            return true;
        }
        let Some(parent) = identifier
            .parent()
            .filter(|node| node.child_by_field_name("name") == Some(identifier))
        else {
            return false;
        };
        let declaration = if matches!(parent.kind(), "variable_declarator" | "type_spec") {
            parent
                .parent()
                .filter(|node| {
                    matches!(
                        node.kind(),
                        "lexical_declaration" | "variable_declaration" | "type_declaration"
                    )
                })
                .unwrap_or(parent)
        } else {
            parent
        };
        std::iter::successors(Some(declaration), |node| {
            ast_api::declaration_wrapper(*node)
        })
        .any(starts_at)
    });
    declares.then_some(offset)
}

#[cfg(test)]
mod selector_tests {
    use super::*;

    #[test]
    fn enum_member_lsp_lookup_keeps_its_exact_name_position() {
        let source = "export enum Status { Ready }\n";
        let at = source.find("Ready").unwrap();
        assert_eq!(
            declaration_offset(
                source,
                Language::TypeScript,
                "Ready",
                1,
                at as u32 + 1,
                Some("Status"),
            ),
            Some(at)
        );
        assert_eq!(
            declaration_offset(
                source,
                Language::TypeScript,
                "Ready",
                1,
                at as u32,
                Some("Status")
            ),
            None
        );
    }

    #[test]
    fn exported_declarations_keep_exact_identifier_selectors() {
        for (declaration, names) in [
            ("export class Span {}", &["Span"][..]),
            ("export interface Span {}", &["Span"][..]),
            ("export type Span = number;", &["Span"][..]),
            ("export function Span() {}", &["Span"][..]),
            ("export const Span = () => 1;", &["Span"][..]),
            (
                "export const left = () => 1, right = () => 2;",
                &["left", "right"][..],
            ),
        ] {
            let source = format!("/* 🦀 */ {declaration}\n");
            for name in names {
                let at = source.find(name).unwrap();
                let col = source.get(..at).unwrap().encode_utf16().count() as u32 + 1;
                assert_ne!(col, at as u32 + 1);
                assert_eq!(
                    declaration_offset(&source, Language::TypeScript, name, 1, col, None),
                    Some(at),
                    "{declaration}: {name}",
                );
                for wrong in [col + 1, at as u32 + 1, 0] {
                    assert_eq!(
                        declaration_offset(&source, Language::TypeScript, name, 1, wrong, None),
                        None,
                    );
                }
                assert_eq!(
                    declaration_offset(&source, Language::TypeScript, name, 1, col, Some("Other")),
                    None,
                );
            }
        }
    }

    #[test]
    fn decorated_python_keeps_the_inner_symbol_identifier_selector() {
        let source = "@audit(\n    1,\n)\ndef Span():\n    return 1\n";
        assert_eq!(
            declaration_offset(source, Language::Python, "Span", 4, 5, None),
            source.find("Span"),
        );
    }

    #[test]
    fn exact_column_keeps_same_line_method_selectors_on_the_selected_declaration() {
        let source = "/* 🦀 */ class First { load() {} } class Second { load() {} }";
        let second = source.rfind("load").unwrap();
        let col = source.get(..second).unwrap().encode_utf16().count() as u32 + 1;
        assert_ne!(col, second as u32 + 1);
        assert_eq!(
            declaration_offset(source, Language::TypeScript, "load", 1, col, Some("Second")),
            Some(second)
        );
        for (wrong_col, owner) in [
            (col - 1, "Second"),
            (col, "First"),
            (second as u32 + 1, "Second"),
        ] {
            assert_eq!(
                declaration_offset(
                    source,
                    Language::TypeScript,
                    "load",
                    1,
                    wrong_col,
                    Some(owner)
                ),
                None
            );
        }
        let usage = "const load = other.load;";
        assert_eq!(
            declaration_offset(
                usage,
                Language::TypeScript,
                "load",
                1,
                usage.rfind("load").unwrap() as u32 + 1,
                None
            ),
            None
        );
    }
}
