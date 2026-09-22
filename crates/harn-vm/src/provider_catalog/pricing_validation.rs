//! Validation of catalog rate cards and recurring pricing windows.

use super::*;

pub(super) fn validate_pricing(
    model: &CatalogModel,
    pricing: &ModelPricing,
    result: &mut ProviderCatalogValidation,
) {
    for (tool, fee) in &pricing.hosted_tool_fees {
        if tool.is_empty()
            || !fee.per_1k_calls.is_finite()
            || fee.per_1k_calls < 0.0
            || !fee.source_url.starts_with("https://")
        {
            result.errors.push(format!(
                "model {} hosted tool {tool} requires a nonnegative finite fee and HTTPS source",
                model.id
            ));
        }
    }
    if let Some(rates) = &pricing.modality_rates {
        for rate in [
            rates.audio_input_per_mtok,
            rates.audio_output_per_mtok,
            rates.cached_audio_input_per_mtok,
        ]
        .into_iter()
        .flatten()
        {
            if !rate.is_finite() || rate < 0.0 {
                result.errors.push(format!(
                    "model {} modality rates must be finite and nonnegative",
                    model.id
                ));
            }
        }
    }
    for (field, value) in [
        ("input_per_mtok", Some(pricing.input_per_mtok)),
        ("output_per_mtok", Some(pricing.output_per_mtok)),
        ("cache_read_per_mtok", pricing.cache_read_per_mtok),
        ("cache_write_per_mtok", pricing.cache_write_per_mtok),
        ("cache_write_1h_per_mtok", pricing.cache_write_1h_per_mtok),
    ] {
        if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
            result.errors.push(format!(
                "model {} pricing.{} must be non-negative",
                model.id, field
            ));
        }
    }
    let mut previous_minimum = 0;
    for band in &pricing.input_token_bands {
        if band.minimum_input_tokens == 0 {
            result.errors.push(format!(
                "model {} pricing.input_token_bands minimum_input_tokens must be positive",
                model.id
            ));
        }
        if band.minimum_input_tokens <= previous_minimum {
            result.errors.push(format!(
                "model {} pricing.input_token_bands must be ordered by unique ascending minimum_input_tokens",
                model.id
            ));
        }
        previous_minimum = band.minimum_input_tokens;
        for (field, value) in [
            ("input_multiplier", band.input_multiplier),
            ("output_multiplier", band.output_multiplier),
        ] {
            if value <= 0.0 {
                result.errors.push(format!(
                    "model {} pricing.input_token_bands.{} must be positive",
                    model.id, field
                ));
            }
        }
    }

    let mut promotion_ids = BTreeSet::new();
    let mut promotion_windows = Vec::new();
    for promotion in &pricing.promotions {
        if promotion.id.trim().is_empty() || !promotion_ids.insert(promotion.id.as_str()) {
            result.errors.push(format!(
                "model {} pricing.promotions must use unique non-empty ids",
                model.id
            ));
        }
        if promotion.source_url.trim().is_empty() {
            result.errors.push(format!(
                "model {} pricing.promotions[{}].source_url cannot be empty",
                model.id, promotion.id
            ));
        }
        for (field, value) in [
            ("input_per_mtok", Some(promotion.input_per_mtok)),
            ("output_per_mtok", Some(promotion.output_per_mtok)),
            ("cache_read_per_mtok", promotion.cache_read_per_mtok),
            ("cache_write_per_mtok", promotion.cache_write_per_mtok),
            ("cache_write_1h_per_mtok", promotion.cache_write_1h_per_mtok),
        ] {
            if value.is_some_and(|value| value < 0.0) {
                result.errors.push(format!(
                    "model {} pricing.promotions[{}].{} must be non-negative",
                    model.id, promotion.id, field
                ));
            }
        }
        let Ok(starts_on) = NaiveDate::parse_from_str(&promotion.starts_on, "%Y-%m-%d") else {
            result.errors.push(format!(
                "model {} pricing.promotions[{}].starts_on must be YYYY-MM-DD",
                model.id, promotion.id
            ));
            continue;
        };
        let starts_at = match promotion.starts_at.as_deref() {
            Some(value) => match OffsetDateTime::parse(value, &Rfc3339) {
                Ok(value) => value.unix_timestamp_nanos(),
                Err(_) => {
                    result.errors.push(format!(
                        "model {} pricing.promotions[{}].starts_at must be RFC 3339",
                        model.id, promotion.id
                    ));
                    continue;
                }
            },
            None => catalog_date_start_nanos(starts_on),
        };
        let ends_on = match promotion.ends_on.as_deref() {
            Some(value) => match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
                Ok(date) => Some(date),
                Err(_) => {
                    result.errors.push(format!(
                        "model {} pricing.promotions[{}].ends_on must be YYYY-MM-DD",
                        model.id, promotion.id
                    ));
                    continue;
                }
            },
            None => None,
        };
        let ends_at = match promotion.ends_at.as_deref() {
            Some(value) => match OffsetDateTime::parse(value, &Rfc3339) {
                Ok(value) => Some(value.unix_timestamp_nanos()),
                Err(_) => {
                    result.errors.push(format!(
                        "model {} pricing.promotions[{}].ends_at must be RFC 3339",
                        model.id, promotion.id
                    ));
                    continue;
                }
            },
            None => ends_on
                .and_then(|end| end.succ_opt())
                .map(catalog_date_start_nanos),
        };
        if ends_at.is_some_and(|end| end <= starts_at) {
            result.errors.push(format!(
                "model {} pricing.promotions[{}] ends before it starts",
                model.id, promotion.id
            ));
        }
        if let Some(value) = promotion.review_after.as_deref() {
            match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
                Ok(date) if date >= starts_on => {}
                Ok(_) => result.errors.push(format!(
                    "model {} pricing.promotions[{}].review_after precedes starts_on",
                    model.id, promotion.id
                )),
                Err(_) => result.errors.push(format!(
                    "model {} pricing.promotions[{}].review_after must be YYYY-MM-DD",
                    model.id, promotion.id
                )),
            }
        }
        promotion_windows.push((promotion.id.as_str(), starts_at, ends_at));
    }
    for (index, (left_id, left_start, left_end)) in promotion_windows.iter().enumerate() {
        for (right_id, right_start, right_end) in promotion_windows.iter().skip(index + 1) {
            let left_reaches_right = left_end.is_none_or(|end| *right_start < end);
            let right_reaches_left = right_end.is_none_or(|end| *left_start < end);
            if left_reaches_right && right_reaches_left {
                result.errors.push(format!(
                    "model {} pricing promotions {:?} and {:?} overlap",
                    model.id, left_id, right_id
                ));
            }
        }
    }

    validate_pricing_schedules(model, pricing, result);
}

/// Validate the recurring rate windows on one model's card.
///
/// Each window is normalized to the minutes-of-week it occupies in UTC, so two
/// windows authored in different offsets are still compared on one clock.
/// Overlap is a hard error: with two windows in force at one instant the card
/// has no single answer, and settlement would be picking one by authoring
/// order.
fn validate_pricing_schedules(
    model: &CatalogModel,
    pricing: &ModelPricing,
    result: &mut ProviderCatalogValidation,
) {
    const WEEK_MINUTES: u32 = 7 * 24 * 60;
    let mut schedule_ids = BTreeSet::new();
    let mut spans: Vec<(&str, u32, u32)> = Vec::new();
    for window in &pricing.schedules {
        let id = window.id.as_str();
        if id.trim().is_empty() || !schedule_ids.insert(id) {
            result.errors.push(format!(
                "model {} pricing.schedules must use unique non-empty ids",
                model.id
            ));
        }
        if window.source_url.trim().is_empty() {
            result.errors.push(format!(
                "model {} pricing.schedules[{}].source_url cannot be empty",
                model.id, id
            ));
        }
        if window.days.is_empty() {
            result.errors.push(format!(
                "model {} pricing.schedules[{}].days cannot be empty",
                model.id, id
            ));
        }
        let mut seen_days = BTreeSet::new();
        for day in &window.days {
            if !seen_days.insert(*day) {
                result.errors.push(format!(
                    "model {} pricing.schedules[{}].days repeats {}",
                    model.id,
                    id,
                    day.as_str()
                ));
            }
        }
        for (field, value) in [
            ("input_multiplier", Some(window.input_multiplier)),
            ("output_multiplier", Some(window.output_multiplier)),
            ("cache_read_multiplier", window.cache_read_multiplier),
            ("cache_write_multiplier", window.cache_write_multiplier),
        ] {
            if value.is_some_and(|value| value <= 0.0) {
                result.errors.push(format!(
                    "model {} pricing.schedules[{}].{} must be positive",
                    model.id, id, field
                ));
            }
        }
        if let Some(value) = window.review_after.as_deref() {
            if NaiveDate::parse_from_str(value, "%Y-%m-%d").is_err() {
                result.errors.push(format!(
                    "model {} pricing.schedules[{}].review_after must be YYYY-MM-DD",
                    model.id, id
                ));
            }
        }
        if llm_config::parse_window_minutes(&window.start).is_none_or(|minutes| minutes >= 24 * 60)
        {
            result.errors.push(format!(
                "model {} pricing.schedules[{}].start must be HH:MM before 24:00",
                model.id, id
            ));
        }
        if llm_config::parse_window_minutes(&window.end).is_none() {
            result.errors.push(format!(
                "model {} pricing.schedules[{}].end must be HH:MM through 24:00",
                model.id, id
            ));
        }
        if llm_config::parse_utc_offset_seconds(&window.utc_offset).is_none() {
            result.errors.push(format!(
                "model {} pricing.schedules[{}].utc_offset must be +HH:MM or -HH:MM",
                model.id, id
            ));
        }
        let Some(window_spans) = window.utc_minutes_of_week() else {
            // The field errors above already name what did not parse. A window
            // with no span cannot be compared, so it is left out of the
            // overlap check rather than silently treated as empty.
            continue;
        };
        if window_spans.is_empty() {
            continue;
        }
        spans.extend(
            window_spans
                .into_iter()
                .map(|(start, end)| (id, start, end)),
        );
    }
    // Every span is a half-open arc on a 10080-minute circle. Comparing them as
    // minute sets is exact and cheap at catalog size, and it does not need the
    // arcs sorted or non-wrapping.
    let occupied = |start: u32, end: u32| -> Vec<u32> {
        let length = if end > start {
            end - start
        } else {
            WEEK_MINUTES - start + end
        };
        (0..length)
            .map(|offset| (start + offset) % WEEK_MINUTES)
            .collect()
    };
    for (index, (left_id, left_start, left_end)) in spans.iter().enumerate() {
        let left: BTreeSet<u32> = occupied(*left_start, *left_end).into_iter().collect();
        for (right_id, right_start, right_end) in spans.iter().skip(index + 1) {
            if left_id == right_id {
                continue;
            }
            if occupied(*right_start, *right_end)
                .into_iter()
                .any(|minute| left.contains(&minute))
            {
                result.errors.push(format!(
                    "model {} pricing schedules {:?} and {:?} overlap",
                    model.id, left_id, right_id
                ));
            }
        }
    }
}

fn catalog_date_start_nanos(date: NaiveDate) -> i128 {
    i128::from(
        date.and_hms_opt(0, 0, 0)
            .expect("a valid date has a midnight")
            .and_utc()
            .timestamp(),
    ) * 1_000_000_000
}
