#!/usr/bin/env bash
# End-to-end cold compile benchmark: `harn precompile --warm-cache` over the
# repository's own multi-module trees, one process per tree, each with an empty
# cache. That prepares the embedded stdlib catalog plus every entry, imported
# module, and interface, so it times lexing, parsing, import resolution, type
# checking, code generation, and artifact writes together.
#
# See bench/frontend/README.md for the corpus and the before/after method.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runs=3
bins=()
roots=()

usage() {
  cat <<'EOF'
Usage: scripts/bench_cold_compile.sh --bin LABEL=PATH [--bin LABEL=PATH ...] [options]

Cold-compile each tree with each binary, alternating binaries every round,
and print per-tree and total medians of wall, user, and system seconds.

Options:
  --bin LABEL=PATH   A harn binary to time; repeat to compare binaries
  --runs N           Rounds per binary (default: 3)
  --root DIR         A tree to compile; repeat to replace the default trees
                     (personas, scripts, tests, examples)
  -h, --help         Show this help

Every tree compiles with --keep-going, so a source with type errors is counted
as failed and the run continues. A changed failed count between binaries is a
behavior change, not a speedup.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) bins+=("$2"); shift 2 ;;
    --runs) runs="$2"; shift 2 ;;
    --root) roots+=("$2"); shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
if [[ ${#bins[@]} -eq 0 ]]; then
  usage >&2
  exit 2
fi
if [[ ${#roots[@]} -eq 0 ]]; then
  roots=(personas scripts tests examples)
fi

scratch="$(mktemp -d "${TMPDIR:-/tmp}/harn-cold-compile.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
samples="$scratch/samples.tsv"
: > "$samples"

time_one() {
  local label=$1 bin=$2 root=$3 round=$4
  local cache="$scratch/cache-$label-$round"
  local log="$scratch/log"
  local timing
  TIMEFORMAT='%R %U %S'
  timing=$({ time HARN_CACHE_DIR="$cache" "$bin" precompile --warm-cache --quiet \
    --keep-going "$repo_root/$root" > /dev/null 2> "$log"; } 2>&1) || true
  local summary
  summary=$(grep '^precompile cache:' "$log" | tail -n 1)
  if [[ -z "$summary" ]]; then
    echo "$label: $root produced no summary:" >&2
    tail -n 5 "$log" >&2
    exit 1
  fi
  rm -rf "$cache"
  printf '%s\t%s\t%s\t%s\n' "$label" "$root" "$timing" "$summary" >> "$samples"
}

for round in $(seq 1 "$runs"); do
  for pair in "${bins[@]}"; do
    label=${pair%%=*}
    bin=${pair#*=}
    for root in "${roots[@]}"; do
      time_one "$label" "$bin" "$root" "$round"
    done
  done
done

# Per-tree medians, then the median of each round's total.
awk -F '\t' -v runs="$runs" '
function median(list, n,    i, j, t, a) {
  n = split(list, a, " ")
  for (i = 1; i <= n; i++)
    for (j = i + 1; j <= n; j++)
      if (a[j] + 0 < a[i] + 0) { t = a[i]; a[i] = a[j]; a[j] = t }
  return (n % 2) ? a[(n + 1) / 2] : (a[n / 2] + a[n / 2 + 1]) / 2
}
{
  split($3, t, " ")
  key = $1 SUBSEP $2
  wall[key] = wall[key] " " t[1]; user[key] = user[key] " " t[2]; sys[key] = sys[key] " " t[3]
  summary[key] = $4
  n = ++seen[key]
  round = $1 SUBSEP n
  tw[round] += t[1]; tu[round] += t[2]; ts[round] += t[3]
  if (!($1 in label_seen)) { label_seen[$1] = 1; labels[++nl] = $1 }
  if (!($2 in root_seen)) { root_seen[$2] = 1; roots[++nr] = $2 }
}
END {
  printf "%-10s %-10s %8s %8s %8s  %s\n", "binary", "tree", "wall_s", "user_s", "sys_s", "result"
  for (l = 1; l <= nl; l++) {
    for (r = 1; r <= nr; r++) {
      key = labels[l] SUBSEP roots[r]
      printf "%-10s %-10s %8.2f %8.2f %8.2f  %s\n", labels[l], roots[r], median(wall[key]), median(user[key]), median(sys[key]), summary[key]
    }
    w = ""; u = ""; s = ""
    for (i = 1; i <= runs; i++) {
      round = labels[l] SUBSEP i
      w = w " " tw[round]; u = u " " tu[round]; s = s " " ts[round]
    }
    printf "%-10s %-10s %8.2f %8.2f %8.2f  median of %d rounds\n", labels[l], "TOTAL", median(w), median(u), median(s), runs
  }
}' "$samples"
