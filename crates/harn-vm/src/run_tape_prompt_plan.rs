//! Choose a replay boundary from recorded source attribution, not UI selection.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use serde::Serialize;

use super::{EventTape, PromptEditActivation, PromptSourceEdit, TapeRecordKind};
use crate::stdlib::template::{PromptSourceSpan, PromptSpanKind};
use crate::testbench::tape::AttributedSegment;

/// The earliest recorded request containing any proposed source edit.
/// A fresh sample starts there; this does not establish treatment effectiveness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PromptBranchPoint {
    pub first_live_call: u64,
    pub live_after_calls: u64,
    /// First attributed consumer of each edit, in the supplied edit order.
    pub first_consumers: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Source {
    uri: String,
    revision: String,
    line: usize,
    column: usize,
    text: String,
}

impl From<&PromptSourceEdit> for Source {
    fn from(edit: &PromptSourceEdit) -> Self {
        Self {
            uri: edit.template_uri.clone(),
            revision: edit.template_revision.clone(),
            line: edit.line,
            column: edit.column,
            text: edit.original.clone(),
        }
    }
}

pub(super) struct Literal {
    source: Source,
    range: Range<u64>,
}

type Sources = BTreeMap<String, Vec<Literal>>;

/// Validate source identities once for both planning and branch admission.
pub(super) fn validated_sources(
    tape: &EventTape,
    edits: &[PromptSourceEdit],
) -> Result<Sources, String> {
    // Stored render text may have different bytes and length from its spans.
    // Apply the same eligibility rule as replay before reading source proofs.
    super::require_unredacted_branch_payloads(tape)?;
    let mut sources = BTreeMap::new();
    let mut proofs = BTreeSet::new();
    for record in &tape.records {
        let TapeRecordKind::PromptRender {
            render_id,
            rendered,
            spans,
            ..
        } = &record.kind
        else {
            continue;
        };
        let rendered = tape.resolve_payload(rendered)?;
        let spans: Vec<PromptSourceSpan> = serde_json::from_slice(&tape.resolve_payload(spans)?)
            .map_err(|error| format!("recorded prompt source spans: {error}"))?;
        let mut literals = Vec::new();
        for span in spans {
            if span.kind != PromptSpanKind::Text {
                continue;
            }
            let text = rendered
                .get(span.output_start..span.output_end)
                .ok_or("recorded prompt span exceeds its rendered text")?;
            let text =
                std::str::from_utf8(text).map_err(|_| "recorded prompt span splits a character")?;
            let source = Source {
                uri: span.template_uri,
                revision: span.template_revision,
                line: span.template_line,
                column: span.template_col,
                text: text.to_string(),
            };
            proofs.insert(source.clone());
            literals.push(Literal {
                source,
                range: span.output_start as u64..span.output_end as u64,
            });
        }
        if sources.insert(render_id.clone(), literals).is_some() {
            return Err(format!("duplicate recorded prompt render `{render_id}`"));
        }
    }
    let mut identities = BTreeSet::new();
    for edit in edits {
        if edit.original.is_empty() || edit.original == edit.replacement {
            return Err("a prompt edit must change a nonempty recorded literal".to_string());
        }
        if edit.replacement.contains("{{") || edit.replacement.contains("}}") {
            return Err(
                "prompt replacements must not contain template syntax delimiters".to_string(),
            );
        }
        if !identities.insert((&edit.template_uri, edit.line, edit.column)) {
            return Err("multiple prompt edits target the same source position".to_string());
        }
        if !proofs.contains(&Source::from(edit)) {
            return Err(format!(
                "prompt edit {}:{}:{} is not a recorded literal at that revision",
                edit.template_uri, edit.line, edit.column
            ));
        }
    }
    Ok(sources)
}

/// Plan an origin edit at its first attributed use. Short, unused, or ambiguous
/// renders refuse instead of implying that missing attribution means call one.
pub fn plan_prompt_branch(
    tape: &EventTape,
    edits: &[PromptSourceEdit],
) -> Result<PromptBranchPoint, String> {
    if edits.is_empty() {
        return Err("a source-edit replay needs at least one prompt edit".to_string());
    }
    if edits
        .iter()
        .any(|edit| edit.activation != PromptEditActivation::Origin)
    {
        return Err("automatic source-edit planning requires origin activation".to_string());
    }
    let sources = validated_sources(tape, edits)?;
    let identities: Vec<_> = edits.iter().map(Source::from).collect();
    let mut consumers = vec![None; edits.len()];
    for record in &tape.records {
        let TapeRecordKind::LlmCall {
            call_index,
            attribution: Some(attribution),
            ..
        } = &record.kind
        else {
            continue;
        };
        let call = call_index
            .filter(|index| *index > 0)
            .ok_or("prompt branch planning needs numbered model calls")?;
        for segment in &attribution.segments {
            let literals = sources
                .get(&segment.render_id)
                .ok_or("request attribution names a missing prompt render")?;
            for literal in literals {
                let Some(index) = identities
                    .iter()
                    .position(|source| *source == literal.source)
                else {
                    continue;
                };
                let range = request_range(segment, &literal.range)?;
                for other in &attribution.segments {
                    if other.field != segment.field || other.render_id == segment.render_id {
                        continue;
                    }
                    let overlap = range.start.max(other.start)..range.end.min(other.end);
                    if overlap.start >= overlap.end {
                        continue;
                    }
                    let other_literals = sources
                        .get(&other.render_id)
                        .ok_or("request attribution names a missing prompt render")?;
                    let mut same_source = false;
                    for candidate in other_literals {
                        if candidate.source == literal.source {
                            let candidate_range = request_range(other, &candidate.range)?;
                            same_source |= candidate_range.start <= overlap.start
                                && candidate_range.end >= overlap.end;
                        }
                    }
                    if !same_source {
                        return Err(format!(
                            "prompt edit {}:{}:{} has ambiguous request attribution at model call {call}",
                            literal.source.uri, literal.source.line, literal.source.column
                        ));
                    }
                }
                consumers[index] =
                    Some(consumers[index].map_or(call, |prior: u64| prior.min(call)));
            }
        }
    }
    let missing: Vec<_> = edits
        .iter()
        .zip(&consumers)
        .filter(|(_, call)| call.is_none())
        .map(|(edit, _)| format!("{}:{}:{}", edit.template_uri, edit.line, edit.column))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} prompt edits have no attributed model request: {}",
            missing.len(),
            missing.join(", ")
        ));
    }
    let first_consumers: Vec<_> = consumers.into_iter().flatten().collect();
    let first_live_call = *first_consumers
        .iter()
        .min()
        .expect("nonempty checked edits");
    Ok(PromptBranchPoint {
        first_live_call,
        live_after_calls: first_live_call - 1,
        first_consumers,
    })
}

fn request_range(segment: &AttributedSegment, range: &Range<u64>) -> Result<Range<u64>, String> {
    let start = segment.start.checked_add(range.start);
    let end = segment.start.checked_add(range.end);
    match (start, end) {
        (Some(start), Some(end)) if start < end && end <= segment.end => Ok(start..end),
        _ => Err("recorded prompt attribution does not cover its source literal".to_string()),
    }
}
