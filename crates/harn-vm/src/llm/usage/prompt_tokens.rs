//! Convert provider counters to the cache-inclusive prompt accounting contract.

/// The wire counter's meaning, selected by the adapter that owns its dialect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum InputTokenBasis {
    #[default]
    Inclusive,
    Fresh,
}

/// Reported counters in a saved usage object or one incremental usage frame.
/// Missing counters remain absent until the provider supplies them.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ReportedTokenUsage {
    pub(crate) input_tokens: Option<i64>,
    pub(crate) output_tokens: Option<i64>,
    pub(crate) cache_read_tokens: Option<i64>,
    pub(crate) cache_write_tokens: Option<i64>,
    pub(crate) cache_supported: Option<bool>,
    pub(crate) cache_unreported: bool,
    parse_error: Option<&'static str>,
    input_basis: Option<InputTokenBasis>,
}

impl ReportedTokenUsage {
    pub(crate) fn from_value(value: &serde_json::Value) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        let input_tokens = first_counter(
            value,
            &[
                "input_tokens",
                "prompt_tokens",
                "promptTokenCount",
                "prompt_token_count",
                "inputTokens",
                "total_input_tokens",
            ],
        );
        let output = first_counter(
            value,
            &[
                "output_tokens",
                "completion_tokens",
                "candidatesTokenCount",
                "completion_token_count",
                "outputTokenCount",
                "outputTokens",
                "total_output_tokens",
            ],
        );
        let thoughts = first_counter(value, &["thoughtsTokenCount", "thought_tokens"]);
        let cache_read = super::reported_cache_read_tokens(value);
        let cache_write = super::reported_cache_write_tokens(value);
        let parse_error = [
            input_tokens.err(),
            output.err(),
            thoughts.err(),
            cache_read.err(),
            cache_write.err(),
        ]
        .into_iter()
        .flatten()
        .next();
        let output_tokens = match (output.ok().flatten(), thoughts.ok().flatten()) {
            (Some(output), Some(thoughts)) => Some(output.checked_add(thoughts).unwrap_or(-1)),
            (output, thoughts) => output.or(thoughts),
        };
        // Harn's normalized fields and compatible gateway totals are inclusive
        // even when a gateway also supplies Anthropic cache aliases.
        let input_basis = if [
            "cache_read_tokens",
            "cache_write_tokens",
            "prompt_tokens",
            "promptTokenCount",
            "prompt_token_count",
            "total_input_tokens",
            "input_tokens_details",
            "prompt_tokens_details",
            "cache",
        ]
        .iter()
        .any(|key| object.contains_key(*key))
        {
            Some(InputTokenBasis::Inclusive)
        } else if [
            "inputTokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ]
        .iter()
        .any(|key| object.contains_key(*key))
        {
            Some(InputTokenBasis::Fresh)
        } else {
            None
        };
        Self {
            input_tokens: input_tokens.ok().flatten(),
            output_tokens,
            cache_read_tokens: cache_read.ok().flatten(),
            cache_write_tokens: cache_write.ok().flatten(),
            cache_unreported: value
                .get("cache_visibility")
                .and_then(serde_json::Value::as_str)
                == Some("undeclared")
                || value
                    .get("cache_accounting_declared")
                    .is_some_and(serde_json::Value::is_null),
            parse_error,
            cache_supported: object
                .get("cache_supported")
                .and_then(serde_json::Value::as_bool),
            input_basis,
        }
    }

    pub(crate) fn has_any(self) -> bool {
        self.parse_error.is_some()
            || self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some()
    }

    /// Stream counters are cumulative snapshots, but a frame can update only
    /// one component. Replace reported components without erasing siblings.
    pub(crate) fn merge_reported(&mut self, newer: Self) {
        self.input_tokens = newer.input_tokens.or(self.input_tokens);
        self.output_tokens = newer.output_tokens.or(self.output_tokens);
        self.cache_read_tokens = newer.cache_read_tokens.or(self.cache_read_tokens);
        self.cache_write_tokens = newer.cache_write_tokens.or(self.cache_write_tokens);
        self.cache_supported = newer.cache_supported.or(self.cache_supported);
        self.input_basis = newer.input_basis.or(self.input_basis);
        self.parse_error = newer.parse_error.or(self.parse_error);
        self.cache_unreported |= newer.cache_unreported;
    }

    pub(crate) fn prompt_counts(self) -> Result<Option<PromptTokenCounts>, &'static str> {
        if let Some(error) = self.parse_error {
            return Err(error);
        }
        if self.cache_supported == Some(false)
            && (self.cache_read_tokens.unwrap_or(0) > 0 || self.cache_write_tokens.unwrap_or(0) > 0)
        {
            return Err("cache tokens reported while cache_supported=false");
        }
        self.input_tokens
            .map(|input| {
                PromptTokenCounts::from_reported(
                    input,
                    self.cache_read_tokens.unwrap_or(0),
                    self.cache_write_tokens.unwrap_or(0),
                    self.input_basis.unwrap_or_default(),
                )
            })
            .transpose()
    }
}

fn first_counter(value: &serde_json::Value, keys: &[&str]) -> Result<Option<i64>, &'static str> {
    let mut reported = None;
    for key in keys {
        if let Some(value) = value.get(*key) {
            let count = value.as_i64().ok_or("token counter is not an integer")?;
            if count < 0 {
                return Err("negative token count");
            }
            if reported.is_some_and(|previous| previous != count) {
                return Err("token counter aliases disagree");
            }
            reported = Some(count);
        }
    }
    Ok(reported)
}

/// A validated partition of one prompt. Counts never determine their own basis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PromptTokenCounts {
    pub(crate) total: i64,
    pub(crate) fresh: i64,
}

impl PromptTokenCounts {
    pub(crate) fn from_reported(
        input: i64,
        read: i64,
        write: i64,
        basis: InputTokenBasis,
    ) -> Result<Self, &'static str> {
        if input < 0 || read < 0 || write < 0 {
            return Err("negative token count");
        }
        let cached = read
            .checked_add(write)
            .ok_or("prompt token count overflow")?;
        match basis {
            InputTokenBasis::Inclusive => {
                let fresh = input
                    .checked_sub(cached)
                    .filter(|n| *n >= 0)
                    .ok_or("cache-read + cache-write exceed prompt tokens")?;
                Ok(Self {
                    total: input,
                    fresh,
                })
            }
            InputTokenBasis::Fresh => {
                let total = input
                    .checked_add(cached)
                    .ok_or("prompt token count overflow")?;
                Ok(Self {
                    total,
                    fresh: input,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_basis_preserves_fresh_input_on_both_sides_of_cache_size() {
        for (fresh, read, write) in [(40, 5000, 0), (6000, 5000, 100), (0, 0, 5000)] {
            let raw = PromptTokenCounts::from_reported(fresh, read, write, InputTokenBasis::Fresh)
                .unwrap();
            let inclusive = PromptTokenCounts::from_reported(
                fresh + read + write,
                read,
                write,
                InputTokenBasis::Inclusive,
            )
            .unwrap();
            assert_eq!(raw, inclusive);
            assert_eq!(raw.fresh, fresh);
            assert_eq!(raw.total, fresh + read + write);
        }
    }

    #[test]
    fn invalid_counts_cannot_be_reinterpreted_as_another_basis() {
        assert!(PromptTokenCounts::from_reported(40, 5000, 0, InputTokenBasis::Inclusive).is_err());
        for basis in [InputTokenBasis::Inclusive, InputTokenBasis::Fresh] {
            for (input, read, write) in [(-1, 0, 0), (40, -1, 0), (40, 0, -1), (40, i64::MAX, 1)] {
                assert!(PromptTokenCounts::from_reported(input, read, write, basis).is_err());
            }
        }
        assert!(PromptTokenCounts::from_reported(i64::MAX, 1, 0, InputTokenBasis::Fresh).is_err());
    }
}
