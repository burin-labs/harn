#!/usr/bin/env bash
# Run the current target-cache collector even when this host's checkout is old.
# Install this small wrapper once per host; each invocation fetches main and
# executes only the collector and its time helper from that fetched revision.
set -euo pipefail

# Cron provides a sparse PATH. Keep the common user and package-manager bin
# directories available to Git credential helpers as well as to Git itself.
export PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:${PATH:-/usr/bin:/bin}"

source_repo="${HARN_TARGET_GC_SOURCE_REPO:-$HOME/projects/harn}"
if ! git -C "$source_repo" rev-parse --git-dir >/dev/null 2>&1; then
  echo "harn-target maintenance: source checkout is missing: $source_repo" >&2
  exit 1
fi

export GIT_TERMINAL_PROMPT=0
git -C "$source_repo" fetch --quiet origin \
  '+refs/heads/main:refs/remotes/origin/main'

scratch_base="${TMPDIR:-/tmp}"
scratch_base="${scratch_base%/}"
scratch="$(mktemp -d "${scratch_base}/harn-target-gc-maintenance.XXXXXX")"
cleanup() {
  case "$scratch" in
    "$scratch_base"/harn-target-gc-maintenance.*) rm -rf -- "$scratch" ;;
    *) echo "harn-target maintenance: refusing unexpected scratch path" >&2; return 1 ;;
  esac
}
trap cleanup EXIT

git -C "$source_repo" archive refs/remotes/origin/main \
  scripts/prune_stale_targets.sh scripts/lib/file_time.sh \
  | tar -x -C "$scratch"

# The host policy (retention cap, size ceiling, full size walk) lives in the
# fetched collector, not here, so a policy change reaches every host on its next
# run without reinstalling this wrapper. Disk-pressure triggers call this same
# wrapper. A live compiler and the calling entry still outrank every bound.
"$scratch/scripts/prune_stale_targets.sh" --host-maintenance "$@"
