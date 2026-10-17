//! Provider-confirmed reasoning effort and selection provenance.

/// An absent echo remains explicit, including on old recordings.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectiveReasoningEffort {
    #[default]
    NotReported,
    Reported {
        level: String,
        source: ReasoningEffortSource,
    },
}

/// Who selected the echoed level. An unannotated request is not an operator pin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffortSource {
    Operator,
    Catalog,
    Policy,
    #[default]
    Request,
    ProviderDefault,
    ProviderAdjusted,
}

impl EffectiveReasoningEffort {
    /// Read the Responses echo against the final body, after overrides.
    pub fn from_responses(response: &serde_json::Value, body: &serde_json::Value) -> Self {
        // An opaque previous response may retain a configuration update that
        // supersedes the request setting without changing this echo.
        if body
            .get("previous_response_id")
            .is_some_and(|id| !id.is_null())
            || body
                .get("input")
                .and_then(|input| input.as_array())
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("type").and_then(|v| v.as_str()) == Some("configuration_update")
                    })
                })
        {
            return Self::NotReported;
        }
        let Some(level) = response
            .pointer("/reasoning/effort")
            .and_then(|v| v.as_str())
            .filter(|level| !level.trim().is_empty())
        else {
            return Self::NotReported;
        };
        let source = match body.pointer("/reasoning/effort").and_then(|v| v.as_str()) {
            None => ReasoningEffortSource::ProviderDefault,
            Some(requested) if requested == level => ReasoningEffortSource::Request,
            Some(_) => ReasoningEffortSource::ProviderAdjusted,
        };
        Self::Reported {
            level: level.to_string(),
            source,
        }
    }

    pub fn resolve_source(&mut self, selected: ReasoningEffortSource) {
        let Self::Reported {
            source: ReasoningEffortSource::Request,
            level,
        } = self
        else {
            return;
        };
        *self = Self::Reported {
            level: level.clone(),
            source: selected,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_requires_a_level_and_cannot_override_hidden_configuration() {
        let echo = serde_json::json!({"reasoning": {"effort": "medium"}});
        assert_eq!(
            EffectiveReasoningEffort::from_responses(&echo, &serde_json::json!({})),
            EffectiveReasoningEffort::Reported {
                level: "medium".into(),
                source: ReasoningEffortSource::ProviderDefault,
            }
        );
        for response in [
            serde_json::json!({}),
            serde_json::json!({"reasoning": {"effort": null}}),
            serde_json::json!({"reasoning": {"effort": 4}}),
            serde_json::json!({"reasoning": {"effort": " "}}),
        ] {
            assert_eq!(
                EffectiveReasoningEffort::from_responses(
                    &response,
                    &serde_json::json!({"reasoning": {"effort": "high"}})
                ),
                EffectiveReasoningEffort::NotReported
            );
        }
        for body in [
            serde_json::json!({"previous_response_id": "resp_previous"}),
            serde_json::json!({"input": [{"type": "configuration_update", "reasoning": {"effort": "high"}}]}),
        ] {
            assert_eq!(
                EffectiveReasoningEffort::from_responses(&echo, &body),
                EffectiveReasoningEffort::NotReported
            );
        }
        assert_eq!(
            EffectiveReasoningEffort::from_responses(
                &echo,
                &serde_json::json!({"reasoning": {"effort": "high"}})
            ),
            EffectiveReasoningEffort::Reported {
                level: "medium".into(),
                source: ReasoningEffortSource::ProviderAdjusted,
            }
        );
    }
}
