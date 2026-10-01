//! Provider-confirmed reasoning effort for one completed call.

/// An absent echo is an explicit observation, including on old recordings.
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

/// Who selected the echoed level. `Request` covers callers without a
/// resolution receipt; it must not be presented as an operator selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffortSource {
    Operator,
    Catalog,
    Policy,
    Request,
    ProviderDefault,
    ProviderAdjusted,
}

impl EffectiveReasoningEffort {
    /// Read the Responses echo against the final body, after overrides.
    pub(crate) fn from_responses(response: &serde_json::Value, body: &serde_json::Value) -> Self {
        // Configuration updates supersede the request setting without changing
        // this echo. An opaque previous response may retain such an update.
        // https://developers.openai.com/api/docs/guides/reasoning#change-reasoning-mid-conversation
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

    pub(crate) fn resolve_source(&mut self, selected: ReasoningEffortSource) {
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

pub(crate) fn request_source(opts: &crate::llm::api::LlmCallOptions) -> ReasoningEffortSource {
    if opts
        .provider_overrides
        .as_ref()
        .and_then(|v| v.pointer("/reasoning/effort"))
        .is_some()
    {
        ReasoningEffortSource::Operator
    } else {
        match opts
            .resolution
            .iter()
            .find(|row| row.setting == "reasoning")
            .map(|row| row.source)
        {
            Some("caller.effort" | "caller.thinking") => ReasoningEffortSource::Operator,
            Some("catalog.model_defaults.reasoning_effort") => ReasoningEffortSource::Catalog,
            Some("reasoning_policy") => ReasoningEffortSource::Policy,
            _ => ReasoningEffortSource::Request,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_effort_types_and_selection_provenance_agree() {
        use harn_parser::builtin_signatures::TyExt;
        use harn_parser::{Node, TypeExpr};
        let declarations = harn_parser::parse_source(
            harn_stdlib::get_stdlib_source("llm/envelope").expect("embedded envelope"),
        )
        .expect("envelope parses");
        let alias = |name: &str| {
            declarations
                .iter()
                .find_map(|node| match &node.node {
                    Node::TypeDecl {
                        name: declared,
                        type_expr,
                        ..
                    } if declared == name => Some(type_expr.clone()),
                    _ => None,
                })
                .expect("public alias")
        };
        let sources = alias("ReasoningEffortSource");
        assert_eq!(
            sources,
            harn_builtin_meta::shapes::REASONING_EFFORT_SOURCE.to_type_expr()
        );
        let mut observation = alias("EffectiveReasoningEffort");
        let TypeExpr::Union(variants) = &mut observation else {
            panic!("observation must be a union")
        };
        for variant in variants {
            let TypeExpr::Shape(fields) = variant else {
                panic!("observation arms must be closed records")
            };
            for field in fields {
                if field.name == "source" {
                    assert_eq!(
                        field.type_expr,
                        TypeExpr::Named("ReasoningEffortSource".into())
                    );
                    field.type_expr = sources.clone();
                }
            }
        }
        assert_eq!(
            observation,
            harn_builtin_meta::shapes::EFFECTIVE_REASONING_EFFORT.to_type_expr()
        );
        for (resolved_source, expected) in [
            ("caller.effort", ReasoningEffortSource::Operator),
            ("caller.thinking", ReasoningEffortSource::Operator),
            (
                "catalog.model_defaults.reasoning_effort",
                ReasoningEffortSource::Catalog,
            ),
            ("reasoning_policy", ReasoningEffortSource::Policy),
            ("unset", ReasoningEffortSource::Request),
        ] {
            let mut opts = crate::llm::api::options::base_opts("openai");
            opts.resolution.push(crate::llm::api::ResolvedSetting {
                setting: "reasoning",
                source: resolved_source,
                ..Default::default()
            });
            let mut observation = EffectiveReasoningEffort::Reported {
                level: "high".into(),
                source: ReasoningEffortSource::Request,
            };
            observation.resolve_source(request_source(&opts));
            assert_eq!(
                observation,
                EffectiveReasoningEffort::Reported {
                    level: "high".into(),
                    source: expected
                }
            );
        }
    }

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
