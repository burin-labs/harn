`harn test --plan` no longer refuses because of an environment variable. The
plan prints the affected-test files and runs nothing, so it refuses execution,
report, filter, shard, coverage and skill options rather than ignoring them
silently. Four of those options read their value straight from an environment
variable, so the refusal could not tell what a caller typed from what a machine
exported, and on any host that exports `HARN_TEST_JOBS` every `--plan`
invocation was refused. The command now records the caller's own options first
and resolves ambient defaults afterwards, in one place. A typed `--jobs` is
still refused, and a shard or worker variable that is set but is not a number is
now named instead of ignored.
