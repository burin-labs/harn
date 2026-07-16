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

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == "fmt --all -- --check" ]]; then
  exit 0
fi
printf '%s %s\n' "$(basename "$0")" "$*" >> "$UNEXPECTED_COMMAND_RECORD"
exit 91
SH
chmod +x "$fake_bin/cargo"

cat > "$fake_bin/harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s %s\n' "$(basename "$0")" "$*" >> "$UNEXPECTED_COMMAND_RECORD"
exit 91
SH
chmod +x "$fake_bin/harn"

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
  "diff --name-only --no-renames --diff-filter=ACMRD base...HEAD")
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
  "check-ref-format refs/heads/obsolete"|"check-ref-format refs/tags/old")
    ;;
  "check-ref-format "*)
    exit 1
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

zero_oid=0000000000000000000000000000000000000000
delete_update="(delete) $zero_oid refs/heads/obsolete deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
multiple_delete_updates="$delete_update
(delete) $zero_oid refs/tags/old feedfacefeedfacefeedfacefeedfacefeedface"
mixed_updates="$delete_update
refs/heads/current cafebabecafebabecafebabecafebabecafebabe refs/heads/current deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
malformed_update="refs/heads/obsolete $zero_oid refs/heads/obsolete"
wrong_local_ref_update="refs/heads/obsolete $zero_oid refs/heads/obsolete deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
short_oid_update="(delete) 0000 refs/heads/obsolete deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
invalid_remote_oid_update="(delete) $zero_oid refs/heads/obsolete not-an-object-id"
invalid_remote_ref_update="(delete) $zero_oid refs/heads/bad..name deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"

run_prepush() {
  input=$1
  output=$2
  : > "$record"
  (
    cd "$work"
    printf '%s' "$input" | \
      UNEXPECTED_COMMAND_RECORD="$record" \
      PATH="$prepush_fake_bin:$PATH" \
      ./.githooks/pre-push origin git@example.com:burin-labs/harn.git
  ) > "$output"
}

run_prepush "$delete_update" "$tmp_root/pre-push-delete.out"
if [[ -s "$record" ]]; then
  echo "deletion-only pre-push invoked a validation command" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq "deletion-only ref update" "$tmp_root/pre-push-delete.out"; then
  echo "deletion-only pre-push did not report its early exit" >&2
  cat "$tmp_root/pre-push-delete.out" >&2
  exit 1
fi

run_prepush "$multiple_delete_updates" "$tmp_root/pre-push-multiple-delete.out"
if [[ -s "$record" ]]; then
  echo "multiple deletion-only pre-push invoked a validation command" >&2
  cat "$record" >&2
  exit 1
fi
if ! grep -Fq "deletion-only ref update" "$tmp_root/pre-push-multiple-delete.out"; then
  echo "multiple deletion-only pre-push did not report its early exit" >&2
  cat "$tmp_root/pre-push-multiple-delete.out" >&2
  exit 1
fi

(
  cd "$work"
  : > "$record"
  if ! UNEXPECTED_COMMAND_RECORD="$record" \
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

run_prepush "$mixed_updates" "$tmp_root/pre-push-mixed.out"
if [[ -s "$record" ]]; then
  echo "mixed pre-push unexpectedly invoked a build-capable command" >&2
  cat "$record" >&2
  exit 1
fi
if grep -Fq "deletion-only ref update" "$tmp_root/pre-push-mixed.out"; then
  echo "mixed pre-push was incorrectly classified as deletion-only" >&2
  cat "$tmp_root/pre-push-mixed.out" >&2
  exit 1
fi
if ! grep -Fq "skipping expensive local checks" "$tmp_root/pre-push-mixed.out"; then
  echo "mixed pre-push did not follow the normal validation path" >&2
  cat "$tmp_root/pre-push-mixed.out" >&2
  exit 1
fi

run_prepush "$malformed_update" "$tmp_root/pre-push-malformed.out"
if [[ -s "$record" ]]; then
  echo "malformed pre-push unexpectedly invoked a build-capable command" >&2
  cat "$record" >&2
  exit 1
fi
if grep -Fq "deletion-only ref update" "$tmp_root/pre-push-malformed.out"; then
  echo "malformed pre-push was incorrectly classified as deletion-only" >&2
  cat "$tmp_root/pre-push-malformed.out" >&2
  exit 1
fi
if ! grep -Fq "skipping expensive local checks" "$tmp_root/pre-push-malformed.out"; then
  echo "malformed pre-push did not follow the normal validation path" >&2
  cat "$tmp_root/pre-push-malformed.out" >&2
  exit 1
fi

for malformed_case in wrong_local_ref short_oid invalid_remote_oid invalid_remote_ref; do
  case "$malformed_case" in
    wrong_local_ref) input=$wrong_local_ref_update ;;
    short_oid) input=$short_oid_update ;;
    invalid_remote_oid) input=$invalid_remote_oid_update ;;
    invalid_remote_ref) input=$invalid_remote_ref_update ;;
  esac
  output="$tmp_root/pre-push-${malformed_case}.out"
  run_prepush "$input" "$output"
  if [[ -s "$record" ]]; then
    echo "$malformed_case pre-push unexpectedly invoked a build-capable command" >&2
    cat "$record" >&2
    exit 1
  fi
  if grep -Fq "deletion-only ref update" "$output"; then
    echo "$malformed_case pre-push was incorrectly classified as deletion-only" >&2
    cat "$output" >&2
    exit 1
  fi
  if ! grep -Fq "skipping expensive local checks" "$output"; then
    echo "$malformed_case pre-push did not follow the normal validation path" >&2
    cat "$output" >&2
    exit 1
  fi
done

git -C "$work" commit --quiet --no-verify -m initial

(
  cd "$work"
  # shellcheck source=/dev/null
  . ./.githooks/lib.sh
  hook_fast_default_mode
  if HARN_HOOKS_FULL_LOCAL=1 hook_fast_default_mode; then
    echo "hook_fast_default_mode should be false after explicitly opting into full local validation" >&2
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

echo "hook_fast_default_mode_test: ok"
