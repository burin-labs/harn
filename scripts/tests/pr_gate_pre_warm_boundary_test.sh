#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
mkdir -p "$fake_bin"
real_cargo=$(command -v cargo)
cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
for arg in "$@"; do
  case "$arg" in
    build|check|clippy|install|nextest|run|test)
      printf 'cargo %s\n' "$*" >> "${PRE_WARM_CARGO_RECORD:?}"
      echo "pre-warm PR gates must not build or run Rust" >&2
      exit 97
      ;;
  esac
done
exec "${PRE_WARM_REAL_CARGO:?}" "$@"
SH
chmod +x "$fake_bin/cargo"

record="$tmp_root/cargo-calls.txt"
env -u HARN_BIN \
  PRE_WARM_CARGO_RECORD="$record" \
  PRE_WARM_REAL_CARGO="$real_cargo" \
  PATH="$fake_bin:$PATH" \
  make -C "$repo_root" test-pr-gate-scripts

if [[ -e "$record" ]]; then
  echo "pre-warm PR gates invoked Cargo:" >&2
  cat "$record" >&2
  exit 1
fi

echo "pr_gate_pre_warm_boundary_test: ok"
