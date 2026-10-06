//! `code_index.extract_function` — lift a region into a new function and
//! replace it, and every same-file copy, with a call.
//!
//! # Wire shape
//!
//! See `schemas/code_index/extract_function.{request,response}.json`. The
//! request names the region by `start_line`/`end_line` (1-based, inclusive)
//! or by its exact text in `region`. The response is the shared refactoring
//! envelope (`refactor_core::edit_envelope`, first defined by `rename_symbol`):
//!
//! - `applied` — the plan passed every check. With `dry_run` nothing was
//!   written; a failed write keeps the tag with `applied: false` and
//!   `failed_paths_with_reasons`.
//! - `no_match` — the region is outside the file, missing, outside any
//!   function, or does not line up with an expression, a statement run, or a
//!   closure body.
//! - `ambiguous_symbol` — `region` text occurs more than once and
//!   `all_occurrences` is off; the matches are in `warnings`.
//! - `control_flow_escapes` — `return`, `break`, `continue`, `?`, `await`, or
//!   `yield` would leave the region (`sites`).
//! - `signature_mismatch` — `signature` or `helper` names the function or its
//!   parameters differently from what the region needs.
//! - `types_required` — Rust or strict TypeScript needs `signature`; the
//!   computed `inputs` and `outputs` say what to type.
//! - `name_conflict` — `new_name` already appears in the file (`conflicts`).
//! - `unsupported_region` — the region reads the method receiver, is an
//!   expression that assigns a later-read name, or mixes declared and
//!   assigned outputs.
//! - `unsupported_language`, `syntax_error`.
//!
//! Malformed arguments raise an error instead of returning a tag.
//! Every refusal leaves the file byte-identical. Planning lives in
//! [`super::extract_plan`]; this module owns arguments, I/O, and the
//! response envelope.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_vm::VmValue;

use crate::ast::{Language, TEXT_PATCH_FALLBACK};
use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_string, require_string, resolve_host_path,
    str_value, to_agent_path,
};

use super::builtins::SharedIndex;
use super::extract_plan::{plan, Dialect, FlowSummary, Plan, PlanInput, Refusal, Selection, Site};
use super::refactor_core::{
    edit_envelope, failed_paths_value, file_plan_value, is_identifier_token, plan_file,
    read_source, write_plans, EditEnvelope, EditSymbol, FilePlan, Scope,
};
use super::symbol_graph::NodeKind;

pub(super) const BUILTIN: &str = "hostlib_code_index_extract_function";

pub(super) fn run(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();
    let path = require_string(BUILTIN, dict, "path")?;
    let new_name = require_string(BUILTIN, dict, "new_name")?;
    if !is_identifier_token(&new_name) {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "new_name",
            message: format!("`{new_name}` is not an identifier"),
        });
    }
    let region = optional_string(BUILTIN, dict, "region")?;
    let start_line = optional_line(dict, "start_line")?;
    let end_line = optional_line(dict, "end_line")?;
    let selection = match (&region, start_line, end_line) {
        (Some(text), None, None) => Selection::Text(text),
        (None, Some(start), end) => Selection::Lines(start, end.unwrap_or(start)),
        _ => {
            return Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "region",
                message: "pass exactly one of `region` or `start_line` (with optional `end_line`)"
                    .into(),
            });
        }
    };
    let signature = optional_string(BUILTIN, dict, "signature")?;
    let helper = optional_string(BUILTIN, dict, "helper")?;
    let all_occurrences = optional_bool(BUILTIN, dict, "all_occurrences", true)?;
    let dry_run = optional_bool(BUILTIN, dict, "dry_run", false)?;
    let session_id = optional_string(BUILTIN, dict, "session_id")?;

    let guard = index.lock().expect("code_index mutex poisoned");
    let (root, absolute) = match guard.as_ref() {
        Some(state) => {
            let absolute =
                state
                    .absolute_path(&path)
                    .ok_or_else(|| HostlibError::InvalidParameter {
                        builtin: BUILTIN,
                        param: "path",
                        message: "path must stay within the indexed workspace root".into(),
                    })?;
            (state.root.clone(), absolute)
        }
        None => {
            let absolute = resolve_host_path(&path);
            let root = absolute.parent().map(Path::to_path_buf).unwrap_or_default();
            (root, absolute)
        }
    };
    let relative = absolute
        .strip_prefix(&root)
        .map(to_agent_path)
        .unwrap_or_else(|_| to_agent_path(&absolute));

    let env = Response {
        path: &relative,
        new_name: &new_name,
    };
    let Some(language) = Language::detect(&absolute, None) else {
        return Ok(env.refusal(&Refusal::new(
            "unsupported_language",
            format!("no tree-sitter grammar for `{relative}`"),
        )));
    };
    let source = read_source(BUILTIN, &absolute, session_id.as_deref())?;
    let input = PlanInput {
        source: &source,
        language,
        selection,
        new_name: &new_name,
        signature: signature.as_deref(),
        helper: helper.as_deref(),
        all_occurrences,
        strict_types: Dialect::of(language) == Some(Dialect::TypeScript)
            && typescript_is_strict(&absolute),
    };
    let planned = match plan(&input) {
        Ok(planned) => planned,
        Err(refusal) => return Ok(env.refusal(&refusal)),
    };
    let file_plan = match plan_file(
        relative.clone(),
        language,
        source,
        planned.edits.clone(),
        true,
    ) {
        Ok(file_plan) => file_plan,
        Err(detail) => {
            return Ok(env.refusal(&Refusal::new(
                "syntax_error",
                format!("the extracted file would not parse: {detail}"),
            )));
        }
    };
    if dry_run {
        return Ok(env.success(&planned, &file_plan, true, Vec::new()));
    }
    let failed = write_plans(
        BUILTIN,
        &root,
        std::slice::from_ref(&file_plan),
        session_id.as_deref(),
    );
    Ok(env.success(&planned, &file_plan, false, failed))
}

fn optional_line(
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<usize>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(n)) if *n >= 1 => Ok(Some(*n as usize)),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: key,
            message: format!("expected a 1-based line number, got {}", other.type_name()),
        }),
    }
}

/// Whether the nearest `tsconfig.json` turns on `strict` or `noImplicitAny`,
/// which makes an untyped parameter a compile error.
fn typescript_is_strict(file: &Path) -> bool {
    let mut dir: Option<PathBuf> = file.parent().map(Path::to_path_buf);
    while let Some(current) = dir {
        if let Ok(text) = std::fs::read_to_string(current.join("tsconfig.json")) {
            let compact: String = text
                .lines()
                .map(|l| l.split("//").next().unwrap_or(""))
                .collect::<String>()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            return compact.contains("\"strict\":true")
                || compact.contains("\"noImplicitAny\":true");
        }
        dir = current.parent().map(Path::to_path_buf);
    }
    false
}

// === Response shaping ===
//
// The shared refactoring envelope (`refactor_core::edit_envelope`), with the
// extraction's own fields in `extra` so a consumer reads the same keys
// whatever the outcome.

struct Response<'a> {
    path: &'a str,
    new_name: &'a str,
}

#[derive(Default)]
struct Fields {
    applied: bool,
    dry_run: bool,
    flow: Option<FlowSummary>,
    helper: String,
    call: String,
    touched_files: Vec<VmValue>,
    sites: Vec<Site>,
    conflicts: Vec<Site>,
    candidates: Vec<Site>,
    failed: Vec<(String, String)>,
    occurrences: usize,
    details: String,
    fallback: bool,
}

impl Response<'_> {
    fn emit(&self, tag: &'static str, fields: Fields) -> VmValue {
        let flow = fields.flow.unwrap_or_default();
        let names = |v: &[String]| VmValue::List(Arc::new(v.iter().map(str_value).collect()));
        let region = if flow.region_kind.is_empty() {
            VmValue::Nil
        } else {
            build_dict([
                ("kind", str_value(flow.region_kind)),
                ("start_line", VmValue::Int(flow.start_line as i64)),
                ("end_line", VmValue::Int(flow.end_line as i64)),
            ])
        };
        let conflicts = fields
            .conflicts
            .iter()
            .map(|site| {
                build_dict([
                    ("path", str_value(self.path)),
                    ("line", VmValue::Int(site.line as i64)),
                    ("row", VmValue::Int(site.line.saturating_sub(1) as i64)),
                    ("shadow", str_value(self.new_name)),
                    ("text", str_value(&site.text)),
                ])
            })
            .collect();
        // The symbol a response describes is the function being created.
        let symbol = EditSymbol {
            name: self.new_name,
            new_name: None,
            path: self.path,
            line: (flow.start_line > 0).then_some(flow.start_line as u32),
            kind: Some(NodeKind::Function),
        };
        let envelope = EditEnvelope {
            applied: fields.applied,
            dry_run: fields.dry_run,
            touched_files: fields.touched_files,
            conflicts,
            warnings: sites_list(self.path, &fields.candidates),
            failed_paths: failed_paths_value(&fields.failed),
            match_count: fields.occurrences,
            details: fields.details,
            fallback_suggestion: fields.fallback.then(|| TEXT_PATCH_FALLBACK.to_string()),
            extra: vec![
                ("region", region),
                ("inputs", names(&flow.inputs)),
                ("outputs", names(&flow.outputs)),
                ("returns_value", VmValue::Bool(flow.returns_value)),
                ("helper", str_value(&fields.helper)),
                ("call", str_value(&fields.call)),
                (
                    "sites",
                    VmValue::List(Arc::new(sites_list(self.path, &fields.sites))),
                ),
                (
                    "occurrences_replaced",
                    VmValue::Int(fields.occurrences as i64),
                ),
                (
                    "call_sites_updated",
                    VmValue::Int(fields.occurrences as i64),
                ),
                ("comments_left_behind", VmValue::List(Arc::new(Vec::new()))),
            ],
        };
        edit_envelope(tag, Scope::File, &symbol, envelope)
    }

    fn refusal(&self, refusal: &Refusal) -> VmValue {
        let name_conflict = refusal.tag == "name_conflict";
        self.emit(
            refusal.tag,
            Fields {
                flow: refusal.flow.clone(),
                sites: if name_conflict {
                    Vec::new()
                } else {
                    refusal.sites.clone()
                },
                conflicts: if name_conflict {
                    refusal.sites.clone()
                } else {
                    Vec::new()
                },
                candidates: refusal.candidates.clone(),
                details: refusal.details.clone(),
                fallback: refusal.tag == "unsupported_language",
                ..Default::default()
            },
        )
    }

    /// `applied` for a write or a dry run; a failed write keeps the tag
    /// with `applied: false` and the failed paths, like `rename_symbol`.
    fn success(
        &self,
        planned: &Plan,
        file: &FilePlan,
        dry_run: bool,
        failed: Vec<(String, String)>,
    ) -> VmValue {
        let occurrences = planned.replaced_lines.len();
        let lines = planned
            .replaced_lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let mut details = if dry_run {
            format!("dry run; would replace {occurrences} occurrence(s) at line(s) {lines}")
        } else if failed.is_empty() {
            format!("replaced {occurrences} occurrence(s) at line(s) {lines}")
        } else {
            "the write failed; see failed_paths_with_reasons".to_string()
        };
        if !planned.skipped_copies.is_empty() {
            details.push_str(&format!(
                "; left {} token-equal cop(ies) with different bindings unchanged (see `sites`)",
                planned.skipped_copies.len()
            ));
        }
        let written = dry_run || failed.is_empty();
        self.emit(
            "applied",
            Fields {
                applied: !dry_run && failed.is_empty(),
                dry_run,
                flow: Some(planned.flow.clone()),
                helper: planned.helper_text.clone(),
                call: planned.call_text.clone(),
                touched_files: if written {
                    vec![file_plan_value(file)]
                } else {
                    Vec::new()
                },
                sites: planned.skipped_copies.clone(),
                failed,
                occurrences: if written { occurrences } else { 0 },
                details,
                ..Default::default()
            },
        )
    }
}

fn sites_list(path: &str, sites: &[Site]) -> Vec<VmValue> {
    sites
        .iter()
        .map(|site| {
            build_dict([
                ("path", str_value(path)),
                ("line", VmValue::Int(site.line as i64)),
                ("kind", str_value(site.kind)),
                ("reason", str_value(&site.reason)),
                ("text", str_value(&site.text)),
            ])
        })
        .collect()
}

#[cfg(test)]
#[path = "extract_tests.rs"]
mod tests;
