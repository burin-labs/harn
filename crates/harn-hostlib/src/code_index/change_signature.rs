//! `code_index.change_signature` — change a function's parameter list and
//! rewrite every call site across the workspace in one all-or-nothing edit.
//!
//! # Wire shape
//!
//! See `schemas/code_index/change_signature.{request,response}.json`. The
//! request names the function (`symbol_ref`, as for `rename_symbol`) and the
//! complete new parameter list. Each entry keeps an existing parameter (by
//! its own name, or by `from` when it is renamed) or adds one, which then
//! needs a `call_value` written at every call or a `default`. Parameters the
//! list leaves out are removed.
//!
//! The response is the shared code_index edit envelope
//! ([`super::refactor_core::edit_envelope`], the `rename_symbol` shape):
//! `applied` (with `dry_run`, or `applied: false` plus
//! `failed_paths_with_reasons` after a failed write) | `no_match` |
//! `ambiguous_symbol` (candidates in `warnings`) | `conflict` |
//! `unsupported_language` | `syntax_error` | `error`, plus this operation's
//! refusals `parameter_in_use` | `value_reference` | `unsupported_call_site` |
//! `overrides_present` and its `call_sites_updated`, `sites`, and parameter
//! lists. A request that does not fit the declaration raises an invalid
//! parameter error. Every refusal leaves every file byte-identical.
//!
//! # Algorithm
//!
//! 1. Resolve the seed through the symbol graph and refuse when another
//!    declaration shares the name: a method's namesakes are overrides, and a
//!    free function's make the name-based call sites ambiguous.
//! 2. Read the declaration's parameters and plan the new list.
//! 3. Refuse when the body still uses a removed parameter. A renamed
//!    parameter's body uses are renamed with it.
//! 4. Classify every reference through [`reference_sites`]: calls are
//!    rewritten, imports and type positions are left alone, and any value
//!    use (`.map(f)`) refuses the change.
//! 5. Remap each call's arguments: positional arguments by index, Python
//!    keyword arguments by name, a `Type::m(value, ..)` receiver kept first.
//!    Splats, spreads and arity mismatches refuse the change.
//! 6. Splice every file in memory, re-parse it, and write only after every
//!    file passed.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use harn_vm::VmValue;
use tree_sitter::Node;

use crate::ast::{api as ast_api, Language, TEXT_PATCH_FALLBACK};
use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_string, require_string, str_value,
};

use super::builtins::SharedIndex;
use super::refactor_core::{
    candidates_value, competing_declarations, edit_envelope, failed_paths_value, file_plan_value,
    files_in_scope, first_syntax_error, is_identifier_token, parse_kind, plan_file, read_source,
    reference_sites, resolve_seed, write_plans, EditEnvelope, EditSpan, EditSymbol, FilePlan,
    IdentifierSpan, ReferenceKind, Scope, SeedCandidate, SeedLookup,
};
use super::signature_syntax::{
    body_uses, find_declaration, read_call_arguments, read_declaration, read_macro_call,
    render_param, slice, Argument, Declaration, Family, ListLayout, MacroCallShape, ParamShape,
    Receiver, UseKind,
};
use super::state::IndexState;
use super::symbol_graph::NodeKind;

pub(super) const BUILTIN: &str = "hostlib_code_index_change_signature";

/// One entry of the requested parameter list.
#[derive(Clone, Debug)]
struct ParamSpec {
    name: String,
    from: Option<String>,
    type_text: Option<String>,
    default_text: Option<String>,
    call_value: Option<String>,
}

struct Request {
    name: String,
    path: String,
    line: Option<u32>,
    kind: Option<NodeKind>,
    params: Vec<ParamSpec>,
    dry_run: bool,
    session_id: Option<String>,
}

pub(super) fn run(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let request = parse_request(args)?;
    let guard = index.lock().expect("code_index mutex poisoned");
    let Some(state) = guard.as_ref() else {
        return Err(HostlibError::Backend {
            builtin: BUILTIN,
            message: "code index has not been initialised — call \
                 `hostlib_code_index_rebuild` first"
                .into(),
        });
    };
    let mut parameters_before = Vec::new();
    Ok(match plan(state, &request, &mut parameters_before)? {
        Ok(planned) => finish(state, &request, planned, parameters_before),
        Err(refusal) => refusal.into_value(&request, parameters_before),
    })
}

fn parse_request(args: &[VmValue]) -> Result<Request, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();
    let symbol_ref = match dict.get("symbol_ref") {
        Some(VmValue::Dict(d)) => d.clone(),
        Some(other) => {
            return Err(invalid(
                "symbol_ref",
                format!("expected dict, got {}", other.type_name()),
            ));
        }
        None => {
            return Err(HostlibError::MissingParameter {
                builtin: BUILTIN,
                param: "symbol_ref",
            });
        }
    };
    let symbol = symbol_ref.as_ref();
    let line = match symbol.get("line") {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::Int(n)) if *n >= 1 => Some(*n as u32),
        Some(other) => {
            return Err(invalid(
                "symbol_ref.line",
                format!("expected an integer >= 1, got {other:?}"),
            ));
        }
    };
    let kind = optional_string(BUILTIN, symbol, "kind")?
        .map(|raw| parse_kind(BUILTIN, &raw))
        .transpose()?;
    let params = match dict.get("params") {
        Some(VmValue::List(list)) => list
            .iter()
            .map(parse_param)
            .collect::<Result<Vec<_>, _>>()?,
        Some(other) => {
            return Err(invalid(
                "params",
                format!("expected list, got {}", other.type_name()),
            ));
        }
        None => {
            return Err(HostlibError::MissingParameter {
                builtin: BUILTIN,
                param: "params",
            });
        }
    };
    Ok(Request {
        name: require_string(BUILTIN, symbol, "name")?,
        path: require_string(BUILTIN, symbol, "path")?,
        line,
        kind,
        params,
        dry_run: optional_bool(BUILTIN, dict, "dry_run", false)?,
        session_id: optional_string(BUILTIN, dict, "session_id")?,
    })
}

fn parse_param(value: &VmValue) -> Result<ParamSpec, HostlibError> {
    let VmValue::Dict(dict) = value else {
        return Err(invalid(
            "params",
            format!("every entry must be a dict, got {}", value.type_name()),
        ));
    };
    let dict = dict.as_ref();
    let text = |key: &'static str| -> Result<Option<String>, HostlibError> {
        Ok(optional_string(BUILTIN, dict, key)?.filter(|s| !s.trim().is_empty()))
    };
    let name = require_string(BUILTIN, dict, "name")?;
    if !is_identifier_token(&name) {
        return Err(invalid("params", format!("`{name}` is not an identifier")));
    }
    Ok(ParamSpec {
        name,
        from: text("from")?,
        type_text: text("type")?,
        default_text: text("default")?,
        call_value: text("call_value")?,
    })
}

fn invalid(param: &'static str, message: String) -> HostlibError {
    HostlibError::InvalidParameter {
        builtin: BUILTIN,
        param,
        message,
    }
}

// === Planning ===

/// A source location reported back to the caller.
#[derive(Clone, Debug)]
struct Site {
    path: String,
    line: usize,
    kind: &'static str,
    reason: String,
    text: String,
}

/// A refusal: the response tag plus what the caller needs to act on it.
struct Refusal {
    tag: &'static str,
    details: String,
    sites: Vec<Site>,
    candidates: Vec<SeedCandidate>,
    conflicts: Vec<VmValue>,
}

impl Refusal {
    fn new(tag: &'static str, details: impl Into<String>) -> Self {
        Self {
            tag,
            details: details.into(),
            sites: Vec::new(),
            candidates: Vec::new(),
            conflicts: Vec::new(),
        }
    }

    fn with_sites(mut self, sites: Vec<Site>) -> Self {
        self.sites = sites;
        self
    }

    fn into_value(self, request: &Request, parameters_before: Vec<(String, String)>) -> VmValue {
        // `ambiguous_symbol` carries its candidates in `warnings`; a refusal
        // over same-named declarations or calls lists those as candidates.
        let warnings = if self.tag == "ambiguous_symbol" {
            let mut candidates = self.candidates;
            candidates.extend(
                self.sites
                    .iter()
                    .map(|site| (site.path.clone(), site.line as u32, site.kind)),
            );
            candidates_value(&candidates)
        } else {
            Vec::new()
        };
        respond(
            request,
            self.tag,
            EditEnvelope {
                conflicts: self.conflicts,
                warnings,
                details: self.details,
                fallback_suggestion: (self.tag == "unsupported_language")
                    .then(|| TEXT_PATCH_FALLBACK.to_string()),
                ..EditEnvelope::default()
            },
            SignatureFields {
                sites: self.sites,
                parameters_before,
                ..SignatureFields::default()
            },
        )
    }
}

/// Where one new parameter comes from.
#[derive(Clone, Debug)]
struct NewParam {
    spec: ParamSpec,
    /// Index into the declaration's current parameters.
    source: Option<usize>,
    shape: ParamShape,
}

/// One pending replacement in a file. Call edits are rendered after every
/// edit nested inside their arguments, so `f(f(a))` and a renamed parameter
/// passed to a recursive call compose.
struct PendingEdit {
    range: Range<usize>,
    start: (usize, usize),
    end: (usize, usize),
    content: EditContent,
}

enum EditContent {
    Literal(String),
    Arguments {
        layout: ListLayout,
        items: Vec<ArgItem>,
    },
}

enum ArgItem {
    Text(String),
    Source(Range<usize>),
    Keyword(String, Range<usize>),
}

struct Planned {
    plans: Vec<FilePlan>,
    call_sites: Vec<Site>,
    parameters_after: Vec<String>,
}

type Outcome = Result<Result<Planned, Refusal>, HostlibError>;

/// Plan the change. Once the declaration is read, `parameters_before`
/// holds its `(name, text)` parameters so refusals report them too.
fn plan(
    state: &IndexState,
    request: &Request,
    parameters_before: &mut Vec<(String, String)>,
) -> Outcome {
    let session = request.session_id.as_deref();
    let normalized = super::builtins::normalize_relative_path_for(state, &request.path);
    let kind = request.kind.or(Some(NodeKind::Function));
    let seed = match resolve_seed(
        &state.symbols,
        &normalized,
        &request.name,
        request.line,
        kind,
    ) {
        SeedLookup::One(id) => id,
        SeedLookup::None => {
            return Ok(Err(Refusal::new(
                "no_match",
                format!(
                    "no function named `{}` resolved in the code index; check `path` names the file that defines it, or pass `line`",
                    request.name
                ),
            )));
        }
        SeedLookup::Many(candidates) => {
            let mut refusal = Refusal::new(
                "ambiguous_symbol",
                "several functions match; pass `symbol_ref.line` to pick one",
            );
            refusal.candidates = candidates;
            return Ok(Err(refusal));
        }
    };
    let seed_node = state.symbols.node(seed).expect("resolved seed exists");
    if seed_node.kind != NodeKind::Function {
        return Ok(Err(Refusal::new(
            "no_match",
            format!(
                "`{}` is a {}, not a function",
                request.name,
                seed_node.kind.as_str()
            ),
        )));
    }
    let seed_path = seed_node.path.clone();
    let seed_line = seed_node.line;
    let Some(language) = Language::detect(Path::new(&seed_path), None) else {
        return Ok(Err(unsupported_language(&seed_path, None)));
    };
    let Some(family) = Family::of(language) else {
        return Ok(Err(unsupported_language(&seed_path, Some(language))));
    };

    let seed_source = read_source(BUILTIN, &state.root.join(&seed_path), session)?;
    let seed_tree = match ast_api::parse_tree(&seed_source, language) {
        Ok(tree) => tree,
        Err(err) => {
            return Ok(Err(Refusal::new(
                "syntax_error",
                format!("`{seed_path}` failed to parse: {err}"),
            )));
        }
    };
    let Some(decl_node) = find_declaration(
        seed_tree.root_node(),
        seed_source.as_bytes(),
        family,
        &request.name,
        seed_line,
    ) else {
        return Ok(Err(Refusal::new(
            "no_match",
            format!(
                "the index places `{}` at {seed_path}:{seed_line}, but no declaration with that name parses there; rebuild the index",
                request.name
            ),
        )));
    };
    if let Some(detail) = first_syntax_error(&seed_source, language) {
        return Ok(Err(Refusal::new(
            "syntax_error",
            format!("`{seed_path}` does not parse before the edit ({detail}); fix it first"),
        )));
    }
    let decl = match read_declaration(decl_node, &seed_source, family) {
        Ok(decl) => decl,
        Err(reason) => {
            return Ok(Err(Refusal::new(
                "error",
                format!(
                    "cannot rewrite the parameters of `{}`: {reason}",
                    request.name
                ),
            )));
        }
    };
    *parameters_before = decl
        .params
        .iter()
        .map(|p| (p.name.clone(), p.text.clone()))
        .collect();

    let in_scope = files_in_scope(state, Scope::Workspace, &request.name, &seed_path, session);
    let competing = competing_declarations(&state.symbols, seed, &request.name, &in_scope);
    if !competing.is_empty() {
        let sites = competing
            .iter()
            .map(|(path, line, kind)| Site {
                path: path.clone(),
                line: *line as usize,
                kind,
                reason: "declaration with the same name".into(),
                text: String::new(),
            })
            .collect();
        let refusal = if decl.is_method {
            Refusal::new(
                "overrides_present",
                format!(
                    "`{}` is a method and other declarations share its name; overrides and implementations would have to change together",
                    request.name
                ),
            )
        } else {
            Refusal::new(
                "ambiguous_symbol",
                format!(
                    "other declarations are named `{}`; call sites are matched by name, so they cannot be told apart",
                    request.name
                ),
            )
        };
        return Ok(Err(refusal.with_sites(sites)));
    }
    if let Some(reason) = &decl.contract {
        return Ok(Err(Refusal::new(
            "overrides_present",
            format!(
                "`{}` {reason}; every signature bound by it would have to change together",
                request.name
            ),
        )
        .with_sites(vec![Site {
            path: seed_path,
            line: seed_line as usize,
            kind: "declaration",
            reason: reason.clone(),
            text: seed_source
                .lines()
                .nth(seed_line.saturating_sub(1) as usize)
                .unwrap_or("")
                .trim()
                .to_string(),
        }])));
    }

    let new_params = match plan_params(&decl, &request.params) {
        Ok(params) => params,
        // The request does not fit this declaration: bad arguments.
        Err(message) => return Err(invalid("params", message)),
    };

    let mut edits: BTreeMap<String, Vec<PendingEdit>> = BTreeMap::new();
    let seed_root = seed_tree.root_node();
    if let Err(refusal) = plan_body(
        seed_root,
        &seed_source,
        &seed_path,
        &decl,
        &new_params,
        edits.entry(seed_path.clone()).or_default(),
    ) {
        return Ok(Err(refusal));
    }
    let parameters_after = header_items(&decl, &new_params);
    edits.entry(seed_path).or_default().push(PendingEdit {
        range: decl.params_range.clone(),
        start: decl.params_start,
        end: decl.params_end,
        content: EditContent::Literal(decl.layout.render(&parameters_after)),
    });

    let call_sites = match plan_call_sites(
        state,
        request,
        seed,
        &decl,
        &new_params,
        &mut edits,
        session,
    )? {
        Ok(sites) => sites,
        Err(refusal) => return Ok(Err(refusal)),
    };

    let mut plans = Vec::new();
    for (path, file_edits) in edits {
        if file_edits.is_empty() {
            continue;
        }
        let language = Language::detect(Path::new(&path), None).expect("planned files parse");
        let source = read_source(BUILTIN, &state.root.join(&path), session)?;
        let spans = match materialize_file(&source, file_edits) {
            Ok(spans) => spans,
            Err(message) => return Ok(Err(Refusal::new("error", format!("{path}: {message}")))),
        };
        match plan_file(path.clone(), language, source, spans, true) {
            Ok(plan) => plans.push(plan),
            Err(detail) => {
                return Ok(Err(Refusal::new(
                    "syntax_error",
                    format!("rewriting `{path}` produced a syntax error: {detail}"),
                )));
            }
        }
    }
    Ok(Ok(Planned {
        plans,
        call_sites,
        parameters_after,
    }))
}

fn unsupported_language(path: &str, language: Option<Language>) -> Refusal {
    let supported = Language::all()
        .iter()
        .filter(|l| Family::of(**l).is_some())
        .map(|l| l.name())
        .collect::<Vec<_>>()
        .join(", ");
    Refusal::new(
        "unsupported_language",
        format!(
            "change_signature has no signature grammar for `{}` in `{path}`; it supports {supported}",
            language.map(|l| l.name()).unwrap_or("?")
        ),
    )
}

/// Match the requested list against the declaration. `Err` is the reason the
/// list cannot be applied.
fn plan_params(decl: &Declaration, specs: &[ParamSpec]) -> Result<Vec<NewParam>, String> {
    let by_name: HashMap<&str, usize> = decl
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.as_str(), i))
        .collect();
    let mut taken: HashMap<usize, &str> = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for spec in specs {
        if !seen.insert(spec.name.as_str()) {
            return Err(format!("`{}` appears twice in `params`", spec.name));
        }
        if decl.receiver_text.as_deref().map(str::trim) == Some(spec.name.as_str()) {
            return Err(format!(
                "`{}` is the receiver; it stays first implicitly, so leave it out of `params`",
                spec.name
            ));
        }
        let source = match &spec.from {
            Some(from) => Some(*by_name.get(from.as_str()).ok_or_else(|| {
                format!(
                    "`from: {from}` names no current parameter; the parameters are {}",
                    current_names(decl)
                )
            })?),
            None => by_name.get(spec.name.as_str()).copied(),
        };
        if let Some(index) = source {
            if let Some(previous) = taken.insert(index, &spec.name) {
                return Err(format!(
                    "`{previous}` and `{}` both keep parameter `{}`",
                    spec.name, decl.params[index].name
                ));
            }
        } else {
            if spec.call_value.is_none() && spec.default_text.is_none() {
                return Err(format!(
                    "new parameter `{}` needs `call_value` (written at every call) or `default`",
                    spec.name
                ));
            }
            if decl.family == Family::Rust && spec.type_text.is_none() {
                return Err(format!("new Rust parameter `{}` needs `type`", spec.name));
            }
        }
        if decl.family == Family::Rust && spec.default_text.is_some() {
            return Err("Rust has no default arguments; use `call_value`".into());
        }
        out.push(NewParam {
            spec: spec.clone(),
            source,
            shape: source.map_or(ParamShape::Plain, |i| decl.params[i].shape),
        });
    }
    if out.len() == decl.params.len()
        && out.iter().enumerate().all(|(i, p)| {
            p.source == Some(i)
                && p.spec.name == decl.params[i].name
                && p.spec.type_text.is_none()
                && p.spec.default_text.is_none()
        })
    {
        return Err(format!(
            "`params` repeats the current parameter list ({}); nothing would change",
            current_names(decl)
        ));
    }
    let mut seen_default = false;
    let mut seen_variadic = false;
    for param in &out {
        let base = param.source.map(|i| &decl.params[i]);
        if param.shape != ParamShape::Plain {
            seen_variadic = true;
            continue;
        }
        let has_default = param.spec.default_text.is_some()
            || base.is_some_and(|p| p.default_text.is_some() || p.optional);
        match decl.family {
            Family::Python | Family::TypeScript
                if seen_default && !has_default && !seen_variadic =>
            {
                return Err(format!(
                    "required parameter `{}` would follow a parameter with a default",
                    param.spec.name
                ));
            }
            _ => {}
        }
        seen_default |= has_default;
    }
    Ok(out)
}

fn current_names(decl: &Declaration) -> String {
    let names: Vec<String> = decl
        .params
        .iter()
        .map(|p| format!("`{}`", p.name))
        .collect();
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

/// The new header's items: the receiver, then every new parameter.
fn header_items(decl: &Declaration, params: &[NewParam]) -> Vec<String> {
    let mut items: Vec<String> = decl.receiver_text.iter().cloned().collect();
    for param in params {
        items.push(render_param(
            decl.family,
            param.source.map(|i| &decl.params[i]),
            &param.spec.name,
            param.spec.type_text.as_deref(),
            param.spec.default_text.as_deref(),
        ));
    }
    items
}

/// Refuse removed parameters the body still uses; rename the body uses of
/// renamed ones.
fn plan_body(
    root: Node<'_>,
    source: &str,
    path: &str,
    decl: &Declaration,
    params: &[NewParam],
    edits: &mut Vec<PendingEdit>,
) -> Result<(), Refusal> {
    let Some(body) = decl.body.clone() else {
        return Ok(());
    };
    let line_text = |row: usize| source.lines().nth(row).unwrap_or("").trim().to_string();
    let mut blocking = Vec::new();
    for (index, old) in decl.params.iter().enumerate() {
        if params.iter().any(|p| p.source == Some(index)) {
            continue;
        }
        for use_site in body_uses(root, source, decl.family, body.clone(), &old.name) {
            blocking.push(Site {
                path: path.to_string(),
                line: use_site.row + 1,
                kind: "parameter_use",
                reason: format!("uses removed parameter `{}`", old.name),
                text: line_text(use_site.row),
            });
        }
    }
    let old_names: Vec<&str> = decl.params.iter().map(|p| p.name.as_str()).collect();
    for param in params {
        let Some(index) = param.source else { continue };
        let old = &decl.params[index].name;
        if *old == param.spec.name {
            continue;
        }
        if !old_names.contains(&param.spec.name.as_str()) {
            let captured = body_uses(root, source, decl.family, body.clone(), &param.spec.name);
            if let Some(first) = captured.first() {
                let mut refusal = Refusal::new(
                    "conflict",
                    format!(
                        "renaming `{old}` to `{}` would capture the existing `{}` at line {}",
                        param.spec.name,
                        param.spec.name,
                        first.row + 1
                    ),
                );
                refusal.conflicts = vec![build_dict([
                    ("path", str_value(path)),
                    ("row", VmValue::Int(first.row as i64)),
                    ("col", VmValue::Int(first.col as i64)),
                    ("shadow", str_value(&param.spec.name)),
                ])];
                return Err(refusal);
            }
        }
        for use_site in body_uses(root, source, decl.family, body.clone(), old) {
            let replacement = match use_site.kind {
                UseKind::Identifier => param.spec.name.clone(),
                UseKind::Shorthand => format!("{old}: {}", param.spec.name),
                UseKind::Opaque(reason) => {
                    blocking.push(Site {
                        path: path.to_string(),
                        line: use_site.row + 1,
                        kind: "parameter_use",
                        reason: format!("{reason} of renamed parameter `{old}`"),
                        text: line_text(use_site.row),
                    });
                    continue;
                }
            };
            edits.push(PendingEdit {
                range: use_site.range,
                start: (use_site.row, use_site.col),
                end: (use_site.end_row, use_site.end_col),
                content: EditContent::Literal(replacement),
            });
        }
    }
    if blocking.is_empty() {
        Ok(())
    } else {
        Err(Refusal::new(
            "parameter_in_use",
            "the function body still uses a parameter the new list removes or cannot rename; change the body first",
        )
        .with_sites(blocking))
    }
}

/// Classify every reference and plan each call's argument rewrite.
fn plan_call_sites(
    state: &IndexState,
    request: &Request,
    seed: super::symbol_graph::NodeId,
    decl: &Declaration,
    params: &[NewParam],
    edits: &mut BTreeMap<String, Vec<PendingEdit>>,
    session: Option<&str>,
) -> Result<Result<Vec<Site>, Refusal>, HostlibError> {
    let found = reference_sites(BUILTIN, state, seed, session)?;
    if let Some(skipped) = found
        .skipped
        .iter()
        .find(|s| s.reason.starts_with("parse failed"))
    {
        return Ok(Err(Refusal::new(
            "syntax_error",
            format!("`{}` could not be parsed: {}", skipped.path, skipped.reason),
        )));
    }
    let mut by_path: BTreeMap<&str, Vec<&super::refactor_core::ReferenceSite>> = BTreeMap::new();
    for site in &found.sites {
        if Family::of(site.language) == Some(decl.family) {
            by_path.entry(site.path.as_str()).or_default().push(site);
        }
    }

    let mut updated = Vec::new();
    let mut value_refs = Vec::new();
    let mut foreign_calls = Vec::new();
    let mut unsupported = Vec::new();
    for (path, sites) in by_path {
        let language = sites[0].language;
        let source = read_source(BUILTIN, &state.root.join(path), session)?;
        let tree = match ast_api::parse_tree(&source, language) {
            Ok(tree) => tree,
            Err(err) => {
                return Ok(Err(Refusal::new(
                    "syntax_error",
                    format!("`{path}` failed to parse: {err}"),
                )));
            }
        };
        let root = tree.root_node();
        if let Some(detail) = first_syntax_error(&source, language) {
            return Ok(Err(Refusal::new(
                "syntax_error",
                format!("`{path}` does not parse before the edit ({detail}); fix it first"),
            )));
        }
        let line_text = |row: usize| source.lines().nth(row).unwrap_or("").trim().to_string();
        for site in sites {
            let row = site.span.start_row;
            let report = |kind: &'static str, reason: String| Site {
                path: path.to_string(),
                line: row + 1,
                kind,
                reason,
                text: line_text(row),
            };
            // Rust macro arguments are token trees, so the core reads a call
            // written inside one as a value use.
            let macro_call = (site.kind == ReferenceKind::ValueReference)
                .then(|| read_macro_call(root, &source, site.span.start_byte..site.span.end_byte))
                .flatten();
            let (kind, macro_call) = match macro_call {
                Some(Ok((shape, call))) => (
                    match shape {
                        MacroCallShape::Plain => ReferenceKind::Call,
                        MacroCallShape::Qualified => ReferenceKind::QualifiedCall,
                        MacroCallShape::Method => ReferenceKind::MethodCall,
                    },
                    Some(Ok(call)),
                ),
                Some(Err(reason)) => (ReferenceKind::Call, Some(Err(reason))),
                None => (site.kind, None),
            };
            let receiver_explicit = match (kind, decl.is_method) {
                (ReferenceKind::Import | ReferenceKind::TypeReference, _) => continue,
                (ReferenceKind::ValueReference, _) => {
                    if !is_reexport(root, site.span.start_byte) {
                        value_refs.push(report(
                            "value_reference",
                            "the function is used as a value, not called".into(),
                        ));
                    }
                    continue;
                }
                (ReferenceKind::MethodCall, false) => {
                    foreign_calls.push(report(
                        "method_call",
                        format!(
                            "`{}.{}(..)` calls a method; `{}` is a free function",
                            site.qualifier.as_deref().unwrap_or("?"),
                            request.name,
                            request.name
                        ),
                    ));
                    continue;
                }
                (ReferenceKind::Call, true) => {
                    foreign_calls.push(report(
                        "call",
                        format!(
                            "bare call `{}(..)` cannot reach method `{}`",
                            request.name, request.name
                        ),
                    ));
                    continue;
                }
                (ReferenceKind::QualifiedCall | ReferenceKind::MethodCall, true) => {
                    receiver_is_explicit(decl, kind, site.qualifier.as_deref())
                }
                _ => false,
            };
            let call = match macro_call.unwrap_or_else(|| {
                read_call_arguments(root, &source, decl.family, site.enclosing.clone())
            }) {
                Ok(call) => call,
                Err(reason) => {
                    unsupported.push(report("unsupported_call_site", reason));
                    continue;
                }
            };
            match remap_arguments(decl, params, &call.args, receiver_explicit) {
                Ok(items) => {
                    updated.push(report(kind.as_str(), String::new()));
                    edits
                        .entry(path.to_string())
                        .or_default()
                        .push(PendingEdit {
                            range: call.range,
                            start: call.start,
                            end: call.end,
                            content: EditContent::Arguments {
                                layout: call.layout,
                                items,
                            },
                        });
                }
                Err(reason) => unsupported.push(report("unsupported_call_site", reason)),
            }
        }
    }
    if !value_refs.is_empty() {
        return Ok(Err(Refusal::new(
            "value_reference",
            format!(
                "`{}` is passed or stored as a value; those uses would break and cannot be rewritten",
                request.name
            ),
        )
        .with_sites(value_refs)));
    }
    if !foreign_calls.is_empty() {
        return Ok(Err(Refusal::new(
            "ambiguous_symbol",
            format!(
                "some calls named `{}` cannot be matched to this declaration by name",
                request.name
            ),
        )
        .with_sites(foreign_calls)));
    }
    if !unsupported.is_empty() {
        return Ok(Err(Refusal::new(
            "unsupported_call_site",
            "some calls cannot be rewritten automatically",
        )
        .with_sites(unsupported)));
    }
    Ok(Ok(updated))
}

/// Whether a method call passes the instance as its first argument:
/// Rust `Type::m(value, ..)`, Python `Class.m(value, ..)`.
fn receiver_is_explicit(decl: &Declaration, kind: ReferenceKind, qualifier: Option<&str>) -> bool {
    if decl.receiver != Receiver::Instance {
        return false;
    }
    match decl.family {
        Family::Rust => kind == ReferenceKind::QualifiedCall,
        // The core reads `m.f()` as qualified when an import binds the
        // leftmost name of `m`, which `Formatter().f()` also satisfies, so
        // only the class name itself marks an explicit receiver.
        Family::Python => match (qualifier, decl.owner.as_deref()) {
            (Some(qualifier), Some(owner)) => qualifier.rsplit('.').next() == Some(owner),
            _ => false,
        },
        Family::TypeScript => false,
    }
}

/// A value use inside `export { f }` / `export default f` keeps working.
fn is_reexport(root: Node<'_>, byte: usize) -> bool {
    let mut node = root.descendant_for_byte_range(byte, byte);
    while let Some(current) = node {
        if current.kind() == "export_statement" {
            return current.child_by_field_name("declaration").is_none();
        }
        node = current.parent();
    }
    false
}

/// The new argument list for one call, or why it cannot be built.
fn remap_arguments(
    decl: &Declaration,
    params: &[NewParam],
    args: &[Argument],
    receiver_explicit: bool,
) -> Result<Vec<ArgItem>, String> {
    let mut args = args.iter();
    let mut items = Vec::new();
    if receiver_explicit {
        match args.next() {
            Some(Argument::Positional(range)) => items.push(ArgItem::Source(range.clone())),
            _ => return Err("the call passes no receiver argument".into()),
        }
    }
    // Old parameter index → (value range, passed by keyword).
    let mut values: HashMap<usize, (Range<usize>, bool)> = HashMap::new();
    let positional_slots: Vec<usize> = decl
        .params
        .iter()
        .enumerate()
        .take_while(|(_, p)| p.shape == ParamShape::Plain)
        .map(|(i, _)| i)
        .collect();
    let mut next_slot = positional_slots.iter();
    for arg in args {
        match arg {
            Argument::Positional(range) => {
                let Some(slot) = next_slot.next() else {
                    return Err(
                        if decl.params.iter().any(|p| p.shape == ParamShape::Variadic) {
                            "the call passes variadic arguments".to_string()
                        } else {
                            "the call passes more arguments than the function declares".to_string()
                        },
                    );
                };
                values.insert(*slot, (range.clone(), false));
            }
            Argument::Keyword(name, range) => {
                let slot = decl
                    .params
                    .iter()
                    .position(|p| p.name == *name && p.shape == ParamShape::Plain)
                    .ok_or_else(|| format!("keyword argument `{name}` names no parameter"))?;
                if values.insert(slot, (range.clone(), true)).is_some() {
                    return Err(format!("`{name}` is passed twice"));
                }
            }
        }
    }
    if decl.family == Family::Rust && values.len() != decl.params.len() {
        return Err(format!(
            "the call passes {} argument(s) but the function declares {}",
            values.len(),
            decl.params.len()
        ));
    }

    let mut keyword_mode = false;
    let mut pending_gaps = 0usize;
    for param in params {
        if param.shape != ParamShape::Plain {
            keyword_mode = true;
            continue;
        }
        let value = match param.source {
            Some(index) => values
                .get(&index)
                .map(|(range, keyword)| (ArgItem::Source(range.clone()), *keyword)),
            None => param
                .spec
                .call_value
                .as_ref()
                .map(|v| (ArgItem::Text(v.clone()), false)),
        };
        match (decl.family, value) {
            (_, None) => match decl.family {
                Family::Python => keyword_mode = true,
                Family::TypeScript => pending_gaps += 1,
                Family::Rust => {
                    return Err(format!("no value for parameter `{}`", param.spec.name));
                }
            },
            (Family::Python, Some((item, by_keyword))) => {
                keyword_mode |= by_keyword;
                if keyword_mode {
                    let range = match item {
                        ArgItem::Source(range) => range,
                        ArgItem::Text(text) => {
                            items.push(ArgItem::Text(format!("{}={text}", param.spec.name)));
                            continue;
                        }
                        ArgItem::Keyword(..) => unreachable!("values are never keywords"),
                    };
                    items.push(ArgItem::Keyword(param.spec.name.clone(), range));
                } else {
                    items.push(item);
                }
            }
            (_, Some((item, _))) => {
                for _ in 0..pending_gaps {
                    items.push(ArgItem::Text("undefined".into()));
                }
                pending_gaps = 0;
                items.push(item);
            }
        }
    }
    Ok(items)
}

/// Render every pending edit, innermost first, and return the outermost
/// ones as splice spans.
fn materialize_file(source: &str, mut edits: Vec<PendingEdit>) -> Result<Vec<EditSpan>, String> {
    edits.sort_by_key(|e| (e.range.end - e.range.start, e.range.start));
    let mut rendered: Vec<(Range<usize>, String)> = Vec::new();
    let mut spans: Vec<(usize, EditSpan)> = Vec::new();
    for edit in &edits {
        let after = match &edit.content {
            EditContent::Literal(text) => text.clone(),
            EditContent::Arguments { layout, items } => {
                let texts: Vec<String> = items
                    .iter()
                    .map(|item| match item {
                        ArgItem::Text(text) => text.clone(),
                        ArgItem::Source(range) => splice_inner(source, range, &rendered),
                        ArgItem::Keyword(name, range) => {
                            format!("{name}={}", splice_inner(source, range, &rendered))
                        }
                    })
                    .collect();
                layout.render(&texts)
            }
        };
        rendered.push((edit.range.clone(), after.clone()));
        spans.push((
            edit.range.end - edit.range.start,
            EditSpan {
                span: IdentifierSpan {
                    start_byte: edit.range.start,
                    end_byte: edit.range.end,
                    start_row: edit.start.0,
                    start_col: edit.start.1,
                    end_row: edit.end.0,
                    end_col: edit.end.1,
                },
                before: slice(source, edit.range.clone()).to_string(),
                after,
            },
        ));
    }
    let outermost: Vec<EditSpan> = spans
        .iter()
        .filter(|(_, span)| {
            !spans.iter().any(|(_, other)| {
                !std::ptr::eq(other, span)
                    && other.span.start_byte <= span.span.start_byte
                    && span.span.end_byte <= other.span.end_byte
                    && (other.span.start_byte, other.span.end_byte)
                        != (span.span.start_byte, span.span.end_byte)
            })
        })
        .map(|(_, span)| span.clone())
        .collect();
    let mut ordered: Vec<&EditSpan> = outermost.iter().collect();
    ordered.sort_by_key(|e| e.span.start_byte);
    for pair in ordered.windows(2) {
        if pair[0].span.end_byte > pair[1].span.start_byte {
            return Err("two rewrites overlap".into());
        }
    }
    Ok(outermost)
}

/// `range`'s source text with every already-rendered edit inside it applied.
fn splice_inner(source: &str, range: &Range<usize>, rendered: &[(Range<usize>, String)]) -> String {
    let mut inner: Vec<&(Range<usize>, String)> = rendered
        .iter()
        .filter(|(r, _)| range.start <= r.start && r.end <= range.end)
        .collect();
    inner.sort_by_key(|(r, _)| (r.start, std::cmp::Reverse(r.end)));
    let mut out = String::new();
    let mut cursor = range.start;
    for (r, text) in inner {
        if r.start < cursor {
            continue;
        }
        out.push_str(slice(source, cursor..r.start));
        out.push_str(text);
        cursor = r.end;
    }
    out.push_str(slice(source, cursor..range.end));
    out
}

// === Response ===
//
// The shared code_index edit envelope ([`edit_envelope`]) plus this
// operation's fields: `call_sites_updated`, `sites` (rewritten calls, or the
// blocking locations of a refusal), and the parameter lists.

/// Operation-specific fields of one response.
#[derive(Default)]
struct SignatureFields {
    sites: Vec<Site>,
    call_sites_updated: usize,
    parameters_before: Vec<(String, String)>,
    parameters_after: Vec<String>,
}

fn respond(
    request: &Request,
    tag: &'static str,
    mut envelope: EditEnvelope,
    fields: SignatureFields,
) -> VmValue {
    let list = |items: Vec<VmValue>| VmValue::List(Arc::new(items));
    envelope.extra = vec![
        (
            "call_sites_updated",
            VmValue::Int(fields.call_sites_updated as i64),
        ),
        (
            "sites",
            list(
                fields
                    .sites
                    .iter()
                    .map(|site| {
                        build_dict([
                            ("path", str_value(&site.path)),
                            ("line", VmValue::Int(site.line as i64)),
                            ("kind", str_value(site.kind)),
                            ("reason", str_value(&site.reason)),
                            ("text", str_value(&site.text)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "parameters_before",
            list(
                fields
                    .parameters_before
                    .iter()
                    .map(|(name, text)| {
                        build_dict([("name", str_value(name)), ("text", str_value(text))])
                    })
                    .collect(),
            ),
        ),
        (
            "parameters_after",
            list(fields.parameters_after.into_iter().map(str_value).collect()),
        ),
    ];
    edit_envelope(
        tag,
        Scope::Workspace,
        &EditSymbol {
            name: &request.name,
            new_name: None,
            path: &request.path,
            line: request.line,
            kind: request.kind,
        },
        envelope,
    )
}

fn finish(
    state: &IndexState,
    request: &Request,
    planned: Planned,
    parameters_before: Vec<(String, String)>,
) -> VmValue {
    let failed = if request.dry_run {
        Vec::new()
    } else {
        write_plans(
            BUILTIN,
            &state.root,
            &planned.plans,
            request.session_id.as_deref(),
        )
    };
    let details = if request.dry_run {
        "dry_run — no files were written"
    } else if failed.is_empty() {
        "signature changed; update the function body to use the new parameters, then build"
    } else {
        "signature change partially applied; see failed_paths_with_reasons"
    };
    let mut sites = planned.call_sites;
    sites.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    respond(
        request,
        "applied",
        EditEnvelope {
            applied: !request.dry_run && failed.is_empty(),
            dry_run: request.dry_run,
            touched_files: planned.plans.iter().map(file_plan_value).collect(),
            failed_paths: failed_paths_value(&failed),
            match_count: planned.plans.iter().map(|p| p.edits.len()).sum(),
            details: details.to_string(),
            ..EditEnvelope::default()
        },
        SignatureFields {
            call_sites_updated: sites.len(),
            sites,
            parameters_before,
            parameters_after: planned.parameters_after,
        },
    )
}

#[cfg(test)]
#[path = "change_signature_tests.rs"]
mod tests;
