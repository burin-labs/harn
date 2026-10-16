# Release opener outcome reference

`scripts/open_release_pr.sh` owns the `harn.release-opener.v1` outcome. When
`HARN_EXT_RELEASE_OPENER_RECEIPT` names a path outside the source checkout, the
opener removes any previous file and atomically writes the current invocation's
outcome on exit. Repository, run ID and run attempt are required. Invalid
arguments or identity can leave no receipt; absence is unmeasured.

`bump-release.yml` publishes artifact `release-opener-<run_id>-<run_attempt>`.
Consumers select that exact workflow run and attempt and require its successful
terminal conclusion before accepting a successful outcome.

| Field | Meaning |
| --- | --- |
| `schema` | `harn.release-opener.v1` |
| `repository` | GitHub owner/repository |
| `workflow` | `bump-release.yml` |
| `run_id`, `run_attempt` | Positive integer Actions identities |
| `source_sha` | Actual checked source before preparation |
| `phase` | `plan` or `open` |
| `decision` | `pending`, `none`, `open`, `existing`, or `opened` |
| `version` | Selected version, or empty when no development release is due |
| `pr_url` | Selected pull request URL, otherwise empty |
| `release_source_sha` | Verified immutable release-attempt source, otherwise empty |
| `exit_code` | Invocation exit status; zero is successful observation |

A successful `none` is a measured no-op. `open` in phase `plan` only proposes
preparation. `existing` identifies a verified immutable attempt and leaves its
pull request unchanged. `opened` is written only after queue auto-merge is armed.
An arming failure retains the created URL and source with a nonzero exit code.

The receipt does not prove integration, archive production, promotion or package
publication. Those outcomes retain their existing workflow and artifact owners.
