use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::llm::tool_conformance::{
    ToolConformanceCase, ToolConformanceReport, ToolProbeCase, ToolProbeFormat,
};

use super::{
    classification_key, rate, upper_percentile_ms, ToolFormatFitnessRecommendation,
    ToolFormatFitnessRecord, ToolFormatFitnessStore,
};

pub const TOOL_FORMAT_FITNESS_SCHEMA_VERSION: u32 = 1;
pub const TOOL_FORMAT_FITNESS_PATH_ENV: &str = "HARN_TOOL_FORMAT_FITNESS_PATH";

static PINNED_TOOL_FORMAT_FITNESS: OnceLock<ToolFormatFitnessStore> = OnceLock::new();

pub fn pinned_tool_format(provider: &str, model: &str) -> Option<String> {
    recommended_tool_format_from_store(pinned_tool_format_fitness(), provider, model)
}

pub fn recommended_tool_format_from_store(
    store: &ToolFormatFitnessStore,
    provider: &str,
    model: &str,
) -> Option<String> {
    (store.schema_version == TOOL_FORMAT_FITNESS_SCHEMA_VERSION)
        .then_some(())
        .and_then(|_| {
            store.recommendations.iter().find(|recommendation| {
                recommendation.provider == provider
                    && recommendation.model == model
                    && matches!(
                        recommendation.tool_format.as_str(),
                        "native" | "json" | "text"
                    )
            })
        })
        .map(|recommendation| recommendation.tool_format.clone())
}

fn pinned_tool_format_fitness() -> &'static ToolFormatFitnessStore {
    PINNED_TOOL_FORMAT_FITNESS.get_or_init(|| {
        let configured = std::env::var_os(TOOL_FORMAT_FITNESS_PATH_ENV)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|raw| parse_fitness_store(&raw));
        configured
            .or_else(|| parse_fitness_store(include_str!("../tool_format_fitness.json")))
            .unwrap_or_else(|| ToolFormatFitnessStore {
                schema_version: TOOL_FORMAT_FITNESS_SCHEMA_VERSION,
                ..ToolFormatFitnessStore::default()
            })
    })
}

fn parse_fitness_store(raw: &str) -> Option<ToolFormatFitnessStore> {
    serde_json::from_str::<ToolFormatFitnessStore>(raw)
        .ok()
        .filter(|store| store.schema_version == TOOL_FORMAT_FITNESS_SCHEMA_VERSION)
}

#[derive(Debug, Default)]
struct FitnessAccumulator {
    attempts: usize,
    successes: usize,
    classifications: BTreeMap<String, usize>,
    latency_ms: Vec<u64>,
    observed_usage_count: usize,
    token_observation_count: usize,
    input_token_count: usize,
    input_tokens: i64,
    output_token_count: usize,
    output_tokens: i64,
}

impl FitnessAccumulator {
    fn record(&mut self, case: &ToolConformanceCase) {
        self.attempts += 1;
        self.successes += usize::from(case.ok);
        *self
            .classifications
            .entry(classification_key(&case.classification).to_string())
            .or_insert(0) += 1;
        if let Some(elapsed_ms) = case.elapsed_ms {
            self.latency_ms.push(elapsed_ms);
        }
        if let Some(usage) = &case.usage {
            self.observed_usage_count += 1;
            self.token_observation_count +=
                usize::from(usage.input_tokens.is_some() || usage.output_tokens.is_some());
            if let Some(tokens) = usage.input_tokens {
                self.input_token_count += 1;
                self.input_tokens = self.input_tokens.saturating_add(tokens.max(0));
            }
            if let Some(tokens) = usage.output_tokens {
                self.output_token_count += 1;
                self.output_tokens = self.output_tokens.saturating_add(tokens.max(0));
            }
        }
    }

    fn pass_rate(&self) -> f64 {
        rate(self.successes, self.attempts)
    }

    fn latency_p50_ms(&self) -> Option<u64> {
        upper_percentile_ms(&self.latency_ms, 50)
    }

    fn latency_p95_ms(&self) -> Option<u64> {
        upper_percentile_ms(&self.latency_ms, 95)
    }

    fn average_tokens(&self) -> Option<f64> {
        (self.token_observation_count > 0).then(|| {
            (self.input_tokens.saturating_add(self.output_tokens)) as f64
                / self.token_observation_count as f64
        })
    }
}

pub fn fitness_store_from_tool_reports(
    reports: &[ToolConformanceReport],
) -> ToolFormatFitnessStore {
    let mut records =
        BTreeMap::<(String, String, ToolProbeFormat, ToolProbeCase), FitnessAccumulator>::new();
    let mut formats = BTreeMap::<(String, String, ToolProbeFormat), FitnessAccumulator>::new();
    for report in reports {
        for case in &report.cases {
            records
                .entry((
                    report.provider.clone(),
                    report.model.clone(),
                    report.tool_format,
                    report.probe_case,
                ))
                .or_default()
                .record(case);
            formats
                .entry((
                    report.provider.clone(),
                    report.model.clone(),
                    report.tool_format,
                ))
                .or_default()
                .record(case);
        }
    }

    let records = records
        .into_iter()
        .map(|((provider, model, tool_format, probe_case), stats)| {
            let pass_rate = stats.pass_rate();
            let latency_p50_ms = stats.latency_p50_ms();
            let latency_p95_ms = stats.latency_p95_ms();
            ToolFormatFitnessRecord {
                provider,
                model,
                tool_format: tool_format.as_str().to_string(),
                probe_case: probe_case.as_str().to_string(),
                attempts: stats.attempts,
                successes: stats.successes,
                pass_rate,
                classification_histogram: stats.classifications,
                observed_latency_count: stats.latency_ms.len(),
                latency_p50_ms,
                latency_p95_ms,
                observed_usage_count: stats.observed_usage_count,
                input_tokens: (stats.input_token_count > 0).then_some(stats.input_tokens),
                output_tokens: (stats.output_token_count > 0).then_some(stats.output_tokens),
            }
        })
        .collect();

    let mut by_route =
        BTreeMap::<(String, String), Vec<(ToolProbeFormat, FitnessAccumulator)>>::new();
    for ((provider, model, format), stats) in formats {
        by_route
            .entry((provider, model))
            .or_default()
            .push((format, stats));
    }
    let mut recommendations = Vec::new();
    for ((provider, model), mut candidates) in by_route {
        candidates.retain(|(_, stats)| stats.successes > 0);
        candidates.sort_by(|(left_format, left), (right_format, right)| {
            compare_pass_rate(right, left)
                .then_with(|| compare_optional_low(left.latency_p50_ms(), right.latency_p50_ms()))
                .then_with(|| {
                    compare_optional_f64_low(left.average_tokens(), right.average_tokens())
                })
                .then_with(|| left_format.cmp(right_format))
        });
        if let Some((format, stats)) = candidates.into_iter().next() {
            recommendations.push(ToolFormatFitnessRecommendation {
                provider,
                model,
                tool_format: format.as_str().to_string(),
                attempts: stats.attempts,
                successes: stats.successes,
                pass_rate: stats.pass_rate(),
                latency_p50_ms: stats.latency_p50_ms(),
                average_tokens: stats.average_tokens(),
            });
        }
    }

    ToolFormatFitnessStore {
        schema_version: TOOL_FORMAT_FITNESS_SCHEMA_VERSION,
        records,
        recommendations,
    }
}

fn compare_pass_rate(left: &FitnessAccumulator, right: &FitnessAccumulator) -> std::cmp::Ordering {
    left.successes
        .saturating_mul(right.attempts)
        .cmp(&right.successes.saturating_mul(left.attempts))
}

fn compare_optional_low<T: Ord>(left: Option<T>, right: Option<T>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn compare_optional_f64_low(left: Option<f64>, right: Option<f64>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.total_cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}
