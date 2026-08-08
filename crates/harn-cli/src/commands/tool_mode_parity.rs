use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub(crate) const TOOL_MODE_PARITY_OVERLAY_SCHEMA_VERSION: u32 = 1;
pub(crate) const TOOL_MODE_PARITY_FIXTURE_SUITE: &str = "coding-agent";
pub(crate) const TOOL_MODE_PARITY_OVERLAY_FILENAME: &str = "tool_mode_parity_overlay.toml";
pub(crate) const TOOL_MODE_PARITY_DIRECTORY: &str = "parity";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolModeParityFixtureInput {
    pub provider: String,
    pub model: String,
    pub fixture_id: String,
    pub native_verdict: String,
    pub text_verdict: String,
    pub native_passed: bool,
    pub text_passed: bool,
    pub agreement: bool,
    pub verifier_agreement: bool,
    pub native_tool_call_count: usize,
    pub text_tool_call_count: usize,
    pub native_rejected_tool_call_count: usize,
    pub text_rejected_tool_call_count: usize,
    pub native_evidence_path: String,
    pub text_evidence_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ToolModeParityEvidencePaths {
    pub native: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ToolModeParityFixtureReport {
    pub fixture_id: String,
    pub provider: String,
    pub model: String,
    pub native_verdict: String,
    pub text_verdict: String,
    pub native_passed: bool,
    pub text_passed: bool,
    pub agreement: bool,
    pub verifier_agreement: bool,
    pub divergence_class: String,
    pub native_tool_call_count: usize,
    pub text_tool_call_count: usize,
    pub native_rejected_tool_call_count: usize,
    pub text_rejected_tool_call_count: usize,
    pub evidence_paths: ToolModeParityEvidencePaths,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ToolModeParityDivergenceCounts {
    pub native_only_pass: usize,
    pub text_only_pass: usize,
    pub both_pass: usize,
    pub both_fail: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct ToolModeParityFormatStats {
    pub total_runs: usize,
    pub passed_runs: usize,
    pub unique_fixtures: usize,
    pub replicate_count: usize,
    pub pass_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ToolModeParityPairSummary {
    pub provider: String,
    pub model: String,
    pub sample_size: usize,
    pub agreement_rate: f64,
    pub verifier_divergence_rate: f64,
    pub native: ToolModeParityFormatStats,
    pub text: ToolModeParityFormatStats,
    pub divergence_counts: ToolModeParityDivergenceCounts,
    pub evidence_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ToolModeParityOverlay {
    pub schema_version: u32,
    pub generated_at: String,
    pub fixture_suite: String,
    pub rows: Vec<ToolModeParityOverlayRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ToolModeParityOverlayRow {
    pub provider: String,
    pub model: String,
    pub tool_mode_parity: String,
    pub preferred_tool_format: String,
    pub confidence: String,
    pub sample_size: usize,
    pub last_updated: String,
    pub evidence_path: String,
    pub verifier_divergence_rate: f64,
    pub native: ToolModeParityFormatStats,
    pub text: ToolModeParityFormatStats,
}

pub(crate) fn build_fixture_reports(
    inputs: &[ToolModeParityFixtureInput],
) -> Vec<ToolModeParityFixtureReport> {
    inputs
        .iter()
        .map(|input| ToolModeParityFixtureReport {
            fixture_id: input.fixture_id.clone(),
            provider: input.provider.clone(),
            model: input.model.clone(),
            native_verdict: input.native_verdict.clone(),
            text_verdict: input.text_verdict.clone(),
            native_passed: input.native_passed,
            text_passed: input.text_passed,
            agreement: input.agreement,
            verifier_agreement: input.verifier_agreement,
            divergence_class: divergence_class(input.native_passed, input.text_passed),
            native_tool_call_count: input.native_tool_call_count,
            text_tool_call_count: input.text_tool_call_count,
            native_rejected_tool_call_count: input.native_rejected_tool_call_count,
            text_rejected_tool_call_count: input.text_rejected_tool_call_count,
            evidence_paths: ToolModeParityEvidencePaths {
                native: input.native_evidence_path.clone(),
                text: input.text_evidence_path.clone(),
            },
        })
        .collect()
}

pub(crate) fn build_pair_summaries(
    reports: &[ToolModeParityFixtureReport],
) -> Vec<ToolModeParityPairSummary> {
    let mut grouped: BTreeMap<(String, String), Vec<&ToolModeParityFixtureReport>> =
        BTreeMap::new();
    for report in reports {
        grouped
            .entry((report.provider.clone(), report.model.clone()))
            .or_default()
            .push(report);
    }

    grouped
        .into_iter()
        .map(|((provider, model), bucket)| {
            let sample_size = bucket.len();
            let native = format_stats(&bucket, true);
            let text = format_stats(&bucket, false);
            let agreement_rate = ratio(
                bucket.iter().filter(|report| report.agreement).count(),
                sample_size,
            );
            let verifier_divergence_rate = ratio(
                bucket
                    .iter()
                    .filter(|report| !report.verifier_agreement)
                    .count(),
                sample_size,
            );
            let mut evidence_paths = bucket
                .iter()
                .flat_map(|report| {
                    [
                        report.evidence_paths.native.clone(),
                        report.evidence_paths.text.clone(),
                    ]
                })
                .collect::<Vec<_>>();
            evidence_paths.sort();
            evidence_paths.dedup();

            ToolModeParityPairSummary {
                provider,
                model,
                sample_size,
                agreement_rate,
                verifier_divergence_rate,
                native,
                text,
                divergence_counts: ToolModeParityDivergenceCounts {
                    native_only_pass: bucket
                        .iter()
                        .filter(|report| report.divergence_class == "native_only_pass")
                        .count(),
                    text_only_pass: bucket
                        .iter()
                        .filter(|report| report.divergence_class == "text_only_pass")
                        .count(),
                    both_pass: bucket
                        .iter()
                        .filter(|report| report.divergence_class == "both_pass")
                        .count(),
                    both_fail: bucket
                        .iter()
                        .filter(|report| report.divergence_class == "both_fail")
                        .count(),
                },
                evidence_paths,
            }
        })
        .collect()
}

pub(crate) fn build_overlay(
    parity_by_pair: &[ToolModeParityPairSummary],
    generated_at: &str,
    fixture_suite: &str,
    evidence_path: &Path,
) -> ToolModeParityOverlay {
    let rows = parity_by_pair
        .iter()
        .map(|summary| {
            let tool_mode_parity = classify_tool_mode_parity(
                summary.sample_size,
                summary.native.pass_rate,
                summary.text.pass_rate,
            );
            let preferred_tool_format = preferred_tool_format(
                &tool_mode_parity,
                summary.native.pass_rate,
                summary.text.pass_rate,
            );
            let confidence = parity_confidence(summary.sample_size, &summary.native, &summary.text);

            ToolModeParityOverlayRow {
                provider: summary.provider.clone(),
                model: summary.model.clone(),
                tool_mode_parity,
                preferred_tool_format,
                confidence,
                sample_size: summary.sample_size,
                last_updated: generated_at.to_string(),
                evidence_path: evidence_path.display().to_string(),
                verifier_divergence_rate: summary.verifier_divergence_rate,
                native: summary.native.clone(),
                text: summary.text.clone(),
            }
        })
        .collect();

    ToolModeParityOverlay {
        schema_version: TOOL_MODE_PARITY_OVERLAY_SCHEMA_VERSION,
        generated_at: generated_at.to_string(),
        fixture_suite: fixture_suite.to_string(),
        rows,
    }
}

pub(crate) fn write_fixture_report(
    path: &Path,
    report: &ToolModeParityFixtureReport,
) -> Result<(), String> {
    let body = serde_json::to_string_pretty(report)
        .map_err(|error| format!("failed to render {}: {error}", path.display()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    fs::write(path, format!("{body}\n"))
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

pub(crate) fn write_overlay(path: &Path, overlay: &ToolModeParityOverlay) -> Result<(), String> {
    let body = toml::to_string_pretty(overlay)
        .map_err(|error| format!("failed to render {}: {error}", path.display()))?;
    fs::write(path, body).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

pub(crate) fn read_overlay(path: &Path) -> Result<ToolModeParityOverlay, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    toml::from_str(&raw).map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

pub(crate) fn render_promotion_note(row: &ToolModeParityOverlayRow) -> String {
    format!(
        "Empirical coding-agent parity overlay at {} observed native {:.1}% ({}/{}) vs text {:.1}% ({}/{}) across {} fixtures; verifier divergence {:.1}%; confidence {}; updated {}.",
        row.evidence_path,
        row.native.pass_rate * 100.0,
        row.native.passed_runs,
        row.native.total_runs,
        row.text.pass_rate * 100.0,
        row.text.passed_runs,
        row.text.total_runs,
        row.sample_size,
        row.verifier_divergence_rate * 100.0,
        row.confidence,
        row.last_updated
    )
}

fn divergence_class(native_passed: bool, text_passed: bool) -> String {
    match (native_passed, text_passed) {
        (true, false) => "native_only_pass".to_string(),
        (false, true) => "text_only_pass".to_string(),
        (true, true) => "both_pass".to_string(),
        (false, false) => "both_fail".to_string(),
    }
}

fn format_stats(
    reports: &[&ToolModeParityFixtureReport],
    native: bool,
) -> ToolModeParityFormatStats {
    if reports.is_empty() {
        return ToolModeParityFormatStats::default();
    }

    let total_runs = reports.len();
    let passed_runs = reports
        .iter()
        .filter(|report| {
            if native {
                report.native_passed
            } else {
                report.text_passed
            }
        })
        .count();
    let mut by_fixture: BTreeMap<&str, usize> = BTreeMap::new();
    for report in reports {
        *by_fixture.entry(report.fixture_id.as_str()).or_insert(0) += 1;
    }

    ToolModeParityFormatStats {
        total_runs,
        passed_runs,
        unique_fixtures: by_fixture.len(),
        replicate_count: by_fixture.values().copied().min().unwrap_or(0),
        pass_rate: ratio(passed_runs, total_runs),
    }
}

fn classify_tool_mode_parity(
    sample_size: usize,
    native_pass_rate: f64,
    text_pass_rate: f64,
) -> String {
    if sample_size < 5 {
        return "unknown".to_string();
    }
    if native_pass_rate > text_pass_rate && native_pass_rate >= text_pass_rate * 1.5 {
        return "text_unreliable".to_string();
    }
    if text_pass_rate > native_pass_rate && text_pass_rate >= native_pass_rate * 1.5 {
        return "native_unreliable".to_string();
    }
    let high = native_pass_rate.max(text_pass_rate);
    if high == 0.0 || ((native_pass_rate - text_pass_rate).abs() / high) <= 0.2 {
        return "interchangeable".to_string();
    }
    "unknown".to_string()
}

fn preferred_tool_format(
    tool_mode_parity: &str,
    native_pass_rate: f64,
    text_pass_rate: f64,
) -> String {
    match tool_mode_parity {
        "text_unreliable" => "native".to_string(),
        "native_unreliable" => "text".to_string(),
        _ if text_pass_rate > native_pass_rate => "text".to_string(),
        _ => "native".to_string(),
    }
}

fn parity_confidence(
    sample_size: usize,
    native: &ToolModeParityFormatStats,
    text: &ToolModeParityFormatStats,
) -> String {
    if sample_size >= 5 && native.replicate_count >= 2 && text.replicate_count >= 2 {
        "high".to_string()
    } else {
        "low".to_string()
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        (((numerator as f64 / denominator as f64) * 10_000.0).round()) / 10_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_input(
        fixture_id: &str,
        native_passed: bool,
        text_passed: bool,
        verifier_agreement: bool,
    ) -> ToolModeParityFixtureInput {
        ToolModeParityFixtureInput {
            provider: "openrouter".to_string(),
            model: "qwen/qwen3-coder".to_string(),
            fixture_id: fixture_id.to_string(),
            native_verdict: if native_passed {
                "passed".to_string()
            } else {
                "failed".to_string()
            },
            text_verdict: if text_passed {
                "passed".to_string()
            } else {
                "failed".to_string()
            },
            native_passed,
            text_passed,
            agreement: native_passed == text_passed,
            verifier_agreement,
            native_tool_call_count: 1,
            text_tool_call_count: 2,
            native_rejected_tool_call_count: 0,
            text_rejected_tool_call_count: 1,
            native_evidence_path: format!("native/{fixture_id}.jsonl"),
            text_evidence_path: format!("text/{fixture_id}.jsonl"),
        }
    }

    #[test]
    fn fixture_reports_capture_divergence_classes() {
        let reports = build_fixture_reports(&[
            fixture_input("a", true, false, false),
            fixture_input("b", false, true, true),
            fixture_input("c", true, true, true),
            fixture_input("d", false, false, true),
        ]);

        assert_eq!(reports[0].divergence_class, "native_only_pass");
        assert_eq!(reports[1].divergence_class, "text_only_pass");
        assert_eq!(reports[2].divergence_class, "both_pass");
        assert_eq!(reports[3].divergence_class, "both_fail");
    }

    #[test]
    fn pair_summary_aggregates_rates_and_divergence_counts() {
        let reports = build_fixture_reports(&[
            fixture_input("a", false, true, false),
            fixture_input("b", false, true, true),
            fixture_input("c", false, true, false),
            fixture_input("d", true, true, true),
            fixture_input("e", false, true, false),
        ]);
        let summaries = build_pair_summaries(&reports);
        let summary = summaries.first().expect("summary");

        assert_eq!(summary.sample_size, 5);
        assert_eq!(summary.native.pass_rate, 0.2);
        assert_eq!(summary.text.pass_rate, 1.0);
        assert_eq!(summary.agreement_rate, 0.2);
        assert_eq!(summary.verifier_divergence_rate, 0.6);
        assert_eq!(summary.divergence_counts.native_only_pass, 0);
        assert_eq!(summary.divergence_counts.text_only_pass, 4);
        assert_eq!(summary.divergence_counts.both_pass, 1);
        assert_eq!(summary.divergence_counts.both_fail, 0);
    }

    #[test]
    fn overlay_uses_pair_summary_for_classification_and_confidence() {
        let reports = build_fixture_reports(&[
            fixture_input("a", false, true, false),
            fixture_input("b", false, true, true),
            fixture_input("c", false, true, false),
            fixture_input("d", true, true, true),
            fixture_input("e", false, true, false),
        ]);
        let summaries = build_pair_summaries(&reports);
        let overlay = build_overlay(
            &summaries,
            "2026-05-24T00:00:00Z",
            TOOL_MODE_PARITY_FIXTURE_SUITE,
            Path::new(".harn-runs/coding-agent-bench/latest"),
        );

        let row = overlay.rows.first().expect("row");
        assert_eq!(row.sample_size, 5);
        assert_eq!(row.tool_mode_parity, "native_unreliable");
        assert_eq!(row.preferred_tool_format, "text");
        assert_eq!(row.confidence, "low");
        assert_eq!(row.verifier_divergence_rate, 0.6);
    }
}
