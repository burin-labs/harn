#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
record="$tmp_root/unexpected-command-record.txt"
work="$tmp_root/work"
mkdir -p "$fake_bin" "$work/.githooks" "$work/crates/harn-vm/src" \
  "$work/crates/harn-lexer/src" "$work/crates/harn-stdlib/src/stdlib/agent" \
  "$work/conformance/tests" "$work/scripts"

for name in cargo harn; do
  cat > "$fake_bin/$name" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s %s\n' "$(basename "$0")" "$*" >> "$UNEXPECTED_COMMAND_RECORD"
exit 91
SH
  chmod +x "$fake_bin/$name"
done

cat > "$fake_bin/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  lint-actions|lint-md)
    exit 0
    ;;
esac
printf 'make %s\n' "$*" >> "$UNEXPECTED_COMMAND_RECORD"
exit 91
SH
chmod +x "$fake_bin/make"

cp "$repo_root/.githooks/lib.sh" "$work/.githooks/lib.sh"
cp "$repo_root/.githooks/pre-commit" "$work/.githooks/pre-commit"
cp "$repo_root/.githooks/pre-push" "$work/.githooks/pre-push"
chmod +x "$work/.githooks/pre-commit"
chmod +x "$work/.githooks/pre-push"

git -C "$work" init --quiet
git -C "$work" config user.email "test@example.com"
git -C "$work" config user.name "Test User"
git -C "$work" config commit.gpgsign false

printf '%s\n' 'pub fn touched() {}' > "$work/crates/harn-vm/src/lib.rs"
printf '%s\n' 'pub const KEYWORDS: &[&str] = &["let"];' > "$work/crates/harn-lexer/src/token.rs"
printf '%s\n' 'pub fn schema_closed_object() {}' > "$work/crates/harn-stdlib/src/stdlib/stdlib_schema.harn"
printf '%s\n' '// @harn-entrypoint-category agent.stdlib' 'pub fn agent_loop() {}' > "$work/crates/harn-stdlib/src/stdlib/agent/loop.harn"
printf '%s\n' 'fn main(harness: Harness) {}' > "$work/conformance/tests/demo.harn"
printf '%s\n' '[artifacts]' > "$work/scripts/generated_artifacts.toml"
git -C "$work" add .

(
  cd "$work"
  HARN_HOOKS_NO_LOCAL_BUILD=1 \
    UNEXPECTED_COMMAND_RECORD="$record" \
    PATH="$fake_bin:$PATH" \
    ./.githooks/pre-commit > "$tmp_root/pre-commit.out"
)

if [[ -s "$record" ]]; then
  echo "no-local-build mode still invoked a build-capable command" >&2
  cat "$record" >&2
  exit 1
fi

if ! grep -Fq "skipping pre-commit CI ratchets" "$tmp_root/pre-commit.out"; then
  echo "pre-commit did not report skipping Harn-backed ratchets" >&2
  cat "$tmp_root/pre-commit.out" >&2
  exit 1
fi

if ! grep -Fq "skipping pre-commit local build/format/lint phases" "$tmp_root/pre-commit.out"; then
  echo "pre-commit did not exit before local build/format/lint phases" >&2
  cat "$tmp_root/pre-commit.out" >&2
  exit 1
fi

prepush_fake_bin="$tmp_root/prepush-bin"
mkdir -p "$prepush_fake_bin"
cp "$fake_bin/cargo" "$prepush_fake_bin/cargo"
cp "$fake_bin/harn" "$prepush_fake_bin/harn"
cp "$fake_bin/make" "$prepush_fake_bin/make"

cat > "$prepush_fake_bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  "rev-parse --abbrev-ref --symbolic-full-name @{upstream}")
    exit 1
    ;;
  "rev-parse --verify origin/main")
    printf '%s\n' base
    ;;
  "merge-base HEAD origin/main")
    printf '%s\n' base
    ;;
  "diff --name-only --diff-filter=ACMR base...HEAD")
    printf '%s\n' \
      ".githooks/pre-push" \
      "CHANGELOG.md" \
      "crates/harn-lexer/src/token.rs" \
      "crates/harn-vm/src/lib.rs" \
      "scripts/generated_artifacts.toml"
    ;;
  "rev-list base..HEAD")
    ;;
  "rev-parse --abbrev-ref HEAD")
    printf '%s\n' codex2/hooks-no-local-build-test
    ;;
  "rev-parse --show-toplevel")
    pwd
    ;;
  "rev-parse HEAD")
    printf '%s\n' deadbeef
    ;;
  *)
    printf 'git %s\n' "$*" >> "$UNEXPECTED_COMMAND_RECORD"
    exit 91
    ;;
esac
SH
chmod +x "$prepush_fake_bin/git"

cat > "$prepush_fake_bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
exit 0
SH
chmod +x "$prepush_fake_bin/gh"

(
  cd "$work"
  : > "$record"
  if ! HARN_HOOKS_NO_LOCAL_BUILD=1 \
    UNEXPECTED_COMMAND_RECORD="$record" \
    PATH="$prepush_fake_bin:$PATH" \
    ./.githooks/pre-push origin git@example.com:burin-labs/harn.git > "$tmp_root/pre-push.out"; then
    echo "pre-push no-local-build simulation failed" >&2
    cat "$tmp_root/pre-push.out" >&2 || true
    cat "$record" >&2 || true
    exit 1
  fi
)

if [[ -s "$record" ]]; then
  echo "pre-push no-local-build mode still invoked a build-capable command" >&2
  cat "$record" >&2
  exit 1
fi

if ! grep -Fq "skipping pre-push tree-sitter keyword mirror check" "$tmp_root/pre-push.out"; then
  echo "pre-push did not report skipping tree-sitter keyword mirror check" >&2
  cat "$tmp_root/pre-push.out" >&2
  exit 1
fi

if ! grep -Fq "skipping pre-push CHANGELOG retroactive-edit check" "$tmp_root/pre-push.out"; then
  echo "pre-push did not report skipping CHANGELOG Harn-backed check" >&2
  cat "$tmp_root/pre-push.out" >&2
  exit 1
fi

if ! grep -Fq "skipping expensive local checks" "$tmp_root/pre-push.out"; then
  echo "pre-push did not exit before expensive local checks" >&2
  cat "$tmp_root/pre-push.out" >&2
  exit 1
fi

git -C "$work" commit --quiet --no-verify -m initial

(
  cd "$work"
  # shellcheck source=/dev/null
  . ./.githooks/lib.sh
  HARN_HOOKS_NO_LOCAL_BUILD=1 hook_no_local_build_mode
  HARN_HOOKS_NO_LOCAL_BUILD=0 HARN_HOOKS_FAST_ONLY=1 hook_no_local_build_mode
  if HARN_HOOKS_NO_LOCAL_BUILD=0 HARN_HOOKS_FAST_ONLY=0 hook_no_local_build_mode; then
    echo "hook_no_local_build_mode should be false when both env vars are unset/0" >&2
    exit 1
  fi

  highlight_changed="$tmp_root/highlight-changed.txt"

  printf '%s\n' 'crates/harn-stdlib/src/stdlib/stdlib_schema.harn' > "$highlight_changed"
  if hook_paths_need_highlight "$highlight_changed" --cached; then
    echo "ordinary imported stdlib module should not require highlight regeneration" >&2
    exit 1
  fi

  printf '%s\n' 'crates/harn-stdlib/src/stdlib/agent/loop.harn' > "$highlight_changed"
  if ! hook_paths_need_highlight "$highlight_changed" --cached; then
    echo "entrypoint stdlib module should require highlight regeneration" >&2
    exit 1
  fi

  printf '%s\n' 'crates/harn-lexer/src/token.rs' > "$highlight_changed"
  if ! hook_paths_need_highlight "$highlight_changed" --cached; then
    echo "lexer keyword change should require highlight regeneration" >&2
    exit 1
  fi

  printf '%s\n' 'pub fn agent_loop() {}' > crates/harn-stdlib/src/stdlib/agent/loop.harn
  git add crates/harn-stdlib/src/stdlib/agent/loop.harn
  printf '%s\n' 'crates/harn-stdlib/src/stdlib/agent/loop.harn' > "$highlight_changed"
  if ! hook_paths_need_highlight "$highlight_changed" --cached; then
    echo "removing an entrypoint marker should require highlight regeneration" >&2
    exit 1
  fi
)

echo "hook_no_local_build_mode_test: ok"
