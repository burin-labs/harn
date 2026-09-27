#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
POLICY="${HARN_RELEASE_RUNNER_POLICY:-${ROOT}/.github/release-runner-policy.json}"
MODE=""
PROFILE="policy"
TARGETS=""
ENABLE_BLACKSMITH_MACOS="${HARN_RELEASE_ENABLE_BLACKSMITH_MACOS:-false}"

usage() {
  cat <<'EOF'
Usage: scripts/release_runner_matrix.sh --mode MODE [--profile PROFILE] [--targets TARGETS]

MODE is warm, primary, recovery, candidate, or benchmark. PROFILE is policy,
standard, or fast. Warm, primary, and candidate always use policy; benchmark
requires an explicit standard or fast profile. TARGETS is a comma/space-
separated target subset. Candidate uses the primary runner map.
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
  warm:policy|primary:policy|candidate:policy|recovery:policy|recovery:standard|recovery:fast|benchmark:standard|benchmark:fast) ;;
  benchmark:policy)
    echo 'benchmark mode requires --profile standard or --profile fast' >&2
    exit 2
    ;;
  *)
    printf 'unsupported release runner mode/profile: %s/%s\n' "$MODE" "$PROFILE" >&2
    exit 2
    ;;
esac

case "$ENABLE_BLACKSMITH_MACOS" in
  true|false) ;;
  *)
    echo 'HARN_RELEASE_ENABLE_BLACKSMITH_MACOS must be true or false' >&2
    exit 2
    ;;
esac

jq -e '
  . as $policy |
  (keys == ["jobs", "pricing", "runner_registry", "schema_version", "targets"]) and
  .schema_version == 4 and
  (.jobs | keys == ["cli_aot"]) and
  (.jobs.cli_aot | keys == ["primary", "standard"]) and
  all(.jobs.cli_aot[]; type == "string" and $policy.runner_registry[.]?.host_os == "linux") and
  (.pricing | keys == ["as_of", "sources"]) and
  (.pricing.as_of | type == "string" and length > 0) and
  (.pricing.sources | keys == ["blacksmith", "github"]) and
  (.pricing.sources.blacksmith | type == "string" and startswith("https://www.blacksmith.sh/")) and
  (.pricing.sources.github | type == "string" and startswith("https://docs.github.com/")) and
  (.runner_registry | type == "object" and length > 0) and
  all(.runner_registry | to_entries[];
    .value.provider as $provider |
    (.key | type == "string" and length > 0) and
    (.value | keys == ["glibc_version", "host_arch", "host_os", "provider", "usd_per_minute"]) and
    (.value.host_os == "linux" or .value.host_os == "macos" or .value.host_os == "windows") and
    (.value.host_arch == "x86_64" or .value.host_arch == "aarch64") and
    (if .value.host_os == "linux"
      then (.value.glibc_version | type == "string" and test("^[0-9]+\\.[0-9]+$"))
      else .value.glibc_version == null
      end) and
    ($policy.pricing.sources | has($provider)) and
    (.value.usd_per_minute == null or (.value.usd_per_minute | type == "number" and . > 0))
  ) and
  (.targets | type == "array" and length > 0) and
  ([.targets[].target] | length == (unique | length)) and
  all(.targets[];
    (keys == ["glibc_max", "release_codegen_units", "runners", "sccache_backend", "target", "use_sccache"]) and
    (.runners | keys == ["fast", "primary", "recovery", "standard", "warm"]) and
    (.target | type == "string" and length > 0) and
    (if (.target | endswith("unknown-linux-gnu"))
      then .glibc_max == "2.35"
      else .glibc_max == null
      end) and
    (.release_codegen_units | type == "number" and floor == . and . >= 1 and . <= 256) and
    (.use_sccache == "true" or .use_sccache == "false") and
    (.sccache_backend == "sticky"
      or .sccache_backend == "local"
      or .sccache_backend == "install"
      or .sccache_backend == "none") and
    ((.use_sccache == "false") == (.sccache_backend == "none")) and
    (. as $target | all(.runners[];
      $policy.runner_registry[.]? as $runner |
      ($runner != null) and
      (if ($target.target | endswith("unknown-linux-gnu")) then
         $runner.host_os == "linux" and
         (($runner.glibc_version | split(".") | map(tonumber))
           <= ($target.glibc_max | split(".") | map(tonumber)))
       elif ($target.target | endswith("apple-darwin")) then $runner.host_os == "macos"
       elif ($target.target | endswith("pc-windows-msvc")) then $runner.host_os == "windows"
       else false end)
    )))
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
# Candidate archives use the same runners as primary releases.
if [[ "$RUNNER_KEY" == "candidate" ]]; then
  RUNNER_KEY="primary"
fi

jq -c \
  --arg runner_key "$RUNNER_KEY" \
  --argjson enable_blacksmith_macos "$ENABLE_BLACKSMITH_MACOS" \
  --argjson requested "$REQUESTED_JSON" '
    . as $policy |
    def rust_cache_broad_restore_prefix($target):
      if $target == "x86_64-apple-darwin" then
        "v0-rust-release-x86_64-apple-darwin-Darwin-x64-"
      elif $target == "aarch64-apple-darwin" then
        "v0-rust-release-aarch64-apple-darwin-Darwin-arm64-"
      elif $target == "x86_64-unknown-linux-gnu" then
        "v1-rust-release-glibc235-x86_64-unknown-linux-gnu-Linux-x64-"
      elif $target == "aarch64-unknown-linux-gnu" then
        "v1-rust-release-glibc235-aarch64-unknown-linux-gnu-Linux-x64-"
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
          glibc_max,
          runner: (
            if ($runner_key == "primary" or $runner_key == "recovery" or $runner_key == "candidate")
              and (.target | endswith("apple-darwin"))
              and $policy.runner_registry[.runners[$runner_key]].provider == "blacksmith"
              and ($enable_blacksmith_macos | not)
            then .runners.standard
            else .runners[$runner_key]
            end
          ),
          rust_cache_broad_restore_prefix: rust_cache_broad_restore_prefix(.target),
          release_codegen_units,
          use_sccache,
          sccache_backend
        }]
  ' "$POLICY"
