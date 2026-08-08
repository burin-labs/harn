use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use harn_vm::llm::capabilities;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

const SEED_READINESS_TOML: &str = include_str!("../../data/local_model_readiness.toml");
const DEFAULT_REPORT_PATH: &str = ".harn-runs/coding-agent-bench/latest/local_readiness.json";
const DEFAULT_SUMMARY_PATH: &str = ".harn-runs/coding-agent-bench/latest/summary.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocalOutcomeClass {
    Passed,
    ProviderTransportFailure,
    BehavioralFailure,
    UnsupportedCapabilityFailure,
    Skipped,
}

impl LocalOutcomeClass {
    fn rank(self) -> u8 {
        match self {
            Self::Passed => 0,
            Self::ProviderTransportFailure => 1,
            Self::UnsupportedCapabilityFailure => 2,
            Self::BehavioralFailure => 3,
            Self::Skipped => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocalReadinessReport {
    pub schema_version: u32,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    pub recommendations: Vec<LocalModelRecommendation>,
    pub outcomes: Vec<LocalModelOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocalModelRecommendation {
    pub rank: usize,
    pub provider: String,
    pub model: String,
    pub selector: String,
    pub status: String,
    pub recommended_tool_format: Option<String>,
    pub score: i64,
    pub evidence: Vec<String>,
    pub caveats: Vec<String>,
    pub outcome_classes: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocalModelOutcome {
    pub provider: String,
    pub model: String,
    pub selector: String,
    pub tool_format: String,
    pub class: LocalOutcomeClass,
    pub status: String,
    pub passed: bool,
    pub score: i64,
    pub evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_detail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SeedReadiness {
    outcomes: Vec<SeedOutcome>,
}

#[derive(Debug, Deserialize)]
struct SeedOutcome {
    provider: String,
    model: String,
    selector: String,
    tool_format: String,
    class: LocalOutcomeClass,
    status: String,
    score: i64,
    evidence: String,
}

pub(crate) fn report_from_summary_path(path: &Path) -> Result<LocalReadinessReport, String> {
    let value = read_json(path)?;
    report_from_summary_json(&value, path.display().to_string())
}

pub(crate) fn report_from_summary_json(
    summary: &JsonValue,
    source: impl Into<String>,
) -> Result<LocalReadinessReport, String> {
    let outcomes = summary
        .get("runs")
        .and_then(JsonValue::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(outcome_from_run)
                .collect::<Vec<LocalModelOutcome>>()
        })
        .unwrap_or_default();
    Ok(report_from_outcomes(
        source.into(),
        summary
            .get("case_id")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        summary
            .get("output_dir")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        outcomes,
    ))
}

pub(crate) fn load_report_or_summary(path: &Path) -> Result<LocalReadinessReport, String> {
    let value = read_json(path)?;
    if value.get("recommendations").is_some() && value.get("outcomes").is_some() {
        serde_json::from_value(value)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))
    } else {
        report_from_summary_json(&value, path.display().to_string())
    }
}

pub(crate) fn load_default_report() -> Result<LocalReadinessReport, String> {
    load_default_report_from_paths(
        Path::new(DEFAULT_REPORT_PATH),
        Path::new(DEFAULT_SUMMARY_PATH),
    )
}

fn load_default_report_from_paths(
    report_path: &Path,
    summary_path: &Path,
) -> Result<LocalReadinessReport, String> {
    if report_path.exists() {
        let report = load_report_or_summary(report_path)?;
        if !report.outcomes.is_empty() {
            return Ok(report);
        }
    }
    if summary_path.exists() {
        let report = report_from_summary_path(summary_path)?;
        if !report.outcomes.is_empty() {
            return Ok(report);
        }
    }
    seed_report()
}

pub(crate) fn filter_report_by_provider(
    mut report: LocalReadinessReport,
    provider: Option<&str>,
) -> LocalReadinessReport {
    let Some(provider) = provider.map(str::trim).filter(|value| !value.is_empty()) else {
        return report;
    };
    report
        .outcomes
        .retain(|outcome| outcome.provider == provider);
    report
        .recommendations
        .retain(|recommendation| recommendation.provider == provider);
    for (idx, recommendation) in report.recommendations.iter_mut().enumerate() {
        recommendation.rank = idx + 1;
    }
    report
}

pub(crate) fn recommended_models_for_provider(
    provider: &str,
    available_models: &[String],
) -> Vec<String> {
    let report = load_default_report().unwrap_or_else(|_| seed_report_unchecked());
    ordered_models_from_report(&report, provider, available_models)
}

pub(crate) fn selector_for_provider_model(
    provider: &str,
    model: &str,
    tool_format: Option<&str>,
) -> String {
    let mut matches = harn_vm::llm_config::alias_entries()
        .into_iter()
        .filter(|(_, alias)| alias.provider == provider && alias.id == model)
        .collect::<Vec<_>>();
    matches.sort_by(|(name_a, alias_a), (name_b, alias_b)| {
        let rank_a = alias_match_rank(name_a, alias_a.tool_format.as_deref(), tool_format);
        let rank_b = alias_match_rank(name_b, alias_b.tool_format.as_deref(), tool_format);
        rank_a.cmp(&rank_b).then_with(|| name_a.cmp(name_b))
    });
    matches
        .into_iter()
        .next()
        .map(|(name, _)| name)
        .unwrap_or_else(|| {
            if provider == "ollama" {
                format!("ollama:{model}")
            } else {
                format!("{provider}:{model}")
            }
        })
}

fn ordered_models_from_report(
    report: &LocalReadinessReport,
    provider: &str,
    available_models: &[String],
) -> Vec<String> {
    let mut ordered = Vec::new();
    for recommendation in report
        .recommendations
        .iter()
        .filter(|recommendation| recommendation.provider == provider)
    {
        if available_models
            .iter()
            .any(|model| model == &recommendation.model)
            && !ordered.iter().any(|model| model == &recommendation.model)
        {
            ordered.push(recommendation.model.clone());
        }
    }
    for model in available_models {
        if !ordered.iter().any(|existing| existing == model) {
            ordered.push(model.clone());
        }
    }
    ordered
}

fn alias_match_rank(
    name: &str,
    alias_tool_format: Option<&str>,
    desired: Option<&str>,
) -> (u8, u8, usize) {
    let format_rank = match (alias_tool_format, desired) {
        (Some(actual), Some(desired)) if actual == desired => 0,
        (None, None) => 1,
        (None, Some(_)) => 2,
        (Some("text"), None) => 2,
        (Some("native"), None) => 3,
        _ => 4,
    };
    let name_rank = u8::from(name.contains("native"));
    (format_rank, name_rank, name.len())
}

fn seed_report() -> Result<LocalReadinessReport, String> {
    let seed: SeedReadiness = toml::from_str(SEED_READINESS_TOML)
        .map_err(|error| format!("failed to parse bundled local readiness data: {error}"))?;
    let outcomes = seed
        .outcomes
        .into_iter()
        .map(|outcome| LocalModelOutcome {
            provider: outcome.provider,
            model: outcome.model,
            selector: outcome.selector,
            tool_format: outcome.tool_format,
            class: outcome.class,
            status: outcome.status,
            passed: outcome.class == LocalOutcomeClass::Passed,
            score: outcome.score,
            evidence: outcome.evidence,
            run_id: None,
            cleanup_action: None,
            cleanup_detail: None,
        })
        .collect();
    Ok(report_from_outcomes(
        "bundled_seed".to_string(),
        Some("python-add".to_string()),
        None,
        outcomes,
    ))
}

fn seed_report_unchecked() -> LocalReadinessReport {
    seed_report().expect("bundled local readiness data must parse")
}

fn read_json(path: &Path) -> Result<JsonValue, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn outcome_from_run(run: &JsonValue) -> Option<LocalModelOutcome> {
    let selector = run.get("selector")?;
    let provider = selector.get("provider")?.as_str()?.to_string();
    if !is_local_provider(&provider) {
        return None;
    }
    let model = selector.get("model")?.as_str()?.to_string();
    let tool_format = run
        .get("tool_format")
        .and_then(JsonValue::as_str)
        .unwrap_or("unknown")
        .to_string();
    let status = run
        .get("status")
        .and_then(JsonValue::as_str)
        .unwrap_or("unknown")
        .to_string();
    let passed = run
        .get("passed")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let class = classify_run(run, &provider, &model, &tool_format, passed);
    let cleanup = run.get("local_cleanup");
    Some(LocalModelOutcome {
        selector: selector_for_provider_model(&provider, &model, Some(&tool_format)),
        provider,
        model,
        tool_format,
        class,
        status,
        passed,
        score: score_for_class(class, run),
        evidence: evidence_for_run(run, class),
        run_id: run
            .get("run_id")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        cleanup_action: cleanup
            .and_then(|value| value.get("action"))
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        cleanup_detail: cleanup
            .and_then(|value| value.get("detail"))
            .and_then(JsonValue::as_str)
            .map(str::to_string),
    })
}

fn classify_run(
    run: &JsonValue,
    provider: &str,
    model: &str,
    tool_format: &str,
    passed: bool,
) -> LocalOutcomeClass {
    if passed {
        return LocalOutcomeClass::Passed;
    }
    if run
        .get("skipped")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return LocalOutcomeClass::Skipped;
    }
    if capability_unsupported(provider, model, tool_format) {
        return LocalOutcomeClass::UnsupportedCapabilityFailure;
    }
    let haystack = failure_text(run);
    if looks_like_transport_failure(&haystack) {
        return LocalOutcomeClass::ProviderTransportFailure;
    }
    LocalOutcomeClass::BehavioralFailure
}

fn capability_unsupported(provider: &str, model: &str, tool_format: &str) -> bool {
    let caps = capabilities::lookup(provider, model);
    match tool_format {
        "native" => !caps.native_tools,
        "text" => !caps.text_tool_wire_format_supported,
        _ => false,
    }
}

fn failure_text(run: &JsonValue) -> String {
    [
        run.get("status").and_then(JsonValue::as_str),
        run.get("error").and_then(JsonValue::as_str),
        run.get("stderr_excerpt").and_then(JsonValue::as_str),
        run.get("skipped_reason").and_then(JsonValue::as_str),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
    .to_ascii_lowercase()
}

fn looks_like_transport_failure(text: &str) -> bool {
    [
        "transport",
        "http 5",
        "status 5",
        "500",
        "502",
        "503",
        "504",
        "eof",
        "connection reset",
        "connection refused",
        "unreachable",
        "timed out",
        "timeout",
        "broken pipe",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn score_for_class(class: LocalOutcomeClass, run: &JsonValue) -> i64 {
    match class {
        LocalOutcomeClass::Passed => {
            let iterations = run
                .get("iterations")
                .and_then(JsonValue::as_i64)
                .unwrap_or(0)
                .max(0);
            100_i64.saturating_sub(iterations)
        }
        LocalOutcomeClass::ProviderTransportFailure => 35,
        LocalOutcomeClass::UnsupportedCapabilityFailure => 10,
        LocalOutcomeClass::BehavioralFailure => 20,
        LocalOutcomeClass::Skipped => 0,
    }
}

fn evidence_for_run(run: &JsonValue, class: LocalOutcomeClass) -> String {
    let run_id = run
        .get("run_id")
        .and_then(JsonValue::as_str)
        .unwrap_or("unknown-run");
    let status = run
        .get("status")
        .and_then(JsonValue::as_str)
        .unwrap_or("unknown");
    let tool_format = run
        .get("tool_format")
        .and_then(JsonValue::as_str)
        .unwrap_or("unknown");
    let detail = run
        .get("error")
        .and_then(JsonValue::as_str)
        .or_else(|| run.get("skipped_reason").and_then(JsonValue::as_str))
        .or_else(|| run.get("stderr_excerpt").and_then(JsonValue::as_str))
        .map(compact_detail);
    match detail {
        Some(detail) => format!(
            "{run_id} {tool_format}: {status} ({}); {detail}",
            class_key(class)
        ),
        None => format!("{run_id} {tool_format}: {status} ({})", class_key(class)),
    }
}

fn compact_detail(value: &str) -> String {
    const MAX: usize = 240;
    let one_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.len() <= MAX {
        return one_line;
    }
    let mut out = one_line.chars().take(MAX).collect::<String>();
    out.push_str("...");
    out
}

fn report_from_outcomes(
    source: String,
    case_id: Option<String>,
    output_dir: Option<String>,
    outcomes: Vec<LocalModelOutcome>,
) -> LocalReadinessReport {
    let recommendations = build_recommendations(&outcomes);
    LocalReadinessReport {
        schema_version: 1,
        source,
        case_id,
        output_dir,
        recommendations,
        outcomes,
    }
}

fn build_recommendations(outcomes: &[LocalModelOutcome]) -> Vec<LocalModelRecommendation> {
    let mut grouped: BTreeMap<(String, String), Vec<&LocalModelOutcome>> = BTreeMap::new();
    for outcome in outcomes {
        grouped
            .entry((outcome.provider.clone(), outcome.model.clone()))
            .or_default()
            .push(outcome);
    }

    let mut recommendations = grouped
        .into_iter()
        .filter_map(|((provider, model), group)| recommendation_for_group(provider, model, &group))
        .collect::<Vec<_>>();
    recommendations.sort_by(|a, b| {
        recommendation_status_rank(&a.status)
            .cmp(&recommendation_status_rank(&b.status))
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.provider.cmp(&b.provider))
            .then_with(|| a.model.cmp(&b.model))
    });
    for (idx, recommendation) in recommendations.iter_mut().enumerate() {
        recommendation.rank = idx + 1;
    }
    recommendations
}

fn recommendation_for_group(
    provider: String,
    model: String,
    outcomes: &[&LocalModelOutcome],
) -> Option<LocalModelRecommendation> {
    let best = outcomes.iter().min_by(|a, b| {
        a.class
            .rank()
            .cmp(&b.class.rank())
            .then_with(|| b.score.cmp(&a.score))
    })?;
    let status = if outcomes
        .iter()
        .any(|outcome| outcome.class == LocalOutcomeClass::Passed)
    {
        "recommended"
    } else if outcomes
        .iter()
        .any(|outcome| outcome.class == LocalOutcomeClass::ProviderTransportFailure)
    {
        "provider_blocked"
    } else if outcomes
        .iter()
        .all(|outcome| outcome.class == LocalOutcomeClass::Skipped)
    {
        "unranked"
    } else {
        "not_recommended"
    };
    let mut outcome_classes = BTreeMap::new();
    for outcome in outcomes {
        *outcome_classes
            .entry(class_key(outcome.class).to_string())
            .or_insert(0) += 1;
    }
    let caveats = outcomes
        .iter()
        .filter(|outcome| outcome.class != LocalOutcomeClass::Passed)
        .map(|outcome| {
            format!(
                "{} {}: {}",
                outcome.tool_format,
                class_key(outcome.class),
                outcome.evidence
            )
        })
        .collect::<Vec<_>>();
    Some(LocalModelRecommendation {
        rank: 0,
        provider,
        model,
        selector: selector_for_provider_model(&best.provider, &best.model, Some(&best.tool_format)),
        status: status.to_string(),
        recommended_tool_format: (best.class == LocalOutcomeClass::Passed)
            .then(|| best.tool_format.clone()),
        score: outcomes
            .iter()
            .map(|outcome| outcome.score)
            .max()
            .unwrap_or(0),
        evidence: outcomes
            .iter()
            .map(|outcome| outcome.evidence.clone())
            .collect(),
        caveats,
        outcome_classes,
    })
}

fn recommendation_status_rank(status: &str) -> u8 {
    match status {
        "recommended" => 0,
        "provider_blocked" => 1,
        "not_recommended" => 2,
        "unranked" => 3,
        _ => 4,
    }
}

fn class_key(class: LocalOutcomeClass) -> &'static str {
    match class {
        LocalOutcomeClass::Passed => "passed",
        LocalOutcomeClass::ProviderTransportFailure => "provider_transport_failure",
        LocalOutcomeClass::BehavioralFailure => "behavioral_failure",
        LocalOutcomeClass::UnsupportedCapabilityFailure => "unsupported_capability_failure",
        LocalOutcomeClass::Skipped => "skipped",
    }
}

fn is_local_provider(provider: &str) -> bool {
    matches!(
        provider,
        "ollama" | "llamacpp" | "mlx" | "local" | "vllm" | "tgi"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_recommends_passing_model_before_provider_blocked_model() {
        let report = seed_report().expect("seed parses");
        assert_eq!(report.recommendations[0].model, "devstral-small-2:24b");
        assert_eq!(report.recommendations[0].status, "recommended");
        assert_eq!(
            report.recommendations[1].outcome_classes["provider_transport_failure"],
            1
        );
    }

    #[test]
    fn summary_classifies_transport_and_behavioral_failures_separately() {
        let summary = serde_json::json!({
            "case_id": "python-add",
            "output_dir": "out",
            "runs": [
                {
                    "run_id": "ollama_devstral__text",
                    "selector": {"provider": "ollama", "model": "devstral-small-2:24b"},
                    "tool_format": "text",
                    "status": "passed",
                    "passed": true,
                    "skipped": false,
                    "iterations": 2
                },
                {
                    "run_id": "ollama_devstral_blocked__text",
                    "selector": {"provider": "ollama", "model": "devstral-small-2:24b"},
                    "tool_format": "text",
                    "status": "infra_error",
                    "passed": false,
                    "skipped": false,
                    "stderr_excerpt": "Ollama returned HTTP 500: unexpected EOF"
                },
                {
                    "run_id": "ollama_gemma__text",
                    "selector": {"provider": "ollama", "model": "gemma4:26b"},
                    "tool_format": "text",
                    "status": "failed",
                    "passed": false,
                    "skipped": false,
                    "error": "verification failed"
                }
            ]
        });
        let report = report_from_summary_json(&summary, "test").expect("report");
        assert_eq!(report.outcomes[0].class, LocalOutcomeClass::Passed);
        assert_eq!(
            report.outcomes[1].class,
            LocalOutcomeClass::ProviderTransportFailure
        );
        assert_eq!(
            report.outcomes[2].class,
            LocalOutcomeClass::BehavioralFailure
        );
    }

    #[test]
    fn orders_available_models_by_report_then_preserves_unknowns() {
        let report = seed_report().expect("seed parses");
        let available = vec![
            "gemma4:26b".to_string(),
            "devstral-small-2:24b".to_string(),
            "custom:latest".to_string(),
        ];
        assert_eq!(
            ordered_models_from_report(&report, "ollama", &available),
            vec![
                "devstral-small-2:24b".to_string(),
                "gemma4:26b".to_string(),
                "custom:latest".to_string()
            ]
        );
    }

    #[test]
    fn default_loader_ignores_empty_latest_report() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report_path = tmp.path().join("local_readiness.json");
        let summary_path = tmp.path().join("summary.json");
        std::fs::write(
            &report_path,
            r#"{
              "schema_version": 1,
              "source": "empty",
              "recommendations": [],
              "outcomes": []
            }"#,
        )
        .expect("write report");
        std::fs::write(
            &summary_path,
            r#"{
              "case_id": "python-add",
              "runs": [{
                "run_id": "ollama_devstral__text",
                "selector": {"provider": "ollama", "model": "devstral-small-2:24b"},
                "tool_format": "text",
                "status": "passed",
                "passed": true,
                "skipped": false
              }]
            }"#,
        )
        .expect("write summary");

        let loaded =
            load_default_report_from_paths(&report_path, &summary_path).expect("load report");
        assert_eq!(loaded.source, summary_path.display().to_string());
        assert_eq!(loaded.outcomes.len(), 1);
    }
}
