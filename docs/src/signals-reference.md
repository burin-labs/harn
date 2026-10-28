# Signals reference

The `harn signals` command, its hook adapter, the inbox file, and its
configuration. For the design, see [Ambient signals](./concepts/ambient-signals.md).
For setup, see [Deliver signals to Claude Code and Codex](./signals-hooks.md).

## Commands

Every command accepts `--dir <DIR>` to use an inbox other than the default.
Commands that print data accept `--json` and emit the standard
[JSON envelope](./cli-json-contract.md) with `schemaVersion: 1`.

| Command | Effect |
|---|---|
| `harn signals list` | Every signal with its key, state and generation, severity, delivery (`held (<reason>)`, `routed to`, `leased to ... (attempt N)`, `lease lapsed for ... (attempt N)`, or `acked by`), and summary. Control characters, line and paragraph separators, bidirectional marks, zero-width characters, and tag characters print as spaces. |
| `harn signals pending [--session ID]` | Signals whose current generation has not been acked. |
| `harn signals sessions` | Registered sessions: id, harness, busy or idle, repository, role, working directory. |
| `harn signals fire ...` | Fire a signal. See [Firing](#firing). |
| `harn signals resolve KEY [--summary TEXT] [--signature local\|verified\|unverified]` | Resolve a fired signal. `--summary` replaces what the recovery reminder says happened. `--signature` (default `local`) says how the recovery report was checked; one weaker than the signal's own is refused. |
| `harn signals ack KEY [--generation N] [--session ID]` | Ack a leased reminder so it is not redelivered. The generation defaults to the current one. Fails if that generation was never leased or is already acked. |
| `harn signals hook ...` | Run as a harness hook. See [Hook adapter](#hook-adapter). |

### Firing

| Flag | Default | Meaning |
|---|---|---|
| `--key KEY` | `<provider>:<source-url>`, or the event's `dedupe_key` | The condition's identity. |
| `--summary TEXT` | required without `--event` | What happened. |
| `--source-url URL` | required without `--event` | Where the condition can be checked. |
| `--why TEXT` | empty | Why it deserves attention. |
| `--action TEXT` | a generic check-the-source step | The proposed next step. |
| `--provider NAME`, `--kind KIND` | `manual` | Envelope `provider` and `source_kind`. |
| `--severity info\|warning\|urgent` | `warning` | Only `urgent` keeps a stopping session going or wakes an idle one. |
| `--producer NAME` | `harn signals fire` | Who fired the signal; shown in the reminder. |
| `--signature local\|verified\|unverified` | `local` | Whether the alert's origin was verified; shown in the reminder. |
| `--repo OWNER/NAME` | none | Routing hint: sessions in this repository. |
| `--cwd DIR` | none | Routing hint: sessions working inside this directory. |
| `--topic TOPIC` | none | Routing hint for custom routers. |
| `--expires-in DURATION` | `fired_ttl_seconds` | Expire the signal if it is still fired after this long, for example `4h`. |
| `--event FILE` | none | Read a `harn.triage_event.v1` document from a file, or `-` for stdin, instead of the text flags. |
| `--event-id ID` | none | Capture one distinct occurrence, deduplicating retries of this ID. Requires `--session`. |
| `--session SESSION` | none | Exact capture recipient, with no fallback. Requires `--event-id`. |

`fire` and `resolve` print the transition: `fired (generation N)` for a state
change, or `unchanged` when the signal was already in that state. A change owes
one reminder; an unchanged result owes none.

### Capture monitor occurrences

For appended output, give every occurrence a stable cursor or producer sequence
with `harn signals fire --event-id ID --session SESSION`. The ordinary `--key`
identifies the monitor. Distinct event IDs retain distinct reminders even when
their text is identical. Retrying the same session, monitor key and event ID
does not change the stored occurrence or create another delivery while that
record is retained. Include the monitor execution generation in the key when
a restarted producer resets its sequence.
Within each severity, captured occurrences are leased in persisted ingestion
order even when their timestamps are equal or the clock moves backwards.
Condition reminders retain their existing priority within that severity.
The shared `muted` policy matches both the captured monitor key and an individual
capture key, so a muted monitor's occurrences remain held in the inbox.

Captured occurrences route only to the exact named session. With that session
absent, they wait instead of falling back to a repository peer or control role.
They use the existing inbox capacity, expiry, policy and lease-token acknowledgments.
Resolve is for conditions; captured occurrences are immutable and are settled
by acknowledging delivery. Summaries exceeding the storage bound are refused;
split output into bounded chunks or reference the full output. Capture does not
start a process, schedule a model call, or introduce a wake-rate guarantee.

Acknowledged captures may be pruned to make room under `max_signals`; pending
captures are never evicted to accept new input. Producers must retain cursors
and stop or retry on explicit capacity errors.

## Hook adapter

```text
harn signals hook --format claude-code|codex --event EVENT [--role ROLE] [--rewake] [--max-wait DURATION]
```

The hook reads the harness's JSON payload on stdin and uses its `session_id`,
`cwd`, and, from Claude Code, `session_title`. The repository is the `origin`
remote of the Git repository containing `cwd`. The role comes from `--role`,
else `HARN_EXT_SIGNALS_ROLE`.

Every invocation except `SessionEnd` and `Stop --rewake` leases at most three
reminders, prints them, and after flushing its output acks exactly those leases
by token. A hook killed before that leaves its leases to lapse and be leased
again; a hook running beside it never acks them. A failed ack after the output
is flushed is reported on stderr and the hook still exits 0, because a harness
discards the output of a failed hook; the lease lapses and is leased again.

| `--event` | Registers the session as | Leases | Output |
|---|---|---|---|
| `SessionStart` | busy | any severity | `hookSpecificOutput.additionalContext` |
| `UserPromptSubmit` | busy | any severity | `hookSpecificOutput.additionalContext` |
| `PostToolUse` | busy | any severity | `hookSpecificOutput.additionalContext` |
| `Stop` | idle, or busy if it delivers | Claude Code: urgent only. Codex: nothing | Claude Code: `hookSpecificOutput.additionalContext`, which continues the turn. Codex: `{}`, because a Codex `Stop` reason becomes a user prompt. |
| `Stop --rewake` | idle | nothing; watches for urgent reminders while idle | Claude Code only. Exits 2 to wake the session under `asyncRewake`, printing only a fixed notice on stderr: Claude Code shows that stderr as a user-role `Stop hook feedback` turn, so it carries no signal text. The woken turn's `Stop` hook then leases the reminder and delivers it as `additionalContext`. It wakes for an urgent reminder that appears after its first poll, or one pending at its first poll that still has no lease out two polls later (polls are two seconds apart). It wakes at most three times for one reminder. Exits 0 after `--max-wait` (default `9m`, under the hook's default 600-second `timeout`), when the session becomes busy, when it ends, or at once when another watcher for the session is running. |
| `SessionEnd` | removed | nothing | none |

When more reminders are routed to the session than one hook leases, the output
ends with how many follow. With nothing to deliver, the hook prints nothing
(Codex `Stop`: `{}`) and exits 0. A hook that fails
prints `harn signals hook: <reason>` on stderr and exits 1, which both
harnesses treat as a non-blocking hook error. It never exits 2 on failure.

## Files

The inbox directory is `$HARN_EXT_SIGNALS_DIR` when set, else
`~/.harn/signals`. It is created with mode `0700`, and its files with `0600`.

| File | Contents |
|---|---|
| `inbox.json` | Signals and sessions, schema `harn.signals.inbox.v3`. Replaced atomically on every change. |
| `inbox.lock` | Advisory lock file every transaction holds exclusively. |
| `watchers/<hex session id>.lock` | Held by the running `--rewake` watcher for a session. The watcher removes it as it exits; `SessionEnd` removes one a killed watcher left, and so does the next write to the inbox once the session is no longer registered. |
| `.inbox.json.*.tmp` | A write in progress. The next write removes one a killed process left. |
| `config.toml` | Optional configuration and delivery policy. |

### `inbox.json`

```json
{
  "schema": "harn.signals.inbox.v3",
  "signals": {
    "<key>": {
      "key": "<key>",
      "state": "fired | resolved | expired",
      "generation": 1,
      "severity": "info | warning | urgent",
      "event": {"schema": "harn.triage_event.v1", "...": "..."},
      "routing": {"repo": "owner/name", "cwd": "/path", "topic": "..."},
      "provenance": {"producer": "std/signals.monitor_github_ci", "signature": "verified | unverified | local"},
      "fired_at": "RFC 3339",
      "changed_at": "RFC 3339",
      "expires_at": "RFC 3339 or null",
      "delivery": {
        "generation": 1,
        "session_id": "...",
        "assigned_at": "RFC 3339",
        "lease": {"token": "...", "leased_at": "RFC 3339", "expires_at": "RFC 3339"},
        "acked_at": "RFC 3339 or null",
        "wakes": 0
      },
      "attempts": 1,
      "fired_recipient": "session id leased the latest fired generation"
    }
  },
  "sessions": {
    "<id>": {
      "id": "<id>",
      "harness": "claude-code | codex | <other>",
      "cwd": "/path",
      "repo": "owner/name",
      "title": "...",
      "role": "control",
      "activity": "busy | idle",
      "registered_at": "RFC 3339",
      "last_seen": "RFC 3339"
    }
  }
}
```

The inbox stores the event's normalized fields only. It replaces
`raw_payload` with `{}` and sets `privacy.raw_payload_retained` to `false`.

A document with any other `schema` is refused rather than migrated.
Version 2 preserves capture ownership and ingestion order. Older writers refuse
it instead of discarding those fields and routing captures to another session.
Use a separate inbox directory when rolling out the new format; preserve the
old inbox and its reader for any remaining old-format work.

### `config.toml`

| Key | Default | Meaning |
|---|---|---|
| `stale_after_seconds` | `7200` | A session not seen for this long is removed, and its unacked reminders are routed again. |
| `retain_for_seconds` | `604800` | Resolved and expired signals are pruned this long after their last change. |
| `lease_seconds` | `600` | How long a lease runs before an unacked reminder is leased again, to its holder while that session is responsive, otherwise to a responsive session. A session silent this long loses its unleased reminders to a responsive session. |
| `fired_ttl_seconds` | `86400` | A fired signal that is not fired again within this long expires, unless the producer set its own expiry. |
| `max_signals` | `1000` | The most signals the inbox keeps. The oldest settled signal is pruned to make room; with every signal still fired, firing a new key fails. |
| `fallback_role` | `"control"` | The role that receives signals no session matches. |
| `min_severity` | `"info"` | Signals below this severity are held. |
| `muted` | `[]` | Signal keys to hold. An entry ending in `*` matches every key with that prefix, for example `"github.open_prs:*"`. |
| `[quiet_hours]` | none | A daily window in which only signals at or above its `min_severity` route. |
| `quiet_hours.start`, `quiet_hours.end` | required | `HH:MM`. A window whose end is before its start spans midnight. |
| `quiet_hours.utc_offset` | `"+00:00"` | The offset the window is read in, `+HH:MM` or `-HH:MM`. It does not follow daylight saving time. |
| `quiet_hours.min_severity` | `"urgent"` | What still routes during quiet hours. |

The inbox is the only owner of this policy. A held signal stays in the inbox
unrouted and routes once the policy allows it. Unknown keys are refused.

## Environment

| Variable | Meaning |
|---|---|
| `HARN_EXT_SIGNALS_DIR` | Inbox directory. |
| `HARN_EXT_SIGNALS_ROLE` | Default `--role` for `harn signals hook`. |

## Rust API

Hosts embed the inbox in-process through the `harn-signals` crate:
`Inbox::open`, `Inbox::register_session`, `Inbox::check_in`,
`Inbox::ack_leases`, `Inbox::ack`, `Inbox::end_session`, `Inbox::fire`,
`Inbox::resolve`, `Inbox::poll_idle`, `Inbox::claim_watcher`, and
`Inbox::snapshot`. A consumer acks each leased reminder with
`Inbox::ack_leases` and its `lease_token` once the reminder reached the
model. `harn_signals::display_safe` is the character filter for showing signal
text anywhere. Replace the routing rules with `Inbox::with_router` and any
`Router` implementation; the policy in `config.toml` applies before the
router runs. `harn_vm::stdlib::signals::render_reminders` renders reminders
into the text the hook adapter prints. Deliver it as a system reminder, never
as a user message.

## Harn API

`std/signals` is the producer surface for Harn scripts and trigger handlers.

| Function | Effect |
|---|---|
| `signal_apply(runtime, decision, options?)` | Fire the decision's signal when `decision.firing`, otherwise resolve it. Returns `{outcome, state?, generation?}`. |
| `signal_deliver(harness, agent_session_id, session, options?)` | Check in and inject each leased reminder into a running agent session as an advisory system reminder, deduplicated by signal key, then ack each injected lease. Returns `{check_in, reminder_ids, acked}`. |
| `ci_branch_decision(repo, branch, runs, severity?)` | Firing while the newest decisive run of any workflow on `branch` failed. `runs` holds GitHub workflow runs, newest first; the first completed run per workflow that concluded `success`, `failure`, `timed_out`, `startup_failure`, or `action_required` counts, and `cancelled`, `skipped`, `stale`, and `neutral` runs are passed over. Key `github.ci:<repo>@<branch>`. |
| `open_prs_decision(repo, open_count, threshold, severity?)` | Firing while `open_count >= threshold`. Key `github.open_prs:<repo>`. |
| `runner_pool_decision(pool, online, severity?)` | Firing while `online < pool.minimum`. `pool` is `{scope, minimum, group?, required_labels?}`. Key `github.runners:<scope>[/<group>][:<sorted labels>]`. |
| `runner_pool_online(runners, required_labels?)` | Online runners in a runner list that carry every required label. |
| `rate_limit_decision(subject, remaining, limit, min_percent, source_url?, severity?)` | Firing while `remaining` is under `min_percent` of `limit`. Key `github.rate_limit:<subject>`. |
| `alert_webhook_decisions(provider, payload, signature, routing?)` | Sentry, Grafana, or Honeycomb webhook body to decisions; other providers return `[]`. `signature` is `"verified"` or `"unverified"`, from the trigger's `signature_status`. Only the summary and link come from the payload. Keys `sentry:<issue>`, `grafana:<fingerprint>`, `honeycomb:<trigger>`. |
| `monitor_github_ci(harness, repo, branch?, options?)`, `monitor_github_open_prs(harness, repo, threshold, options?)`, `monitor_github_runners(harness, pool, options?)`, `monitor_github_rate_limit(harness, subject, min_percent, options?)` | Fetch from the GitHub REST API, evaluate, and apply. A failed or incomplete read returns `{outcome: "unmeasured", error}` and changes no signal. |

GitHub monitor options:

| Option | Meaning |
|---|---|
| `app` | `{app_id, installation_id, private_key_secret?, private_key_pem?, installation_scope?}`. Each poll mints a fresh installation token through the GitHub connector, because a cached token can read a different rate-limit counter. |
| `token` | A token the caller already resolved. |
| `token_env` | An environment variable holding a token. Default `GITHUB_TOKEN`. |
| `api_url` | API base URL. Default `https://api.github.com`. |
| `severity`, `dir` | Signal severity and inbox directory. |

`monitor_github_runners` lists the organization runner group in `pool.group`
when set, or else the repository's or organization's runners, reading every
page. A listing stops at `total_count`, or at a short page when the response
has none. A page without the `runners` field, a listing that ends short of its
`total_count`, or one past 5,000 runners is unmeasured rather than counted
short.

`monitor_github_ci` lists the repository's active workflows and reads each
one's 20 most recent completed push runs on the branch, one request per
workflow, so a busy workflow cannot push another's runs out of a shared page.
The newest decisive run of each workflow is its reading. A workflow with recent
runs but no decisive one, a branch where no workflow has run, or a response
without `workflow_runs` is unmeasured rather than recovered.
`monitor_github_open_prs` is unmeasured when the search reports
`incomplete_results`.

The runtime builtins are the session lifecycle and the producer calls
underneath `signal_apply`:

| Builtin | Effect |
|---|---|
| `harness.runtime.signal_fire(request)` | Fire a signal. Returns the transition. |
| `harness.runtime.signal_resolve(key, options?)` | Resolve a fired signal. `options` takes `event`, `signature` (default `"unverified"`; one weaker than the signal's own is refused), and `dir`. |
| `harness.runtime.signal_register(session, options?)` | Record a session without leasing or acking. `session` is `{id, harness, cwd, repo?, title?, role?, activity: "busy" \| "idle"}`. |
| `harness.runtime.signal_check_in(session, options?)` | Lease pending reminders. `options` takes `scope` (`"all"` or `"urgent_only"`), `limit` (default 3), and `dir`. Returns `{reminders, waiting}`; each reminder carries a `lease_token`. |
| `harness.runtime.signal_ack(session_id, lease_tokens, options?)` | Ack the leases these tokens name once their reminders reached the model. A token whose lease lapsed and was leased again acks nothing. Returns the `{key, generation}` ids acked. |
| `harness.runtime.signal_end_session(session_id, options?)` | Remove the session; its unacked reminders route elsewhere. |
| `harness.runtime.signal_render(reminders)` | The notification text for leased reminders. |

`request` takes `event` (a normalized `harn.triage_event.v1`), `key`,
`severity`, `routing`, `provenance` (`{producer, signature}`; an omitted
signature, or an omitted provenance, is `unverified`), `expires_in_seconds`,
and `dir`; unknown fields are refused. Timestamps come from the VM clock, so a
mocked clock moves signal lifecycles too.

[`examples/triggers/ambient-signals`](https://github.com/burin-labs/harn/tree/main/examples/triggers/ambient-signals)
wires the monitors to a five-minute cron trigger and the alert normalizers to
webhook triggers.

## Captured-output dispatch

The durable dispatch API separates capturing output from accepting a monitor
batch. It does not launch a command, start a model turn, or install another
watcher. Hosts use their existing scheduler and safe dispatch boundaries.

Start an execution with a stable host session ID and a new execution ID:

```sh
harn signals dispatch --session session-A --execution run-1 start
harn signals fire --session session-A --execution run-1 --event-id file-offset-0 \
  --key build-log --summary 'Build finished' --source-url file:///workspace/build.log
harn signals dispatch --session session-A --execution run-1 claim
```

The first deadline is the oldest eligible capture plus 60 seconds. Later
deadlines are the last accepted batch plus 60 seconds. New captures do not
slide a deadline. A busy host waits until it can dispatch; one claim then
includes every eligible captured occurrence, ordered by ingestion across all
monitors in that session. It does not drain three events per minute. Ordinary
signal hooks cannot lease captures belonging to a dispatch-managed session.

`start --interval-seconds N` selects 1–86400 seconds; the default is 60.
The interval is persisted for the stable session. Repeating `start` with the
same execution is idempotent and cannot reset pacing or reopen a closed epoch.
Replacing the execution cancels its outstanding captures and preserves the
session's last acceptance time. Producers must supply the active execution ID;
unscoped or stale output is refused once a session is dispatch-managed.

A successful claim has a stable batch ID, immutable captured occurrences, a claim token,
and a lease expiry. Another claimant waits. After expiry, recovery claims the
same batch ID and content with a new token. Captures arriving after a claim
remain pending for a trailing batch. Pending captured output does not expire
under the ambient-condition TTL; capacity exhaustion returns backpressure.
The existing shared mute, severity, and quiet-hours policy controls eligibility.

Before initiating inference, the receiver must durably and idempotently accept
the batch ID and its complete content into the owning session. It then commits
that acceptance with `dispatch ... accept --batch ID --token TOKEN`. This
atomically acknowledges the batch's occurrences and advances pacing. Repeating
the latest acceptance returns `accepted: false`. A host recovering after a
crash between acceptance and acknowledgment must consult its own durable
acceptance record before initiating inference again. The inbox alone cannot
guarantee exactly-once inference across that boundary.

Hosts should render a bounded preview with the batch's total count and owned
references to the complete accepted content. The complete batch must reach
durable context before acknowledgment; rendering only a prefix and dropping
the remainder is not acceptance. Treat source text as advisory data through
the existing escaped reminder renderer.

`dispatch ... cancel --monitor KEY` invalidates that monitor's outstanding
captures and refuses its later output within the epoch. Other monitors remain
eligible. Omitting `--monitor` closes the epoch. Ending a managed session also
closes it and invalidates outstanding claims. Cancelled captures keep an
explicit `cancelled_at` disposition rather than pretending they were delivered.
Cancellation does not retract content a host has already accepted.

The same operations are available to scripts through
`harness.runtime.signal_dispatch`, with `session_id`, `execution_id`, `dir`,
and `operation` (`start`, `claim`, `accept`, or `cancel`). Start accepts
`interval_seconds`, acceptance requires `batch_id` and `token`, and cancellation
optionally accepts `monitor_key`. `signal_fire` accepts `session_id`,
`execution_id`, and `event_id` for epoch-scoped capture.

Pacing, execution state, and outstanding claims survive inbox reopening under
the must-understand v3 schema. A persisted observed-time high-water mark holds
dispatch during wall-clock rollback; hosts should use a reliable clock.
Session scopes, retired execution identities, and cancelled-monitor identities
share a `max_signals` metadata bound and refuse new entries at capacity. A
replayed start cannot reactivate a retired execution. Native command restart, process termination,
and host model-wake integration are separate responsibilities.
