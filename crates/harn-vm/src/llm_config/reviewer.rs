//! Complementary-reviewer selection: pick a different-family, price-capped
//! reviewer model for review/critique/plan-review workloads, with
//! deterministic fallback to the author model.
use std::collections::BTreeSet;

use serde::Serialize;

use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct ComplementaryReviewerOptions {
    pub author_model: String,
    pub author_provider: Option<String>,
    pub intent: ComplementaryReviewerIntent,
    pub max_price_multiplier: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplementaryReviewerIntent {
    Review,
    Critique,
    PlanReview,
}

impl ComplementaryReviewerIntent {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "review" => Some(Self::Review),
            "critique" => Some(Self::Critique),
            "plan_review" => Some(Self::PlanReview),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Critique => "critique",
            Self::PlanReview => "plan_review",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComplementaryReviewerSelection {
    pub intent: String,
    pub author: ComplementaryModelIdentity,
    pub reviewer: ComplementaryModelIdentity,
    pub fallback: bool,
    pub fallback_reason: Option<String>,
    /// Machine-readable reason a caller can branch on when `fallback` is
    /// `true`, distinct from the human-readable `fallback_reason`/`reason`
    /// prose. `None` on the success path. Lets a caller hard-fail an
    /// independent-review step rather than silently degrade to self-review.
    /// See [`ReviewerFallbackCode`] for the stable set of values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_code: Option<String>,
    pub reason: String,
    pub estimated_incremental_cost: Option<ComplementaryCostEstimate>,
}

/// Stable, machine-readable reasons `pick_complementary_reviewer` falls back
/// to the author model. Serialized as the `fallback_code` string so harn
/// pipelines and Rust callers can branch deterministically instead of parsing
/// prose. New variants are additive; existing codes are append-only contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerFallbackCode {
    /// The author model's family could not be resolved, so no independent
    /// family comparison is possible.
    UnknownAuthorFamily,
    /// Different-family candidates exist but none satisfy `max_price_multiplier`.
    NoDiffFamilyWithinPrice,
    /// No active, serverless, different-family reviewer is cataloged at all.
    NoDiffFamilyServerless,
    /// Different-family candidates exist but were all excluded (e.g. every
    /// one declares `avoid_as_reviewer_for` the author).
    AllDiffFamilyExcluded,
}

impl ReviewerFallbackCode {
    pub fn as_code(self) -> &'static str {
        match self {
            Self::UnknownAuthorFamily => "unknown_author_family",
            Self::NoDiffFamilyWithinPrice => "no_diff_family_within_price",
            Self::NoDiffFamilyServerless => "no_diff_family_serverless",
            Self::AllDiffFamilyExcluded => "all_diff_family_excluded",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComplementaryModelIdentity {
    pub id: String,
    pub provider: String,
    pub family: String,
    pub lineage: String,
    pub tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComplementaryCostEstimate {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub total_per_mtok: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiplier_vs_author: Option<f64>,
}

pub fn pick_complementary_reviewer(
    options: ComplementaryReviewerOptions,
) -> ComplementaryReviewerSelection {
    let config = effective_config();
    let mut author = resolve_model_info(&options.author_model);
    if let Some(provider) = options
        .author_provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
    {
        author.provider = provider.to_string();
        author.family = model_family_with_config(&config, &author.provider, &author.id);
        author.lineage = model_lineage_with_config(&config, &author.provider, &author.id);
        author.tool_format = default_tool_format_with_config(&config, &author.id, &author.provider);
    }
    let author_entry = config.models.get(&author.id);
    let author_identity = complementary_identity(
        author.id.clone(),
        author.provider.clone(),
        author.family.clone(),
        author.lineage.clone(),
        author.tier.clone(),
        author_entry.and_then(|model| model.pricing.clone()),
    );

    let fallback =
        |code: ReviewerFallbackCode, fallback_reason: String| ComplementaryReviewerSelection {
            intent: options.intent.as_str().to_string(),
            reviewer: author_identity.clone(),
            estimated_incremental_cost: cost_estimate(
                author_identity.pricing.as_ref(),
                author_identity.pricing.as_ref(),
            ),
            author: author_identity.clone(),
            fallback: true,
            reason: format!(
                "using author model {} because {fallback_reason}",
                author_identity.id
            ),
            fallback_reason: Some(fallback_reason),
            fallback_code: Some(code.as_code().to_string()),
        };

    if author_identity.family == "unknown" {
        return fallback(
            ReviewerFallbackCode::UnknownAuthorFamily,
            "author model family is unknown".to_string(),
        );
    }

    let preferred_families = author_entry
        .map(|model| model.complementary_with.clone())
        .unwrap_or_default();
    let author_refs = reviewer_match_refs(&author_identity);
    let mut rejected_by_price = 0usize;
    let mut diff_family_seen = 0usize;
    let mut candidates = Vec::new();

    for (id, model) in config.models.iter() {
        if id == &author_identity.id && model.provider == author_identity.provider {
            continue;
        }
        if !eligible_reviewer_model(model) {
            continue;
        }
        let family = model_family_with_config(&config, &model.provider, id);
        if family == "unknown" || family == author_identity.family {
            continue;
        }
        diff_family_seen += 1;
        let lineage = model_lineage_with_config(&config, &model.provider, id);
        let candidate_identity = complementary_identity(
            id.clone(),
            model.provider.clone(),
            family,
            lineage,
            model_tier_with_config(&config, id),
            model.pricing.clone(),
        );
        if model
            .avoid_as_reviewer_for
            .iter()
            .any(|selector| refs_contain_selector(&author_refs, selector))
        {
            continue;
        }
        if exceeds_price_cap(
            author_identity.pricing.as_ref(),
            candidate_identity.pricing.as_ref(),
            options.max_price_multiplier,
        ) {
            rejected_by_price += 1;
            continue;
        }
        let score = reviewer_score(
            &options,
            &author_identity,
            &candidate_identity,
            model,
            &preferred_families,
        );
        candidates.push(ReviewerCandidate {
            identity: candidate_identity,
            score,
        });
    }

    candidates.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.identity.provider.cmp(&right.identity.provider))
            .then_with(|| left.identity.id.cmp(&right.identity.id))
    });

    let Some(best) = candidates.into_iter().next() else {
        if rejected_by_price > 0 {
            let cap = options.max_price_multiplier.unwrap_or_default();
            return fallback(
                ReviewerFallbackCode::NoDiffFamilyWithinPrice,
                format!("no different-family reviewer satisfied max_price_multiplier {cap}"),
            );
        }
        if diff_family_seen == 0 {
            return fallback(
                ReviewerFallbackCode::NoDiffFamilyServerless,
                "no active serverless different-family reviewer is cataloged".to_string(),
            );
        }
        return fallback(
            ReviewerFallbackCode::AllDiffFamilyExcluded,
            "all different-family reviewer candidates were excluded".to_string(),
        );
    };

    let estimate = cost_estimate(
        best.identity.pricing.as_ref(),
        author_identity.pricing.as_ref(),
    );
    ComplementaryReviewerSelection {
        intent: options.intent.as_str().to_string(),
        reason: reviewer_reason(&author_identity, &best.identity, estimate.as_ref()),
        estimated_incremental_cost: estimate,
        author: author_identity,
        reviewer: best.identity,
        fallback: false,
        fallback_reason: None,
        fallback_code: None,
    }
}

#[derive(Debug, Clone)]
struct ReviewerCandidate {
    identity: ComplementaryModelIdentity,
    score: f64,
}

fn complementary_identity(
    id: String,
    provider: String,
    family: String,
    lineage: String,
    tier: String,
    pricing: Option<ModelPricing>,
) -> ComplementaryModelIdentity {
    ComplementaryModelIdentity {
        id,
        provider,
        family,
        lineage,
        tier,
        pricing,
    }
}

fn reviewer_score(
    options: &ComplementaryReviewerOptions,
    author: &ComplementaryModelIdentity,
    candidate: &ComplementaryModelIdentity,
    model: &ModelDef,
    preferred_families: &[String],
) -> f64 {
    let candidate_refs = reviewer_match_refs(candidate);
    let mut score = 0.0;
    if let Some(rank) = preferred_families
        .iter()
        .position(|selector| refs_contain_selector(&candidate_refs, selector))
    {
        score += 1_000.0 - rank as f64;
    }
    if candidate.provider != author.provider {
        score += 100.0;
    }
    score += match tier_distance(&author.tier, &candidate.tier) {
        0 => 80.0,
        1 => 45.0,
        2 => 15.0,
        _ => 0.0,
    };
    for strength in intent_strengths(options.intent) {
        if model.strengths.iter().any(|tag| tag == strength) {
            score += 8.0;
        }
    }
    if model.capabilities.iter().any(|tag| tag == "tools") {
        score += 4.0;
    }
    if let (Some(author_total), Some(candidate_total)) = (
        pricing_total(author.pricing.as_ref()),
        pricing_total(candidate.pricing.as_ref()),
    ) {
        if author_total > 0.0 {
            let ratio = candidate_total / author_total;
            if ratio <= 1.0 {
                score += 20.0;
            }
            score -= (ratio - 1.0).abs().min(10.0) * 8.0;
        }
    }
    score
}

fn eligible_reviewer_model(model: &ModelDef) -> bool {
    !model.deprecated
        && model.availability == ModelAvailability::Serverless
        && !model.quality_tags.iter().any(|tag| tag == "avoid_reviewer")
}

fn intent_strengths(intent: ComplementaryReviewerIntent) -> &'static [&'static str] {
    match intent {
        ComplementaryReviewerIntent::Review => &["reasoning", "coding", "tool_use"],
        ComplementaryReviewerIntent::Critique => &["reasoning", "long_context", "tool_use"],
        ComplementaryReviewerIntent::PlanReview => {
            &["reasoning", "coding", "agentic", "long_context", "tool_use"]
        }
    }
}

fn tier_distance(left: &str, right: &str) -> u8 {
    let left = tier_rank(left);
    let right = tier_rank(right);
    left.abs_diff(right)
}

fn tier_rank(tier: &str) -> u8 {
    match tier {
        "small" => 0,
        "mid" => 1,
        "frontier" | "reasoning" => 2,
        _ => 1,
    }
}

fn exceeds_price_cap(
    author_pricing: Option<&ModelPricing>,
    candidate_pricing: Option<&ModelPricing>,
    max_price_multiplier: Option<f64>,
) -> bool {
    let Some(max_price_multiplier) = max_price_multiplier else {
        return false;
    };
    let Some(author_total) = pricing_total(author_pricing) else {
        return false;
    };
    let Some(candidate_total) = pricing_total(candidate_pricing) else {
        return true;
    };
    author_total > 0.0 && candidate_total > author_total * max_price_multiplier
}

fn cost_estimate(
    reviewer_pricing: Option<&ModelPricing>,
    author_pricing: Option<&ModelPricing>,
) -> Option<ComplementaryCostEstimate> {
    let reviewer_pricing = reviewer_pricing?;
    let total_per_mtok = reviewer_pricing.input_per_mtok + reviewer_pricing.output_per_mtok;
    let multiplier_vs_author = pricing_total(author_pricing)
        .filter(|author_total| *author_total > 0.0)
        .map(|author_total| total_per_mtok / author_total);
    Some(ComplementaryCostEstimate {
        input_per_mtok: reviewer_pricing.input_per_mtok,
        output_per_mtok: reviewer_pricing.output_per_mtok,
        total_per_mtok,
        multiplier_vs_author,
    })
}

fn pricing_total(pricing: Option<&ModelPricing>) -> Option<f64> {
    pricing.map(|pricing| pricing.input_per_mtok + pricing.output_per_mtok)
}

fn reviewer_reason(
    author: &ComplementaryModelIdentity,
    reviewer: &ComplementaryModelIdentity,
    estimate: Option<&ComplementaryCostEstimate>,
) -> String {
    let cost = estimate
        .and_then(|estimate| estimate.multiplier_vs_author)
        .map(|multiplier| format!("{multiplier:.2}x the author model price"))
        .unwrap_or_else(|| "price ratio unavailable".to_string());
    format!(
        "selected {} via {} because family {} differs from author family {}, tier {} matches author tier {}, and {}",
        reviewer.id,
        reviewer.provider,
        reviewer.family,
        author.family,
        reviewer.tier,
        author.tier,
        cost
    )
}

fn reviewer_match_refs(identity: &ComplementaryModelIdentity) -> BTreeSet<String> {
    BTreeSet::from([
        identity.id.to_ascii_lowercase(),
        identity.provider.to_ascii_lowercase(),
        format!("{}/{}", identity.provider, identity.id).to_ascii_lowercase(),
        format!("{}:{}", identity.provider, identity.id).to_ascii_lowercase(),
        identity.family.to_ascii_lowercase(),
        identity.lineage.to_ascii_lowercase(),
    ])
}

fn refs_contain_selector(refs: &BTreeSet<String>, selector: &str) -> bool {
    normalized_catalog_token(Some(selector))
        .or_else(|| Some(selector.trim().to_ascii_lowercase()))
        .is_some_and(|selector| refs.contains(&selector))
}
