use super::*;

#[test]
fn complementary_reviewer_uses_different_family() {
    let selection = pick_complementary_reviewer_with_availability(
        ComplementaryReviewerOptions {
            author_model: "claude-sonnet-4-6".to_string(),
            author_provider: None,
            intent: ComplementaryReviewerIntent::PlanReview,
            max_price_multiplier: Some(3.0),
        },
        |_| true,
    );

    assert!(!selection.fallback, "{selection:?}");
    assert_eq!(selection.author.family, "anthropic-claude");
    assert_ne!(selection.reviewer.family, selection.author.family);
    assert_eq!(selection.reviewer.tier, "frontier");
    assert!(selection.estimated_incremental_cost.is_some());
    assert_eq!(selection.fallback_code, None, "{selection:?}");
}

#[test]
fn complementary_reviewer_falls_back_deterministically_on_price_cap() {
    let selection = pick_complementary_reviewer_with_availability(
        ComplementaryReviewerOptions {
            author_model: "gpt-4o-mini".to_string(),
            author_provider: Some("openai".to_string()),
            intent: ComplementaryReviewerIntent::Review,
            max_price_multiplier: Some(0.01),
        },
        |_| true,
    );

    assert!(selection.fallback, "{selection:?}");
    assert_eq!(selection.reviewer.id, "gpt-4o-mini");
    assert_eq!(selection.reviewer.family, selection.author.family);
    assert!(selection
        .fallback_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("max_price_multiplier")));
    assert_eq!(
        selection.fallback_code.as_deref(),
        Some(ReviewerFallbackCode::NoDiffFamilyWithinPrice.as_code()),
        "{selection:?}"
    );
}

#[test]
fn reviewer_fallback_codes_are_stable_strings() {
    let cases = [
        (
            ReviewerFallbackCode::UnknownAuthorFamily,
            "unknown_author_family",
        ),
        (
            ReviewerFallbackCode::NoDiffFamilyWithinPrice,
            "no_diff_family_within_price",
        ),
        (
            ReviewerFallbackCode::NoDiffFamilyServerless,
            "no_diff_family_serverless",
        ),
        (
            ReviewerFallbackCode::NoDiffFamilyAvailable,
            "no_diff_family_available",
        ),
        (
            ReviewerFallbackCode::AllDiffFamilyExcluded,
            "all_diff_family_excluded",
        ),
    ];

    for (code, expected) in cases {
        assert_eq!(code.as_code(), expected);
    }
}

#[test]
fn complementary_reviewer_skips_unavailable_provider() {
    let selection = pick_complementary_reviewer_with_availability(
        ComplementaryReviewerOptions {
            author_model: "gpt-5.6-luna".to_string(),
            author_provider: Some("openai".to_string()),
            intent: ComplementaryReviewerIntent::Critique,
            max_price_multiplier: None,
        },
        |provider| provider != "gemini",
    );

    assert!(!selection.fallback, "{selection:?}");
    assert_ne!(selection.reviewer.provider, "gemini");
    assert_ne!(selection.reviewer.family, selection.author.family);
}

#[test]
fn complementary_reviewer_skips_deprecated_model_on_available_provider() {
    let selection = pick_complementary_reviewer_with_availability(
        ComplementaryReviewerOptions {
            author_model: "gpt-5.6-luna".to_string(),
            author_provider: Some("openai".to_string()),
            intent: ComplementaryReviewerIntent::Critique,
            max_price_multiplier: Some(3.0),
        },
        |_| true,
    );

    assert!(!selection.fallback, "{selection:?}");
    assert_ne!(selection.reviewer.id, "gemini-2.5-flash-lite");
    assert_ne!(selection.reviewer.family, selection.author.family);
}

#[test]
fn complementary_reviewer_reports_no_available_independent_route() {
    let selection = pick_complementary_reviewer_with_availability(
        ComplementaryReviewerOptions {
            author_model: "gpt-5.6-luna".to_string(),
            author_provider: Some("openai".to_string()),
            intent: ComplementaryReviewerIntent::Critique,
            max_price_multiplier: None,
        },
        |_| false,
    );

    assert!(selection.fallback, "{selection:?}");
    assert_eq!(
        selection.fallback_code.as_deref(),
        Some(ReviewerFallbackCode::NoDiffFamilyAvailable.as_code())
    );
}
