# Merge overrides

How to land a pull request when the merge queue or required `CI status` check
is the wrong tool for a rare, time-sensitive change.

The organization owner for this mechanism is
[`burin-labs/.github`](https://github.com/burin-labs/.github). Read that README
section for the full contract. This page is the Harn-facing how-to.

## Prerequisites

- You are an organization owner/admin on `burin-labs`.
- The pull request head lives in `burin-labs/harn` (not a fork).
- You can fix forward on `main` if the land is wrong.

## Labels

- `bypass-ci`: cancel competing runs for the head SHA and publish a successful
  `CI status` check. Does not merge.
- `bypass-merge-queue`: squash-merge immediately when `CI status` is already
  green. Skips the merge queue.
- `force-merge`: publish successful `CI status`, then squash-merge immediately.
  Skips CI proof and the merge queue.

Labels are only a trigger. The reusable workflow re-checks that the actor is an
organization admin and refuses fork pull requests. Privileged merges use the
`harn-release-bot` installation token, which is a ruleset bypass actor.

## When to use an override

- The merge queue is backed up enough that speculative CI would burn material
  Actions spend, and one PR must land before the rest.
- Required CI is broken in a way you can fix forward on `main` in the same
  session.
- You need to serialize a single founder land ahead of a long queue (release
  unblock or production incident).

## When not to use an override

- Ordinary feature work, dependency bumps, or “CI is slow today.”
- Changes you cannot fix forward if they break `main`.
- Pull requests from forks.

## Steps

1. Open or identify the same-repo pull request.
2. Add exactly one override label.
3. Read the audit comment posted by `Merge override dispatch`.
4. For `force-merge` or `bypass-merge-queue`, confirm the PR merged.
5. Prefer removing the label after the land; the audit comment remains.

Organization admins can also use GitHub’s “Bypass rules and merge” UI. The
org-wide `main protection` ruleset grants `OrganizationAdmin` bypass in
`pull_request` mode.

## Related

- Org contract and wiring template:
  [`burin-labs/.github` README](https://github.com/burin-labs/.github#merge-overrides)
- Harn agent notes: `AGENTS.md` → “Merge overrides”
