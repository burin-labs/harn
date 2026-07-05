#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

release_root="$tmp_root/release-root"
mkdir -p "$release_root"
cat > "$release_root/Cargo.toml" <<'EOF'
[workspace]
version = "1.2.3"
members = []
EOF

fake_harn_dir="$tmp_root/fake bin"
mkdir -p "$fake_harn_dir"
fake_harn="$fake_harn_dir/fake harn"
cat > "$fake_harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_HARN_RECORD"
if [[ -n "${FAKE_HARN_ENV_RECORD:-}" ]]; then
  {
    printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR-__unset__}"
    printf 'CARGO_BUILD_BUILD_DIR=%s\n' "${CARGO_BUILD_BUILD_DIR-__unset__}"
  } >> "$FAKE_HARN_ENV_RECORD"
fi
if [[ "${1:-}" == "run" && "${2:-}" == "scripts/render_release_notes.harn" ]]; then
  printf 'fake release notes\n'
  exit 0
fi
echo "unexpected fake harn invocation: $*" >&2
exit 2
SH
chmod +x "$fake_harn"

record="$tmp_root/harn-record.txt"
env_record="$tmp_root/harn-env-record.txt"
HARN_RELEASE_ROOT="$release_root" \
  HARN_BIN="$fake_harn" \
  FAKE_HARN_RECORD="$record" \
  FAKE_HARN_ENV_RECORD="$env_record" \
  "$repo_root/scripts/release_gate.sh" notes --version v1.2.3 > "$tmp_root/notes.txt"

expected="run scripts/render_release_notes.harn -- --version v1.2.3"
actual=$(cat "$record")
if [[ "$actual" != "$expected" ]]; then
  printf 'expected release_gate to use HARN_BIN:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
  exit 1
fi

default_tmp="${TMPDIR:-/tmp}"
default_tmp="${default_tmp%/}"
expected_target="$default_tmp/harn-release-gate-target-release-root"
expected_build="$expected_target/build"
if ! grep -Fxq "CARGO_TARGET_DIR=$expected_target" "$env_record"; then
  echo "release_gate did not default CARGO_TARGET_DIR to a release-local path" >&2
  cat "$env_record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$expected_build" "$env_record"; then
  echo "release_gate did not default CARGO_BUILD_BUILD_DIR under the target dir" >&2
  cat "$env_record" >&2
  exit 1
fi

: >"$record"
: >"$env_record"
custom_target="$tmp_root/custom target"
custom_build="$tmp_root/custom build"
HARN_RELEASE_ROOT="$release_root" \
  HARN_BIN="$fake_harn" \
  CARGO_TARGET_DIR="$custom_target" \
  CARGO_BUILD_BUILD_DIR="$custom_build" \
  FAKE_HARN_RECORD="$record" \
  FAKE_HARN_ENV_RECORD="$env_record" \
  "$repo_root/scripts/release_gate.sh" notes --version v1.2.3 > "$tmp_root/notes-custom.txt"

if ! grep -Fxq "CARGO_TARGET_DIR=$custom_target" "$env_record"; then
  echo "release_gate did not preserve explicit CARGO_TARGET_DIR" >&2
  cat "$env_record" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_BUILD_DIR=$custom_build" "$env_record"; then
  echo "release_gate did not preserve explicit CARGO_BUILD_BUILD_DIR" >&2
  cat "$env_record" >&2
  exit 1
fi

make -n -C "$repo_root" \
  conformance \
  protocol-conformance \
  test-harn-scripts \
  check-language-spec \
  check-highlight \
  HARN_BIN="$fake_harn" > "$tmp_root/make-dry-run.txt"

if ! grep -Fq "\"$fake_harn\" test conformance" "$tmp_root/make-dry-run.txt"; then
  echo "make conformance did not route through HARN_BIN" >&2
  exit 1
fi

if ! grep -Fq "\"$fake_harn\" dump-highlight-keywords --check" "$tmp_root/make-dry-run.txt"; then
  echo "make check-highlight did not route Harn CLI commands through HARN_BIN" >&2
  exit 1
fi

if grep -q "cargo run .*harn" "$tmp_root/make-dry-run.txt"; then
  echo "HARN_BIN dry-run unexpectedly fell back to cargo run:" >&2
  grep "cargo run .*harn" "$tmp_root/make-dry-run.txt" >&2
  exit 1
fi

"$repo_root/scripts/tests/hook_harn_build_env_test.sh"

echo "release_gate_harn_bin_test: ok"
