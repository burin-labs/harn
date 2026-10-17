#!/usr/bin/env bash
# Measures Rust build/test resource use in a Claude Code cloud session.
# Usage: bash scripts/cloud-resource-probe.sh <label>
# Writes to $TMPDIR/cloud-probe/<label>/ while running, so the worktree stays
# clean for Harn's freshness check, then copies it to cloud-probe/<label>/ in
# the repo root. Never aborts on a failed phase.
set -uo pipefail

label="${1:?usage: cloud-resource-probe.sh <label>}"
root="$(git rev-parse --show-toplevel)"
cd "$root"
repo="$(basename "$(git remote get-url origin 2>/dev/null || echo "$root")" .git)"
out="${TMPDIR:-/tmp}/cloud-probe/$label"
mkdir -p "$out"
exec > >(tee -a "$out/probe.log") 2>&1

now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
say() { echo "[$(now)] $*"; }

target_dir() {
  if [[ "$repo" == "harn" ]]; then
    ./scripts/cargo_with_worktree_build_dir.sh metadata --format-version 1 --no-deps 2>/dev/null \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])'
  else
    ./scripts/tui-cargo.sh target-dir 2>/dev/null | tail -n 1
  fi
}

disk_report() {
  local tag="$1" t
  t="$(target_dir || true)"
  {
    echo "== $tag $(now)"
    echo "target_dir=$t"
    df -BM / "$root" "$HOME" "${TMPDIR:-/tmp}" 2>/dev/null
    if [[ -n "$t" && -d "$t" ]]; then
      du -sm "$t" 2>/dev/null
      du -sm "$t"/*/ 2>/dev/null
      for p in "$t"/*/; do
        [[ -d "$p/deps" ]] || continue
        du -sm "$p"deps "$p"incremental "$p"build "$p".fingerprint "$p"examples 2>/dev/null
      done
      du -sm "$t"/nextest 2>/dev/null
      echo "-- deps executables: count, total MiB, 15 largest"
      find "$t" -path '*/deps/*' -type f -perm -u+x ! -name '*.so' -printf '%s %p\n' 2>/dev/null \
        | awk '{n++; s+=$1} END {printf "%d %d\n", n, s/1048576}'
      find "$t" -path '*/deps/*' -type f -printf '%s %p\n' 2>/dev/null | sort -rn | head -n 15 \
        | awk '{printf "%d %s\n", $1/1048576, $2}'
    fi
    du -sm "$HOME/.cargo" "$HOME/.cargo/registry" "$HOME/.cargo/git" "$HOME/.cargo/bin" "$HOME/.rustup" 2>/dev/null
    du -sm "$HOME/.cache/sccache" "$HOME/.cache/harn" "$HOME/.cache" 2>/dev/null
    du -sm "$root/.git" 2>/dev/null
    find "$root" -maxdepth 4 -type d -name node_modules -prune -print 2>/dev/null | while read -r d; do du -sm "$d"; done
    du -sm "$root" 2>/dev/null
  } >>"$out/disk.txt"
}

cgroup_report() {
  local tag="$1" cg="/sys/fs/cgroup"
  {
    echo "== $tag $(now)"
    for f in memory.max memory.high memory.peak memory.current memory.swap.max memory.events cpu.max; do
      [[ -r "$cg/$f" ]] && echo "$f: $(tr '\n' ' ' <"$cg/$f")"
    done
    grep -E 'MemTotal|MemAvailable|SwapTotal|SwapFree' /proc/meminfo
    cat /proc/swaps
  } >>"$out/cgroup.txt"
}

sampler() {
  # One line per second: ts, MemAvailable MiB, root disk avail MiB, then
  # pid:name:crate:VmHWM_kB:VmRSS_kB for every compiler/linker/test process.
  while :; do
    local ts mem disk procs="" pid name crate hwm rss
    ts="$(date +%s)"
    mem="$(awk '/MemAvailable/ {print int($2/1024)}' /proc/meminfo)"
    disk="$(df -BM --output=avail "$root" | tail -n 1 | tr -dc 0-9)"
    for pid in $(pgrep -x 'rustc|ld|ld.lld|ld.gold|mold|cc|c\+\+|collect2|cargo-nextest|harn' 2>/dev/null; pgrep -f 'target/.*/deps/' 2>/dev/null); do
      [[ -r "/proc/$pid/status" ]] || continue
      name="$(awk '/^Name:/ {print $2}' "/proc/$pid/status" 2>/dev/null)"
      hwm="$(awk '/^VmHWM:/ {print $2}' "/proc/$pid/status" 2>/dev/null)"
      rss="$(awk '/^VmRSS:/ {print $2}' "/proc/$pid/status" 2>/dev/null)"
      crate="$(tr '\0' '\n' <"/proc/$pid/cmdline" 2>/dev/null | awk 'p {print; exit} $0=="--crate-name" {p=1}')"
      [[ -z "$crate" ]] && crate="$(tr '\0' '\n' <"/proc/$pid/cmdline" 2>/dev/null | head -n 1 | xargs -r basename 2>/dev/null)"
      procs+=" $pid:$name:${crate:--}:${hwm:-0}:${rss:-0}"
    done
    echo "$ts $mem $disk$procs"
    sleep 1
  done >>"$out/samples.txt"
}

phase() {
  local name="$1"; shift
  say "phase $name: $*"
  cgroup_report "before $name"
  local start end status
  start="$(date +%s)"
  if [[ -x /usr/bin/time ]]; then
    /usr/bin/time -v -o "$out/time-$name.txt" "$@" >"$out/phase-$name.log" 2>&1
  else
    "$@" >"$out/phase-$name.log" 2>&1
  fi
  status=$?
  end="$(date +%s)"
  echo "$name status=$status seconds=$((end - start))" | tee -a "$out/phases.txt"
  tail -n 40 "$out/phase-$name.log"
  cgroup_report "after $name"
  disk_report "after $name"
}

say "probe $label in $repo at $(git rev-parse HEAD)"
{
  echo "nproc=$(nproc)"
  uname -a
  rustc -V; cargo -V; rustup show active-toolchain 2>/dev/null
  echo "sccache=$(command -v sccache || echo none)"
  command -v sccache >/dev/null && sccache --show-stats 2>/dev/null | head -n 20
  echo "nextest=$(command -v cargo-nextest || echo none)"
  echo "time=$( [[ -x /usr/bin/time ]] && echo yes || echo no)"
  echo "mold=$(command -v mold || echo none) lld=$(command -v ld.lld || echo none)"
  env | grep -E '^(CARGO|RUST|SCCACHE|HARN_DEV|HARN_CARGO|CLAUDE_CODE_REMOTE|CI|TMPDIR|BURIN_BUILD)[A-Z_]*=' | sed 's/=.*TOKEN.*/=<redacted>/' | sort
  cat "$HOME/.cargo/config.toml" 2>/dev/null
} >"$out/system.txt" 2>&1
cat "$out/system.txt"
cgroup_report start
disk_report start

# Let the session-start warm finish first, so phases measure the agent loop.
waited=0
while [[ -d .claude/dev-setup/warm.lock ]] && (( waited < 3600 )); do
  sleep 15; waited=$((waited + 15))
done
say "waited ${waited}s for session warm"
echo "warm_wait_seconds=$waited" >>"$out/phases.txt"
cp .claude/dev-setup/latest.log "$out/warm.log" 2>/dev/null || true
disk_report "after warm"

sampler &
sampler_pid=$!
trap 'kill "$sampler_pid" 2>/dev/null' EXIT

# Edit loop: a one-line harn-vm edit, then the session's own `make test`
# build for that crate, which rebuilds the library and its unit tests together.
edit_round() {
  local n="$1" file=crates/harn-vm/src/lib.rs
  printf '\n// cloud-resource-probe edit %s\n' "$n" >>"$file"
  phase "edit-$n" make test ARGS="-p harn-vm --no-run"
  git checkout -- "$file"
}

if [[ "$repo" == "harn" ]]; then
  phase build make build
  phase test-vm make test ARGS="-p harn-vm"
  phase test-cli make cli-test
  edit_round 1
  edit_round 2
else
  phase build make build
  phase test make test
fi

kill "$sampler_pid" 2>/dev/null
gzip -f "$out/samples.txt"
cgroup_report end
rm -rf "$root/cloud-probe/$label"
mkdir -p "$root/cloud-probe"
cp -R "$out" "$root/cloud-probe/$label"
say "done; results in cloud-probe/$label"
