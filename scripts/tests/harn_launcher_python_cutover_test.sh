#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_harn="$tmp_root/harn"
record="$tmp_root/harn-record.tsv"

cat > "$fake_harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\t%s\t%s\n' "${HARN_DEMO_REMOTE_PORT-}" "$#" "$*" >> "$HARN_LAUNCHER_RECORD"

case "$1" in
  serve)
    printf 'Harn A2A server listening on http://127.0.0.1:38765\n'
    while :; do
      sleep 1
    done
    ;;
  run)
    exit 0
    ;;
  *)
    echo "unexpected fake harn invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_harn"

HARN_BIN="$fake_harn" \
  HARN_LAUNCHER_RECORD="$record" \
  "$repo_root/scripts/check_no_rust_prompt_prose.sh"

if ! grep -Fq "run scripts/check_rust_prompt_prose.harn" "$record"; then
  echo "check_no_rust_prompt_prose did not delegate to the Harn prompt-prose check" >&2
  cat "$record" >&2
  exit 1
fi

: > "$record"
HARN_BIN="$fake_harn" \
  HARN_LAUNCHER_RECORD="$record" \
  "$repo_root/scripts/demo_local_a2a_dispatch.sh"

if ! grep -Fq $'38765\t2\trun ' "$record"; then
  echo "demo_local_a2a_dispatch did not pass the discovered A2A port to the Harn run" >&2
  cat "$record" >&2
  exit 1
fi

echo "harn_launcher_python_cutover_test: ok"
