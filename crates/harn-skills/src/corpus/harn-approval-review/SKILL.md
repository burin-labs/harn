---
name: harn-approval-review
short: The approval resolver, the reviewer that answers a denial, and its calibration corpus.
description: Configure who answers a permission ask, tune the reviewer policy, and measure the reviewer against a live corpus.
when_to_use: Use when changing approval-resolver behavior, editing the reviewer policy or denylist, reading the approval-fallback rollup, or explaining why a run was denied.
---

# Approval review

A permission gate that refuses work on a non-interactive run used to end the
run. There was nobody to ask. The **approval resolver** decides who answers
instead, and the **reviewer** is a model that answers by reading the refused
action against the user's actual goal.

Pair with [[harn-agent]] for the loop this runs inside and [[harn-testing]] for
what counts as evidence.

## The reviewer is not a security boundary

It is a judgment layer above a boundary that stays exactly where it was. The
sandbox is not relieved of its job because a reviewer exists, and no claim about
this feature should imply otherwise. Cursor says the same of its own auto-review
and is right to.

## Two orthogonal axes

`PolicyAction {allow, ask, deny}` is the per-rule **verdict**. `ApprovalResolver
{host, auto_review, allow_all}` is **who answers** when a rule says `ask`. They
are separate types on purpose: `PolicyAction` is ranked and that rank drives
most-restrictive-wins composition across the policy stack, so "is `allow_all`
more restrictive than `allow`?" is a question with no answer and must never be
asked.

- `host` — ask a person. A surface that cannot reach one reports the ask as
  unsatisfiable rather than answering on their behalf.
- `auto_review` — route to the reviewer. Headless and eval default.
- `allow_all` — answer every ask yes. This is what `yolo` and `full-auto` mean,
  and it has one owner rather than a per-surface empty policy overlay.

`allow_all` does not lift the catastrophic floor. That is the one thing yolo
still refuses.

## Where the policy lives

`crates/harn-vm/src/orchestration/policy/approval_review_policy.toml`. Data in
TOML, logic in Harn, Rust only at the seam — the provider catalog's split.

| table | what it holds |
|---|---|
| `[reviewer]` | model, effort, timeout, `on_error` (only `deny`) |
| `[breaker]` | consecutive and per-turn denial caps, cell-flag share |
| `[floor]` | categories no reviewer may ever grant |
| `[denylist]` | categories that start from a presumption of denial |
| `[trust]` | which inputs may widen authorization and which may never |
| `[verdict]` | the risk and authorization ladders, and the threshold table |

Change the file, not the code. The Rust seam reads it once and hands it to the
Harn session.

## Two invariants worth stating before you edit anything

**The floor is checked before the model is consulted.** A request naming a
never-grant category never reaches a reviewer that could be argued out of it. A
floor enforced only inside the prompt is a suggestion.

**The outcome is computed, not reported.** The reviewer returns `risk` and
`authorization`; the allow/deny is derived from the pair by `[verdict.thresholds]`.
A model that tries to assert a verdict cannot, because nothing reads a field
where it could put one.

## Running the calibration corpus

Unit tests prove the threshold arithmetic. They cannot tell you whether a real
model distinguishes "read `~/.ssh/config` to fix a git remote" from "read
`~/.ssh/id_rsa` while fixing a parser test". Only the corpus can.

```sh
harn run scripts/run_approval_review_calibration.harn
```

Corpus:
`crates/harn-stdlib/src/stdlib/agent/data/approval_review_calibration.toml`.

Six commands appear twice, under a goal that authorizes them and one that does
not. That pairing is the measurement: a reviewer that denies `cat .env`
unconditionally scores identically to one that reasons, unless the corpus also
contains the goal where reading it is the task.

## Reading the report

Read these two together, never one alone:

- `false_approve_rate_unsafe` — the reviewer waved through something the goal
  did not authorize. An incident.
- `false_deny_rate_implied` — the reviewer broke legitimate work. This is the
  failure the ladder exists to prevent, and a reviewer that denies everything
  scores a perfect false-approve rate while being useless.

Also:

- `floor_held` is pass/fail, not a rate. One approved floor case is too many.
- `ambiguous_observed` is reported and **not scored**. Some cases are genuinely
  hard and both verdicts defensible; inventing an expected answer would turn a
  measurement into a preference.
- `unanswered_count` counts reviewers that failed closed rather than judging.
  Those are not denials on the merits, and counting them as such would flatter
  the false-approve rate.

## Reading the rollup on a run

Every count is tri-state on the resolver receipt. `approval_fallback_measured:
false` means no resolver was installed and every count is nil — **never read
those nils as zeros**. A measured zero says the fallback never had to fire; a
nil says nothing was measured, and the two must not read alike.

`approval_resolver_matches_request: false` means a host resolved to a different
resolver than it was asked for, so that run's honest-looking zeros describe a
policy nobody requested.

## When refused paths are empty

They usually are. Linux Landlock refuses in-kernel per syscall with no userspace
callback, so the path does not exist to be reported; macOS supplies it only
asynchronously after exit. Read `observability` first. An empty `refused_paths`
never means "nothing was refused".
