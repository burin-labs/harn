# Approval review

A permission gate that refuses work on a non-interactive run has nobody to ask.
Until there was a second answerer, the only honest response was to refuse the
run. Approval review is that second answerer: a model reads the refused action
against the user's actual goal and says whether the goal authorizes it.

> Approval review is **not a security boundary**. It is a judgment layer above a
> boundary that stays exactly where it was. The sandbox and the permission
> policy are not relieved of their jobs because a reviewer exists.

## Choose who answers

The resolver is who answers an `ask`. It is separate from the `allow`/`ask`/`deny`
verdict a rule produces, because those compose differently: rule verdicts
intersect most-restrictive-wins across the policy stack, and "who answers" is
host authority that a nested scope may not widen.

| resolver | behavior | typical surface |
|---|---|---|
| `host` | ask a person; report unsatisfiable when there is none | TUI, IDE |
| `auto_review` | route to the reviewer | headless, evals |
| `allow_all` | answer every ask yes | `yolo`, `full-auto` |

`allow_all` does not lift the catastrophic floor. `rm -rf /` and its siblings
are refused whoever is asking and whatever they answered.

## Change the reviewer's policy

Edit
`crates/harn-vm/src/orchestration/policy/approval_review_policy.toml`.
Nothing here needs a code change.

```toml
[reviewer]
model = "claude-haiku-4-5-20251001"
effort = "low"
timeout_ms = 30000
on_error = "deny"

[breaker]
max_consecutive_denials = 3
max_denials_per_turn = 10
```

To use a stronger reviewer for an eval, change `[reviewer].model`. The reviewer
runs on a would-be denial rather than on every tool call, so cost tracks
refusals, not turns — but a run that fights its permission policy can still
produce dozens of verdicts, which is why the cheap model is the default.

To stop a category from ever being granted, add it to `[floor].never_grant`. To
make one merely presumed-denied — grantable when the goal plainly requires it —
add it to `[denylist].categories` instead. The difference is real: a floor
category never reaches the model at all.

## Measure the reviewer before trusting it

```sh
harn run scripts/run_approval_review_calibration.harn
```

The corpus pairs the same command under a goal that authorizes it and one that
does not — `cat .env` while debugging a missing environment variable, and
`cat .env` while adding docstrings. Without that pairing a reviewer that refuses
everything scores identically to one that reasons.

Read these two numbers together:

- **`false_approve_rate_unsafe`** — waved through something unauthorized. An
  incident.
- **`false_deny_rate_implied`** — broke legitimate work. This is the failure
  the ladder exists to prevent.

`floor_held` is pass/fail rather than a rate; one approved floor case is too
many. `ambiguous_observed` is reported and deliberately not scored.

## Read the rollup on a run

Every approval count is tri-state on the resolver receipt:

| `approval_fallback_measured` | counts | meaning |
|---|---|---|
| `false` | `nil` | no resolver was installed; nothing was measured |
| `true` | `0` | the resolver ran and never had to fire |

**Never read those nils as zeros.** A runtime pinned without the resolver would
otherwise publish a tidy row of zeros indistinguishable from a healthy run.

`approval_resolver_matches_request: false` means a host installed a different
resolver than it was asked for, so that run's counts describe a policy nobody
requested.

## When the refused path is empty

Usually it is. Linux Landlock refuses in-kernel per syscall with no userspace
callback, so the path does not exist to be reported; macOS supplies it only
asynchronously after the process exits. Check `observability` before reading
`refused_paths`: an empty list never means nothing was refused, and the reviewer
is told so explicitly in its prompt.
