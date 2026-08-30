#!/usr/bin/env bash
# Fail if the tracked public source tree names a specific downstream product or
# a contributor's private infrastructure (fleet hostname, home-LAN address).
#
# Two arms, one rule: nothing in this repository should read as though Harn owns
# a downstream's product or hardware.
#
#   1. Product names, matched as literal patterns. These are public product
#      names, so carrying them here costs nothing.
#   2. Private infrastructure, matched against a committed sha256 denylist
#      (`scripts/consumer-host-denylist.sha256`). The plaintext is deliberately
#      absent: a gate that listed the hostnames it guards would publish them on
#      every clone and echo them into every public CI log on a match, which is
#      the leak it exists to prevent.
#
# Neither arm prints the matched text. Both report `path:line` and a sha256
# prefix, which is enough to find the line locally and reveals nothing to a
# reader of a public log.
#
# Immutable history, captured measurements, provenance, and compatibility paths
# are an explicit allowlist so the exception surface remains inspectable.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
product="burin"
brand="Burin"
pattern="${product}-code|${product}-evals|${product}-commerce|${brand} Code"
denylist="$repo_root/scripts/consumer-host-denylist.sha256"
scanner="$repo_root/scripts/scan_hashed_denylist.mjs"

stdin_label=""
if [[ "$#" -ne 0 ]]; then
  if [[ "$#" -ne 2 || "$1" != "--stdin-label" || ! "$2" =~ ^[A-Za-z0-9._/-]+$ ]]; then
    echo "usage: check_public_product_names.sh [--stdin-label <public-label>]" >&2
    exit 2
  fi
  stdin_label="$2"
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-public-product-names.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

sha256_of_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  else
    shasum -a 256 | awk '{print $1}'
  fi
}

report_verdict() {
  if [[ -s "$tmp_dir/hits.txt" ]]; then
    echo "error: public text names a downstream host product or its private infrastructure:" >&2
    cat "$tmp_dir/hits.txt" >&2
    echo >&2
    echo "Use host-neutral wording (downstream host, host repo, packager) and" >&2
    echo "RFC-2606/RFC-5737 placeholders (example.internal, 192.0.2.0/24)." >&2
    echo "Locations are reported by digest so this output stays safe in a public log;" >&2
    echo "inspect the named source locally to see what matched." >&2
    exit 1
  fi

  echo "public product-name and infrastructure scan passed"
}

# Paths whose match is deliberate. A path is allowlisted for BOTH arms; keep
# the list short and say why in the commit that adds one.
is_allowlisted() {
  case "$1" in
    CHANGELOG.md|changelog/archive/*|experiments/step-judge/results/*) return 0 ;;
    spec/provider-catalog/provider-catalog.json) return 0 ;;
    crates/harn-vm/src/llm/catalog_sources/50-presentation/00-model-selection.toml) return 0 ;;
    crates/harn-vm/src/llm/providers.toml) return 0 ;;
    scripts/agent_shell_guard_policy.harn|scripts/tests/agent_shell_guard_test.harn) return 0 ;;
    crates/harn-hostlib/src/code_index/walker.rs|crates/harn-hostlib/tests/harn_hostlib/code_index.rs) return 0 ;;
    *) return 1 ;;
  esac
}

if [[ ! -f "$denylist" ]]; then
  echo "error: missing hashed denylist at $denylist" >&2
  exit 2
fi
if ! command -v node >/dev/null 2>&1; then
  echo "error: node is required to evaluate the hashed infrastructure denylist" >&2
  exit 2
fi

if [[ -n "$stdin_label" ]]; then
  metadata="$tmp_dir/input.txt"
  umask 077
  cat >"$metadata"

  set +e
  grep -a -n -o -E -- "$pattern" "$metadata" >"$tmp_dir/all-hits.txt"
  scan_status=$?
  set -e
  if [[ "$scan_status" -gt 1 ]]; then
    echo "error: failed to scan public text for downstream product names" >&2
    exit "$scan_status"
  fi

  while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    line="${hit%%:*}"
    matched="${hit#*:}"
    digest="$(printf '%s' "$matched" | sha256_of_stdin)"
    printf '%s:%s: sha256:%s\n' "$stdin_label" "$line" "${digest:0:12}" >>"$tmp_dir/hits.txt"
  done <"$tmp_dir/all-hits.txt"

  set +e
  node "$scanner" "$denylist" --text-label "$stdin_label" \
    <"$metadata" >"$tmp_dir/host-hits.txt"
  host_status=$?
  set -e
  if [[ "$host_status" -gt 1 ]]; then
    echo "error: failed to evaluate the hashed infrastructure denylist" >&2
    exit "$host_status"
  fi
  cat "$tmp_dir/host-hits.txt" >>"$tmp_dir/hits.txt"
  report_verdict
  exit 0
fi

# --- Arm 1: downstream product names -----------------------------------------
set +e
git -C "$repo_root" grep -n -I -o -E -- "$pattern" >"$tmp_dir/all-hits.txt"
scan_status=$?
set -e
if [[ "$scan_status" -gt 1 ]]; then
  echo "error: failed to scan tracked source files for downstream product names" >&2
  exit "$scan_status"
fi

while IFS= read -r hit; do
  [[ -z "$hit" ]] && continue
  path="${hit%%:*}"
  rest="${hit#*:}"
  line="${rest%%:*}"
  matched="${rest#*:}"
  if is_allowlisted "$path"; then
    continue
  fi
  # Report the location and a digest, never the matched text.
  digest="$(printf '%s' "$matched" | sha256_of_stdin)"
  printf '%s:%s: sha256:%s\n' "$path" "$line" "${digest:0:12}" >>"$tmp_dir/hits.txt"
done <"$tmp_dir/all-hits.txt"

# --- Arm 2: private infrastructure, by hash ----------------------------------
set +e
git -C "$repo_root" ls-files -z \
  | (cd "$repo_root" && node "$scanner" "$denylist") >"$tmp_dir/host-hits.txt"
host_status=$?
set -e
if [[ "$host_status" -gt 1 ]]; then
  echo "error: failed to evaluate the hashed infrastructure denylist" >&2
  exit "$host_status"
fi

while IFS= read -r hit; do
  [[ -z "$hit" ]] && continue
  path="${hit%%:*}"
  if is_allowlisted "$path"; then
    continue
  fi
  printf '%s\n' "$hit" >>"$tmp_dir/hits.txt"
done <"$tmp_dir/host-hits.txt"

# --- Verdict -----------------------------------------------------------------
report_verdict
