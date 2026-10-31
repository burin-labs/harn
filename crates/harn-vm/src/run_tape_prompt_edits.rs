//! Source-checked prompt edits with explicit origin or mid-run activation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{BranchDivergence, EventTape};

/// Origin edits also affect values rendered before their first model consumer.
/// Mid-run interventions leave every render before the served prefix unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptEditActivation {
    Origin,
    AfterPrefix,
}

/// A replacement for one complete literal span from a recorded prompt.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PromptSourceEdit {
    pub template_uri: String,
    pub template_revision: String,
    /// One-based Unicode character coordinates in the recorded source.
    pub line: usize,
    pub column: usize,
    pub original: String,
    pub replacement: String,
    pub activation: PromptEditActivation,
}

/// Loading an edit into a render does not prove it reached a model request.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct PromptEditReport {
    pub template_uri: String,
    pub line: usize,
    pub column: usize,
    pub activation: PromptEditActivation,
    pub prepared_renders: u64,
}

#[derive(Default)]
pub(super) struct PromptEdits {
    by_uri: BTreeMap<String, Vec<(usize, PromptSourceEdit)>>,
}

impl PromptEdits {
    pub(super) fn from_tape(
        tape: &EventTape,
        edits: Vec<PromptSourceEdit>,
        source_roots: &[super::ReplaySourceRoot],
    ) -> Result<(Self, Vec<PromptEditReport>), String> {
        if edits.is_empty() {
            return Ok((Self::default(), Vec::new()));
        }
        super::prompt_plan::validated_sources(tape, &edits)?;
        let mut planned = Self::default();
        let mut reports = Vec::new();
        for edit in edits {
            let index = reports.len();
            reports.push(PromptEditReport {
                template_uri: edit.template_uri.clone(),
                line: edit.line,
                column: edit.column,
                activation: edit.activation,
                prepared_renders: 0,
            });
            planned
                .by_uri
                .entry(normalize_uri(tape, source_roots, &edit.template_uri))
                .or_default()
                .push((index, edit));
        }
        Ok((planned, reports))
    }

    pub(super) fn apply(
        &self,
        uri: &str,
        source: &str,
        reports: &mut [PromptEditReport],
        after_prefix: bool,
    ) -> Result<Option<String>, String> {
        let Some(edits) = self.by_uri.get(uri) else {
            return Ok(None);
        };
        let revision = blake3::hash(source.as_bytes()).to_hex().to_string();
        let mut replacements = Vec::new();
        for (index, edit) in edits {
            if !after_prefix && edit.activation == PromptEditActivation::AfterPrefix {
                continue;
            }
            if edit.template_revision != revision {
                return Err(format!("prompt source changed since recording: {uri}"));
            }
            let start = source_offset(source, edit.line, edit.column)
                .ok_or_else(|| format!("prompt source position no longer exists: {uri}"))?;
            let end = start
                .checked_add(edit.original.len())
                .ok_or_else(|| "prompt source range overflow".to_string())?;
            if source.get(start..end) != Some(edit.original.as_str()) {
                return Err(format!(
                    "recorded literal no longer matches prompt source: {uri}"
                ));
            }
            replacements.push((start, end, *index, &edit.replacement));
        }
        if replacements.is_empty() {
            return Ok(None);
        }
        replacements.sort_by_key(|entry| entry.0);
        if replacements.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err(format!("prompt source edits overlap: {uri}"));
        }
        let mut result = String::new();
        let mut cursor = 0;
        for (start, end, index, replacement) in replacements {
            result.push_str(
                source
                    .get(cursor..start)
                    .expect("validated source boundaries"),
            );
            result.push_str(replacement);
            cursor = end;
            reports[index].prepared_renders += 1;
        }
        result.push_str(source.get(cursor..).expect("validated source boundary"));
        Ok(Some(result))
    }
}

fn normalize_uri(tape: &EventTape, source_roots: &[super::ReplaySourceRoot], uri: &str) -> String {
    for root in source_roots {
        if let Some(relative) =
            super::scope::workspace_relative(std::path::Path::new(uri), &root.recorded)
        {
            return root.recorded.join(relative).display().to_string();
        }
    }
    let Some(root) = tape
        .header
        .run
        .as_ref()
        .and_then(|run| run.workspace_root.as_ref())
    else {
        return uri.to_string();
    };
    super::scope::workspace_relative(std::path::Path::new(uri), root)
        .map(|relative| root.join(relative).display().to_string())
        .unwrap_or_else(|| uri.to_string())
}

fn source_offset(source: &str, wanted_line: usize, wanted_column: usize) -> Option<usize> {
    let (mut line, mut column) = (1, 1);
    for (offset, character) in source.char_indices() {
        if (line, column) == (wanted_line, wanted_column) {
            return Some(offset);
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    None
}

pub(crate) fn prepare(uri: &str, source: &str) -> Result<Option<String>, String> {
    let Some(branch) = super::scope::current_branch() else {
        return Ok(None);
    };
    let uri = branch.prompt_uri(uri);
    let mut state = branch.state();
    let after_prefix = state.report.served_calls >= state.report.live_after_calls;
    if let Some(divergence) = &state.report.divergence {
        return Err(divergence.detail.clone());
    }
    let super::BranchState {
        prompt_edits,
        report,
        ..
    } = &mut *state;
    match prompt_edits.apply(&uri, source, &mut report.prompt_edits, after_prefix) {
        Ok(value) => Ok(value),
        Err(detail) => {
            report.divergence = Some(BranchDivergence {
                call: report.served_calls + report.live_calls + 1,
                what: "prompt source edit".to_string(),
                detail: detail.clone(),
            });
            Err(detail)
        }
    }
}
