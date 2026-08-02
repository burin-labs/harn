//! Deterministic aggregation for benchmark receipts.
//!
//! Hosts own clocks and sample capture. Keeping aggregation here gives native,
//! browser, and future edge adapters one definition of the receipt statistics
//! without giving the portable kernel clock authority.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{opcode_abi_fingerprint, semantic_abi_fingerprint_hex, DataValue, ARTIFACT_VERSION};

mod schema;
pub use schema::portable_benchmark_json_schema;

pub const PORTABLE_BENCHMARK_SCHEMA_VERSION: &str = "harn.portable_kernel.benchmark.v1";
pub const PORTABLE_MAX_DISPATCH_ITERATIONS: usize = 1_000_000;
pub const PORTABLE_MAX_COMPILE_ITERATIONS: usize = 100_000;
pub const PORTABLE_MAX_WORKERS: usize = 256;

/// Closed, shared receipt emitted by native and browser benchmark adapters.
/// Hosts own clocks and sample capture; the kernel owns field names,
/// aggregation, provenance, bounds, and serialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableBenchmarkReceipt {
    pub schema_version: String,
    pub target: BenchmarkTarget,
    pub source: String,
    pub entry: String,
    pub entry_kind: BenchmarkEntryKind,
    pub artifact_bytes: usize,
    pub artifact_digest: String,
    pub iterations: usize,
    pub workers: usize,
    pub provenance: BenchmarkProvenance,
    pub initialization_ms: Option<f64>,
    pub compile: CompileMeasurements,
    pub decode: Option<BenchmarkStatistics>,
    pub dispatch: DispatchMeasurements,
    pub terminal_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkTarget {
    Native,
    Browser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkEntryKind {
    Function,
    Pipeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkBuildProfile {
    Debug,
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BenchmarkProvenance {
    pub harn_version: String,
    pub kernel_version: String,
    pub artifact_format_version: u16,
    pub semantic_abi_fingerprint: String,
    pub opcode_abi_fingerprint: String,
    pub build_profile: BenchmarkBuildProfile,
    pub os: String,
    pub arch: String,
}

impl BenchmarkProvenance {
    pub fn current(
        harn_version: impl Into<String>,
        build_profile: BenchmarkBuildProfile,
        os: impl Into<String>,
        arch: impl Into<String>,
    ) -> Self {
        Self {
            harn_version: harn_version.into(),
            kernel_version: crate::KERNEL_VERSION.to_string(),
            artifact_format_version: ARTIFACT_VERSION,
            semantic_abi_fingerprint: semantic_abi_fingerprint_hex(),
            opcode_abi_fingerprint: opcode_abi_fingerprint()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            build_profile,
            os: os.into(),
            arch: arch.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompileMeasurements {
    pub first_ms: f64,
    pub repeated: BenchmarkStatistics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DispatchMeasurements {
    pub first_ms: f64,
    pub repeated: BenchmarkStatistics,
    pub batch_wall_ms: f64,
    pub throughput_per_second: f64,
}

impl PortableBenchmarkReceipt {
    /// Validate the closed cross-host receipt contract before persistence.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != PORTABLE_BENCHMARK_SCHEMA_VERSION {
            return Err("portable benchmark schema version is not supported".to_string());
        }
        if self.source.is_empty() || self.entry.is_empty() || self.artifact_bytes == 0 {
            return Err("portable benchmark identity fields must not be empty".to_string());
        }
        if self.iterations == 0 || self.iterations > PORTABLE_MAX_DISPATCH_ITERATIONS {
            return Err(
                "portable benchmark dispatch iteration count is outside limits".to_string(),
            );
        }
        if self.workers == 0 || self.workers > PORTABLE_MAX_WORKERS {
            return Err("portable benchmark worker count is outside limits".to_string());
        }
        if self.workers > self.iterations {
            return Err(
                "portable benchmark workers must not exceed dispatch iterations".to_string(),
            );
        }
        if self.compile.repeated.iterations == 0
            || self.compile.repeated.iterations > PORTABLE_MAX_COMPILE_ITERATIONS
        {
            return Err("portable benchmark compile iteration count is outside limits".to_string());
        }
        if self.dispatch.repeated.iterations != self.iterations {
            return Err(
                "portable benchmark dispatch statistics do not match iterations".to_string(),
            );
        }
        if let Some(decode) = self.decode {
            if decode.iterations != self.compile.repeated.iterations {
                return Err(
                    "portable benchmark decode statistics do not match compilation samples"
                        .to_string(),
                );
            }
        }
        match self.target {
            BenchmarkTarget::Native
                if self.initialization_ms.is_some() || self.decode.is_none() =>
            {
                return Err("native portable benchmarks require decode samples and no adapter initialization".to_string());
            }
            BenchmarkTarget::Browser
                if self.initialization_ms.is_none() || self.decode.is_some() =>
            {
                return Err("browser portable benchmarks require adapter initialization and include decode in dispatch".to_string());
            }
            BenchmarkTarget::Native | BenchmarkTarget::Browser => {}
        }
        if !is_digest(&self.artifact_digest)
            || !is_digest(&self.terminal_digest)
            || !is_digest(&self.provenance.semantic_abi_fingerprint)
            || !is_digest(&self.provenance.opcode_abi_fingerprint)
        {
            return Err("portable benchmark digests must be lowercase 32-byte hex".to_string());
        }
        if self.provenance.harn_version.is_empty()
            || self.provenance.kernel_version.is_empty()
            || self.provenance.os.is_empty()
            || self.provenance.arch.is_empty()
            || self.provenance.artifact_format_version == 0
        {
            return Err("portable benchmark provenance is incomplete".to_string());
        }
        for value in [
            self.initialization_ms.unwrap_or(0.0),
            self.compile.first_ms,
            self.dispatch.first_ms,
            self.dispatch.batch_wall_ms,
            self.dispatch.throughput_per_second,
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(
                    "portable benchmark measurements must be finite and non-negative".to_string(),
                );
            }
        }
        for statistics in [
            &self.compile.repeated,
            &self.dispatch.repeated,
            self.decode.as_ref().unwrap_or(&self.compile.repeated),
        ] {
            if !valid_statistics(statistics) {
                return Err(
                    "portable benchmark statistics must be finite and non-negative".to_string(),
                );
            }
        }
        if self.dispatch.batch_wall_ms == 0.0 || self.dispatch.throughput_per_second == 0.0 {
            return Err("portable benchmark batch measurements must be positive".to_string());
        }
        Ok(())
    }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_statistics(statistics: &BenchmarkStatistics) -> bool {
    statistics.iterations > 0
        && [
            statistics.min_ms,
            statistics.mean_ms,
            statistics.p50_ms,
            statistics.p95_ms,
            statistics.max_ms,
            statistics.stddev_ms,
            statistics.total_ms,
        ]
        .into_iter()
        .all(|value| value.is_finite() && value >= 0.0)
}

/// Digest the canonical tagged-JSON representation used in benchmark receipts.
///
/// Native, browser, and future adapters call this projection instead of
/// choosing their own JSON normalization or hash input.
pub fn benchmark_terminal_digest(value: &DataValue) -> String {
    blake3::hash(value.to_json().to_string().as_bytes())
        .to_hex()
        .to_string()
}

/// Aggregate wall-time statistics used by portable benchmark receipts.
///
/// Percentiles use R-7 linear interpolation, the default used by common
/// statistical tools. Inputs must be finite, non-negative millisecond values.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BenchmarkStatistics {
    pub iterations: usize,
    pub min_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    pub stddev_ms: f64,
    pub total_ms: f64,
}

impl BenchmarkStatistics {
    /// Aggregate a finite, non-empty set of elapsed-time samples.
    ///
    /// The population standard deviation is reported because a benchmark
    /// receipt describes every observed iteration rather than estimating a
    /// larger population from a sample.
    pub fn from_samples(
        samples: impl IntoIterator<Item = f64>,
    ) -> Result<Self, BenchmarkStatisticsError> {
        let mut sorted = samples.into_iter().collect::<Vec<_>>();
        if sorted.is_empty() {
            return Err(BenchmarkStatisticsError::Empty);
        }
        for (index, sample) in sorted.iter_mut().enumerate() {
            if !sample.is_finite() {
                return Err(BenchmarkStatisticsError::NonFinite { index });
            }
            if *sample < 0.0 {
                return Err(BenchmarkStatisticsError::Negative { index });
            }
            // Equal elapsed times must have one byte-level representation on
            // every host; JavaScript and native clocks can otherwise surface
            // signed zero differently in receipts.
            if *sample == 0.0 {
                *sample = 0.0;
            }
        }

        sorted.sort_by(f64::total_cmp);
        let iterations = sorted.len();
        let total_ms = compensated_sum(sorted.iter().copied());
        if !total_ms.is_finite() {
            return Err(BenchmarkStatisticsError::AggregateOverflow);
        }
        let mean_ms = total_ms / iterations as f64;
        let variance = compensated_sum(sorted.iter().map(|sample| {
            let delta = sample - mean_ms;
            delta * delta
        })) / iterations as f64;
        if !variance.is_finite() {
            return Err(BenchmarkStatisticsError::AggregateOverflow);
        }

        Ok(Self {
            iterations,
            min_ms: sorted[0],
            mean_ms,
            p50_ms: percentile_r7(&sorted, 0.50),
            p95_ms: percentile_r7(&sorted, 0.95),
            max_ms: sorted[iterations - 1],
            stddev_ms: variance.sqrt(),
            total_ms,
        })
    }
}

/// Deterministic reason benchmark samples could not be aggregated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkStatisticsError {
    Empty,
    NonFinite { index: usize },
    Negative { index: usize },
    AggregateOverflow,
}

impl BenchmarkStatisticsError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "benchmark_samples_empty",
            Self::NonFinite { .. } => "benchmark_sample_non_finite",
            Self::Negative { .. } => "benchmark_sample_negative",
            Self::AggregateOverflow => "benchmark_aggregate_overflow",
        }
    }
}

impl fmt::Display for BenchmarkStatisticsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("benchmark samples must not be empty"),
            Self::NonFinite { index } => {
                write!(formatter, "benchmark sample {index} must be finite")
            }
            Self::Negative { index } => {
                write!(formatter, "benchmark sample {index} must not be negative")
            }
            Self::AggregateOverflow => {
                formatter.write_str("benchmark sample aggregate exceeds finite range")
            }
        }
    }
}

impl std::error::Error for BenchmarkStatisticsError {}

fn percentile_r7(sorted: &[f64], probability: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = probability * (sorted.len() - 1) as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let weight = rank - lower as f64;
        sorted[lower] * (1.0 - weight) + sorted[upper] * weight
    }
}

fn compensated_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut sum = 0.0;
    let mut compensation = 0.0;
    for value in values {
        let corrected = value - compensation;
        let next = sum + corrected;
        compensation = (next - sum) - corrected;
        sum = next;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_unsorted_samples_with_r7_percentiles() {
        let stats = BenchmarkStatistics::from_samples([30.0, 10.0, 40.0, 20.0]).unwrap();

        assert_eq!(stats.iterations, 4);
        assert_eq!(stats.min_ms, 10.0);
        assert_eq!(stats.mean_ms, 25.0);
        assert_eq!(stats.p50_ms, 25.0);
        assert_eq!(stats.p95_ms, 38.5);
        assert_eq!(stats.max_ms, 40.0);
        assert_eq!(stats.stddev_ms, 125.0_f64.sqrt());
        assert_eq!(stats.total_ms, 100.0);
    }

    #[test]
    fn single_sample_has_zero_variance_and_exact_percentiles() {
        let stats = BenchmarkStatistics::from_samples([1.25]).unwrap();

        assert_eq!(stats.p50_ms, 1.25);
        assert_eq!(stats.p95_ms, 1.25);
        assert_eq!(stats.stddev_ms, 0.0);
    }

    #[test]
    fn normalizes_signed_zero_for_cross_host_receipts() {
        let stats = BenchmarkStatistics::from_samples([-0.0, 0.0]).unwrap();

        assert_eq!(stats.min_ms.to_bits(), 0.0_f64.to_bits());
        assert_eq!(stats.max_ms.to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn rejects_empty_or_invalid_elapsed_times() {
        assert_eq!(
            BenchmarkStatistics::from_samples([]).unwrap_err(),
            BenchmarkStatisticsError::Empty
        );
        assert_eq!(
            BenchmarkStatistics::from_samples([1.0, f64::NAN]).unwrap_err(),
            BenchmarkStatisticsError::NonFinite { index: 1 }
        );
        assert_eq!(
            BenchmarkStatistics::from_samples([1.0, f64::INFINITY]).unwrap_err(),
            BenchmarkStatisticsError::NonFinite { index: 1 }
        );
        assert_eq!(
            BenchmarkStatistics::from_samples([1.0, -0.1]).unwrap_err(),
            BenchmarkStatisticsError::Negative { index: 1 }
        );
        assert_eq!(
            BenchmarkStatistics::from_samples([f64::MAX, f64::MAX]).unwrap_err(),
            BenchmarkStatisticsError::AggregateOverflow
        );
    }

    #[test]
    fn serialized_field_names_match_the_receipt_contract() {
        let stats = BenchmarkStatistics::from_samples([10.0, 20.0]).unwrap();
        let value = serde_json::to_value(stats).unwrap();

        assert_eq!(value["iterations"], 2);
        assert_eq!(value["p50_ms"], 15.0);
        assert_eq!(value["p95_ms"], 19.5);
    }

    #[test]
    fn terminal_digest_uses_canonical_tagged_json() {
        let left = DataValue::Record(std::collections::BTreeMap::from([
            ("z".to_string(), DataValue::Float(f64::NAN)),
            ("a".to_string(), DataValue::Int(i64::MAX)),
        ]));
        let right = DataValue::from_json(left.to_json()).unwrap();

        assert_eq!(
            benchmark_terminal_digest(&left),
            benchmark_terminal_digest(&right)
        );
    }
}
