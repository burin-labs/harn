//! `harn usage` — structured LLM spend/usage analytics over the local
//! event log.
//!
//! The Harn runtime already records a `provider_call_response` event on
//! every LLM call, carrying `provider`, `model`, token counts, cache
//! telemetry, and the runtime-computed accounting certainty. This command reuses
//! that data: it reads the same `agent.transcript.llm` topic that
//! `harn portal` reads through the shared event-log reader, filters to
//! `provider_call_response` records, and rolls them up by provider,
//! model, agent-loop stage, or a day/week/month time series. Calls without a
//! meaningful stage are grouped as `unattributed`.
//!
//! It does NOT recompute pricing. Measured cost and explicit unpriced/unknown
//! counts are folded straight from the event payload. `mock`-provider rows are
//! excluded by default.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use harn_vm::event_log::{
    open_event_log, EventLog, EventLogConfig, LogEvent, Topic, HARN_LLM_TRANSCRIPT_TOPIC,
};
use serde::Serialize;
use time::{format_description::well_known::Rfc3339, Date, Month, OffsetDateTime, Time};

use crate::cli::{UsageArgs, UsageGroupBy};
use crate::json_envelope::{to_string_pretty, JsonEnvelope};

/// Schema version for the `harn usage --json` envelope. Bump when the
/// [`UsageReport`] shape changes in a way agents must detect.
pub const USAGE_SCHEMA_VERSION: u32 = 2;

/// Kind stored on `provider_call_response` events (mirrors the `type`
/// field the runtime observability writer sets before append).
const PROVIDER_CALL_RESPONSE_KIND: &str = "provider_call_response";

/// Provider name for fixture/mock rows, excluded from spend rollups by
/// default.
const MOCK_PROVIDER: &str = "mock";

/// One `provider_call_response` record, normalized to the fields the
/// rollup needs. Everything is sourced from the event payload except
/// `occurred_at_ms`, which is the authoritative row timestamp.
#[derive(Debug, Clone)]
struct UsageCall {
    provider: String,
    model: String,
    stage: Option<String>,
    occurred_at_ms: i64,
    provider_call_count: i64,
    unpriced_calls: i64,
    usage_unknown_calls: i64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    cache_savings_usd: f64,
    known_cost_usd: f64,
    response_ms: f64,
}

/// Aggregated metrics for one group (provider, model, or time bucket).
#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageGroup {
    pub key: String,
    /// Durable response records folded into this group.
    pub responses: u64,
    /// Physical provider requests represented by those response records.
    pub provider_calls: i64,
    /// Completeness of provider price accounting for this group.
    pub cost_status: AccountingCompleteness,
    /// Completeness of provider token accounting for this group.
    pub usage_status: AccountingCompleteness,
    /// Measured cost only. Unpriced calls are counted separately rather than
    /// silently converted to zero.
    pub known_cost_usd: f64,
    pub unpriced_calls: i64,
    pub usage_unknown_calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_savings_usd: f64,
    /// Response-record-weighted mean cache-hit ratio across the group.
    pub cache_hit_ratio: Option<f64>,
    pub mean_response_ms: f64,
    /// Running cost total through this row, for time-series group-bys.
    /// `None` for provider/model group-bys where order is not temporal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cumulative_known_cost_usd: Option<f64>,
    // Accumulators used while folding; not serialized.
    #[serde(skip)]
    response_ms_total: f64,
    #[serde(skip)]
    cache_hit_ratio_total: f64,
    #[serde(skip)]
    cache_hit_ratio_response_count: u64,
}

/// Completeness of one accounting dimension across a rollup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingCompleteness {
    #[default]
    NoCalls,
    Reported,
    Partial,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageReport {
    /// `provider` | `model` | `stage` | `day` | `week` | `month`.
    pub group_by: String,
    /// Absolute paths of the event logs that contributed rows.
    pub sources: Vec<String>,
    /// Number of `provider_call_response` rows aggregated (post-filter).
    pub responses: u64,
    /// Physical provider requests represented by those rows.
    pub provider_calls: i64,
    pub groups: Vec<UsageGroup>,
    pub totals: UsageGroup,
}

pub(crate) async fn run(args: UsageArgs) -> i32 {
    if let Some(source) = &args.backfill {
        let message = format!(
            "--backfill {source} (provider-billing reconciliation) is not implemented yet; \
             local event-log aggregation does not perform billing reconciliation"
        );
        return emit_error(args.json, "usage_backfill_unimplemented", &message);
    }

    let since = match parse_boundary(args.since.as_deref(), false) {
        Ok(value) => value,
        Err(error) => return emit_error(args.json, "usage_bad_since", &error),
    };
    let until = match parse_boundary(args.until.as_deref(), true) {
        Ok(value) => value,
        Err(error) => return emit_error(args.json, "usage_bad_until", &error),
    };

    let base_dirs = match discover_base_dirs(&args) {
        Ok(dirs) => dirs,
        Err(error) => return emit_error(args.json, "usage_discovery", &error),
    };

    let mut calls: Vec<UsageCall> = Vec::new();
    let mut sources: Vec<String> = Vec::new();
    for base_dir in &base_dirs {
        match read_provider_calls(base_dir).await {
            Ok(rows) => {
                if !rows.is_empty() || !args.all {
                    sources.push(
                        harn_vm::runtime_paths::event_log_sqlite_path(base_dir)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
                calls.extend(rows);
            }
            Err(error) => {
                // A single unreadable log under `--all` should not abort
                // the whole sweep; surface it and keep going.
                if args.all {
                    eprintln!("warning: skipping {}: {error}", base_dir.display());
                    continue;
                }
                return emit_error(args.json, "usage_read", &error);
            }
        }
    }

    let filtered: Vec<UsageCall> = calls
        .into_iter()
        .filter(|call| call.provider != MOCK_PROVIDER)
        .filter(|call| since.is_none_or(|bound| call.occurred_at_ms >= bound))
        .filter(|call| until.is_none_or(|bound| call.occurred_at_ms < bound))
        .filter(|call| {
            args.provider
                .as_deref()
                .is_none_or(|want| call.provider == want)
        })
        .filter(|call| args.model.as_deref().is_none_or(|want| call.model == want))
        .collect();

    let report = aggregate(&filtered, args.group_by, sources);

    if args.json {
        let envelope = JsonEnvelope::ok(USAGE_SCHEMA_VERSION, report);
        println!("{}", to_string_pretty(&envelope));
    } else if args.csv {
        print_csv(&report);
    } else {
        print_table(&report, args.group_by);
    }
    0
}

/// Resolve which project base dirs to read event logs from.
fn discover_base_dirs(args: &UsageArgs) -> Result<Vec<PathBuf>, String> {
    if args.all {
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            roots.push(cwd);
        }
        if let Some(home) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(home).join("projects"));
        }
        let bases = discover_event_log_bases(&roots);
        if bases.is_empty() {
            return Err("no `.harn/events.sqlite` files found under the search roots".to_string());
        }
        return Ok(bases);
    }

    let base = match &args.project {
        Some(path) => path.clone(),
        None => std::env::current_dir()
            .map_err(|error| format!("failed to resolve current directory: {error}"))?,
    };
    Ok(vec![base])
}

/// Walk the given roots for `.harn/events.sqlite` files and return their
/// project base dirs (the parent of `.harn`). Skips vendored
/// `.cargo/registry` copies and any node_modules trees.
fn discover_event_log_bases(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for root in roots {
        walk_for_event_logs(root, &mut seen, 0);
    }
    seen.into_iter().collect()
}

fn walk_for_event_logs(dir: &Path, seen: &mut BTreeSet<PathBuf>, depth: usize) {
    // Bound the walk so a stray `--all` from `/` does not scan the world.
    const MAX_DEPTH: usize = 8;
    if depth > MAX_DEPTH {
        return;
    }
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if matches!(name, ".cargo" | "node_modules" | "target" | ".git") {
        return;
    }
    let sqlite = dir.join(".harn").join("events.sqlite");
    if sqlite.is_file() {
        if let Ok(canonical) = dir.canonicalize() {
            seen.insert(canonical);
        } else {
            seen.insert(dir.to_path_buf());
        }
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Do not descend into a project's own `.harn` state dir.
            if path.file_name().and_then(|n| n.to_str()) == Some(".harn") {
                continue;
            }
            walk_for_event_logs(&path, seen, depth + 1);
        }
    }
}

/// Read every `provider_call_response` event for one project via the
/// shared event-log reader (the same topic + reader `harn portal` uses).
async fn read_provider_calls(base_dir: &Path) -> Result<Vec<UsageCall>, String> {
    let config = EventLogConfig::for_base_dir(base_dir).map_err(|error| error.to_string())?;
    let sqlite_path = harn_vm::runtime_paths::event_log_sqlite_path(base_dir);
    if !sqlite_path.is_file() {
        // Absent log is empty spend, not an error, in single-project mode.
        return Ok(Vec::new());
    }
    let log = open_event_log(&config).map_err(|error| error.to_string())?;
    let topic = Topic::new(HARN_LLM_TRANSCRIPT_TOPIC).map_err(|error| error.to_string())?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(|error| error.to_string())?;

    let mut calls = Vec::new();
    for (_id, event) in events {
        if event.kind != PROVIDER_CALL_RESPONSE_KIND {
            continue;
        }
        if let Some(call) = normalize_call(&event) {
            calls.push(call);
        }
    }
    Ok(calls)
}

fn normalize_call(event: &LogEvent) -> Option<UsageCall> {
    let payload = &event.payload;
    let provider = payload
        .get("provider")
        .and_then(|v| v.as_str())?
        .to_string();
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let accounting_status = payload
        .get("accounting_status")
        .and_then(|value| value.as_str());
    let provider_call_count = optional_int_field(payload, "provider_call_count")
        .unwrap_or(1)
        .max(1);
    let unpriced_calls = optional_int_field(payload, "unpriced_calls")
        .unwrap_or_else(|| i64::from(accounting_status != Some("reported")) * provider_call_count)
        .clamp(0, provider_call_count);
    let usage_unknown_calls = optional_int_field(payload, "usage_unknown_calls")
        .unwrap_or_else(|| i64::from(accounting_status != Some("reported")) * provider_call_count)
        .clamp(0, provider_call_count);
    let known_cost_usd = optional_float_field(payload, "known_cost_usd").unwrap_or_else(|| {
        if accounting_status == Some("reported") {
            optional_float_field(payload, "cost_usd").unwrap_or(0.0)
        } else {
            0.0
        }
    });
    Some(UsageCall {
        provider,
        model,
        stage: payload
            .get("stage")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|stage| !stage.is_empty())
            .map(str::to_string),
        occurred_at_ms: event.occurred_at_ms,
        provider_call_count,
        unpriced_calls,
        usage_unknown_calls,
        input_tokens: int_field(payload, "input_tokens"),
        output_tokens: int_field(payload, "output_tokens"),
        cache_read_tokens: int_field(payload, "cache_read_tokens"),
        cache_write_tokens: int_field(payload, "cache_write_tokens"),
        cache_savings_usd: float_field(payload, "cache_savings_usd"),
        known_cost_usd,
        response_ms: float_field(payload, "response_ms"),
    })
}

fn optional_int_field(payload: &serde_json::Value, key: &str) -> Option<i64> {
    payload.get(key).and_then(|value| value.as_i64())
}

fn optional_float_field(payload: &serde_json::Value, key: &str) -> Option<f64> {
    payload.get(key).and_then(|value| value.as_f64())
}

fn int_field(payload: &serde_json::Value, key: &str) -> i64 {
    payload.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn float_field(payload: &serde_json::Value, key: &str) -> f64 {
    payload.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

/// Fold normalized calls into grouped metrics. Time-series group-bys
/// (day/week/month) are ordered chronologically and carry a cumulative
/// cost column; provider/model group-bys are ordered by descending cost.
fn aggregate(calls: &[UsageCall], group_by: UsageGroupBy, sources: Vec<String>) -> UsageReport {
    let time_series = matches!(
        group_by,
        UsageGroupBy::Day | UsageGroupBy::Week | UsageGroupBy::Month
    );

    let mut groups: BTreeMap<String, UsageGroup> = BTreeMap::new();
    let mut totals = UsageGroup {
        key: "total".to_string(),
        ..Default::default()
    };

    for call in calls {
        let key = group_key(call, group_by);
        let group = groups.entry(key.clone()).or_insert_with(|| UsageGroup {
            key,
            ..Default::default()
        });
        accumulate(group, call);
        accumulate(&mut totals, call);
    }

    finalize(&mut totals);

    let mut group_list: Vec<UsageGroup> = groups.into_values().collect();
    for group in &mut group_list {
        finalize(group);
    }

    if time_series {
        // BTreeMap already sorts keys; keys are zero-padded date strings,
        // so lexical order is chronological. Fill the cumulative column.
        let mut running = 0.0;
        for group in &mut group_list {
            running += group.known_cost_usd;
            group.cumulative_known_cost_usd = Some(round_usd(running));
        }
    } else {
        group_list.sort_by(|a, b| {
            b.known_cost_usd
                .partial_cmp(&a.known_cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.key.cmp(&b.key))
        });
    }

    UsageReport {
        group_by: group_by_label(group_by).to_string(),
        sources,
        responses: totals.responses,
        provider_calls: totals.provider_calls,
        groups: group_list,
        totals,
    }
}

fn accumulate(group: &mut UsageGroup, call: &UsageCall) {
    group.responses += 1;
    group.provider_calls += call.provider_call_count;
    group.known_cost_usd += call.known_cost_usd;
    group.unpriced_calls += call.unpriced_calls;
    group.usage_unknown_calls += call.usage_unknown_calls;
    group.input_tokens += call.input_tokens;
    group.output_tokens += call.output_tokens;
    group.cache_read_tokens += call.cache_read_tokens;
    group.cache_write_tokens += call.cache_write_tokens;
    group.cache_savings_usd += call.cache_savings_usd;
    group.response_ms_total += call.response_ms;
    if let Some(ratio) = call_cache_hit_ratio(call) {
        group.cache_hit_ratio_total += ratio;
        group.cache_hit_ratio_response_count += 1;
    }
}

/// Per-call cache-hit ratio: cache_read / (input + cache_read). Matches
/// the runtime's `cache_hit_ratio` intent (read tokens over the tokens
/// that could have been cache-served), computed here from the durable
/// token counts so the rollup stays self-contained.
fn call_cache_hit_ratio(call: &UsageCall) -> Option<f64> {
    if call.usage_unknown_calls >= call.provider_call_count {
        return None;
    }
    let denom = call.input_tokens + call.cache_read_tokens;
    if denom <= 0 {
        Some(0.0)
    } else {
        Some(call.cache_read_tokens as f64 / denom as f64)
    }
}

fn finalize(group: &mut UsageGroup) {
    let responses = group.responses.max(1) as f64;
    group.mean_response_ms = round2(group.response_ms_total / responses);
    group.cache_hit_ratio = (group.cache_hit_ratio_response_count > 0)
        .then(|| round4(group.cache_hit_ratio_total / group.cache_hit_ratio_response_count as f64));
    group.known_cost_usd = round_usd(group.known_cost_usd);
    group.cache_savings_usd = round_usd(group.cache_savings_usd);
    group.cost_status = accounting_completeness(group.provider_calls, group.unpriced_calls);
    group.usage_status = accounting_completeness(group.provider_calls, group.usage_unknown_calls);
}

fn accounting_completeness(provider_calls: i64, unknown_calls: i64) -> AccountingCompleteness {
    if provider_calls == 0 {
        AccountingCompleteness::NoCalls
    } else if unknown_calls == 0 {
        AccountingCompleteness::Reported
    } else if unknown_calls < provider_calls {
        AccountingCompleteness::Partial
    } else {
        AccountingCompleteness::Unknown
    }
}

fn group_key(call: &UsageCall, group_by: UsageGroupBy) -> String {
    match group_by {
        UsageGroupBy::Provider => call.provider.clone(),
        UsageGroupBy::Model => format!("{}/{}", call.provider, call.model),
        UsageGroupBy::Stage => call
            .stage
            .clone()
            .unwrap_or_else(|| "unattributed".to_string()),
        UsageGroupBy::Day => day_bucket(call.occurred_at_ms),
        UsageGroupBy::Week => week_bucket(call.occurred_at_ms),
        UsageGroupBy::Month => month_bucket(call.occurred_at_ms),
    }
}

fn to_utc(occurred_at_ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(occurred_at_ms as i128 * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn day_bucket(occurred_at_ms: i64) -> String {
    let date = to_utc(occurred_at_ms).date();
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

fn week_bucket(occurred_at_ms: i64) -> String {
    let date = to_utc(occurred_at_ms).date();
    format!("{}-W{:02}", iso_week_year(date), date.iso_week())
}

fn month_bucket(occurred_at_ms: i64) -> String {
    let date = to_utc(occurred_at_ms).date();
    format!("{:04}-{:02}", date.year(), u8::from(date.month()))
}

fn iso_week_year(date: Date) -> i32 {
    let week = date.iso_week();
    match (date.month(), week) {
        (Month::January, 52 | 53) => date.year() - 1,
        (Month::December, 1) => date.year() + 1,
        _ => date.year(),
    }
}

/// Parse a `--since`/`--until` boundary into epoch millis. Accepts a
/// bare `YYYY-MM-DD` (interpreted as UTC midnight; the `--until` end is
/// exclusive of the *next* day so a bare date is inclusive of the whole
/// day) or a full RFC3339 timestamp.
fn parse_boundary(raw: Option<&str>, is_until: bool) -> Result<Option<i64>, String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if let Ok(dt) = OffsetDateTime::parse(raw, &Rfc3339) {
        return Ok(Some(dt.unix_timestamp_nanos() as i64 / 1_000_000));
    }
    let date_format = time::format_description::parse_borrowed::<1>("[year]-[month]-[day]")
        .map_err(|error| format!("failed to build date parser: {error}"))?;
    if let Ok(date) = Date::parse(raw, &date_format) {
        let date = if is_until {
            date.next_day().unwrap_or(date)
        } else {
            date
        };
        return Ok(Some(
            date.with_time(Time::MIDNIGHT)
                .assume_utc()
                .unix_timestamp_nanos() as i64
                / 1_000_000,
        ));
    }
    Err(format!(
        "could not parse date `{raw}` (expected YYYY-MM-DD or an RFC3339 timestamp)"
    ))
}

fn group_by_label(group_by: UsageGroupBy) -> &'static str {
    match group_by {
        UsageGroupBy::Provider => "provider",
        UsageGroupBy::Model => "model",
        UsageGroupBy::Stage => "stage",
        UsageGroupBy::Day => "day",
        UsageGroupBy::Week => "week",
        UsageGroupBy::Month => "month",
    }
}

fn round_usd(value: f64) -> f64 {
    (value * 1_000_000_000.0).round() / 1_000_000_000.0
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn print_table(report: &UsageReport, group_by: UsageGroupBy) {
    if report.responses == 0 {
        println!("No LLM usage recorded (0 provider_call_response events matched).");
        if !report.sources.is_empty() {
            println!("Sources: {}", report.sources.join(", "));
        }
        return;
    }

    let header = group_by_label(group_by);
    let time_series = report.totals.cumulative_known_cost_usd.is_some()
        || matches!(
            group_by,
            UsageGroupBy::Day | UsageGroupBy::Week | UsageGroupBy::Month
        );

    println!(
        "harn usage — {} provider calls in {} responses across {} source{}, {} by {}",
        report.provider_calls,
        report.responses,
        report.sources.len(),
        if report.sources.len() == 1 { "" } else { "s" },
        format_accounted_cost(&report.totals),
        header,
    );
    println!();

    if time_series {
        println!(
            "{:<12}  {:>6}  {:>10}  {:>12}  {:>10}  {:>9}  {:>7}",
            header, "calls", "known cost", "known cumulative", "in_tok", "out_tok", "cache%"
        );
        for group in &report.groups {
            println!(
                "{:<12}  {:>6}  {:>10}  {:>12}  {:>10}  {:>9}  {:>7}",
                group.key,
                group.provider_calls,
                format_accounted_cost(group),
                format_usd(group.cumulative_known_cost_usd.unwrap_or(0.0)),
                group.input_tokens,
                group.output_tokens,
                format_cache_ratio(group.cache_hit_ratio),
            );
        }
    } else {
        println!(
            "{:<20}  {:>6}  {:>10}  {:>10}  {:>9}  {:>7}  {:>10}  {:>9}",
            header,
            "calls",
            "known cost",
            "in_tok",
            "out_tok",
            "cache%",
            "cache_save",
            "ms/response"
        );
        for group in &report.groups {
            println!(
                "{:<20}  {:>6}  {:>10}  {:>10}  {:>9}  {:>7}  {:>10}  {:>9.0}",
                group.key,
                group.provider_calls,
                format_accounted_cost(group),
                group.input_tokens,
                group.output_tokens,
                format_cache_ratio(group.cache_hit_ratio),
                format_usd(group.cache_savings_usd),
                group.mean_response_ms,
            );
        }
    }

    println!();
    println!(
        "total: {} provider calls, {}, {} in / {} out tok, {} cache savings",
        report.totals.provider_calls,
        format_accounted_cost(&report.totals),
        report.totals.input_tokens,
        report.totals.output_tokens,
        format_usd(report.totals.cache_savings_usd),
    );
    if report.totals.usage_unknown_calls > 0 {
        println!(
            "warning: token usage is unknown for {} provider call{}; zero token fields are not measurements",
            report.totals.usage_unknown_calls,
            if report.totals.usage_unknown_calls == 1 { "" } else { "s" },
        );
    }
}

fn print_csv(report: &UsageReport) {
    println!(
        "group,responses,provider_calls,cost_status,usage_status,known_cost_usd,unpriced_calls,\
usage_unknown_calls,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,\
cache_savings_usd,cache_hit_ratio,mean_response_ms,cumulative_known_cost_usd"
    );
    for group in &report.groups {
        println!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv_field(&group.key),
            group.responses,
            group.provider_calls,
            completeness_label(group.cost_status),
            completeness_label(group.usage_status),
            group.known_cost_usd,
            group.unpriced_calls,
            group.usage_unknown_calls,
            group.input_tokens,
            group.output_tokens,
            group.cache_read_tokens,
            group.cache_write_tokens,
            group.cache_savings_usd,
            group
                .cache_hit_ratio
                .map(|ratio| ratio.to_string())
                .unwrap_or_default(),
            group.mean_response_ms,
            group
                .cumulative_known_cost_usd
                .map(|v| v.to_string())
                .unwrap_or_default(),
        );
    }
}

fn format_accounted_cost(group: &UsageGroup) -> String {
    match group.cost_status {
        AccountingCompleteness::Reported => format_usd(group.known_cost_usd),
        AccountingCompleteness::Partial => format!(
            "{} known + {} unpriced",
            format_usd(group.known_cost_usd),
            group.unpriced_calls,
        ),
        _ => format!("unknown ({} unpriced)", group.unpriced_calls),
    }
}

const fn completeness_label(status: AccountingCompleteness) -> &'static str {
    match status {
        AccountingCompleteness::NoCalls => "no_calls",
        AccountingCompleteness::Reported => "reported",
        AccountingCompleteness::Partial => "partial",
        AccountingCompleteness::Unknown => "unknown",
    }
}

fn format_cache_ratio(ratio: Option<f64>) -> String {
    ratio
        .map(|value| format!("{:.1}%", value * 100.0))
        .unwrap_or_else(|| "unknown".to_string())
}

fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn format_usd(value: f64) -> String {
    if value != 0.0 && value.abs() < 0.0001 {
        format!("${value:.8}")
    } else {
        format!("${value:.4}")
    }
}

fn emit_error(json: bool, code: &str, message: &str) -> i32 {
    if json {
        let envelope: JsonEnvelope<UsageReport> =
            JsonEnvelope::err(USAGE_SCHEMA_VERSION, code, message);
        println!("{}", to_string_pretty(&envelope));
    } else {
        eprintln!("error: {message}");
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(
        provider: &str,
        model: &str,
        occurred_at_ms: i64,
        input: i64,
        output: i64,
        cache_read: i64,
        cost: f64,
    ) -> UsageCall {
        UsageCall {
            stage: None,
            provider: provider.to_string(),
            model: model.to_string(),
            occurred_at_ms,
            provider_call_count: 1,
            unpriced_calls: 0,
            usage_unknown_calls: 0,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: 0,
            cache_savings_usd: 0.0,
            known_cost_usd: cost,
            response_ms: 100.0,
        }
    }

    fn timestamp_millis(year: i32, month: Month, day: u8, hour: u8, minute: u8, second: u8) -> i64 {
        let date = Date::from_calendar_date(year, month, day).unwrap();
        let time = Time::from_hms(hour, minute, second).unwrap();
        date.with_time(time).assume_utc().unix_timestamp_nanos() as i64 / 1_000_000
    }

    #[test]
    fn provider_rollup_sums_cost_and_tokens() {
        let calls = vec![
            call("openrouter", "qwen", 1_000, 100, 10, 0, 0.02),
            call("openrouter", "qwen", 2_000, 200, 20, 50, 0.03),
            call("anthropic", "sonnet", 3_000, 300, 30, 0, 0.10),
        ];
        let report = aggregate(&calls, UsageGroupBy::Provider, vec![]);
        assert_eq!(report.responses, 3);
        assert_eq!(report.provider_calls, 3);
        assert_eq!(report.groups.len(), 2);
        // Ordered by descending cost: anthropic (0.10) before openrouter (0.05).
        assert_eq!(report.groups[0].key, "anthropic");
        assert!((report.groups[0].known_cost_usd - 0.10).abs() < 1e-9);
        assert_eq!(report.groups[1].key, "openrouter");
        assert!((report.groups[1].known_cost_usd - 0.05).abs() < 1e-9);
        assert_eq!(report.groups[1].input_tokens, 300);
        assert_eq!(report.groups[1].output_tokens, 30);
        assert_eq!(report.groups[1].cache_read_tokens, 50);
        assert!((report.totals.known_cost_usd - 0.15).abs() < 1e-9);
        assert_eq!(report.totals.cost_status, AccountingCompleteness::Reported);
        assert_eq!(report.totals.usage_status, AccountingCompleteness::Reported);
        assert_eq!(report.totals.input_tokens, 600);
    }

    #[test]
    fn empty_report_does_not_claim_reported_zero_accounting() {
        let report = aggregate(&[], UsageGroupBy::Provider, vec![]);
        assert_eq!(report.responses, 0);
        assert_eq!(report.provider_calls, 0);
        assert_eq!(report.totals.cost_status, AccountingCompleteness::NoCalls);
        assert_eq!(report.totals.usage_status, AccountingCompleteness::NoCalls);
    }

    #[test]
    fn known_microcost_never_renders_as_free() {
        let rendered = format_usd(0.000_001_25);
        assert_eq!(rendered, "$0.00000125");
        assert_ne!(rendered, "$0.0000");
    }

    #[test]
    fn absent_accounting_fields_do_not_read_as_zero_cost_or_zero_usage() {
        let event = LogEvent::new(
            PROVIDER_CALL_RESPONSE_KIND,
            serde_json::json!({
                "provider": "openrouter",
                "model": "priced/model",
                "input_tokens": 0,
                "output_tokens": 0,
                "cost_usd": 0.0,
            }),
        );
        let call = normalize_call(&event).expect("provider response");
        assert_eq!(call.provider_call_count, 1);
        assert_eq!(call.unpriced_calls, 1);
        assert_eq!(call.usage_unknown_calls, 1);
        assert_eq!(call.known_cost_usd, 0.0);

        let report = aggregate(&[call], UsageGroupBy::Provider, vec![]);
        assert_eq!(report.totals.cost_status, AccountingCompleteness::Unknown);
        assert_eq!(report.totals.usage_status, AccountingCompleteness::Unknown);
        assert_eq!(report.totals.unpriced_calls, 1);
        assert_eq!(report.totals.usage_unknown_calls, 1);
        assert_eq!(
            format_accounted_cost(&report.totals),
            "unknown (1 unpriced)"
        );
    }

    #[test]
    fn cost_and_usage_completeness_are_independent() {
        let mut usage_known = call("openrouter", "unpriced/model", 1_000, 12, 3, 0, 0.0);
        usage_known.unpriced_calls = 1;
        let usage_report = aggregate(&[usage_known], UsageGroupBy::Provider, vec![]);
        assert_eq!(
            usage_report.totals.cost_status,
            AccountingCompleteness::Unknown
        );
        assert_eq!(
            usage_report.totals.usage_status,
            AccountingCompleteness::Reported
        );

        let mut cost_known = call("openrouter", "priced/model", 1_000, 0, 0, 0, 0.01);
        cost_known.usage_unknown_calls = 1;
        let cost_report = aggregate(&[cost_known], UsageGroupBy::Provider, vec![]);
        assert_eq!(
            cost_report.totals.cost_status,
            AccountingCompleteness::Reported
        );
        assert_eq!(
            cost_report.totals.usage_status,
            AccountingCompleteness::Unknown
        );
    }

    #[test]
    fn cache_hit_ratio_is_response_weighted() {
        // Call A: 100 input, 0 cache -> ratio 0. Call B: 0 input, 100 cache -> ratio 1.
        let calls = vec![
            call("p", "m", 1_000, 100, 0, 0, 0.0),
            call("p", "m", 2_000, 0, 0, 100, 0.0),
        ];
        let report = aggregate(&calls, UsageGroupBy::Provider, vec![]);
        // (0 + 1) / 2 = 0.5.
        assert!((report.groups[0].cache_hit_ratio.unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn day_group_by_produces_cumulative_series() {
        // 2026-01-01 and 2026-01-02 UTC.
        let day1 = timestamp_millis(2026, Month::January, 1, 12, 0, 0);
        let day2 = timestamp_millis(2026, Month::January, 2, 12, 0, 0);
        let calls = vec![
            call("p", "m", day1, 10, 1, 0, 1.0),
            call("p", "m", day2, 10, 1, 0, 2.0),
        ];
        let report = aggregate(&calls, UsageGroupBy::Day, vec![]);
        assert_eq!(report.group_by, "day");
        assert_eq!(report.groups.len(), 2);
        assert_eq!(report.groups[0].key, "2026-01-01");
        assert_eq!(report.groups[1].key, "2026-01-02");
        assert_eq!(report.groups[0].cumulative_known_cost_usd, Some(1.0));
        assert_eq!(report.groups[1].cumulative_known_cost_usd, Some(3.0));
    }

    #[test]
    fn boundary_parsing_bare_date_is_inclusive_day() {
        let since = parse_boundary(Some("2026-01-02"), false).unwrap().unwrap();
        let until = parse_boundary(Some("2026-01-02"), true).unwrap().unwrap();
        let expected_since = timestamp_millis(2026, Month::January, 2, 0, 0, 0);
        let expected_until = timestamp_millis(2026, Month::January, 3, 0, 0, 0);
        assert_eq!(since, expected_since);
        assert_eq!(until, expected_until);
    }

    #[test]
    fn rfc3339_boundary_parses() {
        let value = parse_boundary(Some("2026-01-02T06:00:00Z"), false)
            .unwrap()
            .unwrap();
        let expected = timestamp_millis(2026, Month::January, 2, 6, 0, 0);
        assert_eq!(value, expected);
    }
}
