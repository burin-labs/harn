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
  lint-test-patterns \
  check-bindings \
  check-language-spec \
  check-highlight \
  lint-diagnostic-codes \
  check-receipt-structs \
  check-docs-links \
  HARN_BIN="$fake_harn" > "$tmp_root/make-dry-run.txt"

if ! grep -Fq "\"$fake_harn\" test conformance" "$tmp_root/make-dry-run.txt"; then
  echo "make conformance did not route through HARN_BIN" >&2
  exit 1
fi

if ! grep -Fq "\"$fake_harn\" dump-highlight-keywords --check" "$tmp_root/make-dry-run.txt"; then
  echo "make check-highlight did not route Harn CLI commands through HARN_BIN" >&2
  exit 1
fi

for expected in \
  "\"$fake_harn\" run scripts/lint_test_patterns.harn" \
  "\"$fake_harn\" run scripts/check_protocol_bindings.harn" \
  "\"$fake_harn\" run scripts/check_diagnostic_codes.harn" \
  "\"$fake_harn\" run scripts/check_receipt_struct_duplication.harn" \
  "\"$fake_harn\" run scripts/check_docs_links.harn"
do
  if ! grep -Fq "$expected" "$tmp_root/make-dry-run.txt"; then
    echo "make dry-run did not route through HARN_BIN: $expected" >&2
    exit 1
  fi
done

if grep -q "cargo run .*harn" "$tmp_root/make-dry-run.txt"; then
  echo "HARN_BIN dry-run unexpectedly fell back to cargo run:" >&2
  grep "cargo run .*harn" "$tmp_root/make-dry-run.txt" >&2
  exit 1
fi

"$repo_root/scripts/tests/hook_harn_build_env_test.sh"

audit_root="$tmp_root/audit-root"
mkdir -p \
  "$audit_root/scripts" \
  "$audit_root/docs/src" \
  "$audit_root/crates/harn-vm" \
  "$audit_root/crates/harn-cli" \
  "$audit_root/.github" \
  "$audit_root/tree-sitter-harn" \
  "$audit_root/spec"
cat > "$audit_root/Cargo.toml" <<'EOF'
[workspace]
version = "1.2.3"
members = []
EOF
printf 'OAuth MCP trust boundary mutation session worker_update\n' > "$audit_root/README.md"
printf 'spec\n' > "$audit_root/spec/HARN_SPEC.md"
cat > "$audit_root/scripts/verify_crate_packages.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'package-audit HARN_BIN=%s\n' "${HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
if [[ "${HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "package audit received cargo target HARN_BIN" >&2
  exit 1
fi
SH
chmod +x "$audit_root/scripts/verify_crate_packages.sh"
cat > "$audit_root/scripts/build_docs_site.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'docs-build HARN_BIN=%s\n' "${HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
if [[ "${HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "docs build received cargo target HARN_BIN" >&2
  exit 1
fi
SH
chmod +x "$audit_root/scripts/build_docs_site.sh"

fake_tools="$tmp_root/fake-tools"
mkdir -p "$fake_tools"
cat > "$fake_tools/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "build" ]]; then
  mkdir -p "$CARGO_TARGET_DIR/debug"
  cat > "$CARGO_TARGET_DIR/debug/harn" <<'HARN'
#!/usr/bin/env bash
set -euo pipefail
printf 'harn argv=%s HARN_BIN=%s self=%s\n' "$*" "${HARN_BIN-__unset__}" "$0" >> "$FAKE_AUDIT_RECORD"
if [[ "${HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "harn command received cargo target HARN_BIN" >&2
  exit 1
fi
exit 0
HARN
  chmod +x "$CARGO_TARGET_DIR/debug/harn"
  exit 0
fi
if [[ "${1:-}" == "clippy" ]]; then
  printf 'cargo %s HARN_BIN=%s\n' "$*" "${HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
  exit 0
fi
echo "unexpected fake cargo invocation: $*" >&2
exit 2
SH
chmod +x "$fake_tools/cargo"

cat > "$fake_tools/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'make %s HARN_BIN=%s\n' "$*" "${HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
if [[ "${HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "make received cargo target HARN_BIN" >&2
  exit 1
fi
if [[ -n "${CARGO_TARGET_DIR-}" ]]; then
  rm -f "$CARGO_TARGET_DIR/debug/harn"
fi
exit 0
SH
chmod +x "$fake_tools/make"

cat > "$fake_tools/npx" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'npx %s HARN_BIN=%s\n' "$*" "${HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
exit 0
SH
chmod +x "$fake_tools/npx"

cat > "$fake_tools/npm" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'npm %s HARN_BIN=%s\n' "$*" "${HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
exit 0
SH
chmod +x "$fake_tools/npm"

cat > "$fake_tools/rg" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'rg %s HARN_BIN=%s\n' "$*" "${HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
exit 0
SH
chmod +x "$fake_tools/rg"

audit_record="$tmp_root/audit-record.txt"
PATH="$fake_tools:$PATH" \
  HARN_RELEASE_ROOT="$audit_root" \
  TMPDIR="$tmp_root" \
  FAKE_AUDIT_RECORD="$audit_record" \
  "$repo_root/scripts/release_gate.sh" audit > "$tmp_root/audit.txt" 2>&1 || {
    cat "$tmp_root/audit.txt" >&2
    exit 1
  }

if grep -Fq "HARN_BIN=$tmp_root/harn-release-gate-target-audit-root/debug/harn" "$audit_record"; then
  echo "release_gate audit exposed the cargo target harn binary to an audit lane" >&2
  cat "$audit_record" >&2
  exit 1
fi
if ! grep -Fq "/harn-bin/harn" "$audit_record"; then
  echo "release_gate audit did not use the stable harn-bin copy" >&2
  cat "$audit_record" >&2
  exit 1
fi

echo "release_gate_harn_bin_test: ok"
