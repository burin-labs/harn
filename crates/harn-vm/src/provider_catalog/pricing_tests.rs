use super::*;

#[test]
fn serving_tier_response_values_surface_in_generated_contracts() {
    let schema = schema_value();
    assert_eq!(
        schema["$defs"]["serving_tier_request"]["properties"]["response_values"]["uniqueItems"],
        true
    );

    let typescript = typescript_declarations();
    assert!(typescript.contains("response_values?: string[]"));

    let swift = swift_binding().expect("Swift binding renders");
    assert!(swift.contains("public let responseValues: [String]?"));
}

#[test]
fn exact_promotion_boundaries_surface_in_generated_contracts() {
    let schema = schema_value();
    assert_eq!(
        schema["$defs"]["promotional_pricing"]["properties"]["starts_at"]["format"],
        "date-time"
    );
    assert_eq!(
        schema["$defs"]["promotional_pricing"]["properties"]["ends_at"]["format"],
        "date-time"
    );

    let typescript = typescript_declarations();
    assert!(typescript.contains("starts_at?: string"));
    assert!(typescript.contains("ends_at?: string"));

    let swift = swift_binding().expect("Swift binding renders");
    assert!(swift.contains("public let startsAt: String?"));
    assert!(swift.contains("public let endsAt: String?"));
    assert!(swift.contains("case endsAt = \"ends_at\""));
}

/// A minimal artifact carrying one model with the given schedule windows, so
/// validation is exercised through its real entry point rather than a helper.
fn artifact_with_schedules(
    schedules: Vec<llm_config::RecurringPricingWindow>,
) -> ProviderCatalogArtifact {
    let mut artifact = artifact_embedded(None, None);
    artifact.models.truncate(1);
    let model = artifact.models.first_mut().expect("one model survives");
    model.pricing = Some(ModelPricing {
        input_per_mtok: 1.0,
        output_per_mtok: 2.0,
        cache_read_per_mtok: None,
        cache_write_per_mtok: None,
        cache_write_1h_per_mtok: None,
        input_token_bands: Vec::new(),
        promotions: Vec::new(),
        schedules,
    });
    artifact
}

fn window(
    id: &str,
    days: &[llm_config::PricingWeekday],
    start: &str,
    end: &str,
) -> llm_config::RecurringPricingWindow {
    llm_config::RecurringPricingWindow {
        id: id.to_string(),
        days: days.to_vec(),
        start: start.to_string(),
        end: end.to_string(),
        utc_offset: "+00:00".to_string(),
        input_multiplier: 0.5,
        output_multiplier: 0.5,
        cache_read_multiplier: None,
        cache_write_multiplier: None,
        source_url: "https://provider.example/pricing".to_string(),
        review_after: None,
        note: None,
    }
}

fn schedule_errors(schedules: Vec<llm_config::RecurringPricingWindow>) -> Vec<String> {
    validate_artifact(&artifact_with_schedules(schedules))
        .errors
        .into_iter()
        .filter(|error| error.contains("schedules") || error.contains("schedule"))
        .collect()
}

#[test]
fn adjacent_windows_on_the_same_days_validate() {
    use llm_config::PricingWeekday::{Mon, Tue};
    // The positive control. Without it an overlap check that refuses
    // everything would look like a working overlap check.
    let errors = schedule_errors(vec![
        window("morning", &[Mon, Tue], "00:00", "04:00"),
        window("evening", &[Mon, Tue], "04:00", "24:00"),
    ]);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
}

#[test]
fn overlapping_windows_fail_catalog_validation() {
    use llm_config::PricingWeekday::{Mon, Tue};
    let errors = schedule_errors(vec![
        window("morning", &[Mon, Tue], "00:00", "05:00"),
        window("evening", &[Mon, Tue], "04:00", "24:00"),
    ]);
    assert!(
        errors.iter().any(|error| error.contains("overlap")),
        "expected an overlap error, got {errors:?}"
    );
}

#[test]
fn a_window_wrapping_midnight_overlaps_the_next_days_window() {
    use llm_config::PricingWeekday::{Mon, Tue};
    // Monday 22:00 to Tuesday 02:00 collides with a Tuesday 01:00 window even
    // though neither literal mentions the other's day. Comparing on
    // minutes-of-week in UTC is what catches it.
    let errors = schedule_errors(vec![
        window("overnight", &[Mon], "22:00", "02:00"),
        window("tuesday-early", &[Tue], "01:00", "03:00"),
    ]);
    assert!(
        errors.iter().any(|error| error.contains("overlap")),
        "expected an overlap error, got {errors:?}"
    );
}

#[test]
fn windows_in_different_offsets_are_compared_on_one_clock() {
    use llm_config::PricingWeekday::Mon;
    // Monday 09:00+08:00 is Monday 01:00Z. A UTC window over 00:00-02:00 on
    // Monday covers it, so authoring the two in different offsets must not
    // hide the collision.
    let mut shifted = window("shanghai", &[Mon], "09:00", "11:00");
    shifted.utc_offset = "+08:00".to_string();
    let errors = schedule_errors(vec![window("utc-early", &[Mon], "00:00", "02:00"), shifted]);
    assert!(
        errors.iter().any(|error| error.contains("overlap")),
        "expected an overlap error, got {errors:?}"
    );
}

#[test]
fn malformed_window_fields_are_named_individually() {
    use llm_config::PricingWeekday::Mon;
    let mut broken = window("broken", &[Mon], "9:00", "25:00");
    broken.utc_offset = "Z".to_string();
    broken.input_multiplier = 0.0;
    broken.source_url = String::new();
    broken.review_after = Some("later".to_string());
    let errors = schedule_errors(vec![broken]);
    for expected in [
        "start must be HH:MM",
        "end must be HH:MM",
        "utc_offset must be",
        "input_multiplier must be positive",
        "source_url cannot be empty",
        "review_after must be YYYY-MM-DD",
    ] {
        assert!(
            errors.iter().any(|error| error.contains(expected)),
            "missing {expected:?} in {errors:?}"
        );
    }
}

#[test]
fn duplicate_and_empty_schedule_ids_fail_validation() {
    use llm_config::PricingWeekday::{Mon, Tue};
    let errors = schedule_errors(vec![
        window("same", &[Mon], "00:00", "02:00"),
        window("same", &[Tue], "00:00", "02:00"),
    ]);
    assert!(
        errors
            .iter()
            .any(|error| error.contains("unique non-empty ids")),
        "expected a duplicate-id error, got {errors:?}"
    );
    let mut dayless = window("dayless", &[], "00:00", "02:00");
    dayless.days = Vec::new();
    let errors = schedule_errors(vec![dayless]);
    assert!(
        errors
            .iter()
            .any(|error| error.contains("days cannot be empty")),
        "expected an empty-days error, got {errors:?}"
    );
}

#[test]
fn recurring_windows_surface_in_generated_contracts() {
    let schema = schema_value();
    assert_eq!(
        schema["$defs"]["recurring_pricing_window"]["properties"]["utc_offset"]["pattern"],
        "^[+-]([01][0-9]|2[0-3]):[0-5][0-9]$"
    );
    assert_eq!(
        schema["$defs"]["pricing"]["properties"]["schedules"]["items"]["$ref"],
        "#/$defs/recurring_pricing_window"
    );
    assert_eq!(
        schema["$defs"]["pricing"]["properties"]["cache_write_1h_per_mtok"]["type"][0],
        "number"
    );

    let typescript = typescript_declarations();
    assert!(typescript.contains("schedules?: HarnRecurringPricingWindow[]"));
    assert!(typescript.contains("cache_write_1h_per_mtok?: number | null"));

    let swift = swift_binding().expect("Swift binding renders");
    assert!(swift.contains("public struct HarnRecurringPricingWindow"));
    assert!(swift.contains("case cacheWrite1hPerMTok = \"cache_write_1h_per_mtok\""));
}

#[test]
fn the_checked_in_catalog_declares_the_deepseek_off_peak_windows() {
    // The catalog row is the thing settlement reads, so the row itself is
    // asserted here rather than only through a synthetic card.
    let artifact = artifact_embedded(None, None);
    let model = artifact
        .models
        .iter()
        .find(|model| model.id == "deepseek/deepseek-v4-pro-0813")
        .expect("dated DeepSeek OpenRouter row is catalogued");
    let pricing = model.pricing.as_ref().expect("row is priced");
    assert_eq!(pricing.input_per_mtok, 1.32);
    assert_eq!(pricing.output_per_mtok, 3.96);
    let ids: Vec<&str> = pricing
        .schedules
        .iter()
        .map(|window| window.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "deepseek-offpeak-weekday-0000",
            "deepseek-offpeak-weekday-0400",
            "deepseek-offpeak-weekday-1000",
            "deepseek-offpeak-weekend",
        ]
    );
    for window in &pricing.schedules {
        assert_eq!(window.input_multiplier, 0.5);
        assert_eq!(window.output_multiplier, 0.5);
    }
    // The holiday exclusion is recorded as an approximation, not modeled.
    assert!(pricing.schedules[0]
        .note
        .as_deref()
        .is_some_and(|note| note.contains("holiday")));
    // And the whole catalog validates with them in place.
    let validation = validate_artifact(&artifact);
    assert!(
        validation.errors.is_empty(),
        "catalog errors: {:?}",
        validation.errors
    );
}
