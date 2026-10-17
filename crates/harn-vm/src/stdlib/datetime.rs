use crate::value::VmDictExt;

use chrono::{
    DateTime, Datelike, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone, Timelike, Utc,
};
use chrono_tz::Tz;

use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const DEFAULT_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

pub(crate) fn register_datetime_builtins(vm: &mut Vm) {
    crate::stdlib::macros::register_module_builtins(vm, module_path!());
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "date_now() -> dict", category = "datetime"
)]
fn date_now_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    date_dict_from_millis(crate::stdlib::clock::now_wall_ms())
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "date_now_iso() -> string", category = "datetime"
)]
fn date_now_iso_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let now = utc_from_millis(crate::stdlib::clock::now_wall_ms())?;
    Ok(VmValue::string(
        now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    ))
}

fn utc_from_millis(millis: i64) -> Result<DateTime<Utc>, VmError> {
    DateTime::<Utc>::from_timestamp_millis(millis)
        .ok_or_else(|| VmError::Runtime(format!("clock timestamp is out of range: {millis}")))
}

pub(crate) fn date_dict_from_millis(millis: i64) -> Result<VmValue, VmError> {
    let now = utc_from_millis(millis)?;
    let mut result = utc_datetime_dict(now);
    result.put("timestamp", VmValue::Float(millis as f64 / 1_000.0));
    result.put_str(
        "iso8601",
        now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    );
    Ok(VmValue::dict(result))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "date_format(timestamp: int | float | dict, format?: string, timezone?: string) -> string",
    category = "datetime"
)]
fn date_format_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dt = datetime_from_arg(args.first(), "date_format")?;
    let fmt = args
        .get(1)
        .map_or_else(|| DEFAULT_FORMAT.to_string(), VmValue::display);
    let mut formatted = String::new();
    match args.get(2) {
        Some(VmValue::Nil) | None => dt.format(&fmt).write_to(&mut formatted),
        Some(tz_arg) => dt
            .with_timezone(&parse_timezone(&tz_arg.display(), "date_format")?)
            .format(&fmt)
            .write_to(&mut formatted),
    }
    .map_err(|_| vm_error("date_format: invalid format"))?;
    Ok(VmValue::string(formatted))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "date_parse(text: string?) -> int | float",
    category = "datetime"
)]
fn date_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let input = args.first().map(|a| a.display()).unwrap_or_default();
    let dt = parse_datetime_auto(&input)?;
    Ok(timestamp_value(dt))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "date_in_zone(timestamp: int | float | dict, timezone: string) -> dict",
    category = "datetime"
)]
fn date_in_zone_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dt = datetime_from_arg(args.first(), "date_in_zone")?;
    let tz_arg = require_arg(args, 1, "date_in_zone", "timezone")?;
    let tz_name = tz_arg.display();
    let tz = parse_timezone(&tz_name, "date_in_zone")?;
    let local = dt.with_timezone(&tz);
    let mut result = zoned_datetime_dict(local);
    result.put_str("zone", tz_name);
    Ok(VmValue::dict(result))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "date_to_zone(timestamp: int | float | dict, timezone: string) -> string",
    category = "datetime"
)]
fn date_to_zone_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dt = datetime_from_arg(args.first(), "date_to_zone")?;
    let tz_arg = require_arg(args, 1, "date_to_zone", "timezone")?;
    let tz = parse_timezone(&tz_arg.display(), "date_to_zone")?;
    Ok(VmValue::string(dt.with_timezone(&tz).to_rfc3339()))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "date_from_components(components: dict, timezone?: string) -> int | float",
    category = "datetime"
)]
fn date_from_components_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let components = require_dict(args.first(), "date_from_components")?;
    let tz = match args.get(1) {
        Some(VmValue::Nil) | None => chrono_tz::UTC,
        Some(value) => parse_timezone(&value.display(), "date_from_components")?,
    };
    let dt = datetime_from_components(components, tz)?;
    Ok(timestamp_value(dt.with_timezone(&Utc)))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "duration_ms(count: int | float) -> duration",
    category = "datetime"
)]
fn duration_ms_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    duration_from_number(args.first(), 1, "duration_ms").map(VmValue::Duration)
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "duration_seconds(count: int | float) -> duration",
    category = "datetime"
)]
fn duration_seconds_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    duration_from_number(args.first(), 1_000, "duration_seconds").map(VmValue::Duration)
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "duration_minutes(count: int | float) -> duration",
    category = "datetime"
)]
fn duration_minutes_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    duration_from_number(args.first(), 60_000, "duration_minutes").map(VmValue::Duration)
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "duration_hours(count: int | float) -> duration",
    category = "datetime"
)]
fn duration_hours_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    duration_from_number(args.first(), 3_600_000, "duration_hours").map(VmValue::Duration)
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "duration_days(count: int | float) -> duration",
    category = "datetime"
)]
fn duration_days_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    duration_from_number(args.first(), 86_400_000, "duration_days").map(VmValue::Duration)
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "date_add(timestamp: int | float | dict, duration: duration) -> int | float",
    category = "datetime"
)]
fn date_add_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let millis = timestamp_millis_from_arg(args.first(), "date_add")?;
    let duration = require_duration(args.get(1), "date_add")?;
    timestamp_millis_value(
        millis
            .checked_add(duration as i128)
            .ok_or_else(|| vm_error("date_add: timestamp overflow"))?,
    )
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "date_diff(a: int | float | dict, b: int | float | dict) -> duration",
    category = "datetime"
)]
fn date_diff_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let a = timestamp_millis_from_arg(args.first(), "date_diff")?;
    let b = timestamp_millis_from_arg(args.get(1), "date_diff")?;
    let diff = a
        .checked_sub(b)
        .ok_or_else(|| vm_error("date_diff: duration overflow"))?;
    let diff = i64::try_from(diff).map_err(|_| vm_error("date_diff: duration overflow"))?;
    Ok(VmValue::Duration(diff))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "duration_to_seconds(duration: duration) -> int",
    category = "datetime"
)]
fn duration_to_seconds_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let duration = require_duration(args.first(), "duration_to_seconds")?;
    Ok(VmValue::Int(duration / 1_000))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "duration_to_human(duration: duration) -> string",
    category = "datetime"
)]
fn duration_to_human_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let duration = require_duration(args.first(), "duration_to_human")?;
    Ok(VmValue::string(format_duration_human(duration)))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "weekday_name(timestamp: int | float | dict, timezone?: string) -> string",
    category = "datetime"
)]
fn weekday_name_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dt = datetime_from_arg(args.first(), "weekday_name")?;
    let name = match args.get(1) {
        Some(VmValue::Nil) | None => dt.format("%A").to_string(),
        Some(value) => {
            let tz = parse_timezone(&value.display(), "weekday_name")?;
            dt.with_timezone(&tz).format("%A").to_string()
        }
    };
    Ok(VmValue::string(name))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "month_name(timestamp: int | float | dict, timezone?: string) -> string",
    category = "datetime"
)]
fn month_name_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dt = datetime_from_arg(args.first(), "month_name")?;
    let name = match args.get(1) {
        Some(VmValue::Nil) | None => dt.format("%B").to_string(),
        Some(value) => {
            let tz = parse_timezone(&value.display(), "month_name")?;
            dt.with_timezone(&tz).format("%B").to_string()
        }
    };
    Ok(VmValue::string(name))
}

fn require_arg<'a>(
    args: &'a [VmValue],
    index: usize,
    builtin: &str,
    label: &str,
) -> Result<&'a VmValue, VmError> {
    args.get(index)
        .ok_or_else(|| vm_error(format!("{builtin}: missing {label} argument")))
}

fn require_dict<'a>(
    value: Option<&'a VmValue>,
    builtin: &str,
) -> Result<&'a crate::value::DictMap, VmError> {
    match value {
        Some(VmValue::Dict(map)) => Ok(map),
        Some(other) => Err(vm_error(format!(
            "{builtin}: expected dict, got {}",
            other.type_name()
        ))),
        None => Err(vm_error(format!("{builtin}: missing components argument"))),
    }
}

pub(crate) fn vm_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::string(message.into()))
}

fn utc_datetime_dict(dt: DateTime<Utc>) -> crate::value::DictMap {
    let mut result = crate::value::DictMap::new();
    result.put_int("year", dt.year() as i64);
    result.put_int("month", dt.month() as i64);
    result.put_int("day", dt.day() as i64);
    result.put_int("hour", dt.hour() as i64);
    result.put_int("minute", dt.minute() as i64);
    result.put_int("second", dt.second() as i64);
    result.put_int("weekday", dt.weekday().num_days_from_sunday() as i64);
    result
}

fn zoned_datetime_dict(dt: DateTime<Tz>) -> crate::value::DictMap {
    let mut result = crate::value::DictMap::new();
    result.put_int("year", dt.year() as i64);
    result.put_int("month", dt.month() as i64);
    result.put_int("day", dt.day() as i64);
    result.put_int("hour", dt.hour() as i64);
    result.put_int("minute", dt.minute() as i64);
    result.put_int("second", dt.second() as i64);
    result.put_int("weekday", dt.weekday().num_days_from_sunday() as i64);
    result.put_int("offset_seconds", dt.offset().fix().local_minus_utc() as i64);
    result.insert(
        crate::value::intern_key("timestamp"),
        timestamp_value(dt.with_timezone(&Utc)),
    );
    result.put_str("iso8601", dt.to_rfc3339());
    result
}

pub(crate) fn parse_timezone(raw: &str, builtin: &str) -> Result<Tz, VmError> {
    raw.parse::<Tz>()
        .map_err(|_| vm_error(format!("{builtin}: unknown timezone '{raw}'")))
}

fn parse_datetime_auto(input: &str) -> Result<DateTime<Utc>, VmError> {
    let trimmed = input.trim();

    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }

    // RFC 3339 requires seconds; ISO 8601 also permits minute precision.
    for fmt in ["%Y-%m-%dT%H:%M%:z", "%Y-%m-%d %H:%M%:z"] {
        if let Ok(dt) = DateTime::parse_from_str(trimmed, fmt) {
            return Ok(dt.with_timezone(&Utc));
        }
    }

    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%MZ",
        "%Y-%m-%dT%H:%MZ",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Ok(Utc.from_utc_datetime(&dt));
        }
    }

    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let dt = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| vm_error(format!("Cannot parse date: {input}")))?;
        return Ok(Utc.from_utc_datetime(&dt));
    }

    Err(vm_error(format!("Cannot parse date: {input}")))
}

fn validate_component_ranges(
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Result<(), VmError> {
    if !(1..=12).contains(&month) {
        return Err(vm_error(format!("Invalid month: {month} (must be 1-12)")));
    }
    if !(1..=31).contains(&day) {
        return Err(vm_error(format!("Invalid day: {day} (must be 1-31)")));
    }
    if hour > 23 {
        return Err(vm_error(format!("Invalid hour: {hour} (must be 0-23)")));
    }
    if minute > 59 {
        return Err(vm_error(format!("Invalid minute: {minute} (must be 0-59)")));
    }
    if second > 59 {
        return Err(vm_error(format!("Invalid second: {second} (must be 0-59)")));
    }
    Ok(())
}

pub(crate) fn datetime_from_arg(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<DateTime<Utc>, VmError> {
    let value = value.ok_or_else(|| vm_error(format!("{builtin}: missing timestamp argument")))?;
    match value {
        VmValue::Dict(map) => datetime_from_arg(map.get("timestamp"), builtin),
        VmValue::Int(seconds) => DateTime::from_timestamp(*seconds, 0)
            .ok_or_else(|| vm_error(format!("{builtin}: timestamp out of range"))),
        VmValue::Float(seconds) => datetime_from_float(*seconds, builtin),
        other => Err(vm_error(format!(
            "{builtin}: `timestamp` must be an int, a float, or a date dict; got {}",
            other.type_name()
        ))),
    }
}

fn datetime_from_float(seconds: f64, builtin: &str) -> Result<DateTime<Utc>, VmError> {
    if !seconds.is_finite() {
        return Err(vm_error(format!("{builtin}: timestamp must be finite")));
    }
    let micros = (seconds * 1_000_000.0).round();
    if micros < i128::MIN as f64 || micros > i128::MAX as f64 {
        return Err(vm_error(format!("{builtin}: timestamp out of range")));
    }
    let micros = micros as i128;
    let secs = i64::try_from(micros.div_euclid(1_000_000))
        .map_err(|_| vm_error(format!("{builtin}: timestamp out of range")))?;
    let nanos = (micros.rem_euclid(1_000_000) as u32) * 1_000;
    DateTime::from_timestamp(secs, nanos)
        .ok_or_else(|| vm_error(format!("{builtin}: timestamp out of range")))
}

pub(crate) fn timestamp_value(dt: DateTime<Utc>) -> VmValue {
    if dt.timestamp_subsec_nanos() == 0 {
        VmValue::Int(dt.timestamp())
    } else {
        VmValue::Float(dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_nanos()) / 1e9)
    }
}

fn timestamp_millis_from_arg(value: Option<&VmValue>, builtin: &str) -> Result<i128, VmError> {
    let dt = datetime_from_arg(value, builtin)?;
    Ok(i128::from(dt.timestamp()) * 1_000 + i128::from(dt.timestamp_subsec_millis()))
}

fn timestamp_millis_value(millis: i128) -> Result<VmValue, VmError> {
    if millis % 1_000 == 0 {
        let seconds =
            i64::try_from(millis / 1_000).map_err(|_| vm_error("timestamp value out of range"))?;
        Ok(VmValue::Int(seconds))
    } else {
        Ok(VmValue::Float(millis as f64 / 1_000.0))
    }
}

fn component_i64(
    map: &crate::value::DictMap,
    key: &str,
    default: Option<i64>,
) -> Result<i64, VmError> {
    match map.get(key) {
        Some(VmValue::Int(value)) => Ok(*value),
        Some(VmValue::Float(value)) if value.fract() == 0.0 => Ok(*value as i64),
        Some(other) => Err(vm_error(format!(
            "date_from_components: {key} must be an int, got {}",
            other.type_name()
        ))),
        None => default.ok_or_else(|| vm_error(format!("date_from_components: missing {key}"))),
    }
}

fn datetime_from_components(map: &crate::value::DictMap, tz: Tz) -> Result<DateTime<Tz>, VmError> {
    let year = i32::try_from(component_i64(map, "year", None)?)
        .map_err(|_| vm_error("date_from_components: year out of range"))?;
    let month = u32::try_from(component_i64(map, "month", None)?).unwrap_or(0);
    let day = u32::try_from(component_i64(map, "day", None)?).unwrap_or(0);
    let hour = u32::try_from(component_i64(map, "hour", Some(0))?).unwrap_or(u32::MAX);
    let minute = u32::try_from(component_i64(map, "minute", Some(0))?).unwrap_or(u32::MAX);
    let second = u32::try_from(component_i64(map, "second", Some(0))?).unwrap_or(u32::MAX);
    validate_component_ranges(month, day, hour, minute, second)?;
    let date = NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| vm_error(format!("Invalid date: {year:04}-{month:02}-{day:02}")))?;
    let naive = date
        .and_hms_opt(hour, minute, second)
        .ok_or_else(|| vm_error(format!("Invalid time: {hour:02}:{minute:02}:{second:02}")))?;
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Ok(dt),
        LocalResult::Ambiguous(earlier, _) => Ok(earlier),
        LocalResult::None => Err(vm_error(
            "date_from_components: local time does not exist in that timezone",
        )),
    }
}

fn duration_from_number(
    value: Option<&VmValue>,
    multiplier_ms: i64,
    builtin: &str,
) -> Result<i64, VmError> {
    let value = value.ok_or_else(|| vm_error(format!("{builtin}: missing value argument")))?;
    let number = match value {
        VmValue::Int(value) => value
            .checked_mul(multiplier_ms)
            .ok_or_else(|| vm_error(format!("{builtin}: duration overflow")))?,
        VmValue::Float(value) if value.is_finite() => {
            let millis = *value * multiplier_ms as f64;
            if millis < i64::MIN as f64 || millis > i64::MAX as f64 {
                return Err(vm_error(format!("{builtin}: duration overflow")));
            }
            millis.round() as i64
        }
        other => {
            return Err(vm_error(format!(
                "{builtin}: expected an int or a float, got {}",
                other.type_name()
            )));
        }
    };
    Ok(number)
}

fn require_duration(value: Option<&VmValue>, builtin: &str) -> Result<i64, VmError> {
    match value {
        Some(VmValue::Duration(ms)) => Ok(*ms),
        Some(other) => Err(vm_error(format!(
            "{builtin}: expected duration, got {}",
            other.type_name()
        ))),
        None => Err(vm_error(format!("{builtin}: missing duration argument"))),
    }
}

fn format_duration_human(duration: i64) -> String {
    if duration == 0 {
        return "0s".to_string();
    }

    let sign = if duration < 0 { "-" } else { "" };
    let mut remaining = duration.unsigned_abs();
    let units = [
        ("d", 86_400_000_u64),
        ("h", 3_600_000_u64),
        ("m", 60_000_u64),
        ("s", 1_000_u64),
        ("ms", 1_u64),
    ];
    let mut parts = Vec::new();
    for (label, size) in units {
        let count = remaining / size;
        if count > 0 {
            parts.push(format!("{count}{label}"));
            remaining %= size;
        }
        if parts.len() == 3 {
            break;
        }
    }
    format!("{sign}{}", parts.join(" "))
}
