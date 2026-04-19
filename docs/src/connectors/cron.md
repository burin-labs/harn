# Cron connector

The cron connector is Harn's in-process scheduler for time-triggered work. It
implements the shared `Connector` trait, evaluates cron expressions in an IANA
time zone, and persists the last-fired boundary for each trigger in the shared
EventLog.

## Manifest shape

Cron triggers live under `[[triggers]]` and keep their schedule-specific
settings inline with the rest of the trigger manifest entry:

```toml
[[triggers]]
id = "daily-digest"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
handler = "worker://digest-queue"
schedule = "0 9 * * *"
timezone = "America/New_York"
catchup_mode = "skip"
```

Supported fields:

- `schedule`: five-field cron expression parsed by `croner`
- `timezone`: IANA time zone name such as `America/New_York`
- `catchup_mode`: `skip` (default), `all`, or `latest`

Offset literals such as `+02:00` and `UTC-5` are rejected at manifest-load
time. Use a named zone instead so DST transitions can be evaluated correctly.

## DST semantics

The cron connector intentionally favors stable wall-clock semantics over trying
to synthesize impossible local times:

- Fall-back overlaps fire a matching wall-clock slot once, even though the local
  hour appears twice.
- Spring-forward gaps do not invent a firing for a missing local time. A
  schedule like `0 2 * * *` simply does not fire on the DST transition day when
  `02:00` is skipped.
- Named zones continue to track the intended local wall time across standard and
  daylight time. Midnight in `America/New_York` fires at `05:00Z` in winter and
  `04:00Z` in summer.

## Durable state

Every successful firing appends the latest scheduled boundary for that trigger
to the EventLog topic `connectors.cron.state`. On restart, the connector reloads
the latest entry for each `trigger_id` and uses it to determine whether any
ticks were missed while the orchestrator was down.

The current implementation persists:

- `trigger_id`
- `last_fired_at`

This keeps recovery append-only and backend-agnostic across the memory, file,
and SQLite EventLog implementations.

## Catch-up modes

Catch-up behavior is evaluated from the persisted `last_fired_at` boundary to
the connector's current clock on activation.

- `skip`: drop missed ticks and resume from "now"
- `all`: replay every missed scheduled tick in chronological order
- `latest`: replay only the most recent missed scheduled tick

Catch-up reuses the original scheduled boundary as `occurred_at`, so downstream
consumers can distinguish between when a job was due and when the process
actually resumed.

## Event output

Until the broader trigger dispatcher lands, cron firings are emitted as
serialized `TriggerEvent` envelopes on the EventLog topic `connectors.cron.tick`
with provider `cron`, kind `tick`, and a `CronEventPayload` that includes:

- `cron_id`
- `schedule`
- `tick_at`
- `raw.catchup`
- `raw.timezone`

This keeps the connector testable today and preserves a normalized event shape
for the follow-up dispatcher work.
