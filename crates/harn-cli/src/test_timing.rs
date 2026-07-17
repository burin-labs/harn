use serde::Serialize;

/// Aggregate wall-clock durations for a population of test samples.
///
/// Metrics are `None` when the population is empty so a serialized summary
/// distinguishes "not measured" from an observed zero-millisecond duration.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DurationSummary {
    /// Number of measured samples.
    pub sample_count: u64,
    /// Integer mean, or `None` when there are no samples.
    pub average_ms: Option<u64>,
    /// Median sample using the pinned test-runner percentile convention.
    pub p50_ms: Option<u64>,
    /// 90th-percentile sample.
    pub p90_ms: Option<u64>,
    /// 95th-percentile sample.
    pub p95_ms: Option<u64>,
    /// 99th-percentile sample.
    pub p99_ms: Option<u64>,
}

impl DurationSummary {
    /// Summarize millisecond samples without mutating caller-owned data.
    pub fn from_samples(samples_ms: &[u64]) -> Self {
        if samples_ms.is_empty() {
            return Self::default();
        }

        let mut sorted = samples_ms.to_vec();
        sorted.sort_unstable();

        let average_ms = sorted
            .iter()
            .map(|&sample| u128::from(sample))
            .sum::<u128>()
            / sorted.len() as u128;

        Self {
            sample_count: sorted.len() as u64,
            average_ms: Some(average_ms as u64),
            p50_ms: Some(indexed_percentile(&sorted, 50)),
            p90_ms: Some(indexed_percentile(&sorted, 90)),
            p95_ms: Some(indexed_percentile(&sorted, 95)),
            p99_ms: Some(indexed_percentile(&sorted, 99)),
        }
    }
}

/// Preserve the test renderer's existing zero-based `n * percentile / 100`
/// index semantics while avoiding intermediate multiplication overflow.
fn indexed_percentile(sorted: &[u64], percentile: u8) -> u64 {
    let index = (sorted.len() as u128 * u128::from(percentile) / 100) as usize;
    sorted[index.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::DurationSummary;

    #[test]
    fn empty_samples_have_no_observed_metrics() {
        assert_eq!(
            DurationSummary::from_samples(&[]),
            DurationSummary {
                sample_count: 0,
                average_ms: None,
                p50_ms: None,
                p90_ms: None,
                p95_ms: None,
                p99_ms: None,
            }
        );
    }

    #[test]
    fn one_sample_populates_every_metric() {
        assert_eq!(
            DurationSummary::from_samples(&[17]),
            DurationSummary {
                sample_count: 1,
                average_ms: Some(17),
                p50_ms: Some(17),
                p90_ms: Some(17),
                p95_ms: Some(17),
                p99_ms: Some(17),
            }
        );
    }

    #[test]
    fn even_population_pins_existing_renderer_indices() {
        assert_eq!(
            DurationSummary::from_samples(&[10, 20, 30, 40]),
            DurationSummary {
                sample_count: 4,
                average_ms: Some(25),
                p50_ms: Some(30),
                p90_ms: Some(40),
                p95_ms: Some(40),
                p99_ms: Some(40),
            }
        );
    }

    #[test]
    fn input_order_does_not_change_summary() {
        let ordered = DurationSummary::from_samples(&[1, 2, 3, 4, 5]);
        let shuffled = DurationSummary::from_samples(&[5, 2, 4, 1, 3]);

        assert_eq!(shuffled, ordered);
    }

    #[test]
    fn average_does_not_overflow_u64() {
        let summary = DurationSummary::from_samples(&[u64::MAX, u64::MAX]);

        assert_eq!(summary.average_ms, Some(u64::MAX));
    }
}
