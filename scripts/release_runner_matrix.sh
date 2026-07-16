#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
POLICY="${HARN_RELEASE_RUNNER_POLICY:-${ROOT}/.github/release-runner-policy.json}"
MODE=""
PROFILE="policy"
TARGETS=""

usage() {
  cat <<'EOF'
Usage: scripts/release_runner_matrix.sh --mode MODE [--profile PROFILE] [--targets TARGETS]

MODE is warm, primary, recovery, or benchmark. PROFILE is policy, standard,
or fast. Warm and primary always use policy; benchmark requires an explicit
standard or fast profile. TARGETS is a comma/space-separated target subset.
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --mode)
      MODE="${2:-}"
      shift 2
      ;;
    --profile)
      PROFILE="${2:-}"
      shift 2
      ;;
    --targets)
      TARGETS="${2:-}"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$MODE:$PROFILE" in
  warm:policy|primary:policy|recovery:policy|recovery:standard|recovery:fast|benchmark:standard|benchmark:fast) ;;
  benchmark:policy)
    echo 'benchmark mode requires --profile standard or --profile fast' >&2
    exit 2
    ;;
  *)
    printf 'unsupported release runner mode/profile: %s/%s\n' "$MODE" "$PROFILE" >&2
    exit 2
    ;;
esac

jq -e '
  (keys == ["pricing", "schema_version", "targets"]) and
  .schema_version == 1 and
  (.pricing | keys == ["as_of", "macos_large_usd_per_minute", "source"]) and
  (.pricing.as_of | type == "string" and length > 0) and
  (.pricing.source | type == "string" and startswith("https://docs.github.com/")) and
  (.pricing.macos_large_usd_per_minute | type == "number" and . > 0) and
  (.targets | type == "array" and length > 0) and
  ([.targets[].target] | length == (unique | length)) and
  all(.targets[];
    (keys == ["release_codegen_units", "runners", "target", "use_sccache"]) and
    (.runners | keys == ["fast", "primary", "recovery", "standard", "warm"]) and
    (.target | type == "string" and length > 0) and
    (.release_codegen_units | type == "number") and
    (.use_sccache == "true" or .use_sccache == "false"))
' "$POLICY" >/dev/null

# Validate runner fields separately so jq evaluates them against each target.
jq -e 'all(.targets[]; .runners as $r | all(["warm", "primary", "recovery", "standard", "fast"][]; ($r[.] | type == "string" and length > 0)))' \
  "$POLICY" >/dev/null

REQUESTED_JSON="$({
  printf '%s\n' "$TARGETS" | tr ',[:space:]' '\n' | sed '/^$/d' | LC_ALL=C sort -u | jq -R . | jq -sc 'unique'
})"

if [[ "$MODE" == "benchmark" && "$(jq 'length' <<<"$REQUESTED_JSON")" -eq 0 ]]; then
  echo 'benchmark mode requires at least one target' >&2
  exit 2
fi

UNKNOWN="$(jq -nr \
  --argjson requested "$REQUESTED_JSON" \
  --slurpfile policy "$POLICY" \
  '$requested - [$policy[0].targets[].target] | .[]')"
if [[ -n "$UNKNOWN" ]]; then
  printf 'unknown release target(s):\n%s\n' "$UNKNOWN" >&2
  exit 2
fi

RUNNER_KEY="$PROFILE"
if [[ "$PROFILE" == "policy" ]]; then
  RUNNER_KEY="$MODE"
fi

jq -c \
  --arg runner_key "$RUNNER_KEY" \
  --argjson requested "$REQUESTED_JSON" '
    def rust_cache_broad_restore_prefix($target):
      if $target == "x86_64-apple-darwin" then
        "v0-rust-release-x86_64-apple-darwin-Darwin-x64-"
      elif $target == "aarch64-apple-darwin" then
        "v0-rust-release-aarch64-apple-darwin-Darwin-arm64-"
      elif $target == "x86_64-unknown-linux-gnu" then
        "v0-rust-release-x86_64-unknown-linux-gnu-Linux-x64-"
      elif $target == "aarch64-unknown-linux-gnu" then
        "v0-rust-release-aarch64-unknown-linux-gnu-Linux-arm64-"
      elif $target == "x86_64-pc-windows-msvc" then
        "v0-rust-release-x86_64-pc-windows-msvc-Windows_NT-x64-"
      else
        error("missing release Rust cache prefix for " + $target)
      end;

    [.targets[]
      | . as $entry
      | select(($requested | length) == 0 or ($requested | index($entry.target)))
      | {
          target,
          runner: .runners[$runner_key],
          rust_cache_broad_restore_prefix: rust_cache_broad_restore_prefix(.target),
          release_codegen_units,
          use_sccache
        }]
  ' "$POLICY"
