#!/usr/bin/env bash
# Fail if the tracked public source tree names a specific downstream product or
# a contributor's private infrastructure (fleet hostname, home-LAN address).
#
# Two arms, one rule: nothing in this repository should read as though Harn owns
# a downstream's product or hardware.
#
#   1. Product names, matched as literal patterns. These are public product
#      names, so carrying them here costs nothing. New public text (pull-request
#      metadata, commit messages, comments, and the lines a pull request adds)
#      is held to a stricter rule: it must not name the downstream brand at all,
#      not even as a bare word ("after the <brand> integration lands"). The
#      tracked tree predates that rule and is held only to the compound names.
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
brand="$(printf '%s' "${product:0:1}" | tr '[:lower:]' '[:upper:]')${product:1}"
pattern="${product}-code|${product}-evals|${product}-commerce|${brand} Code"
# New text also refuses the bare brand word. The organisation's legal name
# (`<brand> Labs`, e.g. a license copyright line) is the repository owner, not a
# downstream product, so it is masked before matching.
new_text_pattern="${pattern}|\\<${brand}\\>"
legal_name="${brand} Labs"
denylist="$repo_root/scripts/consumer-host-denylist.sha256"
scanner="$repo_root/scripts/scan_hashed_denylist.mjs"

usage() {
  echo "usage: check_public_product_names.sh [--stdin-label <public-label> | --added-lines <base-sha> <head-sha>]" >&2
  exit 2
}

stdin_label=""
added_base=""
added_head=""
if [[ "$#" -ne 0 ]]; then
  if [[ "$#" -eq 2 && "$1" == "--stdin-label" && "$2" =~ ^[A-Za-z0-9._/-]+$ ]]; then
    stdin_label="$2"
  elif [[ "$#" -eq 3 && "$1" == "--added-lines" && "$2" =~ ^[0-9a-f]{40,64}$ \
    && "$3" =~ ^[0-9a-f]{40,64}$ ]]; then
    added_base="$2"
    added_head="$3"
  else
    usage
  fi
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

# Print `-n -o` matches of the new-text vocabulary in file $1, one
# `line:match` per hit, with the organisation's legal name masked first.
scan_new_text() {
  local masked="$tmp_dir/masked.txt" status
  sed "s/${legal_name}/${brand}_Labs/g" "$1" >"$masked"
  set +e
  grep -a -n -o -E -- "$new_text_pattern" "$masked"
  status=$?
  set -e
  if [[ "$status" -gt 1 ]]; then
    echo "error: failed to scan public text for downstream product names" >&2
    exit "$status"
  fi
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

  scan_new_text "$metadata" >"$tmp_dir/all-hits.txt"

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

if [[ -n "$added_base" ]]; then
  # Lines a pull request adds are new public text. Project each added line onto
  # one row of `added.txt` and its `path:line` onto the same row of
  # `added-locations.txt`, so a hit's row number names the source location.
  added="$tmp_dir/added.txt"
  locations="$tmp_dir/added-locations.txt"
  if ! git -C "$repo_root" -c core.quotePath=false diff --no-color --no-ext-diff \
    --unified=0 --diff-filter=d "$added_base" "$added_head" >"$tmp_dir/diff.txt"; then
    echo "error: the added-line range could not be diffed" >&2
    exit 2
  fi
  awk -v added="$added" -v locations="$locations" '
    # An added line whose text starts with "++ " looks like a file header, so
    # headers are read only between `diff --git` and the first hunk.
    /^diff --git / { in_hunk = 0; path = ""; next }
    !in_hunk && /^\+\+\+ / { path = substr($0, 7); next }
    /^@@ / {
      split($3, target, ",")
      line = substr(target[1], 2) + 0
      in_hunk = 1
      next
    }
    in_hunk && /^\+/ && path != "" {
      print substr($0, 2) >added
      print path ":" line >locations
      line += 1
    }
  ' "$tmp_dir/diff.txt"
  touch "$added" "$locations"

  scan_new_text "$added" >"$tmp_dir/all-hits.txt"
  while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    row="${hit%%:*}"
    matched="${hit#*:}"
    location="$(sed -n "${row}p" "$locations")"
    path="${location%:*}"
    if [[ "$path" == "README.md" ]] || is_allowlisted "$path"; then
      continue
    fi
    digest="$(printf '%s' "$matched" | sha256_of_stdin)"
    printf '%s: sha256:%s\n' "$location" "${digest:0:12}" >>"$tmp_dir/hits.txt"
  done <"$tmp_dir/all-hits.txt"
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
  # The introduction names public applications as examples of Harn adoption.
  # This exception applies only to product names, never private infrastructure.
  if [[ "$path" == "README.md" ]]; then
    continue
  fi
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
