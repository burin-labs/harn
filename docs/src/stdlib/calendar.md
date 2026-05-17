# Calendar stdlib

`std/calendar` layers civil-time, timezone, country, and business-calendar
helpers over Harn's timestamp builtins. Use it when a workflow needs local
calendar semantics instead of raw elapsed seconds.

Core timestamp builtins such as `date_parse`, `date_format`, `date_to_zone`,
and `duration_hours` stay available globally. `std/calendar` adds ISO week
fields, quarter and boundary helpers, local-time construction with explicit DST
overlap behavior, supported country metadata, and business-day arithmetic.

## Civil calendar helpers

```harn
import { iso_week, next_weekday, quarter, start_of_day } from "std/calendar"

pipeline default() {
  let now = date_parse("2026-05-11T17:00:00-04:00")
  let next_monday = start_of_day(
    next_weekday(now, "monday", "America/New_York"),
    "America/New_York",
  )

  println(date_to_zone(next_monday, "America/New_York"))
  println(json_stringify(iso_week(next_monday, "America/New_York")))
  println(quarter(next_monday, "America/New_York"))
}
```

Boundary helpers include `start_of_day`, `end_of_day`, `start_of_week`,
`end_of_week`, `start_of_month`, `end_of_month`, `start_of_quarter`,
`end_of_quarter`, `start_of_year`, and `end_of_year`. Weeks are ISO weeks that
start on Monday.

`date_range(start, end, unit, timezone, options?)` returns local calendar ticks
in the half-open range `[start, end)`. Supported units are `day`, `week`,
`month`, `quarter`, and `year`.

## DST semantics

`local_datetime(parts, timezone, disambiguation?)` builds a timestamp from
local civil fields. Fall-back overlaps are deterministic:

| Disambiguation | Overlap behavior |
|---|---|
| `"earlier"` | choose the first matching instant |
| `"later"` | choose the second matching instant |
| `"reject"` | throw when the local time is ambiguous |

Spring-forward gaps always throw because there is no instant with those local
fields.

```harn
import { local_datetime } from "std/calendar"

pipeline default() {
  let first = local_datetime(
    {year: 2024, month: 11, day: 3, hour: 1, minute: 30},
    "America/New_York",
    "earlier",
  )
  let second = local_datetime(
    {year: 2024, month: 11, day: 3, hour: 1, minute: 30},
    "America/New_York",
    "later",
  )

  println(date_to_zone(first, "UTC"))
  println(date_to_zone(second, "UTC"))
}
```

## Business calendars

The v1 built-in holiday calendar is `US-FEDERAL`, using observed United States
federal holidays and Saturday/Sunday weekends. Unsupported named calendars fail
clearly. Custom calendars are plain dicts with optional `timezone`, `weekends`,
`holiday_calendar`, and `holidays` fields, so scripts can layer local closures
or company holidays without waiting for a new runtime release.

```harn
import {
  add_business_days,
  business_days_between,
  is_business_time,
  next_business_day,
} from "std/calendar"

pipeline default() {
  let calendar = "US-FEDERAL"
  let timezone = "America/New_York"
  let window = {timezone: timezone, start: "09:00", end: "17:00"}

  println(date_format(next_business_day("2026-07-03", calendar, timezone), "%Y-%m-%d", timezone))
  println(date_format(add_business_days("2026-07-01", 3, calendar, timezone), "%Y-%m-%d", timezone))
  println(business_days_between("2026-07-01", "2026-07-08", calendar, timezone))
  println(is_business_time(date_parse("2026-07-06T14:00:00-04:00"), calendar, window))
}
```

Custom calendar example:

```harn
import { is_business_day } from "std/calendar"

pipeline default() {
  let company_calendar = {
    timezone: "UTC",
    holiday_calendar: "US-FEDERAL",
    holidays: [{date: "2026-12-24", name: "Company winter closure"}],
  }

  println(is_business_day("2026-12-24", company_calendar))
}
```

## Country and timezone metadata

Country helpers use a deterministic static dataset keyed by ISO alpha-2 codes.
`default_timezone_for_country` only returns a timezone when the supported
country has a single unambiguous entry; multi-timezone countries return `nil`
instead of picking an arbitrary default.

```harn
import { country_info, country_timezones, default_timezone_for_country } from "std/calendar"

pipeline default() {
  let us = country_info("US")
  println(us.name)
  println(default_timezone_for_country("US") == nil)
  println(country_timezones("GB")[0])
  println(default_timezone_for_country("GB"))
}
```
