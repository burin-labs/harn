//! Typed audit contract for provider usage-accounting declarations.
//!
//! The public provider catalog keeps its stable v9 projection: two optional
//! booleans consumed by existing hosts. This source-only registry owns the
//! evidence behind each boolean and the finite queue of fields whose behavior
//! remains unverified. Catalog generation refuses missing, stale, duplicate,
//! or vacuous audit coverage.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use serde::Deserialize;

use super::{ProviderDef, ProvidersConfig};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UsageAccountingAuditRegistry {
    pub reviewed_on: String,
    pub expires_on: String,
    pub tracking_issue: u64,
    #[serde(default)]
    pub unverified: Vec<String>,
    #[serde(default)]
    pub verified: Vec<VerifiedUsageAccountingAudit>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedUsageAccountingAudit {
    pub provider: String,
    pub checked_on: String,
    pub fields: Vec<UsageAccountingField>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum UsageAccountingField {
    Cache,
    Stream,
}

/// Provider-level transport family used by the catalog projection and audit.
///
/// This replaces two copies of the provider-id match that previously decided
/// which rows were OpenAI-shaped. Per-model capability resolution remains the
/// authority for live request dispatch; this enum describes the provider
/// endpoint advertised by the provider catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderWireProtocol {
    AnthropicMessages,
    GeminiGenerateContent,
    VertexGenerateContent,
    BedrockConverse,
    AzureOpenAiChatCompletions,
    OllamaNative,
    OpenAiChatCompletions,
}

impl ProviderWireProtocol {
    pub fn catalog_name(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "anthropic_messages",
            Self::GeminiGenerateContent => "gemini_generate_content",
            Self::VertexGenerateContent => "vertex_generate_content",
            Self::BedrockConverse => "bedrock_converse",
            Self::AzureOpenAiChatCompletions => "azure_openai_chat_completions",
            Self::OllamaNative => "ollama_native",
            Self::OpenAiChatCompletions => "openai_chat_completions",
        }
    }

    pub fn uses_openai_sse(self) -> bool {
        matches!(
            self,
            Self::AzureOpenAiChatCompletions | Self::OpenAiChatCompletions
        )
    }
}

pub fn provider_wire_protocol(id: &str, provider: &ProviderDef) -> ProviderWireProtocol {
    match id {
        "anthropic" => ProviderWireProtocol::AnthropicMessages,
        "gemini" => ProviderWireProtocol::GeminiGenerateContent,
        "vertex" => ProviderWireProtocol::VertexGenerateContent,
        "bedrock" => ProviderWireProtocol::BedrockConverse,
        "azure_openai" => ProviderWireProtocol::AzureOpenAiChatCompletions,
        "ollama" if provider.chat_endpoint.starts_with("/api/") => {
            ProviderWireProtocol::OllamaNative
        }
        _ => ProviderWireProtocol::OpenAiChatCompletions,
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct UsageAccountingAuditValidation {
    pub openai_sse_provider_count: usize,
    pub errors: Vec<String>,
}

impl UsageAccountingAuditValidation {
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }
}

pub fn validate_usage_accounting_audit(
    config: &ProvidersConfig,
    as_of: &str,
) -> UsageAccountingAuditValidation {
    let openai_sse: BTreeSet<&str> = config
        .providers
        .iter()
        .filter(|(id, provider)| provider_wire_protocol(id, provider).uses_openai_sse())
        .map(|(id, _)| id.as_str())
        .collect();
    let mut result = UsageAccountingAuditValidation {
        openai_sse_provider_count: openai_sse.len(),
        errors: Vec::new(),
    };
    if openai_sse.is_empty() {
        result
            .errors
            .push("usage-accounting audit reached no OpenAI-SSE providers".to_string());
    }

    let Some(registry) = &config.usage_accounting_audit else {
        result
            .errors
            .push("usage_accounting_audit registry is missing".to_string());
        return result;
    };

    let as_of = parse_date("usage-accounting audit check date", as_of, &mut result);

    let reviewed_on = parse_date(
        "usage_accounting_audit.reviewed_on",
        &registry.reviewed_on,
        &mut result,
    );
    let expires_on = parse_date(
        "usage_accounting_audit.expires_on",
        &registry.expires_on,
        &mut result,
    );
    if registry.tracking_issue == 0 {
        result
            .errors
            .push("usage_accounting_audit.tracking_issue must be positive".to_string());
    }
    if let (Some(reviewed_on), Some(expires_on)) = (reviewed_on, expires_on) {
        if expires_on < reviewed_on {
            result.errors.push(format!(
                "usage-accounting audit expires_on {} precedes reviewed_on {}",
                registry.expires_on, registry.reviewed_on
            ));
        }
        let has_partial_audit = registry.verified.iter().any(|audit| audit.fields.len() < 2);
        if (!registry.unverified.is_empty() || has_partial_audit)
            && as_of.is_some_and(|as_of| expires_on < as_of)
        {
            result.errors.push(format!(
                "usage-accounting unverified queue expired on {} (tracking issue #{})",
                registry.expires_on, registry.tracking_issue
            ));
        }
    }

    let mut covered = BTreeSet::new();
    for provider in &registry.unverified {
        if !covered.insert(provider.as_str()) {
            result.errors.push(format!(
                "duplicate usage-accounting audit row for {provider}"
            ));
        }
        validate_provider_membership(provider, &openai_sse, &mut result);
        if config.providers.get(provider).is_some_and(|definition| {
            definition.cache_usage_accounting.is_some()
                || definition.stream_usage_accounting.is_some()
        }) {
            result.errors.push(format!(
                "provider {provider} declares usage-accounting fields but has no verified evidence"
            ));
        }
    }

    for audit in &registry.verified {
        if !covered.insert(audit.provider.as_str()) {
            result.errors.push(format!(
                "duplicate usage-accounting audit row for {}",
                audit.provider
            ));
        }
        validate_provider_membership(&audit.provider, &openai_sse, &mut result);
        parse_date(
            &format!(
                "usage_accounting_audit.verified.{}.checked_on",
                audit.provider
            ),
            &audit.checked_on,
            &mut result,
        );
        let fields: BTreeSet<_> = audit.fields.iter().copied().collect();
        if fields.is_empty() {
            result.errors.push(format!(
                "verified provider {} must name at least one usage-accounting field",
                audit.provider
            ));
        }
        if fields.len() != audit.fields.len() {
            result.errors.push(format!(
                "verified provider {} repeats a usage-accounting field",
                audit.provider
            ));
        }
        if audit.sources.is_empty()
            || audit
                .sources
                .iter()
                .any(|source| !source.starts_with("https://"))
        {
            result.errors.push(format!(
                "verified provider {} must cite at least one HTTPS evidence source",
                audit.provider
            ));
        }
        if let Some(provider) = config.providers.get(&audit.provider) {
            validate_field_evidence(
                &audit.provider,
                UsageAccountingField::Cache,
                provider.cache_usage_accounting,
                &fields,
                &mut result,
            );
            validate_field_evidence(
                &audit.provider,
                UsageAccountingField::Stream,
                provider.stream_usage_accounting,
                &fields,
                &mut result,
            );
        }
    }

    for provider in openai_sse.difference(&covered) {
        result.errors.push(format!(
            "OpenAI-SSE provider {provider} has neither verified evidence nor an unverified audit entry"
        ));
    }
    result
}

fn validate_field_evidence(
    provider: &str,
    field: UsageAccountingField,
    declaration: Option<bool>,
    verified: &BTreeSet<UsageAccountingField>,
    result: &mut UsageAccountingAuditValidation,
) {
    let field_name = match field {
        UsageAccountingField::Cache => "cache_usage_accounting",
        UsageAccountingField::Stream => "stream_usage_accounting",
    };
    match (verified.contains(&field), declaration.is_some()) {
        (true, false) => result.errors.push(format!(
            "verified provider {provider} must declare {field_name}"
        )),
        (false, true) => result.errors.push(format!(
            "provider {provider} declares {field_name} without verified evidence"
        )),
        _ => {}
    }
}

fn validate_provider_membership(
    provider: &str,
    openai_sse: &BTreeSet<&str>,
    result: &mut UsageAccountingAuditValidation,
) {
    if !openai_sse.contains(provider) {
        result.errors.push(format!(
            "usage-accounting audit row {provider} does not name an OpenAI-SSE provider"
        ));
    }
}

fn parse_date(
    field: &str,
    value: &str,
    result: &mut UsageAccountingAuditValidation,
) -> Option<NaiveDate> {
    match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        Ok(date) => Some(date),
        Err(_) => {
            result
                .errors
                .push(format!("{field} must be an ISO date, got {value:?}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_audit_covers_every_openai_sse_provider() {
        let config = super::super::embedded_config(None);
        let validation = validate_usage_accounting_audit(&config, "2026-08-25");

        // A census, not a threshold: it moves by exactly one when a provider
        // on this dialect is added, and the move is the point. Meta's Model
        // API is the 40th.
        assert_eq!(validation.openai_sse_provider_count, 40);
        assert!(validation.is_clean(), "{:?}", validation.errors);
    }

    #[test]
    fn audit_fails_when_a_verified_declaration_is_removed() {
        let mut config = super::super::embedded_config(None);
        config
            .providers
            .get_mut("openai")
            .expect("OpenAI provider")
            .cache_usage_accounting = None;

        let validation = validate_usage_accounting_audit(&config, "2026-08-25");
        assert!(validation.errors.iter().any(|error| {
            error.contains("verified provider openai must declare cache_usage_accounting")
        }));
    }

    #[test]
    fn audit_accepts_partial_evidence_without_inventing_the_other_field() {
        let mut config = super::super::embedded_config(None);
        let cerebras = config.providers.get("cerebras").expect("Cerebras provider");
        assert_eq!(cerebras.cache_usage_accounting, Some(true));
        assert_eq!(cerebras.stream_usage_accounting, None);

        let audit = config
            .usage_accounting_audit
            .as_ref()
            .expect("usage accounting audit");
        let cerebras = audit
            .verified
            .iter()
            .find(|entry| entry.provider == "cerebras")
            .expect("Cerebras evidence");
        assert_eq!(cerebras.fields, vec![UsageAccountingField::Cache]);

        let validation = validate_usage_accounting_audit(&config, "2026-08-25");
        assert!(validation.is_clean(), "{:?}", validation.errors);

        config
            .providers
            .get_mut("cerebras")
            .expect("Cerebras provider")
            .cache_usage_accounting = None;
        let validation = validate_usage_accounting_audit(&config, "2026-08-25");
        assert!(validation.errors.iter().any(|error| {
            error.contains("verified provider cerebras must declare cache_usage_accounting")
        }));
    }

    #[test]
    fn audit_refuses_vacuous_provider_input() {
        let config = ProvidersConfig {
            usage_accounting_audit: super::super::embedded_config(None).usage_accounting_audit,
            ..ProvidersConfig::default()
        };

        let validation = validate_usage_accounting_audit(&config, "2026-08-25");
        assert_eq!(validation.openai_sse_provider_count, 0);
        assert!(validation
            .errors
            .iter()
            .any(|error| error.contains("reached no OpenAI-SSE providers")));
    }

    #[test]
    fn audit_expires_the_unverified_queue() {
        let config = super::super::embedded_config(None);
        let validation = validate_usage_accounting_audit(&config, "2026-11-01");

        assert!(validation
            .errors
            .iter()
            .any(|error| error.contains("unverified queue expired")));
    }
}
