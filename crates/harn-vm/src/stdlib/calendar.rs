//! Civil calendar helpers backing the `std/calendar` Harn module.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, LocalResult, Months, NaiveDate, NaiveDateTime,
    NaiveTime, Offset, TimeZone, Timelike, Utc, Weekday,
};
use chrono_tz::Tz;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::datetime::{datetime_from_arg, parse_timezone, timestamp_value, vm_error};

const DEFAULT_TIMEZONE: &str = "UTC";
const DEFAULT_HOLIDAY_CALENDAR: &str = "US-FEDERAL";
const DEFAULT_BUSINESS_TIMEZONE: &str = "America/New_York";
const MAX_CALENDAR_RANGE_ITEMS: usize = 10_000;
const MAX_BUSINESS_DAY_SCAN: i64 = 100_000;
const MIN_HOLIDAY_YEAR: i32 = 1900;
const MAX_HOLIDAY_YEAR: i32 = 9999;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalDisambiguation {
    Earlier,
    Later,
    Reject,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CalendarUnit {
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoundaryEdge {
    Start,
    End,
}

#[derive(Clone, Copy)]
struct CountryMeta {
    code: &'static str,
    name: &'static str,
    timezones: &'static [&'static str],
    currency_code: &'static str,
    currency_name: &'static str,
    holiday_calendars: &'static [&'static str],
}

#[derive(Clone)]
struct Holiday {
    date: NaiveDate,
    actual_date: NaiveDate,
    name: &'static str,
    observed: bool,
}

#[derive(Clone)]
struct BusinessCalendar {
    name: String,
    timezone: Tz,
    weekends: BTreeSet<u32>,
    supported_holiday_calendar: Option<&'static str>,
    extra_holidays: BTreeMap<NaiveDate, String>,
}

static COUNTRIES: &[CountryMeta] = &[
    CountryMeta {
        code: "AU",
        name: "Australia",
        timezones: &[
            "Australia/Perth",
            "Australia/Darwin",
            "Australia/Adelaide",
            "Australia/Brisbane",
            "Australia/Sydney",
            "Australia/Melbourne",
            "Australia/Hobart",
        ],
        currency_code: "AUD",
        currency_name: "Australian dollar",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "BR",
        name: "Brazil",
        timezones: &[
            "America/Noronha",
            "America/Belem",
            "America/Fortaleza",
            "America/Recife",
            "America/Araguaina",
            "America/Maceio",
            "America/Bahia",
            "America/Sao_Paulo",
            "America/Campo_Grande",
            "America/Cuiaba",
            "America/Manaus",
            "America/Porto_Velho",
            "America/Boa_Vista",
            "America/Rio_Branco",
            "America/Eirunepe",
        ],
        currency_code: "BRL",
        currency_name: "Brazilian real",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "CA",
        name: "Canada",
        timezones: &[
            "America/St_Johns",
            "America/Halifax",
            "America/Toronto",
            "America/Winnipeg",
            "America/Regina",
            "America/Edmonton",
            "America/Vancouver",
            "America/Whitehorse",
            "America/Yellowknife",
            "America/Iqaluit",
        ],
        currency_code: "CAD",
        currency_name: "Canadian dollar",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "CN",
        name: "China",
        timezones: &["Asia/Shanghai"],
        currency_code: "CNY",
        currency_name: "Chinese yuan",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "DE",
        name: "Germany",
        timezones: &["Europe/Berlin"],
        currency_code: "EUR",
        currency_name: "Euro",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "FR",
        name: "France",
        timezones: &["Europe/Paris"],
        currency_code: "EUR",
        currency_name: "Euro",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "GB",
        name: "United Kingdom",
        timezones: &["Europe/London"],
        currency_code: "GBP",
        currency_name: "Pound sterling",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "IN",
        name: "India",
        timezones: &["Asia/Kolkata"],
        currency_code: "INR",
        currency_name: "Indian rupee",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "JP",
        name: "Japan",
        timezones: &["Asia/Tokyo"],
        currency_code: "JPY",
        currency_name: "Japanese yen",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "MX",
        name: "Mexico",
        timezones: &[
            "America/Mexico_City",
            "America/Cancun",
            "America/Chihuahua",
            "America/Hermosillo",
            "America/Mazatlan",
            "America/Tijuana",
        ],
        currency_code: "MXN",
        currency_name: "Mexican peso",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "NL",
        name: "Netherlands",
        timezones: &["Europe/Amsterdam"],
        currency_code: "EUR",
        currency_name: "Euro",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "NZ",
        name: "New Zealand",
        timezones: &["Pacific/Auckland", "Pacific/Chatham"],
        currency_code: "NZD",
        currency_name: "New Zealand dollar",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "SG",
        name: "Singapore",
        timezones: &["Asia/Singapore"],
        currency_code: "SGD",
        currency_name: "Singapore dollar",
        holiday_calendars: &[],
    },
    CountryMeta {
        code: "US",
        name: "United States",
        timezones: &[
            "America/New_York",
            "America/Chicago",
            "America/Denver",
            "America/Phoenix",
            "America/Los_Angeles",
            "America/Anchorage",
            "Pacific/Honolulu",
        ],
        currency_code: "USD",
        currency_name: "United States dollar",
        holiday_calendars: &["US-FEDERAL"],
    },
    CountryMeta {
        code: "ZA",
        name: "South Africa",
        timezones: &["Africa/Johannesburg"],
        currency_code: "ZAR",
        currency_name: "South African rand",
        holiday_calendars: &[],
    },
];

pub(crate) fn register_calendar_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &CALENDAR_PARTS_BUILTIN_DEF,
    &CALENDAR_FROM_LOCAL_BUILTIN_DEF,
    &CALENDAR_BOUNDARY_BUILTIN_DEF,
    &CALENDAR_ADD_BUILTIN_DEF,
    &CALENDAR_DATE_RANGE_BUILTIN_DEF,
    &CALENDAR_NEXT_WEEKDAY_BUILTIN_DEF,
    &CALENDAR_COUNTRIES_BUILTIN_DEF,
    &CALENDAR_COUNTRY_INFO_BUILTIN_DEF,
    &CALENDAR_SUPPORTED_HOLIDAY_CALENDARS_BUILTIN_DEF,
    &CALENDAR_HOLIDAYS_BUILTIN_DEF,
    &CALENDAR_IS_HOLIDAY_BUILTIN_DEF,
    &CALENDAR_IS_BUSINESS_DAY_BUILTIN_DEF,
    &CALENDAR_NEXT_BUSINESS_DAY_BUILTIN_DEF,
    &CALENDAR_ADD_BUSINESS_DAYS_BUILTIN_DEF,
    &CALENDAR_BUSINESS_DAYS_BETWEEN_BUILTIN_DEF,
    &CALENDAR_BUSINESS_WINDOW_BUILTIN_DEF,
];

#[harn_builtin(
    sig = "__calendar_parts(timestamp: any, timezone?: string) -> dict",
    category = "calendar"
)]
fn calendar_parts_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let timezone = timezone_arg(
        args.get(1),
        DEFAULT_TIMEZONE,
        "__calendar_parts",
        "timezone",
    )?;
    let dt = datetime_like_arg(args.first(), timezone, "__calendar_parts")?;
    Ok(VmValue::dict(parts_dict(dt.with_timezone(&timezone))))
}

#[harn_builtin(
    sig = "__calendar_from_local(parts: dict, timezone?: string, disambiguation?: string) -> int | float",
    category = "calendar"
)]
fn calendar_from_local_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let parts = require_dict(args.first(), "__calendar_from_local", "parts")?;
    let timezone = timezone_arg(
        args.get(1),
        DEFAULT_TIMEZONE,
        "__calendar_from_local",
        "timezone",
    )?;
    let disambiguation = disambiguation_arg(args.get(2), LocalDisambiguation::Earlier)?;
    let naive = naive_datetime_from_parts(parts, "__calendar_from_local")?;
    let dt = resolve_local_datetime(timezone, naive, disambiguation, "__calendar_from_local")?;
    Ok(timestamp_value(dt.with_timezone(&Utc)))
}

#[harn_builtin(
    sig = "__calendar_boundary(timestamp: any, unit: string, edge: string, timezone?: string) -> int | float",
    category = "calendar"
)]
fn calendar_boundary_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let timezone = timezone_arg(
        args.get(3),
        DEFAULT_TIMEZONE,
        "__calendar_boundary",
        "timezone",
    )?;
    let dt = datetime_like_arg(args.first(), timezone, "__calendar_boundary")?;
    let unit = calendar_unit_arg(args.get(1), "__calendar_boundary")?;
    let edge = boundary_edge_arg(args.get(2), "__calendar_boundary")?;
    Ok(timestamp_value(boundary_for(
        dt.with_timezone(&timezone),
        unit,
        edge,
        "__calendar_boundary",
    )?))
}

#[harn_builtin(
    sig = "__calendar_add(timestamp: any, amount: int, unit: string, timezone?: string, disambiguation?: string) -> int | float",
    category = "calendar"
)]
fn calendar_add_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let timezone = timezone_arg(args.get(3), DEFAULT_TIMEZONE, "__calendar_add", "timezone")?;
    let dt = datetime_like_arg(args.first(), timezone, "__calendar_add")?;
    let amount = int_arg(args.get(1), "__calendar_add", "amount")?;
    let unit = calendar_unit_arg(args.get(2), "__calendar_add")?;
    let disambiguation = disambiguation_arg(args.get(4), LocalDisambiguation::Earlier)?;
    Ok(timestamp_value(add_calendar_units(
        dt.with_timezone(&timezone),
        amount,
        unit,
        disambiguation,
        "__calendar_add",
    )?))
}

#[harn_builtin(
    sig = "__calendar_date_range(start: any, end: any, unit: string, timezone?: string, options?: dict) -> list",
    category = "calendar"
)]
fn calendar_date_range_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let timezone = timezone_arg(
        args.get(3),
        DEFAULT_TIMEZONE,
        "__calendar_date_range",
        "timezone",
    )?;
    let start = datetime_like_arg(args.first(), timezone, "__calendar_date_range")?;
    let end = datetime_like_arg(args.get(1), timezone, "__calendar_date_range")?;
    let unit = calendar_unit_arg(args.get(2), "__calendar_date_range")?;
    let options = optional_dict(args.get(4), "__calendar_date_range", "options")?;
    let max_items = options
        .and_then(|map| map.get("max_items"))
        .map(|value| usize_arg(Some(value), "__calendar_date_range", "max_items"))
        .transpose()?
        .unwrap_or(MAX_CALENDAR_RANGE_ITEMS);
    let include_end = match options.and_then(|map| map.get("include_end")) {
        Some(value) => {
            bool_arg(Some(value), "__calendar_date_range", "include_end")?.unwrap_or(false)
        }
        None => false,
    };
    let disambiguation = options
        .and_then(|map| map.get("disambiguation"))
        .map(|value| disambiguation_arg(Some(value), LocalDisambiguation::Earlier))
        .transpose()?
        .unwrap_or(LocalDisambiguation::Earlier);

    let mut out = Vec::new();
    let end_local = end.with_timezone(&timezone);
    let mut current = start.with_timezone(&timezone);
    while if include_end {
        current <= end_local
    } else {
        current < end_local
    } {
        if out.len() >= max_items {
            return Err(vm_error(format!(
                "__calendar_date_range: exceeded max_items ({max_items})"
            )));
        }
        out.push(timestamp_value(current.with_timezone(&Utc)));
        current = add_calendar_units(current, 1, unit, disambiguation, "__calendar_date_range")?
            .with_timezone(&timezone);
    }
    Ok(VmValue::List(std::sync::Arc::new(out)))
}

#[harn_builtin(
    sig = "__calendar_next_weekday(timestamp: any, weekday: any, direction: string, timezone?: string, include_today?: bool) -> int | float",
    category = "calendar"
)]
fn calendar_next_weekday_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let timezone = timezone_arg(
        args.get(3),
        DEFAULT_TIMEZONE,
        "__calendar_next_weekday",
        "timezone",
    )?;
    let dt = datetime_like_arg(args.first(), timezone, "__calendar_next_weekday")?;
    let target = weekday_arg(args.get(1), "__calendar_next_weekday", "weekday")?;
    let direction = string_arg(args.get(2), "next", "__calendar_next_weekday", "direction")?;
    let include_today =
        bool_arg(args.get(4), "__calendar_next_weekday", "include_today")?.unwrap_or(false);
    let delta = weekday_delta(
        dt.with_timezone(&timezone).weekday(),
        target,
        &direction,
        include_today,
    )?;
    let local = dt.with_timezone(&timezone);
    let next_date = local
        .date_naive()
        .checked_add_signed(ChronoDuration::days(delta))
        .ok_or_else(|| vm_error("__calendar_next_weekday: date out of range"))?;
    let naive = NaiveDateTime::new(next_date, local.time());
    let resolved = resolve_local_datetime(
        timezone,
        naive,
        LocalDisambiguation::Earlier,
        "__calendar_next_weekday",
    )?;
    Ok(timestamp_value(resolved.with_timezone(&Utc)))
}

#[harn_builtin(sig = "__calendar_countries() -> list", category = "calendar")]
fn calendar_countries_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::List(std::sync::Arc::new(
        COUNTRIES.iter().map(country_value).collect(),
    )))
}

#[harn_builtin(
    sig = "__calendar_country_info(code: string) -> dict | nil",
    category = "calendar"
)]
fn calendar_country_info_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let code = string_arg(args.first(), "", "__calendar_country_info", "code")?;
    Ok(country_by_code(&code)
        .map(country_value)
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    sig = "__calendar_supported_holiday_calendars() -> list",
    category = "calendar"
)]
fn calendar_supported_holiday_calendars_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(VmValue::List(std::sync::Arc::new(vec![
        holiday_calendar_info(DEFAULT_HOLIDAY_CALENDAR),
    ])))
}

#[harn_builtin(
    sig = "__calendar_holidays(calendar: string?, year: int) -> list",
    category = "calendar"
)]
fn calendar_holidays_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let calendar = holiday_calendar_name_arg(args.first(), DEFAULT_HOLIDAY_CALENDAR)?;
    let year = i32::try_from(int_arg(args.get(1), "__calendar_holidays", "year")?)
        .map_err(|_| vm_error("__calendar_holidays: year out of range"))?;
    let mut holidays = supported_holidays_for_year(calendar, year)?;
    holidays.sort_by_key(|holiday| holiday.date);
    Ok(VmValue::List(std::sync::Arc::new(
        holidays.iter().map(holiday_value).collect(),
    )))
}

#[harn_builtin(
    sig = "__calendar_is_holiday(timestamp: any, calendar?: any, timezone?: string) -> bool",
    category = "calendar"
)]
fn calendar_is_holiday_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let calendar = business_calendar_arg(args.get(1), args.get(2), "__calendar_is_holiday")?;
    let dt = datetime_like_arg(args.first(), calendar.timezone, "__calendar_is_holiday")?;
    let local_date = dt.with_timezone(&calendar.timezone).date_naive();
    Ok(VmValue::Bool(is_holiday_date(local_date, &calendar)?))
}

#[harn_builtin(
    sig = "__calendar_is_business_day(timestamp: any, calendar?: any, timezone?: string) -> bool",
    category = "calendar"
)]
fn calendar_is_business_day_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let calendar = business_calendar_arg(args.get(1), args.get(2), "__calendar_is_business_day")?;
    let dt = datetime_like_arg(
        args.first(),
        calendar.timezone,
        "__calendar_is_business_day",
    )?;
    let local_date = dt.with_timezone(&calendar.timezone).date_naive();
    Ok(VmValue::Bool(is_business_date(local_date, &calendar)?))
}

#[harn_builtin(
    sig = "__calendar_next_business_day(timestamp: any, calendar?: any, timezone?: string, include_today?: bool) -> int | float",
    category = "calendar"
)]
fn calendar_next_business_day_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let calendar = business_calendar_arg(args.get(1), args.get(2), "__calendar_next_business_day")?;
    let dt = datetime_like_arg(
        args.first(),
        calendar.timezone,
        "__calendar_next_business_day",
    )?;
    let include_today =
        bool_arg(args.get(3), "__calendar_next_business_day", "include_today")?.unwrap_or(false);
    let local = dt.with_timezone(&calendar.timezone);
    let mut date = local.date_naive();
    if !include_today {
        date = date
            .checked_add_signed(ChronoDuration::days(1))
            .ok_or_else(|| vm_error("__calendar_next_business_day: date out of range"))?;
    }
    for _ in 0..MAX_BUSINESS_DAY_SCAN {
        if is_business_date(date, &calendar)? {
            let naive = NaiveDateTime::new(date, local.time());
            let resolved = resolve_local_datetime(
                calendar.timezone,
                naive,
                LocalDisambiguation::Earlier,
                "__calendar_next_business_day",
            )?;
            return Ok(timestamp_value(resolved.with_timezone(&Utc)));
        }
        date = date
            .checked_add_signed(ChronoDuration::days(1))
            .ok_or_else(|| vm_error("__calendar_next_business_day: date out of range"))?;
    }
    Err(vm_error(
        "__calendar_next_business_day: business-day search exceeded guardrail",
    ))
}

#[harn_builtin(
    sig = "__calendar_add_business_days(timestamp: any, days: int, calendar?: any, timezone?: string) -> int | float",
    category = "calendar"
)]
fn calendar_add_business_days_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let calendar = business_calendar_arg(args.get(2), args.get(3), "__calendar_add_business_days")?;
    let dt = datetime_like_arg(
        args.first(),
        calendar.timezone,
        "__calendar_add_business_days",
    )?;
    let amount = int_arg(args.get(1), "__calendar_add_business_days", "days")?;
    if amount == 0 {
        return Ok(timestamp_value(dt));
    }
    let step = if amount > 0 { 1 } else { -1 };
    let local = dt.with_timezone(&calendar.timezone);
    let mut date = local.date_naive();
    let mut remaining = amount.unsigned_abs();
    for _ in 0..MAX_BUSINESS_DAY_SCAN {
        date = date
            .checked_add_signed(ChronoDuration::days(step))
            .ok_or_else(|| vm_error("__calendar_add_business_days: date out of range"))?;
        if is_business_date(date, &calendar)? {
            remaining -= 1;
            if remaining == 0 {
                let naive = NaiveDateTime::new(date, local.time());
                let resolved = resolve_local_datetime(
                    calendar.timezone,
                    naive,
                    LocalDisambiguation::Earlier,
                    "__calendar_add_business_days",
                )?;
                return Ok(timestamp_value(resolved.with_timezone(&Utc)));
            }
        }
    }
    Err(vm_error(
        "__calendar_add_business_days: business-day search exceeded guardrail",
    ))
}

#[harn_builtin(
    sig = "__calendar_business_days_between(start: any, end: any, calendar?: any, timezone?: string) -> int",
    category = "calendar"
)]
fn calendar_business_days_between_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let calendar =
        business_calendar_arg(args.get(2), args.get(3), "__calendar_business_days_between")?;
    let start = datetime_like_arg(
        args.first(),
        calendar.timezone,
        "__calendar_business_days_between",
    )?;
    let end = datetime_like_arg(
        args.get(1),
        calendar.timezone,
        "__calendar_business_days_between",
    )?;
    let start_date = start.with_timezone(&calendar.timezone).date_naive();
    let end_date = end.with_timezone(&calendar.timezone).date_naive();
    if start_date == end_date {
        return Ok(VmValue::Int(0));
    }
    let step = if start_date < end_date { 1 } else { -1 };
    let span = (end_date - start_date).num_days().abs();
    if span > MAX_BUSINESS_DAY_SCAN {
        return Err(vm_error(
            "__calendar_business_days_between: date span exceeded guardrail",
        ));
    }
    let mut count = 0i64;
    let mut date = start_date;
    while date != end_date {
        if is_business_date(date, &calendar)? {
            count += step;
        }
        date = date
            .checked_add_signed(ChronoDuration::days(step))
            .ok_or_else(|| vm_error("__calendar_business_days_between: date out of range"))?;
    }
    Ok(VmValue::Int(count))
}

#[harn_builtin(
    sig = "__calendar_business_window(timestamp: any, calendar?: any, options?: dict, timezone?: string) -> dict",
    category = "calendar"
)]
fn calendar_business_window_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let options = optional_dict(args.get(2), "__calendar_business_window", "options")?;
    let timezone_value = options
        .and_then(|opts| opts.get("timezone"))
        .or(args.get(3));
    let calendar =
        business_calendar_arg(args.get(1), timezone_value, "__calendar_business_window")?;
    let dt = datetime_like_arg(
        args.first(),
        calendar.timezone,
        "__calendar_business_window",
    )?;
    let start_time = business_time_option(options, "start", "start_hour", 9, 0)?;
    let end_time = business_time_option(options, "end", "end_hour", 17, 0)?;
    if end_time <= start_time {
        return Err(vm_error(
            "__calendar_business_window: end time must be after start time",
        ));
    }
    let local = dt.with_timezone(&calendar.timezone);
    let date = local.date_naive();
    let business_day = is_business_date(date, &calendar)?;
    let starts_at = resolve_local_datetime(
        calendar.timezone,
        NaiveDateTime::new(date, start_time),
        LocalDisambiguation::Earlier,
        "__calendar_business_window",
    )?;
    let ends_at = resolve_local_datetime(
        calendar.timezone,
        NaiveDateTime::new(date, end_time),
        LocalDisambiguation::Earlier,
        "__calendar_business_window",
    )?;
    let inside = business_day && local.time() >= start_time && local.time() < end_time;
    let reason = if inside {
        "inside"
    } else if !business_day {
        "non_business_day"
    } else if local.time() < start_time {
        "before_window"
    } else {
        "after_window"
    };
    Ok(VmValue::dict(BTreeMap::from([
        ("inside".to_string(), VmValue::Bool(inside)),
        ("business_day".to_string(), VmValue::Bool(business_day)),
        ("reason".to_string(), VmValue::string(reason)),
        (
            "timezone".to_string(),
            VmValue::string(calendar.timezone.name()),
        ),
        ("calendar".to_string(), VmValue::string(&calendar.name)),
        (
            "local_date".to_string(),
            VmValue::string(date.format("%Y-%m-%d").to_string()),
        ),
        (
            "starts_at".to_string(),
            timestamp_value(starts_at.with_timezone(&Utc)),
        ),
        (
            "ends_at".to_string(),
            timestamp_value(ends_at.with_timezone(&Utc)),
        ),
    ])))
}

fn parts_dict(dt: DateTime<Tz>) -> crate::value::DictMap {
    let iso = dt.iso_week();
    crate::value::DictMap::from_iter([
        ("year".to_string(), VmValue::Int(dt.year() as i64)),
        ("month".to_string(), VmValue::Int(dt.month() as i64)),
        ("day".to_string(), VmValue::Int(dt.day() as i64)),
        ("hour".to_string(), VmValue::Int(dt.hour() as i64)),
        ("minute".to_string(), VmValue::Int(dt.minute() as i64)),
        ("second".to_string(), VmValue::Int(dt.second() as i64)),
        (
            "weekday".to_string(),
            VmValue::Int(dt.weekday().num_days_from_sunday() as i64),
        ),
        (
            "iso_weekday".to_string(),
            VmValue::Int(dt.weekday().number_from_monday() as i64),
        ),
        ("iso_week".to_string(), VmValue::Int(iso.week() as i64)),
        ("iso_week_year".to_string(), VmValue::Int(iso.year() as i64)),
        ("ordinal".to_string(), VmValue::Int(dt.ordinal() as i64)),
        (
            "quarter".to_string(),
            VmValue::Int(quarter_for_month(dt.month()) as i64),
        ),
        (
            "timezone".to_string(),
            VmValue::string(dt.timezone().name()),
        ),
        (
            "offset_seconds".to_string(),
            VmValue::Int(dt.offset().fix().local_minus_utc() as i64),
        ),
        (
            "timestamp".to_string(),
            timestamp_value(dt.with_timezone(&Utc)),
        ),
        ("iso8601".to_string(), VmValue::string(dt.to_rfc3339())),
        (
            "date".to_string(),
            VmValue::string(dt.date_naive().format("%Y-%m-%d").to_string()),
        ),
    ])
}

fn boundary_for(
    local: DateTime<Tz>,
    unit: CalendarUnit,
    edge: BoundaryEdge,
    builtin: &str,
) -> Result<DateTime<Utc>, VmError> {
    let start_date = boundary_start_date(local.date_naive(), unit)?;
    let start = local_start_of_date(local.timezone(), start_date, builtin)?;
    if edge == BoundaryEdge::Start {
        return Ok(start.with_timezone(&Utc));
    }
    let next_date = add_date_units(start_date, 1, unit, builtin)?;
    let next = local_start_of_date(local.timezone(), next_date, builtin)?;
    Ok((next - ChronoDuration::milliseconds(1)).with_timezone(&Utc))
}

fn add_calendar_units(
    local: DateTime<Tz>,
    amount: i64,
    unit: CalendarUnit,
    disambiguation: LocalDisambiguation,
    builtin: &str,
) -> Result<DateTime<Utc>, VmError> {
    if amount == 0 {
        return Ok(local.with_timezone(&Utc));
    }
    let next_date = add_date_units(local.date_naive(), amount, unit, builtin)?;
    let naive = NaiveDateTime::new(next_date, local.time());
    let resolved = resolve_local_datetime(local.timezone(), naive, disambiguation, builtin)?;
    Ok(resolved.with_timezone(&Utc))
}

fn add_date_units(
    date: NaiveDate,
    amount: i64,
    unit: CalendarUnit,
    builtin: &str,
) -> Result<NaiveDate, VmError> {
    let days = match unit {
        CalendarUnit::Day => Some(amount),
        CalendarUnit::Week => amount.checked_mul(7),
        CalendarUnit::Month | CalendarUnit::Quarter | CalendarUnit::Year => None,
    };
    if let Some(days) = days {
        return date
            .checked_add_signed(ChronoDuration::days(days))
            .ok_or_else(|| vm_error(format!("{builtin}: date out of range")));
    }
    let months = match unit {
        CalendarUnit::Month => amount,
        CalendarUnit::Quarter => amount
            .checked_mul(3)
            .ok_or_else(|| vm_error(format!("{builtin}: date out of range")))?,
        CalendarUnit::Year => amount
            .checked_mul(12)
            .ok_or_else(|| vm_error(format!("{builtin}: date out of range")))?,
        CalendarUnit::Day | CalendarUnit::Week => unreachable!(),
    };
    let month_count = if months >= 0 {
        u32::try_from(months)
    } else {
        u32::try_from(months.unsigned_abs())
    }
    .map_err(|_| vm_error(format!("{builtin}: date out of range")))?;
    if months >= 0 {
        date.checked_add_months(Months::new(month_count))
    } else {
        date.checked_sub_months(Months::new(month_count))
    }
    .ok_or_else(|| vm_error(format!("{builtin}: date out of range")))
}

fn boundary_start_date(date: NaiveDate, unit: CalendarUnit) -> Result<NaiveDate, VmError> {
    match unit {
        CalendarUnit::Day => Ok(date),
        CalendarUnit::Week => date
            .checked_sub_signed(ChronoDuration::days(
                date.weekday().num_days_from_monday() as i64
            ))
            .ok_or_else(|| vm_error("__calendar_boundary: date out of range")),
        CalendarUnit::Month => NaiveDate::from_ymd_opt(date.year(), date.month(), 1)
            .ok_or_else(|| vm_error("__calendar_boundary: date out of range")),
        CalendarUnit::Quarter => {
            let month = ((date.month() - 1) / 3) * 3 + 1;
            NaiveDate::from_ymd_opt(date.year(), month, 1)
                .ok_or_else(|| vm_error("__calendar_boundary: date out of range"))
        }
        CalendarUnit::Year => NaiveDate::from_ymd_opt(date.year(), 1, 1)
            .ok_or_else(|| vm_error("__calendar_boundary: date out of range")),
    }
}

fn local_start_of_date(tz: Tz, date: NaiveDate, builtin: &str) -> Result<DateTime<Tz>, VmError> {
    let naive = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| vm_error(format!("{builtin}: date out of range")))?;
    resolve_local_datetime(tz, naive, LocalDisambiguation::Earlier, builtin)
}

fn resolve_local_datetime(
    timezone: Tz,
    naive: NaiveDateTime,
    disambiguation: LocalDisambiguation,
    builtin: &str,
) -> Result<DateTime<Tz>, VmError> {
    match timezone.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Ok(dt),
        LocalResult::Ambiguous(earlier, later) => match disambiguation {
            LocalDisambiguation::Earlier => Ok(earlier),
            LocalDisambiguation::Later => Ok(later),
            LocalDisambiguation::Reject => Err(vm_error(format!(
                "{builtin}: local time is ambiguous in timezone {}",
                timezone.name()
            ))),
        },
        LocalResult::None => Err(vm_error(format!(
            "{builtin}: local time does not exist in timezone {}",
            timezone.name()
        ))),
    }
}

fn datetime_like_arg(
    value: Option<&VmValue>,
    timezone: Tz,
    builtin: &str,
) -> Result<DateTime<Utc>, VmError> {
    match value {
        Some(VmValue::String(raw)) => parse_datetime_or_date(raw, timezone, builtin),
        _ => datetime_from_arg(value, builtin),
    }
}

fn parse_datetime_or_date(
    raw: &str,
    timezone: Tz,
    builtin: &str,
) -> Result<DateTime<Utc>, VmError> {
    let trimmed = raw.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return Ok(local_start_of_date(timezone, date, builtin)?.with_timezone(&Utc));
    }
    Err(vm_error(format!(
        "{builtin}: expected RFC 3339 timestamp or YYYY-MM-DD date string"
    )))
}

fn naive_datetime_from_parts(
    parts: &crate::value::DictMap,
    builtin: &str,
) -> Result<NaiveDateTime, VmError> {
    let year = i32::try_from(required_i64_field(parts, "year", builtin)?)
        .map_err(|_| vm_error(format!("{builtin}: year out of range")))?;
    let month = u32::try_from(required_i64_field(parts, "month", builtin)?).unwrap_or(0);
    let day = u32::try_from(required_i64_field(parts, "day", builtin)?).unwrap_or(0);
    let hour = u32::try_from(optional_i64_field(parts, "hour", 0, builtin)?).unwrap_or(u32::MAX);
    let minute =
        u32::try_from(optional_i64_field(parts, "minute", 0, builtin)?).unwrap_or(u32::MAX);
    let second =
        u32::try_from(optional_i64_field(parts, "second", 0, builtin)?).unwrap_or(u32::MAX);
    let date = NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| vm_error(format!("{builtin}: invalid date")))?;
    date.and_hms_opt(hour, minute, second)
        .ok_or_else(|| vm_error(format!("{builtin}: invalid time")))
}

fn required_i64_field(
    parts: &crate::value::DictMap,
    field: &str,
    builtin: &str,
) -> Result<i64, VmError> {
    numeric_i64_field(parts.get(field), field, builtin)
        .and_then(|value| value.ok_or_else(|| vm_error(format!("{builtin}: missing {field}"))))
}

fn optional_i64_field(
    parts: &crate::value::DictMap,
    field: &str,
    default: i64,
    builtin: &str,
) -> Result<i64, VmError> {
    numeric_i64_field(parts.get(field), field, builtin).map(|value| value.unwrap_or(default))
}

fn numeric_i64_field(
    value: Option<&VmValue>,
    field: &str,
    builtin: &str,
) -> Result<Option<i64>, VmError> {
    match value {
        Some(VmValue::Int(value)) => Ok(Some(*value)),
        Some(VmValue::Float(value)) if value.fract() == 0.0 => Ok(Some(*value as i64)),
        Some(other) => Err(vm_error(format!(
            "{builtin}: {field} must be an integer, got {}",
            other.type_name()
        ))),
        None => Ok(None),
    }
}

fn quarter_for_month(month: u32) -> u32 {
    ((month - 1) / 3) + 1
}

fn weekday_delta(
    current: Weekday,
    target: Weekday,
    direction: &str,
    include_today: bool,
) -> Result<i64, VmError> {
    let current = current.num_days_from_monday() as i64;
    let target = target.num_days_from_monday() as i64;
    match direction {
        "next" => {
            let mut delta = (target - current).rem_euclid(7);
            if delta == 0 && !include_today {
                delta = 7;
            }
            Ok(delta)
        }
        "previous" => {
            let mut delta = -((current - target).rem_euclid(7));
            if delta == 0 && !include_today {
                delta = -7;
            }
            Ok(delta)
        }
        other => Err(vm_error(format!(
            "__calendar_next_weekday: direction must be 'next' or 'previous', got '{other}'"
        ))),
    }
}

fn country_by_code(raw: &str) -> Option<&'static CountryMeta> {
    let code = raw.trim().to_ascii_uppercase();
    COUNTRIES.iter().find(|country| country.code == code)
}

fn country_value(country: &CountryMeta) -> VmValue {
    let default_timezone = if country.timezones.len() == 1 {
        VmValue::string(country.timezones[0])
    } else {
        VmValue::Nil
    };
    VmValue::dict(BTreeMap::from([
        ("code".to_string(), VmValue::string(country.code)),
        ("name".to_string(), VmValue::string(country.name)),
        (
            "timezones".to_string(),
            string_list_value(country.timezones.iter().copied()),
        ),
        ("default_timezone".to_string(), default_timezone),
        (
            "default_timezone_ambiguous".to_string(),
            VmValue::Bool(country.timezones.len() != 1),
        ),
        (
            "currency_code".to_string(),
            VmValue::string(country.currency_code),
        ),
        (
            "currency_name".to_string(),
            VmValue::string(country.currency_name),
        ),
        (
            "holiday_calendars".to_string(),
            string_list_value(country.holiday_calendars.iter().copied()),
        ),
    ]))
}

fn holiday_calendar_info(name: &'static str) -> VmValue {
    VmValue::dict(BTreeMap::from([
        ("name".to_string(), VmValue::string(name)),
        ("country".to_string(), VmValue::string("US")),
        (
            "timezone".to_string(),
            VmValue::string(DEFAULT_BUSINESS_TIMEZONE),
        ),
        (
            "description".to_string(),
            VmValue::string("United States federal observed holidays"),
        ),
    ]))
}

fn supported_holidays_for_year(calendar: &str, year: i32) -> Result<Vec<Holiday>, VmError> {
    if !(MIN_HOLIDAY_YEAR..=MAX_HOLIDAY_YEAR).contains(&year) {
        return Err(vm_error(format!(
            "{calendar}: supported holiday years are {MIN_HOLIDAY_YEAR} through {MAX_HOLIDAY_YEAR}"
        )));
    }
    match canonical_holiday_calendar(calendar) {
        Some("US-FEDERAL") => us_federal_holidays_for_observed_year(year),
        Some(other) => Err(vm_error(format!("unsupported holiday calendar '{other}'"))),
        None => Err(vm_error(format!(
            "unsupported holiday calendar '{calendar}'"
        ))),
    }
}

fn canonical_holiday_calendar(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "US" | "US-FEDERAL" | "UNITED_STATES_FEDERAL" => Some("US-FEDERAL"),
        "NONE" | "" => Some("NONE"),
        _ => None,
    }
}

fn us_federal_holidays_for_observed_year(year: i32) -> Result<Vec<Holiday>, VmError> {
    let mut holidays = Vec::new();
    let previous = year
        .checked_sub(1)
        .ok_or_else(|| vm_error("US-FEDERAL: year out of range"))?;
    let next = year
        .checked_add(1)
        .ok_or_else(|| vm_error("US-FEDERAL: year out of range"))?;
    for candidate_year in [previous, year, next] {
        let candidates = us_federal_holidays_actual(candidate_year);
        for holiday in candidates {
            if holiday.date.year() == year {
                holidays.push(holiday);
            }
        }
    }
    holidays.sort_by_key(|holiday| holiday.date);
    Ok(holidays)
}

fn us_federal_holidays_actual(year: i32) -> Vec<Holiday> {
    let mut holidays = vec![
        observed_fixed_holiday(year, 1, 1, "New Year's Day"),
        washington_birthday(year),
        memorial_day(year),
        observed_fixed_holiday(year, 7, 4, "Independence Day"),
        fixed_holiday(nth_weekday_of_month(year, 9, Weekday::Mon, 1), "Labor Day"),
        thanksgiving_day(year),
        observed_fixed_holiday(year, 11, 11, "Veterans Day"),
        observed_fixed_holiday(year, 12, 25, "Christmas Day"),
    ];
    if year >= 1937 {
        holidays.push(columbus_day(year));
    }
    if year >= 1986 {
        holidays.push(fixed_holiday(
            nth_weekday_of_month(year, 1, Weekday::Mon, 3),
            "Martin Luther King Jr. Day",
        ));
    }
    if year >= 2021 {
        holidays.push(observed_fixed_holiday(
            year,
            6,
            19,
            "Juneteenth National Independence Day",
        ));
    }
    holidays.sort_by_key(|holiday| holiday.date);
    holidays
}

fn washington_birthday(year: i32) -> Holiday {
    if year >= 1971 {
        fixed_holiday(
            nth_weekday_of_month(year, 2, Weekday::Mon, 3),
            "Washington's Birthday",
        )
    } else {
        observed_fixed_holiday(year, 2, 22, "Washington's Birthday")
    }
}

fn memorial_day(year: i32) -> Holiday {
    if year >= 1971 {
        fixed_holiday(last_weekday_of_month(year, 5, Weekday::Mon), "Memorial Day")
    } else {
        observed_fixed_holiday(year, 5, 30, "Memorial Day")
    }
}

fn columbus_day(year: i32) -> Holiday {
    if year >= 1971 {
        fixed_holiday(
            nth_weekday_of_month(year, 10, Weekday::Mon, 2),
            "Columbus Day",
        )
    } else {
        observed_fixed_holiday(year, 10, 12, "Columbus Day")
    }
}

fn thanksgiving_day(year: i32) -> Holiday {
    if year >= 1942 {
        fixed_holiday(
            nth_weekday_of_month(year, 11, Weekday::Thu, 4),
            "Thanksgiving Day",
        )
    } else {
        fixed_holiday(
            last_weekday_of_month(year, 11, Weekday::Thu),
            "Thanksgiving Day",
        )
    }
}

fn fixed_holiday(date: NaiveDate, name: &'static str) -> Holiday {
    Holiday {
        date,
        actual_date: date,
        name,
        observed: false,
    }
}

fn observed_fixed_holiday(year: i32, month: u32, day: u32, name: &'static str) -> Holiday {
    let actual = NaiveDate::from_ymd_opt(year, month, day).expect("valid fixed holiday date");
    let observed = match actual.weekday() {
        Weekday::Sat => actual - ChronoDuration::days(1),
        Weekday::Sun => actual + ChronoDuration::days(1),
        _ => actual,
    };
    Holiday {
        date: observed,
        actual_date: actual,
        name,
        observed: observed != actual,
    }
}

fn nth_weekday_of_month(year: i32, month: u32, weekday: Weekday, n: u32) -> NaiveDate {
    let mut date = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    while date.weekday() != weekday {
        date += ChronoDuration::days(1);
    }
    date + ChronoDuration::days(i64::from(n - 1) * 7)
}

fn last_weekday_of_month(year: i32, month: u32, weekday: Weekday) -> NaiveDate {
    let mut date = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1).expect("valid next month")
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1).expect("valid next month")
    } - ChronoDuration::days(1);
    while date.weekday() != weekday {
        date -= ChronoDuration::days(1);
    }
    date
}

fn holiday_value(holiday: &Holiday) -> VmValue {
    VmValue::dict(BTreeMap::from([
        (
            "date".to_string(),
            VmValue::string(holiday.date.format("%Y-%m-%d").to_string()),
        ),
        (
            "actual_date".to_string(),
            VmValue::string(holiday.actual_date.format("%Y-%m-%d").to_string()),
        ),
        ("name".to_string(), VmValue::string(holiday.name)),
        ("observed".to_string(), VmValue::Bool(holiday.observed)),
    ]))
}

fn business_calendar_arg(
    calendar_value: Option<&VmValue>,
    timezone_value: Option<&VmValue>,
    builtin: &str,
) -> Result<BusinessCalendar, VmError> {
    let mut calendar = match calendar_value {
        Some(VmValue::Dict(map)) => custom_business_calendar(map, builtin)?,
        Some(VmValue::Nil) | None => supported_business_calendar(DEFAULT_HOLIDAY_CALENDAR)?,
        Some(value) => supported_business_calendar(&value.display())?,
    };
    if let Some(value) = timezone_value {
        if !matches!(value, VmValue::Nil) {
            calendar.timezone = timezone_arg(Some(value), DEFAULT_TIMEZONE, builtin, "timezone")?;
        }
    }
    Ok(calendar)
}

fn supported_business_calendar(raw: &str) -> Result<BusinessCalendar, VmError> {
    match canonical_holiday_calendar(raw) {
        Some("US-FEDERAL") => Ok(BusinessCalendar {
            name: "US-FEDERAL".to_string(),
            timezone: parse_timezone(DEFAULT_BUSINESS_TIMEZONE, "business_calendar")?,
            weekends: BTreeSet::from([6, 7]),
            supported_holiday_calendar: Some("US-FEDERAL"),
            extra_holidays: BTreeMap::new(),
        }),
        Some("NONE") => Ok(BusinessCalendar {
            name: "NONE".to_string(),
            timezone: parse_timezone(DEFAULT_TIMEZONE, "business_calendar")?,
            weekends: BTreeSet::from([6, 7]),
            supported_holiday_calendar: None,
            extra_holidays: BTreeMap::new(),
        }),
        _ => Err(vm_error(format!("unsupported holiday calendar '{raw}'"))),
    }
}

fn custom_business_calendar(
    map: &crate::value::DictMap,
    builtin: &str,
) -> Result<BusinessCalendar, VmError> {
    let name = map
        .get("name")
        .map(|value| value.display())
        .unwrap_or_else(|| "CUSTOM".to_string());
    let timezone = timezone_arg(map.get("timezone"), DEFAULT_TIMEZONE, builtin, "timezone")?;
    let weekends = match map.get("weekends") {
        Some(VmValue::List(items)) => {
            let mut out = BTreeSet::new();
            for item in items.iter() {
                out.insert(weekday_arg(Some(item), builtin, "weekends")?.number_from_monday());
            }
            out
        }
        Some(VmValue::Nil) | None => BTreeSet::from([6, 7]),
        Some(other) => {
            return Err(vm_error(format!(
                "{builtin}: weekends must be a list, got {}",
                other.type_name()
            )));
        }
    };
    let supported_holiday_calendar = map
        .get("holiday_calendar")
        .and_then(|value| match value {
            VmValue::Nil => None,
            other => Some(other.display()),
        })
        .map(|name| {
            canonical_holiday_calendar(&name)
                .filter(|calendar| *calendar != "NONE")
                .ok_or_else(|| {
                    vm_error(format!("{builtin}: unsupported holiday_calendar '{name}'"))
                })
        })
        .transpose()?;
    let mut extra_holidays = BTreeMap::new();
    if let Some(VmValue::List(items)) = map.get("holidays") {
        for item in items.iter() {
            let (date, label) = custom_holiday_item(item, builtin)?;
            extra_holidays.insert(date, label);
        }
    } else if matches!(map.get("holidays"), Some(value) if !matches!(value, VmValue::Nil)) {
        return Err(vm_error(format!("{builtin}: holidays must be a list")));
    }
    Ok(BusinessCalendar {
        name,
        timezone,
        weekends,
        supported_holiday_calendar,
        extra_holidays,
    })
}

fn custom_holiday_item(value: &VmValue, builtin: &str) -> Result<(NaiveDate, String), VmError> {
    match value {
        VmValue::String(raw) => Ok((
            parse_date_string(raw, builtin, "holiday date")?,
            raw.to_string(),
        )),
        VmValue::Dict(map) => {
            let date_value = map
                .get("date")
                .ok_or_else(|| vm_error(format!("{builtin}: custom holiday missing date")))?;
            let date = parse_date_string(&date_value.display(), builtin, "holiday date")?;
            let name = map
                .get("name")
                .map(|name| name.display())
                .unwrap_or_else(|| date.format("%Y-%m-%d").to_string());
            Ok((date, name))
        }
        other => Err(vm_error(format!(
            "{builtin}: holiday entries must be date strings or dicts, got {}",
            other.type_name()
        ))),
    }
}

fn is_business_date(date: NaiveDate, calendar: &BusinessCalendar) -> Result<bool, VmError> {
    let weekday = date.weekday().number_from_monday();
    if calendar.weekends.contains(&weekday) {
        return Ok(false);
    }
    Ok(!is_holiday_date(date, calendar)?)
}

fn is_holiday_date(date: NaiveDate, calendar: &BusinessCalendar) -> Result<bool, VmError> {
    if calendar.extra_holidays.contains_key(&date) {
        return Ok(true);
    }
    let Some(name) = calendar.supported_holiday_calendar else {
        return Ok(false);
    };
    Ok(supported_holidays_for_year(name, date.year())?
        .iter()
        .any(|holiday| holiday.date == date))
}

fn business_time_option(
    options: Option<&crate::value::DictMap>,
    time_key: &str,
    hour_key: &str,
    default_hour: u32,
    default_minute: u32,
) -> Result<NaiveTime, VmError> {
    if let Some(value) = options.and_then(|opts| opts.get(time_key)) {
        return parse_time_value(value, "__calendar_business_window", time_key);
    }
    if let Some(value) = options.and_then(|opts| opts.get(hour_key)) {
        let hour = u32::try_from(int_arg(
            Some(value),
            "__calendar_business_window",
            hour_key,
        )?)
        .unwrap_or(u32::MAX);
        return NaiveTime::from_hms_opt(hour, 0, 0).ok_or_else(|| {
            vm_error(format!(
                "__calendar_business_window: {hour_key} must be between 0 and 23"
            ))
        });
    }
    NaiveTime::from_hms_opt(default_hour, default_minute, 0)
        .ok_or_else(|| vm_error("__calendar_business_window: invalid default time"))
}

fn parse_time_value(value: &VmValue, builtin: &str, label: &str) -> Result<NaiveTime, VmError> {
    let raw = value.display();
    for fmt in ["%H:%M:%S", "%H:%M"] {
        if let Ok(time) = NaiveTime::parse_from_str(&raw, fmt) {
            return Ok(time);
        }
    }
    Err(vm_error(format!(
        "{builtin}: {label} must be HH:MM or HH:MM:SS"
    )))
}

fn parse_date_string(raw: &str, builtin: &str, label: &str) -> Result<NaiveDate, VmError> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| vm_error(format!("{builtin}: {label} must be YYYY-MM-DD")))
}

fn require_dict<'a>(
    value: Option<&'a VmValue>,
    builtin: &str,
    label: &str,
) -> Result<&'a crate::value::DictMap, VmError> {
    match value {
        Some(VmValue::Dict(map)) => Ok(map),
        Some(other) => Err(vm_error(format!(
            "{builtin}: {label} must be a dict, got {}",
            other.type_name()
        ))),
        None => Err(vm_error(format!("{builtin}: missing {label}"))),
    }
}

fn optional_dict<'a>(
    value: Option<&'a VmValue>,
    builtin: &str,
    label: &str,
) -> Result<Option<&'a crate::value::DictMap>, VmError> {
    match value {
        Some(VmValue::Dict(map)) => Ok(Some(map)),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(vm_error(format!(
            "{builtin}: {label} must be a dict, got {}",
            other.type_name()
        ))),
    }
}

fn timezone_arg(
    value: Option<&VmValue>,
    default: &str,
    builtin: &str,
    label: &str,
) -> Result<Tz, VmError> {
    match value {
        Some(VmValue::Nil) | None => parse_timezone(default, builtin),
        Some(value) => parse_timezone(&value.display(), builtin)
            .map_err(|_| vm_error(format!("{builtin}: unknown {label} '{}'", value.display()))),
    }
}

fn calendar_unit_arg(value: Option<&VmValue>, builtin: &str) -> Result<CalendarUnit, VmError> {
    let raw = string_arg(value, "", builtin, "unit")?;
    match raw.as_str() {
        "day" | "days" => Ok(CalendarUnit::Day),
        "week" | "weeks" => Ok(CalendarUnit::Week),
        "month" | "months" => Ok(CalendarUnit::Month),
        "quarter" | "quarters" => Ok(CalendarUnit::Quarter),
        "year" | "years" => Ok(CalendarUnit::Year),
        _ => Err(vm_error(format!(
            "{builtin}: unit must be day, week, month, quarter, or year"
        ))),
    }
}

fn boundary_edge_arg(value: Option<&VmValue>, builtin: &str) -> Result<BoundaryEdge, VmError> {
    let raw = string_arg(value, "start", builtin, "edge")?;
    match raw.as_str() {
        "start" => Ok(BoundaryEdge::Start),
        "end" => Ok(BoundaryEdge::End),
        _ => Err(vm_error(format!(
            "{builtin}: edge must be 'start' or 'end'"
        ))),
    }
}

fn disambiguation_arg(
    value: Option<&VmValue>,
    default: LocalDisambiguation,
) -> Result<LocalDisambiguation, VmError> {
    let Some(value) = value else {
        return Ok(default);
    };
    if matches!(value, VmValue::Nil) {
        return Ok(default);
    }
    let raw = value.display();
    match raw.as_str() {
        "" => Ok(default),
        "earlier" => Ok(LocalDisambiguation::Earlier),
        "later" => Ok(LocalDisambiguation::Later),
        "reject" => Ok(LocalDisambiguation::Reject),
        _ => Err(vm_error(
            "local_datetime: disambiguation must be 'earlier', 'later', or 'reject'",
        )),
    }
}

fn weekday_arg(value: Option<&VmValue>, builtin: &str, label: &str) -> Result<Weekday, VmError> {
    match value {
        Some(VmValue::Int(value)) => weekday_from_i64(*value, builtin, label),
        Some(VmValue::Float(value)) if value.fract() == 0.0 => {
            weekday_from_i64(*value as i64, builtin, label)
        }
        Some(value) => {
            let raw = value.display().trim().to_ascii_lowercase();
            match raw.as_str() {
                "monday" | "mon" => Ok(Weekday::Mon),
                "tuesday" | "tue" | "tues" => Ok(Weekday::Tue),
                "wednesday" | "wed" => Ok(Weekday::Wed),
                "thursday" | "thu" | "thur" | "thurs" => Ok(Weekday::Thu),
                "friday" | "fri" => Ok(Weekday::Fri),
                "saturday" | "sat" => Ok(Weekday::Sat),
                "sunday" | "sun" => Ok(Weekday::Sun),
                _ => Err(vm_error(format!(
                    "{builtin}: {label} must be ISO weekday 1-7 or weekday name"
                ))),
            }
        }
        None => Err(vm_error(format!("{builtin}: missing {label}"))),
    }
}

fn weekday_from_i64(value: i64, builtin: &str, label: &str) -> Result<Weekday, VmError> {
    match value {
        1 => Ok(Weekday::Mon),
        2 => Ok(Weekday::Tue),
        3 => Ok(Weekday::Wed),
        4 => Ok(Weekday::Thu),
        5 => Ok(Weekday::Fri),
        6 => Ok(Weekday::Sat),
        7 | 0 => Ok(Weekday::Sun),
        _ => Err(vm_error(format!(
            "{builtin}: {label} must be ISO weekday 1-7 or 0 for Sunday"
        ))),
    }
}

fn holiday_calendar_name_arg(
    value: Option<&VmValue>,
    default: &'static str,
) -> Result<&'static str, VmError> {
    match value {
        Some(VmValue::Nil) | None => Ok(default),
        Some(value) => canonical_holiday_calendar(&value.display())
            .filter(|calendar| *calendar != "NONE")
            .ok_or_else(|| {
                vm_error(format!(
                    "unsupported holiday calendar '{}'",
                    value.display()
                ))
            }),
    }
}

fn string_arg(
    value: Option<&VmValue>,
    default: &str,
    builtin: &str,
    label: &str,
) -> Result<String, VmError> {
    match value {
        Some(VmValue::Nil) | None if !default.is_empty() => Ok(default.to_string()),
        Some(VmValue::Nil) | None => Err(vm_error(format!("{builtin}: missing {label}"))),
        Some(value) => Ok(value.display()),
    }
}

fn int_arg(value: Option<&VmValue>, builtin: &str, label: &str) -> Result<i64, VmError> {
    match value {
        Some(VmValue::Int(value)) => Ok(*value),
        Some(VmValue::Float(value)) if value.fract() == 0.0 => Ok(*value as i64),
        Some(other) => Err(vm_error(format!(
            "{builtin}: {label} must be an integer, got {}",
            other.type_name()
        ))),
        None => Err(vm_error(format!("{builtin}: missing {label}"))),
    }
}

fn usize_arg(value: Option<&VmValue>, builtin: &str, label: &str) -> Result<usize, VmError> {
    let value = int_arg(value, builtin, label)?;
    usize::try_from(value).map_err(|_| vm_error(format!("{builtin}: {label} must be non-negative")))
}

fn bool_arg(value: Option<&VmValue>, builtin: &str, label: &str) -> Result<Option<bool>, VmError> {
    match value {
        Some(VmValue::Bool(value)) => Ok(Some(*value)),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(vm_error(format!(
            "{builtin}: {label} must be a bool, got {}",
            other.type_name()
        ))),
    }
}

fn string_list_value<'a>(items: impl IntoIterator<Item = &'a str>) -> VmValue {
    VmValue::List(std::sync::Arc::new(
        items.into_iter().map(VmValue::string).collect(),
    ))
}
