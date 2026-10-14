//! Response shaping for `move_symbol`: refactor_core's shared edit envelope
//! plus the move's own fields.

use std::sync::Arc;

use harn_vm::VmValue;

use crate::ast::TEXT_PATCH_FALLBACK;
use crate::code_index::refactor_core::{
    candidates_value, edit_envelope, failed_paths_value, file_plan_value, EditEnvelope, EditSymbol,
    FilePlan, Scope, SeedCandidate,
};
use crate::tools::args::{build_dict, str_value};

use super::{text, Request};

// === Response ===

/// One reported location.
#[derive(Clone, Debug)]
pub(crate) struct Site {
    pub path: String,
    pub line: u32,
    pub kind: &'static str,
    pub reason: String,
    pub text: String,
}

impl Site {
    pub(crate) fn at(
        path: &str,
        source: &str,
        at: usize,
        kind: &'static str,
        reason: impl Into<String>,
    ) -> Self {
        let start = text::line_start(source, at);
        let end = text::next_line_start(source, at);
        Self {
            path: path.to_string(),
            line: text::line_of(source, at),
            kind,
            reason: reason.into(),
            text: text::slice(source, start..end).trim_end().to_string(),
        }
    }

    pub(crate) fn to_value(&self) -> VmValue {
        build_dict([
            ("path", str_value(&self.path)),
            ("line", VmValue::Int(self.line as i64)),
            ("kind", str_value(self.kind)),
            ("reason", str_value(&self.reason)),
            ("text", str_value(&self.text)),
        ])
    }
}

#[derive(Default)]
pub(crate) struct Response {
    pub(crate) details: String,
    pub(crate) plans: Vec<FilePlan>,
    pub(crate) created: Vec<String>,
    pub(crate) candidates: Vec<SeedCandidate>,
    pub(crate) sites: Vec<Site>,
    pub(crate) comments_left_behind: Vec<Site>,
    pub(crate) warnings: Vec<String>,
    pub(crate) failed: Vec<(String, String)>,
    pub(crate) occurrences_replaced: usize,
    pub(crate) call_sites_updated: usize,
    pub(crate) moved_lines: Option<(u32, u32)>,
}

pub(crate) struct Outcome {
    pub(crate) tag: &'static str,
    pub(crate) response: Response,
}

pub(crate) fn refuse(tag: &'static str, details: impl Into<String>) -> Outcome {
    Outcome {
        tag,
        response: Response {
            details: details.into(),
            ..Default::default()
        },
    }
}

pub(crate) fn refuse_with_sites(
    tag: &'static str,
    details: impl Into<String>,
    sites: Vec<Site>,
) -> Outcome {
    let mut outcome = refuse(tag, details);
    outcome.response.sites = sites;
    outcome
}

// === Response shaping ===

/// The shared code_index edit envelope, plus the move's own fields in
/// `extra`.
pub(crate) fn respond(
    request: &Request,
    source_path: &str,
    dest_path: &str,
    outcome: Outcome,
) -> VmValue {
    let Outcome { tag, response } = outcome;
    let planned = tag == "applied";
    let touched_files: Vec<VmValue> = if planned {
        response.plans.iter().map(file_plan_value).collect()
    } else {
        Vec::new()
    };
    let sites = |list: &[Site]| VmValue::List(Arc::new(list.iter().map(Site::to_value).collect()));
    let moved_lines = match response.moved_lines {
        Some((start, end)) if planned => build_dict([
            ("start", VmValue::Int(start as i64)),
            ("end", VmValue::Int(end as i64)),
        ]),
        _ => VmValue::Nil,
    };
    let envelope = EditEnvelope {
        applied: planned && !request.dry_run && response.failed.is_empty(),
        dry_run: request.dry_run,
        touched_files,
        warnings: if tag == "ambiguous_symbol" {
            candidates_value(&response.candidates)
        } else {
            response.warnings.iter().map(str_value).collect()
        },
        failed_paths: failed_paths_value(&response.failed),
        match_count: response.plans.iter().map(|plan| plan.edits.len()).sum(),
        details: response.details,
        fallback_suggestion: (tag == "unsupported_language")
            .then(|| TEXT_PATCH_FALLBACK.to_string()),
        extra: vec![
            ("to_path", str_value(dest_path)),
            (
                "created_destination",
                VmValue::Bool(planned && response.created.iter().any(|p| p == dest_path)),
            ),
            ("moved_lines", moved_lines),
            ("sites", sites(&response.sites)),
            (
                "comments_left_behind",
                sites(&response.comments_left_behind),
            ),
            (
                "occurrences_replaced",
                VmValue::Int(response.occurrences_replaced as i64),
            ),
            (
                "call_sites_updated",
                VmValue::Int(response.call_sites_updated as i64),
            ),
        ],
        ..Default::default()
    };
    let symbol = EditSymbol {
        name: &request.symbol,
        new_name: None,
        path: source_path,
        line: request.line,
        kind: request.kind,
    };
    edit_envelope(tag, Scope::Workspace, &symbol, envelope)
}
