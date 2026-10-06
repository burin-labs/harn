//! `code_index.move_symbol`: move a top-level declaration to another module
//! and rewrite every import and qualified use of it.
//!
//! # Wire shape
//!
//! See `schemas/code_index/move_symbol.{request,response}.json`. The request
//! is `{symbol, path, to_path, line?, kind?, dry_run?, session_id?}`
//! (aliases: `name`, `file`, `target_file`, `destination`). The response is
//! refactor_core's shared edit envelope (`scope`, `conflicts`, `match_count`,
//! `warnings`, ...) plus move fields, so one parser reads every refactor.
//! Tags, all-or-nothing:
//!
//! - `applied`: every planned file re-parsed cleanly. With `dry_run` nothing
//!   was written; otherwise `applied` is false only when a disk write failed
//!   after pre-flight (see `failed_paths_with_reasons`).
//! - `no_match`, `ambiguous_symbol`: the seed did not resolve to one
//!   top-level declaration in `path`; candidates are in `warnings`.
//! - `unsupported_language`: the source or destination is not Rust,
//!   TypeScript/JavaScript, or Python, or they differ.
//! - `syntax_error`: a file the move would rewrite fails refactor_core's
//!   `first_syntax_error` before planning (files that only mention the name
//!   are not checked), or a planned file fails it after the splice.
//! - `destination_conflict`: the destination already binds the name, or a
//!   name the moved item uses, to something else.
//! - `visibility_required`: a private (Rust) or unexported (TS/JS) item would
//!   become unreachable: the moved item has users outside it, or it uses a
//!   private item, field, or method of the source module.
//! - `import_cycle`: the move would add a module-level import between the
//!   source and destination Python modules that closes a cycle.
//! - `error`: anything else that stops the move; `details` says why.
//!
//! Every refusal leaves every file byte-identical.
//!
//! # Algorithm
//!
//! 1. Resolve the seed through the symbol graph and find its top-level item.
//! 2. Take the item with its attributes or decorators and the comment block
//!    directly above it (no blank line between). A comment block separated by
//!    a blank line stays behind and is reported in `comments_left_behind`.
//! 3. Delete the item from the source and append it to the destination. A
//!    missing destination is created; for Rust the parent module also gains a
//!    `mod` declaration, so the new module is reachable.
//! 4. Import at the destination every name the item uses that the source
//!    module declares or imports.
//! 5. In every file that references the name (refactor_core's
//!    `reference_sites`, never REFS alone), rewrite imports from the source
//!    (splitting grouped and named lists), merge into an existing destination
//!    import when one exists, and rewrite qualified paths. Glob imports and
//!    re-exports of the source gain an explicit destination binding.
//! 6. Re-import the item into the source when the source still uses it, and
//!    drop source imports only the moved item used.
//! 7. Re-parse every planned file, then write them in one pass.
//!
//! Matching is name-based within each file, like `rename_symbol`, but a
//! reference is rewritten only when its import or qualifier resolves to the
//! source module. Module paths are Rust paths from the crate root (the Cargo
//! package name outside `src/`), relative specifiers for TS/JS, and dotted
//! package paths for Python.

mod model;
mod python;
mod response;
mod rust;
mod script;
mod text;

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::path::Path;

use harn_vm::VmValue;
use tree_sitter::Tree;

use crate::ast::{api as ast_api, Language};
use crate::error::HostlibError;
use crate::tools::args::{dict_arg, optional_bool, optional_string};

use super::builtins::SharedIndex;
use super::refactor_core::{
    first_syntax_error, parse_kind, plan_file, read_source, reference_sites, resolve_seed,
    write_plans, ReferenceKind, ReferenceSite, SeedCandidate, SeedLookup,
};
use super::state::IndexState;
use super::symbol_graph::NodeKind;
pub(crate) use model::{
    Binding, Family, FileModel, ImportRequest, Lang, ListStatement, ModuleRef, QualifiedRewrite,
    Shape, TopItem,
};
use response::{refuse, refuse_with_sites, respond, Outcome, Response, Site};
use text::Edit;

pub(super) const BUILTIN: &str = "hostlib_code_index_move_symbol";

// === Request ===

pub(crate) struct Request {
    pub(crate) symbol: String,
    pub(crate) path: String,
    pub(crate) to_path: String,
    pub(crate) line: Option<u32>,
    pub(crate) kind: Option<NodeKind>,
    pub(crate) dry_run: bool,
    pub(crate) session_id: Option<String>,
}

fn first_string(
    dict: &harn_vm::value::DictMap,
    keys: &[&'static str],
) -> Result<Option<String>, HostlibError> {
    for key in keys {
        if let Some(value) = optional_string(BUILTIN, dict, key)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

impl Request {
    fn parse(args: &[VmValue]) -> Result<Self, HostlibError> {
        let raw = dict_arg(BUILTIN, args)?;
        let dict = raw.as_ref();
        let symbol =
            first_string(dict, &["symbol", "name"])?.ok_or(HostlibError::MissingParameter {
                builtin: BUILTIN,
                param: "symbol",
            })?;
        let path =
            first_string(dict, &["path", "file"])?.ok_or(HostlibError::MissingParameter {
                builtin: BUILTIN,
                param: "path",
            })?;
        let to_path = first_string(dict, &["to_path", "target_file", "destination"])?.ok_or(
            HostlibError::MissingParameter {
                builtin: BUILTIN,
                param: "to_path",
            },
        )?;
        let line = match dict.get("line") {
            None | Some(VmValue::Nil) => None,
            Some(VmValue::Int(n)) if *n >= 1 => Some(*n as u32),
            Some(other) => {
                return Err(HostlibError::InvalidParameter {
                    builtin: BUILTIN,
                    param: "line",
                    message: format!("expected an integer >= 1, got {other:?}"),
                })
            }
        };
        let kind = optional_string(BUILTIN, dict, "kind")?
            .map(|raw| parse_kind(BUILTIN, &raw))
            .transpose()?;
        Ok(Self {
            symbol,
            path,
            to_path,
            line,
            kind,
            dry_run: optional_bool(BUILTIN, dict, "dry_run", false)?,
            session_id: optional_string(BUILTIN, dict, "session_id")?,
        })
    }
}

// === Entry point ===

pub(super) fn run(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let request = Request::parse(args)?;
    let mut guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_mut() else {
        return Err(HostlibError::Backend {
            builtin: BUILTIN,
            message: "code index has not been initialised — call \
                 `hostlib_code_index_rebuild` first"
                .into(),
        });
    };
    let source_path = super::builtins::normalize_relative_path_for(state, &request.path);
    let dest_path = super::builtins::normalize_relative_path_for(state, &request.to_path);
    // Every read and write joins these onto the index root, so a path that
    // escapes it (`../x.py`, an absolute path elsewhere, a symlink out of the
    // tree) must not get there.
    for (param, path) in [("path", &source_path), ("to_path", &dest_path)] {
        if !resolves_inside(&state.root, path) {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param,
                message: format!("`{path}` is outside the indexed workspace"),
            });
        }
    }
    if source_path == dest_path {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "to_path",
            message: "to_path must differ from path".into(),
        });
    }
    let mut outcome = plan(state, &request, &source_path, &dest_path)?;
    // Referencing files and a Rust parent module are written too.
    if let Some(escaping) = outcome
        .response
        .plans
        .iter()
        .find(|plan| !resolves_inside(&state.root, &plan.path))
    {
        outcome = refuse(
            "error",
            format!(
                "`{}` resolves outside the indexed workspace; nothing was written",
                escaping.path
            ),
        );
    }
    if outcome.tag == "applied" {
        if request.dry_run {
            outcome.response.details = "dry run: no files were written".into();
        } else {
            let failed = write_plans(
                BUILTIN,
                &state.root,
                &outcome.response.plans,
                request.session_id.as_deref(),
            );
            if request.session_id.is_none() {
                for plan in &outcome.response.plans {
                    if !failed.iter().any(|(path, _)| path == &plan.path) {
                        state.reindex_file(&state.root.join(&plan.path));
                    }
                }
            }
            if !failed.is_empty() {
                outcome.response.details =
                    "some files could not be written; see failed_paths_with_reasons".into();
                outcome.response.failed = failed;
            }
        }
    }
    Ok(respond(&request, &source_path, &dest_path, outcome))
}

/// Whether `rel`, joined onto `root`, stays inside `root` once symlinks
/// resolve: no `..` or absolute component, and the deepest existing ancestor
/// (or the file, or a dangling link at it) canonicalizes under `root`.
fn resolves_inside(root: &Path, rel: &str) -> bool {
    let lexical = Path::new(rel).components().all(|component| {
        matches!(
            component,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    });
    if !lexical {
        return false;
    }
    let Ok(canonical_root) = root.canonicalize() else {
        return false;
    };
    let mut probe = root.join(rel);
    loop {
        if std::fs::symlink_metadata(&probe).is_ok() {
            return probe
                .canonicalize()
                .is_ok_and(|resolved| resolved.starts_with(&canonical_root));
        }
        if !probe.pop() {
            return false;
        }
    }
}

// === Planning ===

/// Per-file working state.
struct FileWork {
    path: String,
    language: Language,
    source: String,
    tree: Tree,
    model: FileModel,
    existed: bool,
    edits: Vec<Edit>,
    imports: Vec<ImportRequest>,
}

fn read_optional(
    root: &Path,
    path: &str,
    session_id: Option<&str>,
) -> Result<Option<String>, HostlibError> {
    let abs = root.join(path);
    if let Some(result) = crate::fs::read(&abs, session_id) {
        return match result {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(_) if !abs.exists() => Ok(None),
            Err(err) => Err(HostlibError::Backend {
                builtin: BUILTIN,
                message: format!("read `{}`: {err}", abs.display()),
            }),
        };
    }
    if !abs.exists() {
        return Ok(None);
    }
    read_source(BUILTIN, &abs, session_id).map(Some)
}

fn parse(source: &str, language: Language) -> Result<Tree, HostlibError> {
    ast_api::parse_tree(source, language)
}

fn plan(
    state: &IndexState,
    request: &Request,
    source_path: &str,
    dest_path: &str,
) -> Result<Outcome, HostlibError> {
    let session = request.session_id.as_deref();
    let name = request.symbol.as_str();

    let Some(language) = Language::detect(Path::new(source_path), None) else {
        return Ok(refuse(
            "unsupported_language",
            format!("no grammar for `{source_path}`"),
        ));
    };
    let Some(family) = Family::of(language) else {
        return Ok(refuse(
            "unsupported_language",
            format!(
                "move_symbol supports Rust, TypeScript/JavaScript, and Python; `{source_path}` is {}",
                language.name()
            ),
        ));
    };
    let dest_language = Language::detect(Path::new(dest_path), None);
    if dest_language.and_then(Family::of) != Some(family) {
        return Ok(refuse(
            "unsupported_language",
            format!(
                "destination `{dest_path}` is not a {} module like `{source_path}`",
                language.name()
            ),
        ));
    }
    let dest_language = dest_language.expect("checked above");

    // 1. Seed.
    let mut lookup = resolve_seed(
        &state.symbols,
        source_path,
        name,
        request.line,
        request.kind,
    );
    if let SeedLookup::Many(candidates) = &lookup {
        // Fields and enum cases never move, so a struct field that shares
        // the item's name does not make the item ambiguous.
        let movable: Vec<&SeedCandidate> = candidates
            .iter()
            .filter(|(path, _, kind)| path == source_path && !matches!(*kind, "Field" | "EnumCase"))
            .collect();
        if let [(_, line, kind)] = movable.as_slice() {
            let narrowed = resolve_seed(
                &state.symbols,
                source_path,
                name,
                Some(*line),
                parse_kind(BUILTIN, kind).ok(),
            );
            if matches!(narrowed, SeedLookup::One(_)) {
                lookup = narrowed;
            }
        }
    }
    let seed = match lookup {
        SeedLookup::One(id) => id,
        SeedLookup::None => {
            return Ok(refuse(
                "no_match",
                format!("no declaration named `{name}` in `{source_path}` resolved against the symbol graph"),
            ))
        }
        SeedLookup::Many(candidates) => {
            let mut outcome = refuse(
                "ambiguous_symbol",
                format!("several declarations are named `{name}`; pass `line` or `kind`"),
            );
            outcome.response.candidates = candidates;
            return Ok(outcome);
        }
    };
    let seed_node = state.symbols.node(seed).expect("resolved seed exists");
    if seed_node.path != source_path {
        return Ok(refuse(
            "no_match",
            format!(
                "`{name}` resolves to `{}`, not `{source_path}`",
                seed_node.path
            ),
        ));
    }
    let seed_line = seed_node.line;

    let source_text = read_source(BUILTIN, &state.root.join(source_path), session)?;
    if let Some(detail) = first_syntax_error(&source_text, language) {
        return Ok(refuse(
            "syntax_error",
            format!("`{source_path}` does not parse before the edit ({detail}); fix it first"),
        ));
    }
    let dest_text = read_optional(&state.root, dest_path, session)?;
    if let Some(text) = &dest_text {
        if let Some(detail) = first_syntax_error(text, dest_language) {
            return Ok(refuse(
                "syntax_error",
                format!("`{dest_path}` does not parse before the edit ({detail}); fix it first"),
            ));
        }
    }

    let mut lang = Lang::new(family, state, &[dest_path]);
    let Some(source_module) = lang.module_of(source_path) else {
        return Ok(refuse(
            "error",
            format!("`{source_path}` is not inside a library module tree this move can address"),
        ));
    };
    let Some(dest_module) = lang.module_of(dest_path) else {
        return Ok(refuse(
            "error",
            format!("`{dest_path}` is not inside the same library module tree as `{source_path}`"),
        ));
    };
    if let (ModuleRef::Rust { crate_dir: a, .. }, ModuleRef::Rust { crate_dir: b, .. }) =
        (&source_module, &dest_module)
    {
        if a != b {
            return Ok(refuse(
                "error",
                format!("`{source_path}` and `{dest_path}` are in different crates"),
            ));
        }
    }

    let mut works: BTreeMap<String, FileWork> = BTreeMap::new();
    let source_tree = parse(&source_text, language)?;
    let source_model = lang.model(source_path, &source_tree, &source_text);
    works.insert(
        source_path.to_string(),
        FileWork {
            path: source_path.to_string(),
            language,
            model: source_model,
            source: source_text,
            tree: source_tree,
            existed: true,
            edits: Vec::new(),
            imports: Vec::new(),
        },
    );
    let dest_existed = dest_text.is_some();
    let dest_source = dest_text.unwrap_or_default();
    let dest_tree = parse(&dest_source, dest_language)?;
    let dest_model = lang.model(dest_path, &dest_tree, &dest_source);
    works.insert(
        dest_path.to_string(),
        FileWork {
            path: dest_path.to_string(),
            language: dest_language,
            model: dest_model,
            source: dest_source,
            tree: dest_tree,
            existed: dest_existed,
            edits: Vec::new(),
            imports: Vec::new(),
        },
    );

    // 2. The item and its attached comments.
    // An owned view of the source, so the item's tree nodes outlive the
    // mutable passes over `works`. Trees clone by reference count.
    struct SourceView {
        source: String,
        tree: Tree,
        model: FileModel,
    }
    let src = SourceView {
        source: works[source_path].source.clone(),
        tree: works[source_path].tree.clone(),
        model: works[source_path].model.clone(),
    };
    let Some(item) = src
        .model
        .items
        .iter()
        .filter(|item| item.name == name)
        .find(|item| {
            let first = text::line_of(&src.source, item.range.start);
            let last = text::line_of(&src.source, item.range.end.saturating_sub(1));
            (first..=last).contains(&seed_line)
        })
        .or_else(|| src.model.item(name))
        .cloned()
    else {
        return Ok(refuse(
            "error",
            format!("`{name}` is not a top-level declaration in `{source_path}`; only top-level items move"),
        ));
    };
    if !item.movable {
        return Ok(refuse(
            "error",
            format!("`{name}` is a top-level declaration this move does not relocate (only functions, types, and constants move)"),
        ));
    }
    let (moved, left_behind) = moved_range(&src.source, &src.tree, &item, family);
    let mut response = Response {
        moved_lines: Some((
            text::line_of(&src.source, moved.start),
            text::line_of(&src.source, moved.end.saturating_sub(1)),
        )),
        comments_left_behind: left_behind
            .into_iter()
            .map(|range| {
                let mut site = Site::at(
                    source_path,
                    &src.source,
                    range.start,
                    "comment",
                    "separated from the item by a blank line, so it stays in the source",
                );
                site.text = text::slice(&src.source, range).trim_end().to_string();
                site
            })
            .collect(),
        ..Default::default()
    };
    let item_node = src
        .tree
        .root_node()
        .descendant_for_byte_range(item.range.start, item.range.end)
        .expect("item range is inside the tree");

    // 3. Destination conflicts.
    let dest = &works[dest_path];
    if let Some(existing) = dest.model.item(name) {
        return Ok(refuse_with_sites(
            "destination_conflict",
            format!("`{dest_path}` already declares `{name}`"),
            vec![Site::at(
                dest_path,
                &dest.source,
                existing.range.start,
                "declaration",
                "same name in the destination",
            )],
        ));
    }
    if let Some(binding) = dest.model.binding_of(name) {
        if !(binding.target == source_module && binding.imported == name) {
            return Ok(refuse_with_sites(
                "destination_conflict",
                format!("`{dest_path}` already imports a different `{name}`"),
                vec![Site::at(
                    dest_path,
                    &dest.source,
                    binding.statement.start,
                    "import",
                    "binds the same name",
                )],
            ));
        }
    }

    // 4. Names the item needs at the destination.
    let (free, members) = lang.free_names(item_node, &src.source, name);
    let mut blocked: Vec<Site> = Vec::new();
    let mut dest_needs: Vec<ImportRequest> = Vec::new();
    let mut unresolved = false;
    for free_name in &free {
        let request = if let Some(decl) = src.model.item(free_name) {
            if family != Family::Python && !decl.exported {
                blocked.push(Site::at(
                    source_path,
                    &src.source,
                    decl.range.start,
                    "declaration",
                    format!("`{free_name}` is private to the source but the moved item uses it"),
                ));
                continue;
            }
            let mut request = ImportRequest::named(source_module.clone(), free_name);
            request.type_only = decl.type_like;
            request
        } else if let Some(binding) = src.model.binding_of(free_name).filter(|b| b.top_level) {
            if binding.target == dest_module && binding.shape == Shape::Named {
                // The destination declares it; no import needed.
                continue;
            }
            ImportRequest {
                target: binding.target.clone(),
                shape: binding.shape,
                imported: binding.imported.clone(),
                local: binding.local.clone(),
                type_only: binding.type_only,
                reexport: false,
                prefix: String::new(),
                anchor: None,
            }
        } else {
            unresolved = true;
            continue;
        };
        if let Some(existing) = dest.model.item(free_name) {
            return Ok(refuse_with_sites(
                "destination_conflict",
                format!(
                    "the moved item uses `{free_name}`, which `{dest_path}` declares differently"
                ),
                vec![Site::at(
                    dest_path,
                    &dest.source,
                    existing.range.start,
                    "declaration",
                    "same name in the destination",
                )],
            ));
        }
        if let Some(binding) = dest.model.binding_of(free_name) {
            if request.satisfied_by(binding)
                || (binding.target == request.target && binding.imported == request.imported)
            {
                continue;
            }
            return Ok(refuse_with_sites(
                "destination_conflict",
                format!(
                    "the moved item uses `{free_name}`, which `{dest_path}` imports from elsewhere"
                ),
                vec![Site::at(
                    dest_path,
                    &dest.source,
                    binding.statement.start,
                    "import",
                    "binds the same name",
                )],
            ));
        }
        dest_needs.push(request);
    }
    if unresolved {
        // Names not declared or explicitly imported may come from a glob.
        for binding in src
            .model
            .bindings
            .iter()
            .filter(|b| b.top_level && b.shape == Shape::Glob && !b.reexport)
        {
            if binding.target != dest_module {
                dest_needs.push(ImportRequest {
                    target: binding.target.clone(),
                    shape: Shape::Glob,
                    imported: String::new(),
                    local: String::new(),
                    type_only: false,
                    reexport: false,
                    prefix: String::new(),
                    anchor: None,
                });
            }
        }
    }
    if let Lang::Rust(ws) = &lang {
        blocked.extend(ws.private_members_used(source_path, &src.source, &src.tree, &members));
    }
    if !blocked.is_empty() {
        return Ok(refuse_with_sites(
            "visibility_required",
            format!(
                "the moved `{name}` uses items the source keeps private; make them public first"
            ),
            blocked,
        ));
    }

    // 5. Every referencing file.
    let references = reference_sites(BUILTIN, state, seed, session)?;
    for skipped in &references.skipped {
        response
            .warnings
            .push(format!("{}: {}", skipped.path, skipped.reason));
    }
    let mut by_file: BTreeMap<String, Vec<ReferenceSite>> = BTreeMap::new();
    for site in references.sites {
        by_file.entry(site.path.clone()).or_default().push(site);
    }
    for path in by_file.keys() {
        if works.contains_key(path) {
            continue;
        }
        let Some(file_language) = Language::detect(Path::new(path), None) else {
            continue;
        };
        if Family::of(file_language) != Some(family) {
            continue;
        }
        let file_source = read_source(BUILTIN, &state.root.join(path), session)?;
        let tree = parse(&file_source, file_language)?;
        let model = lang.model(path, &tree, &file_source);
        works.insert(
            path.clone(),
            FileWork {
                path: path.clone(),
                language: file_language,
                source: file_source,
                tree,
                model,
                existed: true,
                edits: Vec::new(),
                imports: Vec::new(),
            },
        );
    }

    let mut outside_users: Vec<Site> = Vec::new();
    let paths: Vec<String> = works.keys().cloned().collect();
    for path in &paths {
        let work = works.get_mut(path).expect("listed");
        let role = if path == source_path {
            Role::Source
        } else if path == dest_path {
            Role::Dest
        } else {
            Role::Other
        };
        let sites = by_file.get(path).map(Vec::as_slice).unwrap_or(&[]);
        let moved_here = if role == Role::Source {
            Some(moved.clone())
        } else {
            None
        };
        let counts = rewrite_file(
            &mut lang,
            work,
            role,
            sites,
            name,
            &source_module,
            &dest_module,
            moved_here,
            item.type_like,
            &mut outside_users,
        )
        .map_err(|message| HostlibError::Backend {
            builtin: BUILTIN,
            message,
        });
        let (occurrences, calls) = match counts {
            Ok(counts) => counts,
            Err(err) => return Ok(refuse("error", err.to_string())),
        };
        response.occurrences_replaced += occurrences;
        response.call_sites_updated += calls;
    }

    if family != Family::Python && !item.exported && !outside_users.is_empty() {
        return Ok(refuse_with_sites(
            "visibility_required",
            format!("`{name}` is private but is used outside the moved item; make it public first"),
            outside_users,
        ));
    }

    // 6. Source: delete the item, drop imports only it used, re-import if
    // still used.
    {
        let src = works.get_mut(source_path).expect("source work");
        src.edits.push(Edit::replace(moved.clone(), ""));
        let remaining = remaining_names(&src.source, &src.tree, &src.model, &moved, language);
        let protect_reexports = family == Family::Python && source_path.ends_with("__init__.py");
        if !protect_reexports {
            for binding in &src.model.bindings {
                if binding.top_level
                    && binding.shape != Shape::Glob
                    && !binding.reexport
                    && free.contains(&binding.local)
                    && !remaining.contains(&binding.local)
                {
                    src.edits
                        .push(Edit::replace(removal_range(&src.source, binding), ""));
                }
            }
        }
    }

    // 7. Destination: the item, its imports, and a module declaration.
    let moved_text = {
        // The deletion may absorb blank lines above the item; they don't move.
        let body = text::slice(&src.source, moved.clone());
        let lead = body.len() - body.trim_start_matches(['\n', '\r']).len();
        let text_start = moved.start + lead;
        let mut text = text::slice(&src.source, text_start..moved.end).to_string();
        if let Lang::Rust(ws) = &mut lang {
            let rewrites = ws.relative_paths_in(source_path, dest_path, &src.source, item_node);
            let mut rewrites = rewrites.map_err(|message| HostlibError::Backend {
                builtin: BUILTIN,
                message,
            })?;
            rewrites.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
            for (range, replacement) in rewrites {
                text.replace_range(
                    range.start - text_start..range.end - text_start,
                    &replacement,
                );
            }
        }
        format!("{}\n", text.trim_end())
    };
    {
        let dest = works.get_mut(dest_path).expect("dest work");
        let at = dest.source.len();
        let separator = text::separator_before(&dest.source, at, family.item_gap());
        dest.edits
            .push(Edit::insert(at, format!("{separator}{moved_text}"), 2));
        dest.imports.extend(dest_needs.iter().cloned());
    }
    if !dest_existed {
        if let Lang::Rust(ws) = &mut lang {
            match ws.module_declaration(dest_path, item.exported) {
                Ok((parent, line)) => {
                    if !works.contains_key(&parent) {
                        let parent_source =
                            read_source(BUILTIN, &state.root.join(&parent), session)?;
                        let tree = parse(&parent_source, Language::Rust)?;
                        let model = lang.model(&parent, &tree, &parent_source);
                        works.insert(
                            parent.clone(),
                            FileWork {
                                path: parent.clone(),
                                language: Language::Rust,
                                source: parent_source,
                                tree,
                                model,
                                existed: true,
                                edits: Vec::new(),
                                imports: Vec::new(),
                            },
                        );
                    }
                    let parent_work = works.get_mut(&parent).expect("parent work");
                    parent_work.edits.push(rust::module_declaration_edit(
                        &parent_work.source,
                        &parent_work.tree,
                        &line,
                    ));
                }
                Err(message) => return Ok(refuse("error", message)),
            }
        }
    }

    // Python: refuse a new module-level cycle between source and destination.
    if family == Family::Python {
        if let Some(sites) =
            python_cycle(&works, source_path, dest_path, &source_module, &dest_module)
        {
            return Ok(refuse_with_sites(
                "import_cycle",
                format!(
                    "`{source_path}` and `{dest_path}` would import each other at module level"
                ),
                sites,
            ));
        }
    }

    // 8. Render imports, splice, and re-parse every file.
    for path in &paths_with_parent(&works) {
        let work = works.get_mut(path).expect("listed");
        if let Err(message) = resolve_imports(&mut lang, work) {
            return Ok(refuse("error", message));
        }
    }
    // Every file the move rewrites must parse before it is planned; a file
    // that only mentions the name, with no edit, cannot block the move.
    for work in works.values().filter(|work| !work.edits.is_empty()) {
        if let Some(detail) = first_syntax_error(&work.source, work.language) {
            return Ok(refuse(
                "syntax_error",
                format!(
                    "`{}` does not parse before the edit ({detail}); fix it first",
                    work.path
                ),
            ));
        }
    }
    for work in works.values_mut() {
        if work.edits.is_empty() {
            continue;
        }
        let edits = match text::finalize(&work.source, std::mem::take(&mut work.edits)) {
            Ok(edits) => edits,
            Err(message) => {
                return Ok(refuse(
                    "error",
                    format!("could not plan `{}`: {message}", work.path),
                ))
            }
        };
        match plan_file(
            work.path.clone(),
            work.language,
            work.source.clone(),
            edits,
            true,
        ) {
            Ok(plan) => {
                if !work.existed {
                    response.created.push(plan.path.clone());
                }
                response.plans.push(plan);
            }
            Err(detail) => {
                return Ok(refuse(
                    "syntax_error",
                    format!("rewriting `{}` produced syntax errors: {detail}", work.path),
                ))
            }
        }
    }
    response.details = format!(
        "moved `{name}` from `{source_path}` to `{dest_path}`; {} file(s) changed",
        response.plans.len()
    );
    Ok(Outcome {
        tag: "applied",
        response,
    })
}

fn paths_with_parent(works: &BTreeMap<String, FileWork>) -> Vec<String> {
    works.keys().cloned().collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Source,
    Dest,
    Other,
}

/// The source range that moves: whole lines from the first attached comment
/// or attribute through the item. Also returns the detached comment block
/// directly above it, which stays.
///
/// Lines are walked upward and classified through the tree, because
/// grammars disagree on where a comment between items hangs (Python nests
/// a column-0 comment inside the previous function's block).
fn moved_range(
    source: &str,
    tree: &Tree,
    item: &TopItem,
    family: Family,
) -> (Range<usize>, Vec<Range<usize>>) {
    let root = tree.root_node();
    let attached_above = |line_begin: usize, comments_only: bool| -> Option<usize> {
        if line_begin == 0 {
            return None;
        }
        let prev = text::line_start(source, line_begin - 1);
        let line = text::slice(source, prev..line_begin);
        if line.trim().is_empty() {
            return None;
        }
        let first = prev + (line.len() - line.trim_start().len());
        let mut node = root.descendant_for_byte_range(first, first + 1)?;
        loop {
            let kind = node.kind();
            let is_comment = kind.contains("comment");
            let is_attribute = !comments_only && family == Family::Rust && kind == "attribute_item";
            if is_comment || is_attribute {
                let starts_line = text::slice(
                    source,
                    text::line_start(source, node.start_byte())..node.start_byte(),
                )
                .trim()
                .is_empty();
                return (node.end_byte() <= line_begin && starts_line)
                    .then(|| text::line_start(source, node.start_byte()));
            }
            node = node.parent()?;
        }
    };
    let mut start = text::line_start(source, item.range.start);
    while let Some(above) = attached_above(start, false) {
        start = above;
    }
    // Skip blank lines, then collect the comment block above them.
    let mut detached = Vec::new();
    let mut probe = start;
    while probe > 0 {
        let prev = text::line_start(source, probe - 1);
        if text::slice(source, prev..probe).trim().is_empty() {
            probe = prev;
        } else {
            break;
        }
    }
    if probe < start {
        let block_end = probe;
        let mut block_start = probe;
        while let Some(above) = attached_above(block_start, true) {
            block_start = above;
        }
        if block_start < block_end {
            detached.push(block_start..block_end);
        }
    }
    let mut range = text::whole_lines(source, start..item.range.end);
    if range.start != start {
        // Something shares the item's first or last line; move bytes only.
        range = start..item.range.end;
    }
    // Absorb the blank lines before the item when blank lines follow it (or
    // the file ends), so no double gap remains.
    let after = text::slice(source, range.end..);
    if after.trim().is_empty() || after.starts_with('\n') || after.starts_with("\r\n") {
        let mut begin = range.start;
        while begin > 0 {
            let prev = text::line_start(source, begin - 1);
            if text::slice(source, prev..begin).trim().is_empty() {
                begin = prev;
            } else {
                break;
            }
        }
        range.start = begin;
    }
    // At the top of the file, take the blank lines after the item instead.
    if text::slice(source, ..range.start).trim().is_empty() {
        while range.end < source.len() {
            let next = text::next_line_start(source, range.end);
            if text::slice(source, range.end..next).trim().is_empty() {
                range.end = next;
            } else {
                break;
            }
        }
    }
    (range, detached)
}

fn removal_range(source: &str, binding: &Binding) -> Range<usize> {
    if binding.sole {
        text::whole_lines(source, binding.statement.clone())
    } else {
        binding.removal.clone()
    }
}

/// Identifier names used in `source` outside `excluded` and outside imports.
fn remaining_names(
    source: &str,
    tree: &Tree,
    model: &FileModel,
    excluded: &Range<usize>,
    language: Language,
) -> HashSet<String> {
    let kinds = language.rename_identifier_kinds().unwrap_or(&[]);
    let mut names = HashSet::new();
    text::walk(tree.root_node(), |node| {
        if excluded.contains(&node.start_byte()) || model.in_import(node.start_byte()) {
            return false;
        }
        if kinds.contains(&node.kind()) {
            names.insert(text::text(node, source).to_string());
        }
        true
    });
    names
}

/// Rewrite imports and qualified uses of the moved name in one file.
/// Returns `(occurrences, call sites)` rewritten.
#[allow(clippy::too_many_arguments)]
fn rewrite_file(
    lang: &mut Lang,
    work: &mut FileWork,
    role: Role,
    sites: &[ReferenceSite],
    name: &str,
    from: &ModuleRef,
    to: &ModuleRef,
    moved: Option<Range<usize>>,
    type_like: bool,
    outside_users: &mut Vec<Site>,
) -> Result<(usize, usize), String> {
    let mut occurrences = 0;
    let mut calls = 0;
    let inside_moved = |at: usize| moved.as_ref().is_some_and(|r| r.contains(&at));

    // Imports of the moved name from the source module.
    let mut explicitly_bound = false;
    let mut glob: Option<Binding> = None;
    for binding in &work.model.bindings {
        if inside_moved(binding.statement.start) {
            continue;
        }
        if binding.shape == Shape::Glob && &binding.target == from {
            glob = Some(binding.clone());
            continue;
        }
        if binding.shape != Shape::Named || binding.imported != name || &binding.target != from {
            continue;
        }
        explicitly_bound = true;
        occurrences += 1;
        outside_users.push(Site::at(
            &work.path,
            &work.source,
            binding.statement.start,
            "import",
            "imports the moved item",
        ));
        if role == Role::Dest {
            work.edits
                .push(Edit::replace(removal_range(&work.source, binding), ""));
            continue;
        }
        let mut request = ImportRequest {
            target: to.clone(),
            shape: Shape::Named,
            imported: name.to_string(),
            local: binding.local.clone(),
            type_only: binding.type_only,
            reexport: binding.reexport,
            prefix: binding.prefix.clone(),
            anchor: None,
        };
        if binding.sole {
            request.anchor = Some((binding.statement.start, binding.statement.end));
        } else {
            work.edits.push(Edit::replace(binding.removal.clone(), ""));
        }
        work.imports.push(request);
    }

    // Qualified uses whose qualifier resolves to the source module, and
    // unqualified uses that still need the name.
    let mut unqualified = false;
    for site in sites {
        if inside_moved(site.span.start_byte) || site.kind == ReferenceKind::Import {
            continue;
        }
        if work.model.in_import(site.span.start_byte) {
            continue;
        }
        let root = work.tree.root_node();
        let Some(node) = root.descendant_for_byte_range(site.span.start_byte, site.span.end_byte)
        else {
            continue;
        };
        match lang.qualified(
            &work.path,
            &work.source,
            &work.model,
            node,
            from,
            to,
            role == Role::Dest,
        )? {
            Some(rewrite) => {
                occurrences += 1;
                // Inside a Rust macro the call is unparsed: `f` then `(...)`.
                let macro_call = node.next_sibling().is_some_and(|next| {
                    next.kind() == "token_tree" && text::text(next, &work.source).starts_with('(')
                });
                if macro_call
                    || matches!(
                        site.kind,
                        ReferenceKind::Call | ReferenceKind::QualifiedCall
                    )
                {
                    calls += 1;
                }
                outside_users.push(Site::at(
                    &work.path,
                    &work.source,
                    site.span.start_byte,
                    "reference",
                    "qualified use of the moved item",
                ));
                work.edits.push(rewrite.edit);
                work.imports.extend(rewrite.import);
            }
            None => {
                if site.qualifier.is_none() && site.kind != ReferenceKind::MethodCall {
                    unqualified = true;
                    if role == Role::Source {
                        outside_users.push(Site::at(
                            &work.path,
                            &work.source,
                            site.span.start_byte,
                            "reference",
                            "the source still uses the moved item",
                        ));
                    }
                }
            }
        }
    }

    match role {
        Role::Source if unqualified && !explicitly_bound => {
            work.imports.push(ImportRequest::named(to.clone(), name));
        }
        Role::Other if !explicitly_bound => {
            if let Some(glob) = glob {
                if glob.reexport || unqualified {
                    occurrences += 1;
                    outside_users.push(Site::at(
                        &work.path,
                        &work.source,
                        glob.statement.start,
                        "import",
                        "glob import of the source module",
                    ));
                    work.imports.push(ImportRequest {
                        target: to.clone(),
                        shape: Shape::Named,
                        imported: name.to_string(),
                        local: name.to_string(),
                        type_only: false,
                        reexport: glob.reexport,
                        prefix: glob.prefix,
                        anchor: None,
                    });
                }
            }
        }
        _ => {}
    }
    if type_like {
        for request in &mut work.imports {
            if &request.target == to && request.imported == name && request.shape == Shape::Named {
                request.type_only = true;
            }
        }
    }
    Ok((occurrences, calls))
}

/// Turn a file's import requests into edits: skip satisfied ones, merge into
/// an existing list, replace a retargeted statement in place, or insert a
/// block after the file's imports.
fn resolve_imports(lang: &mut Lang, work: &mut FileWork) -> Result<(), String> {
    let mut requests = std::mem::take(&mut work.imports);
    requests.sort();
    requests.dedup();
    let mut block: Vec<String> = Vec::new();
    let mut merged: HashSet<(usize, String)> = HashSet::new();
    for request in requests {
        let anchor = request.anchor.map(|(s, e)| s..e);
        let satisfied =
            work.model.bindings.iter().any(|b| {
                request.satisfied_by(b) && anchor.as_ref().is_none_or(|a| *a != b.statement)
            });
        if satisfied {
            if let Some(anchor) = anchor {
                work.edits
                    .push(Edit::replace(text::whole_lines(&work.source, anchor), ""));
            }
            continue;
        }
        let list = (request.shape == Shape::Named)
            .then(|| {
                work.model.lists.iter().find(|list| {
                    list.target == request.target
                        && list.reexport == request.reexport
                        && list.prefix == request.prefix
                        && (!list.type_only || request.type_only)
                        && anchor.as_ref().is_none_or(|a| !a.contains(&list.append_at))
                })
            })
            .flatten()
            .cloned();
        if let Some(list) = list {
            let text = lang.merge_text(&list, &request);
            if merged.insert((list.append_at, text.clone())) {
                work.edits.push(Edit::insert(list.append_at, text, 1));
            }
            if let Some(anchor) = anchor {
                work.edits
                    .push(Edit::replace(text::whole_lines(&work.source, anchor), ""));
            }
            continue;
        }
        let rendered = lang.render(&work.path, &work.source, &request)?;
        match anchor {
            Some(anchor) => work.edits.push(Edit::replace(anchor, rendered)),
            None => block.push(rendered),
        }
    }
    if block.is_empty() {
        return Ok(());
    }
    let at = work.model.insert_at;
    let mut inserted = String::new();
    if !work.model.has_imports && at > 0 && !text::slice(&work.source, ..at).ends_with("\n\n") {
        inserted.push('\n');
    }
    inserted.push_str(&block.join("\n"));
    inserted.push('\n');
    if !work.model.has_imports {
        // The first imports in a file: keep one gap (two for Python) before
        // whatever follows.
        let after = text::slice(&work.source, at..);
        if after.trim().is_empty() {
            inserted.push_str(&"\n".repeat(lang.insert_separator()));
        } else if !(after.starts_with('\n') || after.starts_with("\r\n")) {
            inserted.push('\n');
        }
    }
    work.edits.push(Edit::insert(at, inserted, 0));
    Ok(())
}

/// A new module-level import edge between source and destination that,
/// with the reverse edge, forms a cycle.
fn python_cycle(
    works: &BTreeMap<String, FileWork>,
    source_path: &str,
    dest_path: &str,
    source_module: &ModuleRef,
    dest_module: &ModuleRef,
) -> Option<Vec<Site>> {
    let edge = |path: &str, target: &ModuleRef| -> (bool, bool, Option<Site>) {
        let work = &works[path];
        let existing = work
            .model
            .bindings
            .iter()
            .find(|b| b.top_level && python::binding_module(b).as_ref() == Some(target));
        let added = work.imports.iter().any(|r| &r.target == target);
        let site = existing.map(|b| {
            Site::at(
                path,
                &work.source,
                b.statement.start,
                "import",
                "module-level import",
            )
        });
        (existing.is_some(), added, site)
    };
    let (dest_has, dest_added, dest_site) = edge(dest_path, source_module);
    let (src_has, src_added, src_site) = edge(source_path, dest_module);
    let forward = dest_has || dest_added;
    let backward = src_has || src_added;
    if forward && backward && (dest_added || src_added) {
        let mut sites: Vec<Site> = dest_site.into_iter().chain(src_site).collect();
        if dest_added {
            sites.push(Site {
                path: dest_path.to_string(),
                line: 1,
                kind: "import",
                reason: "the moved item needs names from the source module".into(),
                text: String::new(),
            });
        }
        if src_added {
            sites.push(Site {
                path: source_path.to_string(),
                line: 1,
                kind: "import",
                reason: "the source still uses the moved item".into(),
                text: String::new(),
            });
        }
        return Some(sites);
    }
    None
}

#[cfg(test)]
#[path = "move_symbol_tests.rs"]
mod tests;
