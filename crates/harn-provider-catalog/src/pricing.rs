//! The provider rate card and everything that resolves it at an instant.
//!
//! One owner for pricing: the base card, dated promotions, whole-request input
//! bands and recurring time-of-day windows all live here, together with the
//! resolution order that combines them. A consumer asks for the card at an
//! instant and gets back both the rates and the name of the card that produced
//! them, so a settled cost is auditable without re-deriving it.

use chrono::{NaiveDate, TimeZone as _, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use time::{format_description::well_known::Rfc3339, OffsetDateTime, UtcOffset, Weekday};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
    /// Rate for a cache write the request asked to keep for one hour.
    /// Anthropic bills that write above the five-minute write it prices in
    /// `cache_write_per_mtok`. Absent means the route publishes no one-hour
    /// tier, and a request that asked for one settles at the five-minute rate
    /// with `cache_ttl_unpriced` on the receipt rather than silently at 2x.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_per_mtok: Option<f64>,
    /// Whole-request pricing that activates once provider-reported input usage
    /// reaches a threshold. Providers such as OpenAI and Gemini charge every
    /// token in a long-context request at the selected band's rates rather
    /// than applying marginal pricing only above the boundary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_token_bands: Vec<InputTokenPricingBand>,
    /// Dated provider promotions. The base fields remain the durable rate
    /// card, so a temporary discount never destroys the price to restore.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promotions: Vec<PromotionalPricing>,
    /// Recurring time-of-day windows that discount or surcharge the base card.
    /// The base fields stay the provider's standard (peak) rate card so the
    /// price to restore survives; a window multiplies it while the request
    /// instant falls inside the window. Overlapping windows are a catalog
    /// validation error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<RecurringPricingWindow>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hosted_tool_fees: BTreeMap<String, HostedToolFee>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modality_rates: Option<ModalityRates>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct HostedToolFee {
    pub per_1k_calls: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_per_month: Option<u64>,
    pub source_url: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ModalityRates {
    pub audio_input_per_mtok: Option<f64>,
    pub audio_output_per_mtok: Option<f64>,
    pub cached_audio_input_per_mtok: Option<f64>,
}

impl ModalityRates {
    fn scaled(&self, input: f64, output: f64, cache: f64) -> Self {
        Self {
            audio_input_per_mtok: self.audio_input_per_mtok.map(|rate| rate * input),
            audio_output_per_mtok: self.audio_output_per_mtok.map(|rate| rate * output),
            cached_audio_input_per_mtok: self.cached_audio_input_per_mtok.map(|rate| rate * cache),
        }
    }
}

/// A day of the week, as the provider publishes its schedule.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PricingWeekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl PricingWeekday {
    /// Monday-based index, matching the order providers publish schedules in.
    pub fn index(self) -> u32 {
        match self {
            Self::Mon => 0,
            Self::Tue => 1,
            Self::Wed => 2,
            Self::Thu => 3,
            Self::Fri => 4,
            Self::Sat => 5,
            Self::Sun => 6,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mon => "mon",
            Self::Tue => "tue",
            Self::Wed => "wed",
            Self::Thu => "thu",
            Self::Fri => "fri",
            Self::Sat => "sat",
            Self::Sun => "sun",
        }
    }

    fn from_time_weekday(weekday: Weekday) -> Self {
        match weekday {
            Weekday::Monday => Self::Mon,
            Weekday::Tuesday => Self::Tue,
            Weekday::Wednesday => Self::Wed,
            Weekday::Thursday => Self::Thu,
            Weekday::Friday => Self::Fri,
            Weekday::Saturday => Self::Sat,
            Weekday::Sunday => Self::Sun,
        }
    }
}

/// A recurring time-of-day rate window, expressed in a fixed UTC offset.
///
/// Every published schedule Harn tracks names a fixed offset (DeepSeek's is
/// UTC), so this deliberately carries no timezone database and no daylight
/// rule. A provider that ever publishes a schedule in a DST-observing local
/// zone needs a new field, not a reinterpretation of this one.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RecurringPricingWindow {
    pub id: String,
    /// Days the window opens on. A window that wraps past midnight opens on
    /// the day named here and closes on the following day.
    pub days: Vec<PricingWeekday>,
    /// Inclusive local start, `HH:MM`.
    pub start: String,
    /// Exclusive local end, `HH:MM`. `24:00` means end of day; any value at or
    /// before `start` wraps past midnight into the next day.
    pub end: String,
    /// Fixed offset the provider publishes the schedule in, `+HH:MM`.
    #[serde(default = "utc_offset_default")]
    pub utc_offset: String,
    pub input_multiplier: f64,
    pub output_multiplier: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_multiplier: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_multiplier: Option<f64>,
    pub source_url: String,
    /// Earliest date maintainers should re-read the provider's schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_after: Option<String>,
    /// What this window approximates but does not model, such as a public
    /// holiday calendar the provider excludes from its peak hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn utc_offset_default() -> String {
    "+00:00".to_string()
}

/// Which card settlement applied at the request instant.
///
/// A schedule window is applied last and on top of whatever promotion was
/// active, so it names the card when both are in force; the promotion is still
/// visible in the settled rates and in the catalog row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateCard {
    Base,
    Promotion(String),
    Schedule(String),
}

impl RateCard {
    /// The receipt label: `base`, `promotion:<id>`, or `schedule:<id>`.
    pub fn label(&self) -> String {
        match self {
            Self::Base => "base".to_string(),
            Self::Promotion(id) => format!("promotion:{id}"),
            Self::Schedule(id) => format!("schedule:{id}"),
        }
    }
}

/// A rate card resolved at one instant, with the card that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPricing {
    pub pricing: ModelPricing,
    pub rate_card: RateCard,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct PromotionalPricing {
    pub id: String,
    pub starts_on: String,
    /// Exact RFC 3339 activation instant when the provider publishes one.
    /// `starts_on` remains the date-only fallback for existing catalogs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_on: Option<String>,
    /// Exact RFC 3339 exclusive expiry instant. This takes precedence over
    /// the inclusive date-only `ends_on` boundary when both are present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<String>,
    /// Earliest date maintainers should confirm an open-ended promotion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_after: Option<String>,
    pub source_url: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
    /// One-hour cache-write rate while this promotion runs. Absent restores
    /// the base card's tier, exactly as the other optional rates do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h_per_mtok: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct InputTokenPricingBand {
    /// Inclusive lower bound for this whole-request rate.
    pub minimum_input_tokens: u64,
    pub input_multiplier: f64,
    pub output_multiplier: f64,
}

impl ModelPricing {
    /// Resolve the rate card for the current UTC instant.
    ///
    /// Deterministic callers and tests should use [`Self::effective_at`].
    pub fn effective_today(&self) -> Self {
        use harn_clock::Clock as _;

        self.effective_at(harn_clock::RealClock::new().now_utc())
    }

    /// Resolve the rate card at midnight UTC on a caller-supplied date.
    /// Date-only promotion ends remain inclusive through that date.
    pub fn effective_on(&self, date: NaiveDate) -> Self {
        let at = Utc
            .from_utc_datetime(
                &date
                    .and_hms_opt(0, 0, 0)
                    .expect("a valid date has a midnight"),
            )
            .timestamp();
        self.effective_at_unix_nanos(i128::from(at) * 1_000_000_000)
    }

    /// Resolve the rate card at a caller-supplied UTC instant. Invalid
    /// promotion windows and schedules are ignored here and reported by
    /// catalog validation.
    pub fn effective_at(&self, at: OffsetDateTime) -> Self {
        self.effective_at_unix_nanos(at.unix_timestamp_nanos())
    }

    /// Resolve the rate card at an instant and name the card that produced it.
    ///
    /// Order: base, then the active promotion (replaces every rate), then the
    /// active schedule window (multiplies whatever the previous step left).
    /// The input band and the serving tier are applied by their own owners
    /// afterwards, so they never change which card is named here.
    pub fn resolve_at(&self, at: OffsetDateTime) -> ResolvedPricing {
        self.resolve_at_unix_nanos(at.unix_timestamp_nanos())
    }

    fn effective_at_unix_nanos(&self, at: i128) -> Self {
        self.resolve_at_unix_nanos(at).pricing
    }

    fn resolve_at_unix_nanos(&self, at: i128) -> ResolvedPricing {
        let active = self
            .promotions
            .iter()
            .filter_map(|promotion| {
                let (starts_at, ends_at) = promotion_window_nanos(promotion)?;
                (starts_at <= at && ends_at.is_none_or(|end| at < end))
                    .then_some((starts_at, promotion))
            })
            .max_by_key(|(starts_at, _)| *starts_at)
            .map(|(_, promotion)| promotion);
        let (mut pricing, mut rate_card) = match active {
            Some(promotion) => (
                Self {
                    input_per_mtok: promotion.input_per_mtok,
                    output_per_mtok: promotion.output_per_mtok,
                    cache_read_per_mtok: promotion.cache_read_per_mtok,
                    cache_write_per_mtok: promotion.cache_write_per_mtok,
                    cache_write_1h_per_mtok: promotion
                        .cache_write_1h_per_mtok
                        .or(self.cache_write_1h_per_mtok),
                    input_token_bands: self.input_token_bands.clone(),
                    promotions: self.promotions.clone(),
                    schedules: self.schedules.clone(),
                    hosted_tool_fees: self.hosted_tool_fees.clone(),
                    modality_rates: self.modality_rates.clone(),
                },
                RateCard::Promotion(promotion.id.clone()),
            ),
            None => (self.clone(), RateCard::Base),
        };
        // `find` rather than a fold: overlapping windows are a catalog
        // validation error, so at most one may be in force. Taking the first
        // keeps runtime settlement deterministic while an authoring mistake
        // is still being reported.
        if let Some(window) = self
            .schedules
            .iter()
            .find(|window| window.contains_unix_nanos(at) == Some(true))
        {
            let cache_read = window
                .cache_read_multiplier
                .unwrap_or(window.input_multiplier);
            let cache_write = window
                .cache_write_multiplier
                .unwrap_or(window.input_multiplier);
            pricing = Self {
                input_per_mtok: pricing.input_per_mtok * window.input_multiplier,
                output_per_mtok: pricing.output_per_mtok * window.output_multiplier,
                cache_read_per_mtok: pricing.cache_read_per_mtok.map(|rate| rate * cache_read),
                cache_write_per_mtok: pricing.cache_write_per_mtok.map(|rate| rate * cache_write),
                cache_write_1h_per_mtok: pricing
                    .cache_write_1h_per_mtok
                    .map(|rate| rate * cache_write),
                modality_rates: pricing.modality_rates.as_ref().map(|rates| {
                    rates.scaled(
                        window.input_multiplier,
                        window.output_multiplier,
                        cache_read,
                    )
                }),
                ..pricing
            };
            rate_card = RateCard::Schedule(window.id.clone());
        }
        ResolvedPricing { pricing, rate_card }
    }

    pub fn scaled(&self, multiplier: f64) -> Self {
        Self {
            input_per_mtok: self.input_per_mtok * multiplier,
            output_per_mtok: self.output_per_mtok * multiplier,
            cache_read_per_mtok: self.cache_read_per_mtok.map(|rate| rate * multiplier),
            cache_write_per_mtok: self.cache_write_per_mtok.map(|rate| rate * multiplier),
            cache_write_1h_per_mtok: self.cache_write_1h_per_mtok.map(|rate| rate * multiplier),
            input_token_bands: self.input_token_bands.clone(),
            // Schedules carry multipliers, not rates, so scaling the card
            // leaves them alone: scaling them too would square the discount.
            schedules: self.schedules.clone(),
            hosted_tool_fees: self.hosted_tool_fees.clone(),
            modality_rates: self
                .modality_rates
                .as_ref()
                .map(|rates| rates.scaled(multiplier, multiplier, multiplier)),
            promotions: self
                .promotions
                .iter()
                .map(|promotion| PromotionalPricing {
                    input_per_mtok: promotion.input_per_mtok * multiplier,
                    output_per_mtok: promotion.output_per_mtok * multiplier,
                    cache_read_per_mtok: promotion
                        .cache_read_per_mtok
                        .map(|rate| rate * multiplier),
                    cache_write_per_mtok: promotion
                        .cache_write_per_mtok
                        .map(|rate| rate * multiplier),
                    cache_write_1h_per_mtok: promotion
                        .cache_write_1h_per_mtok
                        .map(|rate| rate * multiplier),
                    ..promotion.clone()
                })
                .collect(),
        }
    }

    /// Resolve the whole-request rates for provider-reported input usage.
    /// `max_by_key` keeps runtime selection correct even before catalog
    /// validation reports an authoring-order mistake.
    pub fn for_input_tokens(&self, input_tokens: i64) -> Self {
        self.band_for_input_tokens(input_tokens)
            .map(|(_, pricing)| pricing)
            .unwrap_or_else(|| self.clone())
    }

    /// The band that applies to this input usage, with the rates it selects.
    /// The threshold is what the receipt records, so callers that need to say
    /// which band settled a call read it here rather than re-deriving it.
    pub fn band_for_input_tokens(&self, input_tokens: i64) -> Option<(u64, Self)> {
        let input_tokens = u64::try_from(input_tokens).unwrap_or(0);
        let band = self
            .input_token_bands
            .iter()
            .filter(|band| band.minimum_input_tokens <= input_tokens)
            .max_by_key(|band| band.minimum_input_tokens)?;
        Some((
            band.minimum_input_tokens,
            Self {
                input_per_mtok: self.input_per_mtok * band.input_multiplier,
                output_per_mtok: self.output_per_mtok * band.output_multiplier,
                cache_read_per_mtok: self
                    .cache_read_per_mtok
                    .map(|rate| rate * band.input_multiplier),
                cache_write_per_mtok: self
                    .cache_write_per_mtok
                    .map(|rate| rate * band.input_multiplier),
                cache_write_1h_per_mtok: self
                    .cache_write_1h_per_mtok
                    .map(|rate| rate * band.input_multiplier),
                input_token_bands: self.input_token_bands.clone(),
                promotions: self.promotions.clone(),
                schedules: self.schedules.clone(),
                hosted_tool_fees: self.hosted_tool_fees.clone(),
                modality_rates: self.modality_rates.as_ref().map(|rates| {
                    rates.scaled(
                        band.input_multiplier,
                        band.output_multiplier,
                        band.input_multiplier,
                    )
                }),
            },
        ))
    }
}

/// Minutes past local midnight for an `HH:MM` literal. `24:00` is accepted as
/// end of day; nothing past it is.
pub fn parse_window_minutes(value: &str) -> Option<u32> {
    let (hours, minutes) = value.split_once(':')?;
    if hours.len() != 2 || minutes.len() != 2 {
        return None;
    }
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    if minutes > 59 || hours > 24 || (hours == 24 && minutes != 0) {
        return None;
    }
    Some(hours * 60 + minutes)
}

/// Seconds east of UTC for a `+HH:MM` / `-HH:MM` literal.
pub fn parse_utc_offset_seconds(value: &str) -> Option<i32> {
    let (sign, rest) = value.split_at_checked(1)?;
    let sign = match sign {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let minutes = parse_window_minutes(rest)?;
    // A whole-day offset is not a real zone and would make every window
    // ambiguous against its own day list.
    (minutes < 24 * 60).then_some(sign * (minutes as i32) * 60)
}

impl RecurringPricingWindow {
    /// Minutes-of-week the window occupies, as `[start, end)` on a 10080-minute
    /// circle anchored at Monday 00:00 UTC. `None` when the window does not
    /// parse; catalog validation reports that separately.
    pub fn utc_minutes_of_week(&self) -> Option<Vec<(u32, u32)>> {
        let start = parse_window_minutes(&self.start)?;
        let end = parse_window_minutes(&self.end)?;
        if start >= 24 * 60 {
            return None;
        }
        let offset_minutes = parse_utc_offset_seconds(&self.utc_offset)? / 60;
        // A window whose end is at or before its start wraps past midnight.
        let length = if end > start {
            end - start
        } else {
            24 * 60 - start + end
        };
        if length == 0 {
            return None;
        }
        let week = 7 * 24 * 60;
        let mut spans = Vec::with_capacity(self.days.len());
        for day in &self.days {
            let local_start = day.index() * 24 * 60 + start;
            let utc_start =
                (local_start as i64 - offset_minutes as i64).rem_euclid(week as i64) as u32;
            spans.push((utc_start, (utc_start + length) % week));
        }
        Some(spans)
    }

    /// Whether an instant falls inside this window. `None` when the window
    /// does not parse, so an authoring mistake never reads as "not in window".
    pub fn contains(&self, at: OffsetDateTime) -> Option<bool> {
        self.contains_unix_nanos(at.unix_timestamp_nanos())
    }

    fn contains_unix_nanos(&self, at: i128) -> Option<bool> {
        let at = OffsetDateTime::from_unix_timestamp_nanos(at).ok()?;
        let start = parse_window_minutes(&self.start)?;
        let end = parse_window_minutes(&self.end)?;
        if start >= 24 * 60 {
            return None;
        }
        let offset =
            UtcOffset::from_whole_seconds(parse_utc_offset_seconds(&self.utc_offset)?).ok()?;
        let local = at.to_offset(offset);
        let minutes = u32::from(local.hour()) * 60 + u32::from(local.minute());
        let today = PricingWeekday::from_time_weekday(local.weekday());
        if end > start {
            return Some(self.days.contains(&today) && minutes >= start && minutes < end);
        }
        // Wrapped: the tail after `start` belongs to today's opening, and the
        // head before `end` belongs to yesterday's.
        let yesterday_index = (today.index() + 6) % 7;
        let opened_yesterday = self.days.iter().any(|day| day.index() == yesterday_index);
        Some(
            (self.days.contains(&today) && minutes >= start) || (opened_yesterday && minutes < end),
        )
    }
}

fn promotion_window_nanos(promotion: &PromotionalPricing) -> Option<(i128, Option<i128>)> {
    let starts_at = if let Some(value) = promotion.starts_at.as_deref() {
        OffsetDateTime::parse(value, &Rfc3339)
            .ok()?
            .unix_timestamp_nanos()
    } else {
        date_start_nanos(&promotion.starts_on)?
    };
    let ends_at = if let Some(value) = promotion.ends_at.as_deref() {
        Some(
            OffsetDateTime::parse(value, &Rfc3339)
                .ok()?
                .unix_timestamp_nanos(),
        )
    } else if let Some(value) = promotion.ends_on.as_deref() {
        let end = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
        let next_day = end.succ_opt()?;
        Some(date_start_nanos(&next_day.to_string())?)
    } else {
        None
    };
    Some((starts_at, ends_at))
}

fn date_start_nanos(value: &str) -> Option<i128> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    Some(
        i128::from(
            Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?)
                .timestamp(),
        ) * 1_000_000_000,
    )
}

#[cfg(test)]
mod pricing_schedule_tests {
    use super::{
        parse_utc_offset_seconds, parse_window_minutes, ModelPricing, PricingWeekday,
        PromotionalPricing, RateCard, RecurringPricingWindow,
    };
    use time::{format_description::well_known::Rfc3339, OffsetDateTime};

    fn at(value: &str) -> OffsetDateTime {
        OffsetDateTime::parse(value, &Rfc3339).expect("rfc3339 instant")
    }

    fn window(id: &str, days: &[PricingWeekday], start: &str, end: &str) -> RecurringPricingWindow {
        RecurringPricingWindow {
            id: id.to_string(),
            days: days.to_vec(),
            start: start.to_string(),
            end: end.to_string(),
            utc_offset: "+00:00".to_string(),
            input_multiplier: 0.5,
            output_multiplier: 0.5,
            cache_read_multiplier: None,
            cache_write_multiplier: None,
            source_url: "https://example.invalid/pricing".to_string(),
            review_after: None,
            note: None,
        }
    }

    fn deepseek_card() -> ModelPricing {
        use PricingWeekday::{Fri, Mon, Sat, Sun, Thu, Tue, Wed};
        let weekdays = [Mon, Tue, Wed, Thu, Fri];
        ModelPricing {
            input_per_mtok: 1.32,
            output_per_mtok: 3.96,
            cache_read_per_mtok: Some(0.044),
            cache_write_per_mtok: None,
            cache_write_1h_per_mtok: None,
            input_token_bands: Vec::new(),
            promotions: Vec::new(),
            schedules: vec![
                window("deepseek-offpeak-weekday-0000", &weekdays, "00:00", "01:00"),
                window("deepseek-offpeak-weekday-0400", &weekdays, "04:00", "06:00"),
                window("deepseek-offpeak-weekday-1000", &weekdays, "10:00", "24:00"),
                window("deepseek-offpeak-weekend", &[Sat, Sun], "00:00", "24:00"),
            ],
            hosted_tool_fees: Default::default(),
            modality_rates: None,
        }
    }

    #[test]
    fn peak_instant_settles_on_the_base_card() {
        // 2026-09-21 is a Monday; 02:00Z sits between the 00:00-01:00 and
        // 04:00-06:00 off-peak windows, so no window applies.
        let resolved = deepseek_card().resolve_at(at("2026-09-21T02:00:00Z"));
        assert_eq!(resolved.rate_card, RateCard::Base);
        assert_eq!(resolved.rate_card.label(), "base");
        assert_eq!(resolved.pricing.input_per_mtok, 1.32);
        assert_eq!(resolved.pricing.output_per_mtok, 3.96);
    }

    #[test]
    fn off_peak_weekday_window_halves_the_base_card() {
        let resolved = deepseek_card().resolve_at(at("2026-09-21T05:00:00Z"));
        assert_eq!(
            resolved.rate_card.label(),
            "schedule:deepseek-offpeak-weekday-0400"
        );
        assert_eq!(resolved.pricing.input_per_mtok, 0.66);
        assert_eq!(resolved.pricing.output_per_mtok, 1.98);
        assert_eq!(resolved.pricing.cache_read_per_mtok, Some(0.022));
    }

    #[test]
    fn weekend_settles_off_peak_all_day() {
        // 2026-09-26 is a Saturday. An hour that is peak on a weekday is
        // off-peak here, which is the whole point of the day list.
        let resolved = deepseek_card().resolve_at(at("2026-09-26T02:00:00Z"));
        assert_eq!(
            resolved.rate_card.label(),
            "schedule:deepseek-offpeak-weekend"
        );
        assert_eq!(resolved.pricing.input_per_mtok, 0.66);
    }

    #[test]
    fn window_boundaries_are_start_inclusive_and_end_exclusive() {
        let card = deepseek_card();
        assert_eq!(
            card.resolve_at(at("2026-09-21T04:00:00Z"))
                .rate_card
                .label(),
            "schedule:deepseek-offpeak-weekday-0400"
        );
        assert_eq!(
            card.resolve_at(at("2026-09-21T06:00:00Z"))
                .rate_card
                .label(),
            "base"
        );
        // 10:00-24:00 runs to the end of Monday and stops there.
        assert_eq!(
            card.resolve_at(at("2026-09-21T23:59:59Z"))
                .rate_card
                .label(),
            "schedule:deepseek-offpeak-weekday-1000"
        );
        assert_eq!(
            card.resolve_at(at("2026-09-22T00:00:00Z"))
                .rate_card
                .label(),
            "schedule:deepseek-offpeak-weekday-0000"
        );
    }

    #[test]
    fn a_window_that_wraps_midnight_covers_both_sides() {
        let mut card = deepseek_card();
        card.schedules = vec![window(
            "overnight",
            &[PricingWeekday::Fri],
            "22:00",
            "02:00",
        )];
        // 2026-09-25 is a Friday.
        assert_eq!(
            card.resolve_at(at("2026-09-25T23:00:00Z"))
                .rate_card
                .label(),
            "schedule:overnight"
        );
        assert_eq!(
            card.resolve_at(at("2026-09-26T01:00:00Z"))
                .rate_card
                .label(),
            "schedule:overnight"
        );
        assert_eq!(
            card.resolve_at(at("2026-09-26T03:00:00Z"))
                .rate_card
                .label(),
            "base"
        );
        // Saturday's own 22:00 is outside a Friday-only window.
        assert_eq!(
            card.resolve_at(at("2026-09-26T23:00:00Z"))
                .rate_card
                .label(),
            "base"
        );
    }

    #[test]
    fn a_non_utc_offset_shifts_the_window_and_the_day() {
        let mut card = deepseek_card();
        let mut shifted = window("shanghai-peak", &[PricingWeekday::Mon], "09:00", "12:00");
        shifted.utc_offset = "+08:00".to_string();
        card.schedules = vec![shifted];
        // Monday 09:00+08:00 is Monday 01:00Z.
        assert_eq!(
            card.resolve_at(at("2026-09-21T01:00:00Z"))
                .rate_card
                .label(),
            "schedule:shanghai-peak"
        );
        // Monday 09:00Z is Monday 17:00 local, outside the window.
        assert_eq!(
            card.resolve_at(at("2026-09-21T09:00:00Z"))
                .rate_card
                .label(),
            "base"
        );
        // Sunday 20:00Z is Monday 04:00 local, still outside.
        assert_eq!(
            card.resolve_at(at("2026-09-20T20:00:00Z"))
                .rate_card
                .label(),
            "base"
        );
    }

    #[test]
    fn a_schedule_multiplies_the_active_promotion_not_the_base() {
        let mut card = deepseek_card();
        card.promotions = vec![PromotionalPricing {
            id: "launch".to_string(),
            starts_on: "2026-09-01".to_string(),
            starts_at: None,
            ends_on: None,
            ends_at: None,
            review_after: None,
            source_url: "https://example.invalid/promo".to_string(),
            input_per_mtok: 1.0,
            output_per_mtok: 2.0,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            cache_write_1h_per_mtok: None,
        }];
        let peak = card.resolve_at(at("2026-09-21T02:00:00Z"));
        assert_eq!(peak.rate_card.label(), "promotion:launch");
        assert_eq!(peak.pricing.input_per_mtok, 1.0);
        let off_peak = card.resolve_at(at("2026-09-21T05:00:00Z"));
        assert_eq!(
            off_peak.rate_card.label(),
            "schedule:deepseek-offpeak-weekday-0400"
        );
        assert_eq!(off_peak.pricing.input_per_mtok, 0.5);
    }

    #[test]
    fn scaling_a_card_leaves_schedule_multipliers_alone() {
        // Multipliers are dimensionless. Scaling them with the rates would
        // square the discount the next time the window applies.
        let scaled = deepseek_card().scaled(2.0);
        assert_eq!(scaled.input_per_mtok, 2.64);
        assert_eq!(scaled.schedules[1].input_multiplier, 0.5);
        assert_eq!(
            scaled
                .resolve_at(at("2026-09-21T05:00:00Z"))
                .pricing
                .input_per_mtok,
            1.32
        );
    }

    #[test]
    fn an_unparseable_window_never_reads_as_in_window() {
        let mut broken = window("broken", &[PricingWeekday::Mon], "9:00", "12:00");
        assert_eq!(broken.contains(at("2026-09-21T10:00:00Z")), None);
        broken.start = "09:00".to_string();
        broken.utc_offset = "Z".to_string();
        assert_eq!(broken.contains(at("2026-09-21T10:00:00Z")), None);
    }

    #[test]
    fn window_literals_parse_exactly() {
        assert_eq!(parse_window_minutes("00:00"), Some(0));
        assert_eq!(parse_window_minutes("24:00"), Some(1440));
        assert_eq!(parse_window_minutes("24:01"), None);
        assert_eq!(parse_window_minutes("25:00"), None);
        assert_eq!(parse_window_minutes("01:60"), None);
        assert_eq!(parse_window_minutes("1:00"), None);
        assert_eq!(parse_utc_offset_seconds("+00:00"), Some(0));
        assert_eq!(parse_utc_offset_seconds("-05:30"), Some(-19800));
        assert_eq!(parse_utc_offset_seconds("+24:00"), None);
        assert_eq!(parse_utc_offset_seconds("05:30"), None);
    }

    #[test]
    fn a_card_without_the_added_fields_serializes_unchanged() {
        let card = ModelPricing {
            input_per_mtok: 1.0,
            output_per_mtok: 2.0,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            cache_write_1h_per_mtok: None,
            input_token_bands: Vec::new(),
            promotions: Vec::new(),
            schedules: Vec::new(),
            hosted_tool_fees: Default::default(),
            modality_rates: None,
        };
        let json = serde_json::to_string(&card).expect("serialize");
        assert_eq!(
            json,
            r#"{"input_per_mtok":1.0,"output_per_mtok":2.0,"cache_read_per_mtok":null,"cache_write_per_mtok":null}"#
        );
    }
}
