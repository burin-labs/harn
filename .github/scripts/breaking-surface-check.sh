#!/usr/bin/env bash
# Refuse an undeclared break of a published surface.
#
# A caller of the `harn` CLI or a crate that depends on a published Harn crate
# breaks when a pull request removes something they use. That is allowed, but
# it must be declared: the pull request adds a `changelog.d/<id>.breaking.md`
# fragment with a `Migration:` section (lib/breaking-fragments.sh owns that
# rule), so the release notes tell the consumer what to change.
#
# Usage:
#   breaking-surface-check.sh cli
#       Diff spec/cli-surface.txt between the merge base and HEAD_SHA. A
#       removed or renamed line is a break; an added line is not.
#   breaking-surface-check.sh rust-api [REPORT]
#       Run cargo-semver-checks on every publishable crate against the merge
#       base (or read its captured REPORT). A crate whose summary says it
#       requires a new major version is a break. The baseline is the merge
#       base rather than the last release tag so a break is charged to the
#       pull request that makes it, not to every pull request after it until
#       the next release.
#
# Inputs (environment):
#   BASE_SHA  base commit (default: origin/main); the merge base with HEAD_SHA
#             is the "before" side
#   HEAD_SHA  head commit (default: HEAD)
#
# Exit codes: 0 no break, or a declared one; 1 an undeclared break or an
# unreadable report; 2 usage error.

set -euo pipefail

BASE_SHA="${BASE_SHA:-origin/main}"
HEAD_SHA="${HEAD_SHA:-HEAD}"
SURFACE_FILE="spec/cli-surface.txt"
TITLE="Breaking surface"

# shellcheck source=.github/scripts/lib/breaking-fragments.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/breaking-fragments.sh"

usage() {
  echo "usage: $0 cli | rust-api [REPORT]" >&2
  exit 2
}

if ! merge_base=$(git merge-base "$BASE_SHA" "$HEAD_SHA" 2>/dev/null); then
  merge_base="$BASE_SHA"
fi

surface_entries() {
  grep -v '^#' | LC_ALL=C sort -u
}

# Print each removed CLI surface entry, one per line.
cli_breaks() {
  local before after
  if ! before=$(git show "$merge_base:$SURFACE_FILE" 2>/dev/null); then
    echo "::notice title=$TITLE::$SURFACE_FILE does not exist at the merge base; nothing to compare." >&2
    return 0
  fi
  # A deleted listing removes every entry, which is what it reads as.
  after=$(git show "$HEAD_SHA:$SURFACE_FILE" 2>/dev/null || true)
  LC_ALL=C comm -23 \
    <(printf '%s\n' "$before" | surface_entries) \
    <(printf '%s\n' "$after" | surface_entries) \
    | sed '/^$/d'
}

# Print one line per crate the report says needs a new major version. Fail
# when the report does not show that every crate was checked: an empty or
# truncated report, or a crate that ran zero lints, must not read as "no
# break". A pre-release version such as `0.10.145-dev` makes
# cargo-semver-checks assume a major release and skip every lint unless the
# release type is given, which is the zero this refuses.
rust_api_breaks() {
  local report=$1 plain checked summarized unchecked
  if [ ! -s "$report" ]; then
    echo "::error title=$TITLE::cargo-semver-checks report $report is missing or empty." >&2
    return 1
  fi
  # A captured report may carry terminal colors.
  plain=$(sed $'s/\x1b\\[[0-9;]*m//g' "$report")
  checked=$(grep -Ec '^[[:space:]]*Checking[[:space:]]' <<<"$plain" || true)
  summarized=$(grep -Ec '^[[:space:]]*Summary[[:space:]]' <<<"$plain" || true)
  if [ "$checked" -eq 0 ] || [ "$checked" -ne "$summarized" ]; then
    echo "::error title=$TITLE::cargo-semver-checks report $report checked $checked crate(s) and summarized $summarized, so it did not finish; read it for the build error." >&2
    return 1
  fi
  unchecked=$(grep -Ec '^[[:space:]]*Checked[[:space:]].* 0 checks' <<<"$plain" || true)
  if [ "$unchecked" -ne 0 ]; then
    echo "::error title=$TITLE::cargo-semver-checks ran zero lints on $unchecked crate(s), so it measured nothing; pass an explicit release type." >&2
    return 1
  fi
  echo "::notice title=$TITLE::cargo-semver-checks checked $checked crate(s) against $merge_base." >&2
  # semver_report heads each crate with `Crate <package>`; cargo-semver-checks
  # then prints `Checking`, its failed lints, and a `Summary` line. A report
  # from `cargo semver-checks --workspace` names the crate on `Checking`.
  awk '
    /^[[:space:]]*Crate[[:space:]]/ { crate = $2; named = 1 }
    /^[[:space:]]*Checking[[:space:]]/ { if (!named) { crate = $2 } named = 0 }
    /^[[:space:]]*Summary[[:space:]].*requires new major version/ {
      sub(/^[[:space:]]*Summary[[:space:]]+/, "")
      print crate ": " $0
    }
  ' <<<"$plain"
}

# publishable_rustdoc OUT: write rustdoc JSON for every publishable library
# crate of the checked-out tree into OUT, and list them in OUT/crates.tsv as
# `package<TAB>crate`. One `cargo doc` covers every crate, so the dependency
# graph and feature unification build once per side rather than once per
# crate, in the target directory the rest of the job already warmed.
publishable_rustdoc() {
  local out=$1 metadata target_dir package crate
  local -a packages=()
  mkdir -p "$out"
  metadata=$(cargo metadata --format-version 1 --no-deps --locked)
  target_dir=$(jq -r .target_directory <<<"$metadata")
  # `publish = false` reads as []; proc-macro crates have no rustdoc API to diff.
  jq -r '.packages[] | select(.publish != [])
    | . as $package | .targets[] | select(.kind | index("lib") or index("rlib"))
    | [$package.name, (.name | gsub("-"; "_"))] | @tsv' <<<"$metadata" >"$out/crates.tsv"
  while IFS=$'\t' read -r package crate; do
    packages+=(-p "$package")
  done <"$out/crates.tsv"
  rm -f "$target_dir"/doc/*.json
  RUSTC_BOOTSTRAP=1 RUSTDOCFLAGS="-Z unstable-options --output-format json" \
    cargo doc --locked --no-deps --lib "${packages[@]}" >&2
  while IFS=$'\t' read -r package crate; do
    cp "$target_dir/doc/$crate.json" "$out/$crate.json"
  done <"$out/crates.tsv"
}

# semver_report WORK: print a cargo-semver-checks report for every publishable
# crate, current tree against the merge base. The exit status of each check is
# not the verdict: an unchanged 0.x version fails on minor-level additions too.
# The report's per-crate summaries are, and rust_api_breaks reads them.
#
# The baseline is documented by checking the merge base out in place and back.
# A second checkout at another path cannot share the target directory: Cargo
# keys a workspace crate's build-script output by its workspace-relative path,
# so the two checkouts would reuse each other's generated sources, which name
# absolute paths. A private target directory would rebuild every dependency.
semver_report() {
  local work=$1 package crate head
  if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "::error title=$TITLE::rust-api checks out the merge base in place; commit or stash local changes first." >&2
    return 1
  fi
  # Return to the branch when there is one, else to the detached commit.
  head=$(git symbolic-ref --quiet --short HEAD || git rev-parse HEAD)
  publishable_rustdoc "$work/current"
  trap 'git checkout --quiet "'"$head"'"; rm -rf "'"$work"'"' EXIT
  git checkout --quiet --detach "$merge_base"
  publishable_rustdoc "$work/baseline-doc"
  git checkout --quiet "$head"
  while IFS=$'\t' read -r package crate; do
    if [ ! -f "$work/baseline-doc/$crate.json" ]; then
      echo "::notice title=$TITLE::$package is new since $merge_base; nothing to compare." >&2
      continue
    fi
    echo "       Crate $package"
    # `--release-type patch` asks for every lint whatever the version says; a
    # major-level finding still reports that it requires a new major version.
    cargo semver-checks --color never --release-type patch \
      --baseline-rustdoc "$work/baseline-doc/$crate.json" \
      --current-rustdoc "$work/current/$crate.json" 2>&1 || true
  done <"$work/current/crates.tsv"
}

mode=${1:-}
case "$mode" in
  cli)
    [ $# -eq 1 ] || usage
    breaks=$(cli_breaks)
    what="removes or renames CLI surface in $SURFACE_FILE"
    ;;
  rust-api)
    [ $# -le 2 ] || usage
    report=${2:-}
    if [ -z "$report" ]; then
      work=$(mktemp -d)
      trap 'rm -rf "$work"' EXIT
      report="$work/report.log"
      semver_report "$work" >"$report"
      cat "$report"
    fi
    breaks=$(rust_api_breaks "$report")
    what="makes a major (breaking) change to a published crate's public API"
    ;;
  *)
    usage
    ;;
esac

if [ -z "$breaks" ]; then
  echo "::notice title=$TITLE::no $mode break; pass."
  exit 0
fi

declared=$(BREAKING_FRAGMENT_GATE_TITLE="$TITLE" declared_breaking_fragments "$merge_base" "$HEAD_SHA")
if [ -n "$declared" ]; then
  echo "::notice title=$TITLE::$mode break declared by $(printf '%s' "$declared" | tr '\n' ' ')"
  printf '%s\n' "$breaks" | sed 's/^/  - /'
  exit 0
fi

{
  echo "::error title=$TITLE::This change $what without a \`changelog.d/<id>.breaking.md\` fragment with a \`Migration:\` section."
  echo ""
  echo "Removed or broken:"
  printf '%s\n' "$breaks" | sed 's/^/  - /'
  echo ""
  echo "Keep the old spelling working (a Clap alias, a deprecated item, a"
  echo "#[non_exhaustive] enum), or add the fragment and say what a consumer"
  echo "changes. See changelog.d/README.md."
} >&2
exit 1
