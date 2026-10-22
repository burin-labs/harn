//! Escalation-target selection: given the model a run started on, pick the
//! stronger catalog model it should switch to after repeated verification
//! failures. A run that cannot escalate gets a machine-readable
//! `fallback_code` and a route diagnostic instead of an error, so a missing or
//! partial catalog degrades to "no target" rather than failing the agent loop.
use serde::Serialize;

use super::reviewer::{model_selector_refs, pricing_total, refs_contain_selector};
use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct EscalationTargetOptions {
    pub primary_model: String,
    /// Require a usable provider credential on the target. Defaults to `true`
    /// at the builtin boundary.
    pub available_only: bool,
    /// Cap the target's input-plus-output list price at this multiple of the
    /// primary's. A priced primary is required for the cap to apply, and an
    /// unpriced candidate cannot pass it.
    pub max_price_multiple: Option<f64>,
    /// Model ids, providers, `provider/id` pairs, families, or lineages to skip.
    pub exclude: Vec<String>,
}

/// A selected target, shaped as a `ModelLadderStep` plus selection evidence.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EscalationTarget {
    pub provider: String,
    pub model: String,
    pub label: String,
    pub family: String,
    /// Set only when the target's preferred native channel is known to be
    /// unreliable: the reliable channel dispatch should use instead. Kept off
    /// the step's `options` because a ladder rejects non-sampling overrides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_format: Option<&'static str>,
    pub score: f64,
    pub reasons: Vec<String>,
}

/// Stable reasons no target was chosen. Serialized as the `fallback_code`
/// string; new variants are additive and existing codes are append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationFallbackCode {
    /// The primary matches no catalog id, alias, or delimiter-bounded id prefix.
    UnknownPrimary,
    /// The primary is already capable, or no catalog model is a stronger,
    /// routable alternative to it.
    NoStrongerModel,
    /// Stronger models exist but every one was named in `exclude`.
    AllExcluded,
    /// Stronger models exist but none fits `max_price_multiple`.
    NoModelWithinPrice,
    /// Stronger models exist but no provider serving one has a usable credential.
    NoAvailableCredential,
}

impl EscalationFallbackCode {
    pub fn as_code(self) -> &'static str {
        match self {
            Self::UnknownPrimary => "unknown_primary",
            Self::NoStrongerModel => "no_stronger_model",
            Self::AllExcluded => "all_excluded",
            Self::NoModelWithinPrice => "no_model_within_price",
            Self::NoAvailableCredential => "no_available_credential",
        }
    }
}

/// Why the selector did or did not choose a target, for a host to display.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct EscalationRouteDiagnostic {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<EscalationPrimary>,
    pub primary_already_capable: bool,
    /// Catalog rows examined, including the primary itself.
    pub considered: usize,
    pub skipped: EscalationSkips,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EscalationPrimary {
    pub id: String,
    pub provider: String,
    pub family: String,
    pub lineage: String,
    pub tier: String,
    /// How the requested selector reached this row: `id`, `alias`, or `id_prefix`.
    pub matched_by: &'static str,
}

/// Candidates dropped, each counted once at the first rule that dropped it.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct EscalationSkips {
    pub not_text_generation: usize,
    pub deprecated: usize,
    pub dedicated: usize,
    pub not_hosted: usize,
    pub not_strong_enough: usize,
    pub is_primary: usize,
    pub same_lineage_sidegrade: usize,
    pub excluded: usize,
    pub over_price: usize,
    pub no_credential: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EscalationSelection {
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub target: Option<EscalationTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub route_diagnostic: EscalationRouteDiagnostic,
}

/// Rank, score, reasons, id, row, and tool-channel facts of the leading candidate.
type BestCandidate<'a> = (
    u8,
    f64,
    Vec<String>,
    &'a str,
    &'a ModelDef,
    ToolChannelFacts,
);

/// Per-route tool-channel facts the ranking reads from the capability registry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolChannelFacts {
    pub tools: bool,
    pub text: bool,
    /// The preferred channel is `native` but the route's parity says it
    /// silently corrupts multiline tool bodies.
    pub native_unreliable: bool,
}

/// Rank `config` for the target. Credential presence and tool-channel facts
/// come from the dispatch layer, so the caller supplies them.
pub fn pick_escalation_target(
    config: &ProvidersConfig,
    options: &EscalationTargetOptions,
    provider_is_available: impl Fn(&str) -> bool,
    channel: impl Fn(&str, &str) -> ToolChannelFacts,
) -> EscalationSelection {
    let mut diagnostic = EscalationRouteDiagnostic {
        considered: config.models.len(),
        ..Default::default()
    };
    let fallback = |code: EscalationFallbackCode,
                    reason: &str,
                    diagnostic: EscalationRouteDiagnostic| EscalationSelection {
        target: None,
        fallback_code: Some(code.as_code()),
        reason: Some(reason.to_string()),
        route_diagnostic: diagnostic,
    };

    let Some(primary) = resolve_primary(config, &options.primary_model) else {
        return fallback(
            EscalationFallbackCode::UnknownPrimary,
            "primary model matches no catalog id, alias, or id prefix",
            diagnostic,
        );
    };
    let primary_id = primary.id;
    let primary_channel = channel(&primary.provider, primary_id);
    let primary_family = model_family_with_config(config, &primary.provider, primary_id);
    let primary_lineage = model_lineage_with_config(config, &primary.provider, primary_id);
    let primary_tier = model_tier_with_config(config, primary_id);
    diagnostic.primary = Some(EscalationPrimary {
        id: primary_id.to_string(),
        provider: primary.provider.clone(),
        family: primary_family.clone(),
        lineage: primary_lineage.clone(),
        tier: primary_tier.clone(),
        matched_by: primary.matched_by,
    });
    diagnostic.primary_already_capable =
        is_already_capable(primary.row, &primary_tier, primary_channel);
    if diagnostic.primary_already_capable {
        return fallback(
            EscalationFallbackCode::NoStrongerModel,
            "primary model is already capable",
            diagnostic,
        );
    }

    // A ceiling relative to a price the catalog does not state cannot be
    // computed, and an uncapped answer to a capped question is wrong.
    let primary_total = primary
        .row
        .and_then(|row| pricing_total(row.pricing.as_ref()))
        .filter(|total| *total > 0.0);
    let price_cap = options
        .max_price_multiple
        .zip(primary_total)
        .map(|(multiple, total)| multiple * total);
    if options.max_price_multiple.is_some() && price_cap.is_none() {
        return fallback(
            EscalationFallbackCode::NoModelWithinPrice,
            "the primary has no positive catalog price, so max_price_multiple cannot be applied",
            diagnostic,
        );
    }

    let skips = &mut diagnostic.skipped;
    let mut eligible = Vec::new();
    for (id, model) in &config.models {
        if id.eq_ignore_ascii_case(primary_id)
            || id.eq_ignore_ascii_case(options.primary_model.trim())
        {
            skips.is_primary += 1;
            continue;
        }
        if !model.supports_operation(crate::llm_config::ModelOperation::TextGeneration) {
            skips.not_text_generation += 1;
            continue;
        }
        if model.deprecated {
            skips.deprecated += 1;
            continue;
        }
        if model.availability == ModelAvailability::Dedicated {
            skips.dedicated += 1;
            continue;
        }
        if !config
            .providers
            .get(&model.provider)
            .is_some_and(|provider| {
                harn_provider_catalog::provider_classification(provider)
                    == harn_provider_catalog::ProviderClassification::Hosted
            })
        {
            skips.not_hosted += 1;
            continue;
        }
        let tier = model_tier_with_config(config, id);
        let facts = channel(&model.provider, id);
        if !is_strong_enough(model, &tier, facts) {
            skips.not_strong_enough += 1;
            continue;
        }
        let lineage = model_lineage_with_config(config, &model.provider, id);
        if !lineage.is_empty()
            && lineage == primary_lineage
            && tier_rank(&tier) <= tier_rank(&primary_tier)
        {
            skips.same_lineage_sidegrade += 1;
            continue;
        }
        eligible.push((id, model, tier, lineage, facts));
    }
    if eligible.is_empty() {
        return fallback(
            EscalationFallbackCode::NoStrongerModel,
            "no catalog model is a stronger routable alternative to the primary",
            diagnostic,
        );
    }

    let before = eligible.len();
    eligible.retain(|(id, model, _, lineage, _)| {
        let family = model_family_with_config(config, &model.provider, id);
        let refs = model_selector_refs(id, &model.provider, &family, lineage);
        !options
            .exclude
            .iter()
            .any(|selector| refs_contain_selector(&refs, selector))
    });
    diagnostic.skipped.excluded = before - eligible.len();
    if eligible.is_empty() {
        return fallback(
            EscalationFallbackCode::AllExcluded,
            "every stronger model was named in exclude",
            diagnostic,
        );
    }

    if let Some(cap) = price_cap {
        let before = eligible.len();
        eligible.retain(|(_, model, ..)| {
            pricing_total(model.pricing.as_ref()).is_some_and(|total| total <= cap)
        });
        diagnostic.skipped.over_price = before - eligible.len();
        if eligible.is_empty() {
            return fallback(
                EscalationFallbackCode::NoModelWithinPrice,
                "no stronger model fits max_price_multiple",
                diagnostic,
            );
        }
    }

    if options.available_only {
        let before = eligible.len();
        eligible.retain(|(_, model, ..)| provider_is_available(&model.provider));
        diagnostic.skipped.no_credential = before - eligible.len();
        if eligible.is_empty() {
            return fallback(
                EscalationFallbackCode::NoAvailableCredential,
                "no provider serving a stronger model has a usable credential",
                diagnostic,
            );
        }
    }

    let mut best: Option<BestCandidate> = None;
    for (id, model, tier, lineage, facts) in eligible {
        let family = model_family_with_config(config, &model.provider, id);
        let (score, reasons) = score_candidate(
            model,
            &tier,
            &family,
            &lineage,
            facts,
            &primary_family,
            &primary_lineage,
            primary.row,
        );
        let rank = tier_rank(&tier);
        let better =
            best.as_ref()
                .is_none_or(|(best_rank, best_score, _, best_id, best_model, _)| {
                    rank.cmp(best_rank)
                        .then(
                            score
                                .partial_cmp(best_score)
                                .unwrap_or(std::cmp::Ordering::Equal),
                        )
                        .then_with(|| best_model.provider.cmp(&model.provider))
                        .then_with(|| (*best_id).cmp(id.as_str()))
                        .is_gt()
                });
        if better {
            best = Some((rank, score, reasons, id.as_str(), model, facts));
        }
    }
    let (_, score, reasons, id, model, facts) = best.expect("eligible set is non-empty");
    let tool_format = facts
        .native_unreliable
        .then_some(if facts.text { "text" } else { "json" });
    EscalationSelection {
        target: Some(EscalationTarget {
            provider: model.provider.clone(),
            model: id.to_string(),
            label: model.name.clone(),
            family: model_family_with_config(config, &model.provider, id),
            tool_format,
            score,
            reasons,
        }),
        fallback_code: None,
        reason: None,
        route_diagnostic: diagnostic,
    }
}

/// The model a run started on, as far as the catalog can name it.
struct ResolvedPrimary<'a> {
    id: &'a str,
    /// The alias's provider when the selector was an alias, else the row's.
    provider: String,
    /// The catalog row, absent for an alias whose target has none.
    row: Option<&'a ModelDef>,
    matched_by: &'static str,
}

/// Resolve the primary by catalog id, then alias, then the longest catalog id
/// that bounds an unknown revision suffix (`model-1-preview` -> `model-1`).
/// A raw substring never matches.
fn resolve_primary<'a>(config: &'a ProvidersConfig, selector: &str) -> Option<ResolvedPrimary<'a>> {
    let selector = selector.trim();
    if selector.is_empty() {
        return None;
    }
    let row = |id: &'a String, model: &'a ModelDef, matched_by| ResolvedPrimary {
        id,
        provider: model.provider.clone(),
        row: Some(model),
        matched_by,
    };
    if let Some((id, model)) = config.models.get_key_value(selector) {
        return Some(row(id, model, "id"));
    }
    if let Some((id, model)) = config
        .models
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(selector))
    {
        return Some(row(id, model, "id"));
    }
    if let Some(alias) = config.aliases.get(selector) {
        return Some(ResolvedPrimary {
            id: &alias.id,
            provider: alias.provider.clone(),
            row: config.models.get(&alias.id),
            matched_by: "alias",
        });
    }
    let lowered = selector.to_ascii_lowercase();
    config
        .models
        .iter()
        .filter(|(id, _)| {
            let id = id.to_ascii_lowercase();
            ["-", ":", "/", ".", "@"]
                .iter()
                .any(|separator| lowered.starts_with(&format!("{id}{separator}")))
        })
        .max_by_key(|(id, _)| id.len())
        .map(|(id, model)| row(id, model, "id_prefix"))
}

fn tier_rank(tier: &str) -> u8 {
    match tier.trim().to_ascii_lowercase().as_str() {
        "reasoning" => 4,
        "frontier" => 3,
        "mid" => 2,
        "small" => 1,
        _ => 0,
    }
}

fn claims(model: &ModelDef, strength: &str) -> bool {
    model.strengths.iter().any(|claimed| claimed == strength)
}

/// A primary with no catalog row claims no strengths and is judged by its tier
/// alone.
fn is_already_capable(model: Option<&ModelDef>, tier: &str, facts: ToolChannelFacts) -> bool {
    !model.is_some_and(|model| model.deprecated)
        && (tier_rank(tier) >= tier_rank("frontier")
            || model.is_some_and(|model| {
                claims(model, "agentic")
                    || (claims(model, "coding") && (claims(model, "tool_use") || facts.tools))
            }))
}

/// Worth escalating TO: frontier or above, a claimed agentic or coding
/// strength, or a tool-capable mid-tier model.
fn is_strong_enough(model: &ModelDef, tier: &str, facts: ToolChannelFacts) -> bool {
    tier_rank(tier) >= tier_rank("frontier")
        || claims(model, "agentic")
        || claims(model, "coding")
        || (facts.tools && tier_rank(tier) >= tier_rank("mid"))
}

/// Tier dominates; strengths, tool capability, serving shape, and context add
/// capability signal; family and lineage diversity and declared
/// complementarity favor clearing a model-specific blind spot; an unreliable
/// native channel loses ties without ever excluding a sole option.
#[allow(clippy::too_many_arguments)]
fn score_candidate(
    model: &ModelDef,
    tier: &str,
    family: &str,
    lineage: &str,
    facts: ToolChannelFacts,
    primary_family: &str,
    primary_lineage: &str,
    primary: Option<&ModelDef>,
) -> (f64, Vec<String>) {
    let mut score = f64::from(tier_rank(tier)) * 100.0;
    let mut reasons = vec![format!("tier {tier}")];
    let mut add = |points: f64, reason: &str, applies: bool| {
        if applies {
            score += points;
            reasons.push(reason.to_string());
        }
    };
    add(45.0, "claims coding strength", claims(model, "coding"));
    add(45.0, "claims agentic strength", claims(model, "agentic"));
    add(35.0, "claims tool_use strength", claims(model, "tool_use"));
    add(
        25.0,
        "claims reasoning strength",
        claims(model, "reasoning"),
    );
    add(25.0, "supports tools", facts.tools);
    add(
        15.0,
        "serverless",
        model.availability == ModelAvailability::Serverless,
    );
    let window = model.runtime_context_window.unwrap_or(model.context_window);
    add(
        (window / 100_000).min(30) as f64,
        "context window",
        window >= 100_000,
    );
    add(
        55.0,
        "different family from primary",
        !family.is_empty() && family != primary_family,
    );
    add(
        35.0,
        "different lineage from primary",
        !lineage.is_empty() && lineage != primary_lineage,
    );
    add(
        65.0,
        "declared complementary with primary",
        model.complementary_with.iter().any(|f| f == primary_family)
            || primary
                .is_some_and(|primary| primary.complementary_with.iter().any(|f| f == family)),
    );
    add(
        -60.0,
        "native tool channel is unreliable",
        facts.native_unreliable,
    );
    (score, reasons)
}
