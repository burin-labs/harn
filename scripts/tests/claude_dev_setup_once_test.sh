#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
: "${HARN_BIN:?run this integration through make test-pr-gate-post-warm-integrations}"
if [[ ! -x "$HARN_BIN" ]]; then
  echo "HARN_BIN is not executable: $HARN_BIN" >&2
  exit 1
fi

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

real_harn_bin="$HARN_BIN"

fake_repo="$tmp_root/repo with spaces"
mkdir -p "$fake_repo/scripts"
git -C "$fake_repo" init --quiet
fake_repo="$(cd "$fake_repo" && pwd -P)"

cat > "$fake_repo/scripts/dev_setup.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'pwd=%s\n' "$PWD" > "$CLAUDE_DEV_SETUP_TEST_RECORD"
printf 'target=%s\n' "${HARN_DEV_TARGET_WORKTREE_PATH-}" >> "$CLAUDE_DEV_SETUP_TEST_RECORD"
exit "${CLAUDE_DEV_SETUP_TEST_STATUS:-0}"
SH
chmod +x "$fake_repo/scripts/dev_setup.sh"

record="$tmp_root/setup-record.txt"
output="$tmp_root/hook-output.json"

CLAUDE_DEV_SETUP_TEST_RECORD="$record" \
  HARN_BIN="$real_harn_bin" \
  "$repo_root/scripts/claude-dev-setup-once.sh" > "$output" <<JSON
{"cwd":"$fake_repo"}
JSON

if ! grep -Fxq "pwd=$fake_repo" "$record"; then
  echo "claude-dev-setup hook did not use the cwd parsed by Harn" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fxq "target=$fake_repo" "$record"; then
  echo "claude-dev-setup hook did not persist the selected worktree path" >&2
  cat "$record" >&2
  exit 1
fi

parsed_message="$("$real_harn_bin" run -e 'const parsed = json_parse(read_stdin() ?? ""); __io_println(parsed?.hookSpecificOutput?.additionalContext ?? "")' < "$output")"
if [[ "$parsed_message" != "Project dev setup completed in "* ]]; then
  echo "claude-dev-setup hook did not emit a SessionStart context JSON object" >&2
  cat "$output" >&2
  exit 1
fi

rm -f "$fake_repo/.claude/dev-setup/"*.stamp
: > "$record"
CLAUDE_PROJECT_DIR="$fake_repo" \
  CLAUDE_DEV_SETUP_TEST_RECORD="$record" \
  HARN_BIN="$real_harn_bin" \
  "$repo_root/scripts/claude-dev-setup-once.sh" >/dev/null <<'JSON'
not-json
JSON

if ! grep -Fxq "pwd=$fake_repo" "$record"; then
  echo "claude-dev-setup hook did not fall back to CLAUDE_PROJECT_DIR for malformed input" >&2
  cat "$record" >&2
  exit 1
fi

echo "claude_dev_setup_once_test: ok"
