#!/usr/bin/env bash
# Run nextest's bounded stress mode after the ordinary native qualification.
set -euo pipefail
[[ $(uname -s) == Darwin && ${HARN_EXT_LSP_FIXTURES_REQUIRED:-} == 1 ]]
[[ ${SOURCE_SHA:-} =~ ^[0-9a-f]{40}$ && $(git rev-parse HEAD) == "$SOURCE_SHA" ]]
[[ -z ${HARN_LSP_STARTUP_TIMEOUT_MS:-} && -z ${HARN_LSP_REQUEST_TIMEOUT_MS:-} ]]
tools="${RUNNER_TEMP:?}/native-qualification-tools"
# shellcheck source=scripts/lib/sha256.sh
source "$tools/sha256.sh"
evidence="$tools/hostlib-stress"
mkdir -p "$evidence"
binary="${CARGO_TARGET_DIR:-./target}/debug/harn"
[[ $("$binary" version --json | jq -er .data.source_revision) == "$SOURCE_SHA" ]]
HARN_LSP_SERVER_RUST_ANALYZER=$(rustup which rust-analyzer)
export HARN_LSP_SERVER_RUST_ANALYZER
"$HARN_LSP_SERVER_RUST_ANALYZER" --version > "$evidence/rust-analyzer.txt"
sha256_file_hex "$HARN_LSP_SERVER_RUST_ANALYZER" > "$evidence/rust-analyzer.sha256"

test_package=harn-hostlib
# Preserve the feature graph already compiled by the workspace qualification.
cargo-nextest nextest list --locked --workspace --exclude harn-wasm --profile default \
  -E "package(=${test_package})" \
  --message-format json > "$evidence/inventory.json"
"$binary" run --standalone --read-only-root "$evidence" "$tools/nextest_inventory.harn" -- \
  "$evidence/inventory.json" \
  "harn-hostlib::harn_hostlib\$code_index::lsp::rename_lsp_tests::a_method_rename_leaves_a_free_function_path_in_a_macro_alone" \
  "harn-hostlib::harn_hostlib\$code_index::lsp::rename_lsp_tests::unanalyzed_relative_imports_and_macro_arguments_fall_back" \
  "harn-hostlib::harn_hostlib\$process_tools_e2e::real_run_command_isolates_the_child_in_its_own_session" \
  > "$evidence/selected-count.txt"
jq -er --arg package "$test_package" \
  '."rust-suites"[] | select(.status == "listed" and ."package-name" == $package) | ."binary-path"' \
  "$evidence/inventory.json" > "$evidence/binaries.txt"
while IFS= read -r executable; do
  digest=$(sha256_file_hex "$executable")
  printf '%s  %s\n' "$digest" "$executable"
done < "$evidence/binaries.txt" > "$evidence/binaries-before.sha256"

status=0
NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo-nextest nextest run --locked --workspace --exclude harn-wasm \
  -E "package(=${test_package})" \
  --profile default --test-threads 32 --stress-count 20 --retries 0 --no-fail-fast --no-tests fail \
  --message-format libtest-json-plus --message-format-version 0.1 \
  > "$evidence/events.jsonl" 2> "$evidence/runner.log" || status=$?
while IFS= read -r executable; do
  digest=$(sha256_file_hex "$executable")
  printf '%s  %s\n' "$digest" "$executable"
done < "$evidence/binaries.txt" > "$evidence/binaries-after.sha256"
cmp "$evidence/binaries-before.sha256" "$evidence/binaries-after.sha256"
"$binary" run --standalone --read-only-root "$evidence" "$tools/summarize_nextest_receipt.harn" -- \
  --inventory "$evidence/inventory.json" --events "$evidence/events.jsonl" \
  --runner-status "$status" --threads 32 --stress-count 20 --format json \
  > "$evidence/summary.json"
cat "$evidence/summary.json"
exit "$status"
