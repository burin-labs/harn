# Green branch

`green` is a protected branch that names the newest commit on `main` whose
full CI suite passed. Release automation and the consumer canary read it, so
a commit that is red on `main` never becomes a release or a consumer's
update. Nobody pushes to it by hand.

## How green advances

Every push to `main` gets its own CI run. `ci.yml` keys main-push concurrency
by commit and never cancels it, and `scripts/ci/post_merge_tier.sh` selects
every proof family for a push, so a successful main-push run is full-suite
proof for its commit. Green needs no runs of its own.

`green-checkpoint.yml` runs `scripts/ci/green_checkpoint.sh` when a main-push
CI run completes successfully, and every 15 minutes to recover a dropped
event. Each pass:

1. Reads every successful main-push CI run from the last three days, paging
   until the count matches the API's `total_count`. A short or malformed
   census fails the pass before any write.
2. Takes the runs newest first. A run whose commit is still on `main` and
   strictly ahead of green becomes a checkpoint. Runs at or behind green are
   skipped.
3. Writes an annotated tag `green-<created UTC>-<run id>`, then fast-forwards
   `green` to the commit. An existing tag must record the same run.
4. Records each failed run and continues with older ones. The pass fails at
   the end if any run failed.

The release GitHub App writes the branch and tags. Its token needs
`contents: write` and `workflows: write`, because green moves across commits
that change workflow files.

## Checkpoint record

The last line of each `green-*` tag message is one JSON object:

```json
{"schema":"harn.green_checkpoint.v1","sha":"<40-hex>","runId":123,"runAttempt":1,"runUrl":"https://github.com/burin-labs/harn/actions/runs/123","completedAt":"2026-10-17T12:00:00Z"}
```

Read it with:

```bash
git cat-file -p green-20261017T120000Z-123 | tail -n 1 | jq .
```

## Consumers

| Consumer | Reads green as |
| --- | --- |
| `bump-release.yml` and `scripts/open_release_pr.sh` | The release pull request is prepared from a checkout of `green`. The opener refuses a checkout that no `green-*` tag names. The pull request still targets `main`. |
| `promote-release.yml` (`plan` job) | A version commit must be contained in `green`. Its candidate run can finish before its own CI run, so `plan` waits up to 90 minutes for green to reach it. Ordinary commits, which promote nothing, never read green. |
| `consumer-canary.yml` | A scheduled or dispatched run without `source_revision` rehearses green's tip. The `decide` job resolves it once and later jobs read its output. |

Promotion keeps its separate per-job full-suite check at the release commit
(`scripts/ci/require_full_suite.sh`). Green adds the requirement that the
commit sits on the certified line.

## Repository rulesets

Three rulesets protect green. Only the release App (`harn-release-bot`) may
create or move `green` and `green-*` tags. Nobody, including the App, may
rewind or delete `green`.

## When green stops moving

Green stays put while `main` is red, so releases and the canary wait for a
passing commit. A pass that fails names each failed run in its log. To read
the current checkpoint:

```bash
gh api repos/burin-labs/harn/git/ref/heads/green --jq .object.sha
```

A pass that reports `diverged from green` means `main`'s history was
rewritten under green. Repair needs an admin to reset `green` past the
non-fast-forward ruleset.
