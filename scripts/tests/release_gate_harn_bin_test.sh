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

if ! grep -Fq "env HARN_BIN=\"$fake_harn\" ./scripts/harn_bin.sh -- test conformance" "$tmp_root/make-dry-run.txt"; then
  echo "make conformance did not route through HARN_BIN" >&2
  cat "$tmp_root/make-dry-run.txt" >&2
  exit 1
fi

if ! grep -Fq "env HARN_BIN=\"$fake_harn\" ./scripts/harn_bin.sh -- dump-highlight-keywords --check" "$tmp_root/make-dry-run.txt"; then
  echo "make check-highlight did not route Harn CLI commands through HARN_BIN" >&2
  cat "$tmp_root/make-dry-run.txt" >&2
  exit 1
fi

for expected in \
  "env HARN_BIN=\"$fake_harn\" ./scripts/harn_bin.sh -- run scripts/lint_test_patterns.harn" \
  "env HARN_BIN=\"$fake_harn\" ./scripts/harn_bin.sh -- run scripts/check_protocol_bindings.harn" \
  "env HARN_BIN=\"$fake_harn\" ./scripts/harn_bin.sh -- run scripts/check_diagnostic_codes.harn" \
  "env HARN_BIN=\"$fake_harn\" ./scripts/harn_bin.sh -- run scripts/check_receipt_struct_duplication.harn" \
  "env HARN_BIN=\"$fake_harn\" ./scripts/harn_bin.sh -- run scripts/check_docs_links.harn"
do
  if ! grep -Fq "$expected" "$tmp_root/make-dry-run.txt"; then
    echo "make dry-run did not route through HARN_BIN: $expected" >&2
    cat "$tmp_root/make-dry-run.txt" >&2
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
  "$audit_root/scripts/ci" \
  "$audit_root/docs/src" \
  "$audit_root/crates/harn-vm" \
  "$audit_root/crates/harn-cli" \
  "$audit_root/.github/workflows" \
  "$audit_root/tree-sitter-harn" \
  "$audit_root/spec"
cat > "$audit_root/Cargo.toml" <<'EOF'
[workspace]
version = "1.2.3"
members = []
EOF
printf 'OAuth MCP trust boundary mutation session worker_update\n' > "$audit_root/README.md"
printf 'spec\n' > "$audit_root/spec/HARN_SPEC.md"
printf '{}\n' > "$audit_root/scripts/release_audit_contract.json"
printf 'jobs: {}\n' > "$audit_root/.github/workflows/ci.yml"
git -C "$audit_root" init -q
git -C "$audit_root" config user.email test@example.com
git -C "$audit_root" config user.name test
git -C "$audit_root" add .
git -C "$audit_root" commit -qm init
cat > "$audit_root/scripts/verify_crate_packages.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'package-audit HARN_BIN=%s HARN_CONFORMANCE_HARN_BIN=%s\n' "${HARN_BIN-__unset__}" "${HARN_CONFORMANCE_HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
if [[ "${HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "package audit received cargo target HARN_BIN" >&2
  exit 1
fi
if [[ "${HARN_CONFORMANCE_HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "package audit received cargo target HARN_CONFORMANCE_HARN_BIN" >&2
  exit 1
fi
SH
chmod +x "$audit_root/scripts/verify_crate_packages.sh"
cat > "$audit_root/scripts/build_docs_site.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'docs-build HARN_BIN=%s HARN_CONFORMANCE_HARN_BIN=%s\n' "${HARN_BIN-__unset__}" "${HARN_CONFORMANCE_HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
if [[ "${HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "docs build received cargo target HARN_BIN" >&2
  exit 1
fi
if [[ "${HARN_CONFORMANCE_HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "docs build received cargo target HARN_CONFORMANCE_HARN_BIN" >&2
  exit 1
fi
SH
chmod +x "$audit_root/scripts/build_docs_site.sh"
cat > "$audit_root/scripts/ci/run_rust_lint_lane.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
cargo clippy --workspace --all-targets -- -D warnings
SH
chmod +x "$audit_root/scripts/ci/run_rust_lint_lane.sh"

fake_tools="$tmp_root/fake-tools"
mkdir -p "$fake_tools"
cat > "$fake_tools/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo %s HARN_BIN=%s HARN_CONFORMANCE_HARN_BIN=%s\n' "$*" "${HARN_BIN-__unset__}" "${HARN_CONFORMANCE_HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
if [[ "${1:-}" == "build" ]]; then
  mkdir -p "$CARGO_TARGET_DIR/debug"
  cat > "$CARGO_TARGET_DIR/debug/harn" <<'HARN'
#!/usr/bin/env bash
set -euo pipefail
printf 'harn argv=%s HARN_BIN=%s HARN_CONFORMANCE_HARN_BIN=%s self=%s\n' "$*" "${HARN_BIN-__unset__}" "${HARN_CONFORMANCE_HARN_BIN-__unset__}" "$0" >> "$FAKE_AUDIT_RECORD"
if [[ "${HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "harn command received cargo target HARN_BIN" >&2
  exit 1
fi
if [[ "${HARN_CONFORMANCE_HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "harn command received cargo target HARN_CONFORMANCE_HARN_BIN" >&2
  exit 1
fi
exit 0
HARN
  chmod +x "$CARGO_TARGET_DIR/debug/harn"
  exit 0
fi
if [[ "${1:-}" == "clippy" ]]; then
  exit 0
fi
echo "unexpected fake cargo invocation: $*" >&2
exit 2
SH
chmod +x "$fake_tools/cargo"

cat > "$fake_tools/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'make %s HARN_BIN=%s HARN_CONFORMANCE_HARN_BIN=%s\n' "$*" "${HARN_BIN-__unset__}" "${HARN_CONFORMANCE_HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
if [[ -n "${FAIL_FAKE_MAKE_TARGET:-}" && "$*" == "$FAIL_FAKE_MAKE_TARGET" ]]; then
  echo "injected make failure: $*" >&2
  exit 17
fi
if [[ "${HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "make received cargo target HARN_BIN" >&2
  exit 1
fi
if [[ "${HARN_CONFORMANCE_HARN_BIN-}" == "$CARGO_TARGET_DIR/debug/harn" ]]; then
  echo "make received cargo target HARN_CONFORMANCE_HARN_BIN" >&2
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
printf 'npx %s HARN_BIN=%s HARN_CONFORMANCE_HARN_BIN=%s\n' "$*" "${HARN_BIN-__unset__}" "${HARN_CONFORMANCE_HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
if [[ "${FAIL_FAKE_NPX:-0}" == "1" ]]; then
  echo "injected npx failure" >&2
  exit 18
fi
exit 0
SH
chmod +x "$fake_tools/npx"

cat > "$fake_tools/npm" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'npm %s HARN_BIN=%s HARN_CONFORMANCE_HARN_BIN=%s\n' "$*" "${HARN_BIN-__unset__}" "${HARN_CONFORMANCE_HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
exit 0
SH
chmod +x "$fake_tools/npm"

cat > "$fake_tools/rg" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'rg %s HARN_BIN=%s HARN_CONFORMANCE_HARN_BIN=%s\n' "$*" "${HARN_BIN-__unset__}" "${HARN_CONFORMANCE_HARN_BIN-__unset__}" >> "$FAKE_AUDIT_RECORD"
if [[ "${FAIL_FAKE_RG:-0}" == "1" ]]; then
  echo "injected rg failure" >&2
  exit 19
fi
exit 0
SH
chmod +x "$fake_tools/rg"

fake_audit_harn="$fake_tools/harn-plan"
cat > "$fake_audit_harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'harn argv=%s HARN_BIN=%s HARN_CONFORMANCE_HARN_BIN=%s self=%s\n' "$*" "${HARN_BIN-__unset__}" "${HARN_CONFORMANCE_HARN_BIN-__unset__}" "$0" >> "$FAKE_AUDIT_RECORD"
if [[ "${1:-}" == "run" && "${2:-}" == "scripts/release_audit_contract.harn" ]]; then
  if [[ " $* " == *" --receipt "* && "$*" != *"receipt-invalid.json"* ]]; then
    printf '%s\n' '{"ok":true,"receipt_reused":true,"reason":"receipt_accepted","proof_kind":"merge_group","head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","lane_names":["generated-audit","docs-audit","grammar-audit","security-audit","smoke-audit"],"lane_runners":["run_generated_audit","run_docs_audit","run_grammar_audit","run_security_audit","run_smoke_audit"],"lanes":[],"errors":[]}'
  else
    printf '%s\n' '{"ok":true,"receipt_reused":false,"reason":"no_receipt","proof_kind":"full_local","head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","lane_names":["rust-audit","harn-audit","generated-audit","docs-audit","grammar-audit","security-audit","package-audit","smoke-audit"],"lane_runners":["run_rust_audit","run_harn_audit","run_generated_audit","run_docs_audit","run_grammar_audit","run_security_audit","run_package_audit","run_smoke_audit"],"lanes":[],"errors":[]}'
  fi
  exit 0
fi
exit 0
SH
chmod +x "$fake_audit_harn"

audit_record="$tmp_root/audit-record.txt"
run_audit() {
  local label="$1"
  shift
  : > "$audit_record"
  PATH="$fake_tools:$PATH" \
    HARN_RELEASE_ROOT="$audit_root" \
    HARN_BIN="$fake_audit_harn" \
    TMPDIR="$tmp_root" \
    FAKE_AUDIT_RECORD="$audit_record" \
    "$repo_root/scripts/release_gate.sh" audit "$@" > "$tmp_root/audit-$label.txt" 2>&1 || {
    cat "$tmp_root/audit-$label.txt" >&2
    exit 1
  }
}

run_audit full

if grep -Fq "HARN_BIN=$tmp_root/harn-release-gate-target-audit-root/debug/harn" "$audit_record"; then
  echo "release_gate audit exposed the cargo target harn binary to an audit lane" >&2
  cat "$audit_record" >&2
  exit 1
fi
if grep -Fq "HARN_CONFORMANCE_HARN_BIN=$tmp_root/harn-release-gate-target-audit-root/debug/harn" "$audit_record"; then
  echo "release_gate audit exposed the cargo target conformance harn binary to an audit lane" >&2
  cat "$audit_record" >&2
  exit 1
fi
if ! grep -Fq "/harn-bin/harn" "$audit_record"; then
  echo "release_gate audit did not use the stable harn-bin copy" >&2
  cat "$audit_record" >&2
  exit 1
fi
if ! grep -Eq "HARN_CONFORMANCE_HARN_BIN=.*/harn-bin/harn" "$audit_record"; then
  echo "release_gate audit did not expose the stable harn-bin copy to conformance fixtures" >&2
  cat "$audit_record" >&2
  exit 1
fi

for lane in \
  rust-audit \
  harn-audit \
  generated-audit \
  docs-audit \
  grammar-audit \
  security-audit \
  package-audit \
  smoke-audit
do
  if ! grep -Eq "ok: +$lane " "$tmp_root/audit-full.txt"; then
    echo "full audit profile did not run $lane" >&2
    cat "$tmp_root/audit-full.txt" >&2
    exit 1
  fi
done

for expected in \
  "cargo clippy --workspace --all-targets -- -D warnings" \
  "make conformance" \
  "package-audit HARN_BIN=" \
  "make smoke-audit"
do
  if ! grep -Fq "$expected" "$audit_record"; then
    echo "full audit profile missed expected work: $expected" >&2
    cat "$audit_record" >&2
    exit 1
  fi
done

receipt="$tmp_root/receipt.json"
printf '{}\n' > "$receipt"
run_audit residual --receipt "$receipt"

for lane in generated-audit docs-audit grammar-audit security-audit smoke-audit; do
  if ! grep -Eq "ok: +$lane " "$tmp_root/audit-residual.txt"; then
    echo "receipt-authorized residual audit did not run $lane" >&2
    cat "$tmp_root/audit-residual.txt" >&2
    exit 1
  fi
done
for lane in rust-audit harn-audit package-audit; do
  if grep -Fq "log: $lane" "$tmp_root/audit-residual.txt"; then
    echo "receipt-authorized residual audit unexpectedly ran $lane" >&2
    cat "$tmp_root/audit-residual.txt" >&2
    exit 1
  fi
done

for expected in \
  "make check-language-spec" \
  "make check-highlight" \
  "make check-protocol-artifacts" \
  "make check-connector-schemas" \
  "make check-session-bundle-schema" \
  "make check-run-view-fixtures" \
  "run scripts/verify_release_metadata.harn" \
  "run scripts/verify_tree_sitter_parse.harn -- --strict" \
  "npx markdownlint-cli2 **/*.md" \
  "rg -n OAuth|oauth|MCP|trust boundary" \
  "make smoke-audit"
do
  if ! grep -Fq "$expected" "$audit_record"; then
    echo "receipt-authorized residual audit missed work: $expected" >&2
    cat "$audit_record" >&2
    exit 1
  fi
done
for unexpected in \
  "cargo clippy" \
  "make fmt-check" \
  "make test" \
  "make conformance" \
  "make protocol-conformance" \
  "make lint-harn" \
  "make fmt-harn" \
  "package-audit HARN_BIN="
do
  if grep -Fq "$unexpected" "$audit_record"; then
    echo "receipt-authorized residual audit repeated proved work: $unexpected" >&2
    cat "$audit_record" >&2
    exit 1
  fi
done

assert_residual_prerequisite_fails() {
  local label="$1"
  local expected="$2"
  local tool_path="$3"
  : > "$audit_record"
  if PATH="$tool_path" \
    HARN_RELEASE_ROOT="$audit_root" \
    HARN_BIN="$fake_audit_harn" \
    TMPDIR="$tmp_root" \
    FAKE_AUDIT_RECORD="$audit_record" \
    "$repo_root/scripts/release_gate.sh" audit --receipt "$receipt" \
      > "$tmp_root/audit-$label.txt" 2>&1; then
    echo "residual audit passed without required prerequisite: $label" >&2
    exit 1
  fi
  if ! grep -Fq "$expected" "$tmp_root/audit-$label.txt"; then
    echo "residual audit did not report missing prerequisite: $label" >&2
    cat "$tmp_root/audit-$label.txt" >&2
    exit 1
  fi
  if grep -Eq 'cargo clippy|make (fmt-check|test|conformance)|package-audit HARN_BIN=' "$audit_record"; then
    echo "failed residual prerequisite unexpectedly launched merge-group-proved work: $label" >&2
    cat "$audit_record" >&2
    exit 1
  fi
}

mv "$audit_root/tree-sitter-harn" "$audit_root/tree-sitter-harn.missing"
assert_residual_prerequisite_fails \
  missing-grammar \
  "error: tree-sitter-harn is required for the release grammar audit" \
  "$fake_tools:$PATH"
mv "$audit_root/tree-sitter-harn.missing" "$audit_root/tree-sitter-harn"

fake_tools_without_npm="$tmp_root/fake-tools-without-npm"
mkdir -p "$fake_tools_without_npm"
for tool in cargo make npx rg harn-plan; do
  ln -s "$fake_tools/$tool" "$fake_tools_without_npm/$tool"
done
no_npm_path="$fake_tools_without_npm:/usr/bin:/bin"
if PATH="$no_npm_path" command -v npm >/dev/null 2>&1; then
  echo "test environment unexpectedly exposes npm outside the fake tool directory" >&2
  exit 1
fi
assert_residual_prerequisite_fails \
  missing-npm \
  "error: npm (Node.js) is required for the release" \
  "$no_npm_path"

assert_residual_lane_failure() {
  local label="$1"
  local assignment="$2"
  local expected_call="$3"
  : > "$audit_record"
  if env "$assignment" \
    PATH="$fake_tools:$PATH" \
    HARN_RELEASE_ROOT="$audit_root" \
    HARN_BIN="$fake_audit_harn" \
    TMPDIR="$tmp_root" \
    FAKE_AUDIT_RECORD="$audit_record" \
    "$repo_root/scripts/release_gate.sh" audit --receipt "$receipt" \
      > "$tmp_root/audit-$label.txt" 2>&1; then
    echo "injected residual lane failure unexpectedly passed: $label" >&2
    exit 1
  fi
  if ! grep -Fq "$expected_call" "$audit_record"; then
    echo "injected residual lane failure did not reach its command: $label" >&2
    cat "$audit_record" >&2
    exit 1
  fi
  if grep -Eq 'cargo clippy|make (fmt-check|test|conformance)|package-audit HARN_BIN=' "$audit_record"; then
    echo "injected residual lane failure launched merge-group-proved work: $label" >&2
    cat "$audit_record" >&2
    exit 1
  fi
}

assert_residual_lane_failure generated-lane "FAIL_FAKE_MAKE_TARGET=check-language-spec" "make check-language-spec"
assert_residual_lane_failure docs-lane "FAIL_FAKE_NPX=1" "npx markdownlint-cli2"
assert_residual_lane_failure security-lane "FAIL_FAKE_RG=1" "rg -n OAuth|oauth|MCP|trust boundary"
assert_residual_lane_failure smoke-lane "FAIL_FAKE_MAKE_TARGET=smoke-audit" "make smoke-audit"

assert_arg_fails_before_work() {
  local label="$1"
  local expected="$2"
  shift 2
  : > "$audit_record"
  if PATH="$fake_tools:$PATH" \
    HARN_RELEASE_ROOT="$audit_root" \
    TMPDIR="$tmp_root" \
    FAKE_AUDIT_RECORD="$audit_record" \
    "$@" > "$tmp_root/audit-$label.txt" 2>&1; then
    echo "invalid audit argument unexpectedly passed: $label" >&2
    exit 1
  fi
  if ! grep -Fq "$expected" "$tmp_root/audit-$label.txt"; then
    echo "invalid audit argument did not report the expected error: $label" >&2
    cat "$tmp_root/audit-$label.txt" >&2
    exit 1
  fi
  if [[ -s "$audit_record" ]]; then
    echo "invalid audit argument started audit work: $label" >&2
    cat "$audit_record" >&2
    exit 1
  fi
}

assert_arg_fails_before_work \
  removed-profile \
  "error: unknown audit arg: --profile" \
  "$repo_root/scripts/release_gate.sh" audit --profile residual
assert_arg_fails_before_work \
  missing \
  "error: audit --receipt requires a path" \
  "$repo_root/scripts/release_gate.sh" audit --receipt

invalid_receipt="$tmp_root/receipt-invalid.json"
printf '{}\n' > "$invalid_receipt"
run_audit invalid-receipt --receipt "$invalid_receipt"
for lane in rust-audit harn-audit generated-audit docs-audit grammar-audit security-audit package-audit smoke-audit; do
  if ! grep -Eq "ok: +$lane " "$tmp_root/audit-invalid-receipt.txt"; then
    echo "rejected receipt did not fall back to full audit lane: $lane" >&2
    cat "$tmp_root/audit-invalid-receipt.txt" >&2
    exit 1
  fi
done

echo "release_gate_harn_bin_test: ok"
