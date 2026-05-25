//! Categorical profile rollup over completed [`crate::tracing::Span`]s.
//!
//! Turns the raw span tree (parent/child, kinds, durations) into a digestible
//! "where did the time go?" answer for harness writers. This module owns no
//! state of its own — it consumes the snapshot returned by
//! [`crate::tracing::peek_spans`] and folds it into a [`RunProfile`] that
//! callers can render to text or serialize to JSON.
//!
//! Top-level wall time and residual ("VM / script overhead") are computed
//! by summing only spans whose parent is the pipeline root (or `None`),
//! so nested LLM/tool work isn't double-counted under both its category
//! and its containing step.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::tracing::Span;

/// Top-N to surface in the rendered profile. Kept small so the stderr
/// summary stays scannable.
const TOP_N: usize = 5;

/// Aggregate breakdown of one run's spans.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RunProfile {
    pub total_wall_ms: u64,
    pub by_kind: Vec<KindBucket>,
    pub residual_ms: u64,
    pub top_llm_calls: Vec<SpanRef>,
    pub top_tool_calls: Vec<SpanRef>,
    pub steps: Vec<StepSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct KindBucket {
    pub kind: String,
    pub total_ms: u64,
    pub count: u64,
    pub pct_of_wall: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SpanRef {
    pub span_id: u64,
    pub kind: String,
    pub name: String,
    pub duration_ms: u64,
    pub step: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct StepSummary {
    pub name: String,
    pub duration_ms: u64,
    pub llm_ms: u64,
    pub tool_ms: u64,
    pub other_ms: u64,
    pub llm_calls: u64,
    pub tool_calls: u64,
}

/// Build a profile from a slice of completed spans. Designed to be called
/// post-run with `crate::tracing::peek_spans()`. Pure function — does not
/// touch globals.
pub fn build(spans: &[Span]) -> RunProfile {
    if spans.is_empty() {
        return RunProfile::default();
    }

    // The pipeline root is conventionally the only top-level VM span. Host
    // adapters may add sibling setup spans, so top-level non-pipeline spans
    // contribute to wall time and their own buckets.
    let total_wall_ms: u64 = spans
        .iter()
        .filter(|s| s.parent_id.is_none())
        .map(|s| s.duration_ms)
        .sum();

    let by_kind = bucket_by_kind(spans, total_wall_ms);

    // Residual = wall - sum of "real work" categories at the top level.
    // Imports/parallel/spawn count as work too; the category buckets cover
    // them so we subtract depth-1 work plus any host setup sibling spans.
    let accounted_total: u64 = spans
        .iter()
        .filter(|s| {
            (s.parent_id.is_none() && !is_profile_root_span(s))
                || matches!(s.parent_id, Some(pid) if is_pipeline_root(spans, pid))
        })
        .map(|s| s.duration_ms)
        .sum();
    let residual_ms = total_wall_ms.saturating_sub(accounted_total);

    let top_llm_calls = top_n_by_duration(spans, "llm_call");
    let top_tool_calls = top_n_by_duration(spans, "tool_call");
    let steps = build_step_summaries(spans);

    RunProfile {
        total_wall_ms,
        by_kind,
        residual_ms,
        top_llm_calls,
        top_tool_calls,
        steps,
    }
}

/// Build one aggregate profile from independent span snapshots.
///
/// Each input snapshot can reuse span ids starting at 1. The helper remaps ids
/// before folding so parent/child relationships do not collide across runs.
pub fn build_aggregate(span_groups: &[Vec<Span>]) -> RunProfile {
    let mut merged = Vec::new();
    let mut next_offset = 0u64;
    for group in span_groups {
        let offset = next_offset;
        let max_id = group.iter().map(|span| span.span_id).max().unwrap_or(0);
        for span in group {
            let mut remapped = span.clone();
            remapped.span_id += offset;
            remapped.parent_id = remapped.parent_id.map(|id| id + offset);
            merged.push(remapped);
        }
        next_offset += max_id + 1;
    }
    build(&merged)
}

fn is_pipeline_root(spans: &[Span], id: u64) -> bool {
    spans
        .iter()
        .find(|s| s.span_id == id)
        .map(is_profile_root_span)
        .unwrap_or(false)
}

fn is_profile_root_span(span: &Span) -> bool {
    span.parent_id.is_none() && span.kind == crate::tracing::SpanKind::Pipeline
}

fn bucket_by_kind(spans: &[Span], total_wall_ms: u64) -> Vec<KindBucket> {
    // Sum per kind across ALL spans of that kind (any depth). This is
    // the user's mental model of "how much LLM time was there in this
    // run?" — overlapping/nested doesn't matter because LLM calls are
    // leaves.
    let mut totals: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for span in spans {
        if is_profile_root_span(span) {
            // Skip the synthetic pipeline span; it's the wall-time
            // denominator, not a category bucket.
            continue;
        }
        let entry = totals.entry(span.kind.as_str().to_string()).or_default();
        entry.0 += span.duration_ms;
        entry.1 += 1;
    }
    let mut buckets: Vec<KindBucket> = totals
        .into_iter()
        .map(|(kind, (total_ms, count))| KindBucket {
            kind,
            total_ms,
            count,
            pct_of_wall: pct(total_ms, total_wall_ms),
        })
        .collect();
    buckets.sort_by_key(|bucket| std::cmp::Reverse(bucket.total_ms));
    buckets
}

fn top_n_by_duration(spans: &[Span], kind: &str) -> Vec<SpanRef> {
    let mut matches: Vec<&Span> = spans.iter().filter(|s| s.kind.as_str() == kind).collect();
    matches.sort_by_key(|span| std::cmp::Reverse(span.duration_ms));
    matches
        .into_iter()
        .take(TOP_N)
        .map(|span| SpanRef {
            span_id: span.span_id,
            kind: span.kind.as_str().to_string(),
            name: span.name.clone(),
            duration_ms: span.duration_ms,
            step: enclosing_step_name(spans, span.parent_id),
            model: span
                .metadata
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
        .collect()
}

fn enclosing_step_name(spans: &[Span], mut parent_id: Option<u64>) -> Option<String> {
    while let Some(pid) = parent_id {
        let parent = spans.iter().find(|s| s.span_id == pid)?;
        if parent.kind.as_str() == "step" {
            return Some(parent.name.clone());
        }
        parent_id = parent.parent_id;
    }
    None
}

fn build_step_summaries(spans: &[Span]) -> Vec<StepSummary> {
    let mut steps: Vec<StepSummary> = Vec::new();
    for step_span in spans.iter().filter(|s| s.kind.as_str() == "step") {
        let mut summary = StepSummary {
            name: step_span.name.clone(),
            duration_ms: step_span.duration_ms,
            ..StepSummary::default()
        };
        for descendant in descendants(spans, step_span.span_id) {
            match descendant.kind.as_str() {
                "llm_call" => {
                    summary.llm_ms += descendant.duration_ms;
                    summary.llm_calls += 1;
                }
                "tool_call" => {
                    summary.tool_ms += descendant.duration_ms;
                    summary.tool_calls += 1;
                }
                _ => {}
            }
        }
        // "other" approximates VM + script overhead inside the step.
        // LLM/tool durations can overlap (parallel calls), so clamp at 0.
        summary.other_ms = summary
            .duration_ms
            .saturating_sub(summary.llm_ms.saturating_add(summary.tool_ms));
        steps.push(summary);
    }
    steps.sort_by_key(|summary| std::cmp::Reverse(summary.duration_ms));
    steps
}

fn descendants(spans: &[Span], root: u64) -> Vec<&Span> {
    let mut out = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for span in spans {
            if span.parent_id == Some(parent) {
                out.push(span);
                frontier.push(span.span_id);
            }
        }
    }
    out
}

fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 / whole as f64) * 100.0
    }
}

/// Render a profile to a human-readable string suitable for stderr after
/// a `harn run --profile` invocation. ANSI-styled to match the existing
/// `--trace` output.
pub fn render(profile: &RunProfile) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "\n\x1b[2m─── Run profile ───\x1b[0m");
    let _ = writeln!(
        out,
        "  Total wall time: {}",
        format_secs(profile.total_wall_ms)
    );
    let _ = writeln!(out, "\n  By category:");
    for bucket in &profile.by_kind {
        let _ = writeln!(
            out,
            "    {:<14} {:>10}  {:>5.1}%   ({} call{})",
            bucket.kind,
            format_secs(bucket.total_ms),
            bucket.pct_of_wall,
            bucket.count,
            if bucket.count == 1 { "" } else { "s" },
        );
    }
    let _ = writeln!(
        out,
        "    {:<14} {:>10}  {:>5.1}%",
        "vm/residual",
        format_secs(profile.residual_ms),
        pct(profile.residual_ms, profile.total_wall_ms),
    );
    if !profile.top_llm_calls.is_empty() {
        let _ = writeln!(out, "\n  Top LLM calls:");
        for span in &profile.top_llm_calls {
            let model = span.model.as_deref().unwrap_or(&span.name);
            let step = span
                .step
                .as_deref()
                .map(|s| format!("  step={s}"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "    #{:<4} {:<24} {:>10}{}",
                span.span_id,
                model,
                format_secs(span.duration_ms),
                step,
            );
        }
    }
    if !profile.top_tool_calls.is_empty() {
        let _ = writeln!(out, "\n  Top tool calls:");
        for span in &profile.top_tool_calls {
            let step = span
                .step
                .as_deref()
                .map(|s| format!("  step={s}"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "    #{:<4} {:<24} {:>10}{}",
                span.span_id,
                span.name,
                format_secs(span.duration_ms),
                step,
            );
        }
    }
    if !profile.steps.is_empty() {
        let _ = writeln!(out, "\n  Per-@step:");
        for step in &profile.steps {
            let _ = writeln!(
                out,
                "    {:<20} {:>10}   (LLM {} · tools {} · other {})",
                step.name,
                format_secs(step.duration_ms),
                format_secs(step.llm_ms),
                format_secs(step.tool_ms),
                format_secs(step.other_ms),
            );
        }
    }
    out
}

fn format_secs(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.3} s", ms as f64 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing::SpanKind;

    fn span(span_id: u64, parent_id: Option<u64>, kind: SpanKind, name: &str, dur: u64) -> Span {
        Span {
            trace_id: "trace_test".to_string(),
            span_id,
            parent_id,
            kind,
            name: name.into(),
            start_ms: 0,
            start_unix_ms: 0,
            duration_ms: dur,
            metadata: BTreeMap::new(),
            links: Vec::new(),
            events: Vec::new(),
        }
    }

    fn span_with_meta(
        span_id: u64,
        parent_id: Option<u64>,
        kind: SpanKind,
        name: &str,
        dur: u64,
        meta: &[(&str, serde_json::Value)],
    ) -> Span {
        let mut s = span(span_id, parent_id, kind, name, dur);
        for (k, v) in meta {
            s.metadata.insert((*k).to_string(), v.clone());
        }
        s
    }

    #[test]
    fn empty_spans_yield_default_profile() {
        let profile = build(&[]);
        assert_eq!(profile.total_wall_ms, 0);
        assert!(profile.by_kind.is_empty());
        assert_eq!(profile.residual_ms, 0);
    }

    #[test]
    fn buckets_are_sorted_descending_by_total() {
        let spans = vec![
            span(1, None, SpanKind::Pipeline, "main", 1000),
            span(2, Some(1), SpanKind::LlmCall, "llm_call", 600),
            span(3, Some(1), SpanKind::ToolCall, "mcp_call", 250),
            span(4, Some(1), SpanKind::ToolCall, "mcp_call", 50),
        ];
        let profile = build(&spans);
        assert_eq!(profile.total_wall_ms, 1000);
        assert_eq!(profile.by_kind[0].kind, "llm_call");
        assert_eq!(profile.by_kind[0].total_ms, 600);
        assert_eq!(profile.by_kind[1].kind, "tool_call");
        assert_eq!(profile.by_kind[1].total_ms, 300);
        assert_eq!(profile.by_kind[1].count, 2);
        // 1000 wall - (600 + 300) depth-1 = 100 ms residual
        assert_eq!(profile.residual_ms, 100);
    }

    #[test]
    fn top_level_vm_setup_span_gets_its_own_bucket() {
        let spans = vec![
            span(1, None, SpanKind::VmSetup, "acp_vm_setup", 20),
            span(2, None, SpanKind::Pipeline, "main", 80),
            span(3, Some(2), SpanKind::LlmCall, "llm_call", 50),
        ];
        let profile = build(&spans);
        assert_eq!(profile.total_wall_ms, 100);
        assert_eq!(profile.residual_ms, 30);
        assert!(profile
            .by_kind
            .iter()
            .any(|bucket| bucket.kind == "vm_setup" && bucket.total_ms == 20));
    }

    #[test]
    fn nested_spans_do_not_double_count_residual() {
        // Pipeline (1000ms) > Step (800ms) > LlmCall (700ms)
        // Depth-1 sum = 800 (the step), residual = 200, NOT 1000-700-800.
        let spans = vec![
            span(1, None, SpanKind::Pipeline, "main", 1000),
            span(2, Some(1), SpanKind::Step, "research", 800),
            span(3, Some(2), SpanKind::LlmCall, "llm_call", 700),
        ];
        let profile = build(&spans);
        assert_eq!(profile.total_wall_ms, 1000);
        assert_eq!(profile.residual_ms, 200);
    }

    #[test]
    fn step_summaries_split_llm_tool_other() {
        let spans = vec![
            span(1, None, SpanKind::Pipeline, "main", 2000),
            span(2, Some(1), SpanKind::Step, "research", 1500),
            span(3, Some(2), SpanKind::LlmCall, "llm_call", 900),
            span(4, Some(2), SpanKind::ToolCall, "mcp_call", 400),
        ];
        let profile = build(&spans);
        assert_eq!(profile.steps.len(), 1);
        let step = &profile.steps[0];
        assert_eq!(step.name, "research");
        assert_eq!(step.duration_ms, 1500);
        assert_eq!(step.llm_ms, 900);
        assert_eq!(step.tool_ms, 400);
        assert_eq!(step.other_ms, 200);
        assert_eq!(step.llm_calls, 1);
        assert_eq!(step.tool_calls, 1);
    }

    #[test]
    fn top_llm_calls_attribute_enclosing_step_and_model() {
        let spans = vec![
            span(1, None, SpanKind::Pipeline, "main", 2000),
            span(2, Some(1), SpanKind::Step, "research", 1500),
            span_with_meta(
                3,
                Some(2),
                SpanKind::LlmCall,
                "llm_call",
                900,
                &[("model", serde_json::json!("claude-sonnet-4-6"))],
            ),
            span(4, Some(1), SpanKind::LlmCall, "llm_call", 100),
        ];
        let profile = build(&spans);
        assert_eq!(profile.top_llm_calls.len(), 2);
        assert_eq!(profile.top_llm_calls[0].duration_ms, 900);
        assert_eq!(profile.top_llm_calls[0].step.as_deref(), Some("research"));
        assert_eq!(
            profile.top_llm_calls[0].model.as_deref(),
            Some("claude-sonnet-4-6")
        );
        assert!(profile.top_llm_calls[1].step.is_none());
    }

    #[test]
    fn render_produces_nonempty_output_for_real_run() {
        let spans = vec![
            span(1, None, SpanKind::Pipeline, "main", 1000),
            span(2, Some(1), SpanKind::LlmCall, "llm_call", 700),
        ];
        let rendered = render(&build(&spans));
        assert!(rendered.contains("Run profile"));
        assert!(rendered.contains("llm_call"));
        assert!(rendered.contains("vm/residual"));
    }

    #[test]
    fn render_for_empty_profile_still_produces_header() {
        // When --profile was requested but no spans landed, we still
        // render the header + a zero-everything residual line — the
        // user explicitly asked for output and an empty string would
        // look like the flag did nothing.
        let rendered = render(&RunProfile::default());
        assert!(rendered.contains("Run profile"));
        assert!(rendered.contains("vm/residual"));
    }

    #[test]
    fn aggregate_remaps_duplicate_span_ids_across_runs() {
        let first = vec![
            span(1, None, SpanKind::Pipeline, "main", 100),
            span(2, Some(1), SpanKind::LlmCall, "llm_call", 40),
        ];
        let second = vec![
            span(1, None, SpanKind::Pipeline, "main", 200),
            span(2, Some(1), SpanKind::ToolCall, "tool", 50),
        ];

        let profile = build_aggregate(&[first, second]);

        assert_eq!(profile.total_wall_ms, 300);
        assert_eq!(profile.by_kind.len(), 2);
        assert!(profile.by_kind.iter().any(|bucket| {
            bucket.kind == "llm_call" && bucket.total_ms == 40 && bucket.count == 1
        }));
        assert!(profile.by_kind.iter().any(|bucket| {
            bucket.kind == "tool_call" && bucket.total_ms == 50 && bucket.count == 1
        }));
    }
}
