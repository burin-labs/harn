# Review captain persona

Canonical Harn package for `review_captain`. Hosts (Burin Code,
harn-cloud, the CLI) discover it through `harn persona list --json` and
treat the resulting manifest as Harn-owned metadata.

The persona reviews one PR per invocation. It runs deterministic
secret-scanning and risky-path classification first, then optionally
calls `self_review` for one or more LLM rounds, and emits a single JSON
`review_receipt` envelope. The schema is versioned (`schema_version`)
and additive — embedders pin to the persona version they ship with.

## Pipeline input

The pipeline reads its input from stdin as a JSON object. When stdin is
empty the runtime input is consulted; when both are absent the bundled
fixture is used so smoke runs stay deterministic.

```json
{
  "repo": "burin-labs/burin-code",
  "pr_number": 235,
  "head_sha": "...",
  "base_sha": "...",
  "diff": "...",
  "changed_files": ["..."],
  "max_rounds": 1,
  "dry_run": true,
  "rubric_preset": "code",
  "policy_path": "personas/review_captain/policies/burin-code.json"
}
```

`dry_run = true` skips the LLM hop. The deterministic stages still
emit findings, so `secret_scan` hits and risky-path matches still
surface.

## Local checks

```bash
harn persona --manifest personas/review_captain/harn.toml inspect review_captain --json
harn run personas/review_captain/manifest.harn < personas/review_captain/fixtures/pull_request.json
harn test personas/review_captain/tests/
harn persona --manifest personas/review_captain/harn.toml doctor review_captain
```

## Output schema

```json
{
  "persona": "review_captain",
  "kind": "review_receipt",
  "schema_version": 1,
  "repo": "...",
  "pr_number": 0,
  "head_sha": "...",
  "base_sha": "...",
  "verdict": "approve" | "needs_follow_up" | "blocking",
  "blocking": false,
  "summary": "...",
  "findings": [{ "id": "...", "severity": "blocking|warning|info", ... }],
  "secret_scan_findings": [...],
  "risky_paths": ["..."],
  "missing_tests": ["..."],
  "rounds": 0,
  "self_review_skipped_reason": null,
  "handoff": "merge_captain" | null
}
```
