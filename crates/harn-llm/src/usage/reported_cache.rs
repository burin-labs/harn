//! Cache-category presence from validated provider counters.
use serde::{Deserialize, Serialize};

/// Missing categories remain absent; an explicit zero is an observed value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedCacheUsage {
    pub read_tokens: Option<i64>,
    pub write_tokens: Option<i64>,
}

/// Measured cache amounts and the number of calls reporting each category.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct ReportedCacheTotals {
    pub reported_calls: i64,
    pub read_reported_calls: i64,
    pub write_reported_calls: i64,
    pub read_tokens: i64,
    pub write_tokens: i64,
}

impl ReportedCacheTotals {
    pub fn record(&mut self, usage: ReportedCacheUsage) {
        if !usage.has_any() {
            return;
        }
        self.reported_calls = self.reported_calls.saturating_add(1);
        if let Some(tokens) = usage.read_tokens {
            self.read_reported_calls = self.read_reported_calls.saturating_add(1);
            self.read_tokens = self.read_tokens.saturating_add(tokens);
        }
        if let Some(tokens) = usage.write_tokens {
            self.write_reported_calls = self.write_reported_calls.saturating_add(1);
            self.write_tokens = self.write_tokens.saturating_add(tokens);
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.reported_calls = self.reported_calls.saturating_add(other.reported_calls);
        self.read_reported_calls = self
            .read_reported_calls
            .saturating_add(other.read_reported_calls);
        self.write_reported_calls = self
            .write_reported_calls
            .saturating_add(other.write_reported_calls);
        self.read_tokens = self.read_tokens.saturating_add(other.read_tokens);
        self.write_tokens = self.write_tokens.saturating_add(other.write_tokens);
    }

    pub fn since(&self, before: &Self) -> Self {
        Self {
            reported_calls: self.reported_calls.saturating_sub(before.reported_calls),
            read_reported_calls: self
                .read_reported_calls
                .saturating_sub(before.read_reported_calls),
            write_reported_calls: self
                .write_reported_calls
                .saturating_sub(before.write_reported_calls),
            read_tokens: self.read_tokens.saturating_sub(before.read_tokens),
            write_tokens: self.write_tokens.saturating_sub(before.write_tokens),
        }
    }

    pub fn reported_read_tokens(&self) -> Option<i64> {
        (self.read_reported_calls > 0).then_some(self.read_tokens)
    }

    pub fn reported_write_tokens(&self) -> Option<i64> {
        (self.write_reported_calls > 0).then_some(self.write_tokens)
    }
}

impl ReportedCacheUsage {
    pub fn from_value(value: &serde_json::Value) -> Result<Self, &'static str> {
        Ok(Self {
            read_tokens: super::reported_cache_read_tokens(value)?,
            write_tokens: super::reported_cache_write_tokens(value)?,
        })
    }

    pub fn has_any(&self) -> bool {
        self.read_tokens.is_some() || self.write_tokens.is_some()
    }

    pub fn complete_prompt_tokens(&self, fresh: i64) -> Option<i64> {
        super::PromptTokenCounts::from_reported(
            fresh,
            self.read_tokens?,
            self.write_tokens?,
            super::InputTokenBasis::Fresh,
        )
        .ok()
        .map(|counts| counts.total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn omitted_cache_categories_cannot_authorize_prompt_settlement() {
        let usage = ReportedCacheUsage::from_value(&json!({"cache_read_input_tokens": 0})).unwrap();
        assert_eq!(usage.complete_prompt_tokens(7), None);
        let usage = ReportedCacheUsage::from_value(
            &json!({"cache_read_input_tokens": 0, "cache_creation_input_tokens": 20}),
        )
        .unwrap();
        assert_eq!(usage.complete_prompt_tokens(7), Some(27));
        let usage = ReportedCacheUsage::from_value(
            &json!({"cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}),
        )
        .unwrap();
        assert_eq!(usage.complete_prompt_tokens(7), Some(7));
    }

    #[test]
    fn contradictory_or_overflowing_cache_usage_cannot_authorize_settlement() {
        assert!(ReportedCacheUsage::from_value(
            &json!({"cache_read_input_tokens": 5, "cache_read_tokens": 0})
        )
        .is_err());
        let usage = ReportedCacheUsage::from_value(
            &json!({"cache_read_input_tokens": i64::MAX, "cache_creation_input_tokens": 1}),
        )
        .unwrap();
        assert_eq!(usage.complete_prompt_tokens(0), None);
    }

    #[test]
    fn null_cache_counters_remain_unreported_without_masking_measured_zero() {
        let absent = ReportedCacheUsage::from_value(&json!({
            "prompt_tokens_details": {"cached_tokens": null, "cache_write_tokens": null}
        }))
        .unwrap();
        assert_eq!(absent, ReportedCacheUsage::default());
        let measured = ReportedCacheUsage::from_value(&json!({
            "prompt_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0}
        }))
        .unwrap();
        assert_eq!(measured.read_tokens, Some(0));
        assert_eq!(measured.write_tokens, Some(0));
        assert!(ReportedCacheUsage::from_value(
            &json!({"prompt_tokens_details": {"cache_write_tokens": "0"}})
        )
        .is_err());
    }
}
