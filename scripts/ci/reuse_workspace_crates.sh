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
# Usage:
#   scripts/ci/reuse_workspace_crates.sh restore TARGET_DIR
#   scripts/ci/reuse_workspace_crates.sh record TARGET_DIR
set -euo pipefail

RECORD_NAME=".harn-workspace-source"
# 2000-01-01T00:00:00Z, written as an epoch so no implementation reads a zone.
BEFORE_ANY_BUILD="@946684800"

usage() {
  echo "usage: $0 restore|record TARGET_DIR" >&2
  exit 2
}

[[ $# -eq 2 ]] || usage
mode=$1
target_dir=$2
record="$target_dir/$RECORD_NAME"

case "$mode" in
  record)
    mkdir -p "$target_dir"
    git rev-parse --verify HEAD > "$record"
    echo "workspace crate reuse: recorded $(cat "$record") as the source of $target_dir"
    exit 0
    ;;
  restore) ;;
  *) usage ;;
esac

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
  echo "workspace crate reuse: no source record in $target_dir; every workspace crate rebuilds"
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
