use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub(crate) const TOOL_MODE_PARITY_OVERLAY_SCHEMA_VERSION: u32 = 1;
pub(crate) const TOOL_MODE_PARITY_FIXTURE_SUITE: &str = "coding-agent";
pub(crate) const TOOL_MODE_PARITY_OVERLAY_FILENAME: &str = "tool_mode_parity_overlay.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolModeParityObservation {
    pub provider: String,
    pub model: String,
    pub fixture_id: String,
    pub run_id: String,
    pub tool_format: String,
    pub passed: bool,
    pub skipped: bool,
    pub verification_success: bool,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct ToolModeParityFormatStats {
    pub total_runs: usize,
    pub passed_runs: usize,
    pub unique_fixtures: usize,
    pub replicate_count: usize,
    pub pass_rate: f64,
}

pub(crate) fn build_overlay(
    observations: &[ToolModeParityObservation],
    generated_at: &str,
    fixture_suite: &str,
    evidence_path: &Path,
) -> ToolModeParityOverlay {
    let mut grouped: BTreeMap<(String, String), Vec<&ToolModeParityObservation>> = BTreeMap::new();
    for observation in observations {
        grouped
            .entry((observation.provider.clone(), observation.model.clone()))
            .or_default()
            .push(observation);
    }

    let rows = grouped
        .into_iter()
        .map(|((provider, model), bucket)| {
            let native = format_stats(&bucket, "native");
            let text = format_stats(&bucket, "text");
            let sample_size = native.unique_fixtures.min(text.unique_fixtures);
            let verifier_divergence_rate = verifier_divergence_rate(&bucket);
            let tool_mode_parity =
                classify_tool_mode_parity(sample_size, native.pass_rate, text.pass_rate);
            let preferred_tool_format =
                preferred_tool_format(&tool_mode_parity, native.pass_rate, text.pass_rate);
            let confidence = parity_confidence(sample_size, &native, &text);

            ToolModeParityOverlayRow {
                provider,
                model,
                tool_mode_parity,
                preferred_tool_format,
                confidence,
                sample_size,
                last_updated: generated_at.to_string(),
                evidence_path: evidence_path.display().to_string(),
                verifier_divergence_rate,
                native,
                text,
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

fn format_stats(
    observations: &[&ToolModeParityObservation],
    tool_format: &str,
) -> ToolModeParityFormatStats {
    let filtered = observations
        .iter()
        .copied()
        .filter(|observation| observation.tool_format == tool_format && !observation.skipped)
        .collect::<Vec<_>>();
    if filtered.is_empty() {
        return ToolModeParityFormatStats::default();
    }

    let total_runs = filtered.len();
    let passed_runs = filtered
        .iter()
        .filter(|observation| observation.passed)
        .count();
    let mut by_fixture: BTreeMap<&str, usize> = BTreeMap::new();
    for observation in &filtered {
        *by_fixture
            .entry(observation.fixture_id.as_str())
            .or_insert(0) += 1;
    }

    ToolModeParityFormatStats {
        total_runs,
        passed_runs,
        unique_fixtures: by_fixture.len(),
        replicate_count: by_fixture.values().copied().min().unwrap_or(0),
        pass_rate: ratio(passed_runs, total_runs),
    }
}

fn verifier_divergence_rate(observations: &[&ToolModeParityObservation]) -> f64 {
    let mut native_by_fixture: BTreeMap<&str, Vec<&ToolModeParityObservation>> = BTreeMap::new();
    let mut text_by_fixture: BTreeMap<&str, Vec<&ToolModeParityObservation>> = BTreeMap::new();
    for observation in observations.iter().copied().filter(|obs| !obs.skipped) {
        match observation.tool_format.as_str() {
            "native" => native_by_fixture
                .entry(observation.fixture_id.as_str())
                .or_default()
                .push(observation),
            "text" => text_by_fixture
                .entry(observation.fixture_id.as_str())
                .or_default()
                .push(observation),
            _ => {}
        }
    }

    let shared = native_by_fixture
        .keys()
        .filter(|fixture| text_by_fixture.contains_key(**fixture))
        .copied()
        .collect::<BTreeSet<_>>();
    let mut compared = 0usize;
    let mut diverged = 0usize;
    for fixture in shared {
        let mut native = native_by_fixture.remove(fixture).unwrap_or_default();
        let mut text = text_by_fixture.remove(fixture).unwrap_or_default();
        native.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        text.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        for (native, text) in native.iter().zip(text.iter()) {
            compared += 1;
            if native.verification_success != text.verification_success {
                diverged += 1;
            }
        }
    }
    ratio(diverged, compared)
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

    fn observation(
        fixture_id: &str,
        run_id: &str,
        tool_format: &str,
        passed: bool,
        verification_success: bool,
    ) -> ToolModeParityObservation {
        ToolModeParityObservation {
            provider: "openrouter".to_string(),
            model: "qwen/qwen3-coder".to_string(),
            fixture_id: fixture_id.to_string(),
            run_id: run_id.to_string(),
            tool_format: tool_format.to_string(),
            passed,
            skipped: false,
            verification_success,
        }
    }

    #[test]
    fn overlay_classifies_native_unreliable_and_low_confidence() {
        let overlay = build_overlay(
            &[
                observation("a", "a-native", "native", false, false),
                observation("a", "a-text", "text", true, true),
                observation("b", "b-native", "native", false, false),
                observation("b", "b-text", "text", true, true),
                observation("c", "c-native", "native", false, false),
                observation("c", "c-text", "text", true, true),
                observation("d", "d-native", "native", true, true),
                observation("d", "d-text", "text", true, true),
                observation("e", "e-native", "native", false, false),
                observation("e", "e-text", "text", true, true),
            ],
            "2026-05-24T00:00:00Z",
            TOOL_MODE_PARITY_FIXTURE_SUITE,
            Path::new(".harn-runs/coding-agent-bench/latest"),
        );

        let row = overlay.rows.first().expect("row");
        assert_eq!(row.sample_size, 5);
        assert_eq!(row.tool_mode_parity, "native_unreliable");
        assert_eq!(row.preferred_tool_format, "text");
        assert_eq!(row.confidence, "low");
        assert_eq!(row.native.pass_rate, 0.2);
        assert_eq!(row.text.pass_rate, 1.0);
        assert_eq!(row.verifier_divergence_rate, 0.8);
    }

    #[test]
    fn overlay_requires_two_replicates_for_high_confidence() {
        let mut observations = Vec::new();
        for fixture in ["a", "b", "c", "d", "e"] {
            observations.push(observation(
                fixture,
                &format!("{fixture}-native-1"),
                "native",
                true,
                true,
            ));
            observations.push(observation(
                fixture,
                &format!("{fixture}-native-2"),
                "native",
                true,
                true,
            ));
            observations.push(observation(
                fixture,
                &format!("{fixture}-text-1"),
                "text",
                true,
                true,
            ));
            observations.push(observation(
                fixture,
                &format!("{fixture}-text-2"),
                "text",
                true,
                true,
            ));
        }

        let overlay = build_overlay(
            &observations,
            "2026-05-24T00:00:00Z",
            TOOL_MODE_PARITY_FIXTURE_SUITE,
            Path::new(".harn-runs/coding-agent-bench/latest"),
        );

        let row = overlay.rows.first().expect("row");
        assert_eq!(row.confidence, "high");
        assert_eq!(row.tool_mode_parity, "interchangeable");
        assert_eq!(row.native.replicate_count, 2);
        assert_eq!(row.text.replicate_count, 2);
    }
}
