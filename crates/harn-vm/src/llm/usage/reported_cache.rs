//! Cache-category presence from validated provider counters.
use serde::{Deserialize, Serialize};

/// Missing categories remain absent; an explicit zero is an observed value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedCacheUsage {
    pub read_tokens: Option<i64>,
    pub write_tokens: Option<i64>,
}

impl ReportedCacheUsage {
    pub(crate) fn from_value(value: &serde_json::Value) -> Result<Self, &'static str> {
        Ok(Self {
            read_tokens: super::reported_cache_read_tokens(value)?,
            write_tokens: super::reported_cache_write_tokens(value)?,
        })
    }

    pub(crate) fn has_any(&self) -> bool {
        self.read_tokens.is_some() || self.write_tokens.is_some()
    }

    /// Each streamed frame updates only the categories it actually reports.
    pub(crate) fn merge_value(
        &mut self,
        value: &serde_json::Value,
    ) -> Result<(), crate::value::VmError> {
        let newer = Self::from_value(value).map_err(|reason| {
            crate::value::VmError::Runtime(format!("invalid provider cache usage: {reason}"))
        })?;
        self.read_tokens = newer.read_tokens.or(self.read_tokens);
        self.write_tokens = newer.write_tokens.or(self.write_tokens);
        Ok(())
    }

    pub(crate) fn complete_prompt_tokens(&self, fresh: i64) -> Option<i64> {
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
        let mut usage =
            ReportedCacheUsage::from_value(&json!({"cache_read_input_tokens": 0})).unwrap();
        assert_eq!(usage.complete_prompt_tokens(7), None);
        usage
            .merge_value(&json!({"cache_creation_input_tokens": 20}))
            .unwrap();
        assert_eq!(usage.complete_prompt_tokens(7), Some(27));
        usage.merge_value(&json!({"output_tokens": 3})).unwrap();
        assert_eq!(usage.complete_prompt_tokens(7), Some(27));
        usage
            .merge_value(&json!({"cache_creation_input_tokens": 0}))
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
}
