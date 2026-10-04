#!/usr/bin/env bash
# Let a restored Rust cache reuse the workspace crates that did not change.
#
# Cargo decides whether a workspace crate is fresh by comparing source file
# modification times with the dep-info its last build wrote. A fresh checkout
# stamps every file with the checkout time, so after an exact cache hit every
# workspace crate still rebuilds: 33 of 33 for the shared CLI on 2026-10-01,
# even for a commit identical to the cached one. Build scripts that watch whole
# directories add the directories' stamps to the same comparison.
#
# `restore` makes the stamps say what changed. It reads the commit the cache
# was built from, diffs that tree against HEAD, dates every tracked file and
# directory before any build, then stamps the changed paths and their parent
# directories with the current time. Cargo then rebuilds exactly the crates
# whose sources differ from the cached build, and their dependents.
#
# Correctness rests on the record naming the tree every cached workspace unit
# was built from. `record` writes it only after the job's build succeeded, and
# `restore` deletes it first, so a cache saved from a failed job carries no
# record. Without a readable record, or when its commit cannot be fetched,
# `restore` changes nothing and every crate rebuilds, as before.
#
# The record lives in its own directory at the checkout root, which the
# rust-cache action saves beside the target directory. It cannot live inside
# the target directory: Swatinem's save deletes every file at the target's top
# level except CACHEDIR.TAG. At the checkout root it shares the target's
# lifecycle on every runner: checkout cleans both, and one cache entry restores
# both. If the target directory sits somewhere checkout does not clean, the
# record is simply missing and every crate rebuilds.
#
# Usage:
#   scripts/ci/reuse_workspace_crates.sh restore
#   scripts/ci/reuse_workspace_crates.sh record
#   scripts/ci/reuse_workspace_crates.sh current
set -euo pipefail

# Keep in step with the rust-cache action's cache-directories, which saves
# this directory with every workspace-crate cache entry.
RECORD_DIR=".harn-workspace-source"
# 2000-01-01T00:00:00Z, written as an epoch so no implementation reads a zone.
BEFORE_ANY_BUILD="@946684800"
# Projected from NATIVE_SOURCE_PATHS in scripts/ci_cache_policy/policy_core.harn.
SOURCE_PATHS=(Cargo.lock Cargo.toml crates spec tree-sitter-harn)

usage() {
  echo "usage: $0 restore|record|current" >&2
  exit 2
}

[[ $# -eq 1 ]] || usage
mode=$1
cd "$(git rev-parse --show-toplevel)"
record="$RECORD_DIR/commit"

current_source() {
  [[ -f "$record" ]] || return 1
  local recorded head
  recorded=$(cat "$record")
  [[ "$recorded" =~ ^[0-9a-f]{40}$ ]] || return 1
  head=$(git rev-parse --verify HEAD) || return 1
  # These are the canonical native-source fingerprint roots. The owning cache
  # policy checks this projection against NATIVE_SOURCE_FINGERPRINT_BODY.
  if [[ "$recorded" != "$head" ]]; then
    git cat-file -e "${recorded}^{tree}" 2>/dev/null \
      || git fetch --quiet --no-tags --depth=1 origin "$recorded" 2>/dev/null \
      || return 1
    git diff --quiet "$recorded" HEAD -- "${SOURCE_PATHS[@]}" || return 1
  fi
  git diff --quiet HEAD -- && git diff --cached --quiet HEAD --
}

case "$mode" in
  current)
    current_source
    exit $?
    ;;
  record)
    mkdir -p "$RECORD_DIR"
    git rev-parse --verify HEAD > "$record"
    echo "workspace crate reuse: recorded $(cat "$record") as the source of the cached build"
    exit 0
    ;;
  restore) ;;
  *) usage ;;
esac

# An exact dependency key says nothing about which workspace source was built.
# Capture this fact before restore consumes the record. An absent or invalid
# record stays false, including on platforms that cannot back-date files.
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  if current_source; then
    echo "source-current=true" >> "$GITHUB_OUTPUT"
  else
    echo "source-current=false" >> "$GITHUB_OUTPUT"
  fi
fi

# Probe the capability, not the brand: GNU and uutils both accept these flags,
# BSD touch does not.
can_back_date() {
  local probe expected
  probe=$(mktemp "${TMPDIR:-/tmp}/harn-reuse-probe.XXXXXX") || return 1
  expected=${BEFORE_ANY_BUILD#@}
  touch -h -d "$BEFORE_ANY_BUILD" "$probe" 2>/dev/null \
    && [[ "$(stat -c %Y "$probe" 2>/dev/null)" == "$expected" ]]
  local status=$?
  rm -f -- "$probe"
  return "$status"
}
if ! can_back_date; then
  echo "workspace crate reuse: cannot set file times here; every workspace crate rebuilds"
  rm -f -- "$record"
  exit 0
fi
if [[ ! -f "$record" ]]; then
  echo "workspace crate reuse: no source record; every workspace crate rebuilds"
  exit 0
fi
source_commit=$(tr -d '[:space:]' < "$record")
# The record describes the restored artifacts only until this job rebuilds
# them. Drop it now, so a failed job cannot hand it to the next restore.
rm -f -- "$record"
if [[ ! "$source_commit" =~ ^[0-9a-f]{40}$ ]]; then
  echo "workspace crate reuse: unreadable source record; every workspace crate rebuilds"
  exit 0
fi
if ! git cat-file -e "${source_commit}^{tree}" 2>/dev/null \
  && ! git fetch --quiet --no-tags --depth=1 origin "$source_commit" 2>/dev/null; then
  echo "workspace crate reuse: cannot fetch $source_commit; every workspace crate rebuilds"
  exit 0
fi
changed=$(mktemp "${TMPDIR:-/tmp}/harn-reuse-changed.XXXXXX")
trap 'rm -f "$changed"' EXIT
if ! git diff --name-only -z --no-renames "$source_commit" HEAD > "$changed"; then
  echo "workspace crate reuse: cannot diff $source_commit against HEAD; every workspace crate rebuilds"
  exit 0
fi

# Everything first, then the changed paths, so a path in both ends up new.
# Only tracked files and the directories that hold them: the target directory,
# untracked generated inputs, and .git keep their own stamps.
git ls-files -z | xargs -0 -r touch -c -h -d "$BEFORE_ANY_BUILD"
git ls-files \
  | awk -F/ '{ p = ""; for (i = 1; i < NF; i++) { p = (i == 1 ? $1 : p "/" $i); print p } }' \
  | sort -u \
  | while IFS= read -r dir; do [[ -d "$dir" ]] && touch -h -d "$BEFORE_ANY_BUILD" "$dir"; done
touch -h -d "$BEFORE_ANY_BUILD" .
count=0
while IFS= read -r -d '' path; do
  count=$((count + 1))
  [[ -e "$path" || -L "$path" ]] && touch -h "$path"
  # An added or deleted file changes its directory's listing, which a build
  # script watching that directory must see.
  dir=$(dirname "$path")
  while :; do
    [[ -d "$dir" ]] && touch -h "$dir"
    [[ "$dir" == "." || "$dir" == "/" ]] && break
    dir=$(dirname "$dir")
  done
done < "$changed"
echo "workspace crate reuse: $count path(s) changed since $source_commit; unchanged workspace crates stay fresh"
