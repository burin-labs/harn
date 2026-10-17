#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

# Exercise the same sweep CI calls. Replace only the Harn process so this
# regression needs neither a compiler nor a generated artifact. A failed
# grammar check must be reached and must propagate through the policy runner.
cat > "$tmp_root/harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == 'run scripts/sync_grammar_fitness_receipt.harn -- --check' ]]; then
  printf 'grammar-check\n' >> "${POLICY_TEST_RECORD:?}"
  exit "${POLICY_TEST_GRAMMAR_STATUS:?}"
fi
exit 0
SH
chmod +x "$tmp_root/harn"

# Keep the real list and runner, while isolating the one failure being tested.
printf 'include %s/Makefile\n' "$repo_root" > "$tmp_root/Makefile"
cat >> "$tmp_root/Makefile" <<'MAKE'
$(filter-out check-grammar-fitness,$(SOURCE_REPOSITORY_POLICIES)):
	@:
MAKE

record="$tmp_root/record"
if POLICY_TEST_RECORD="$record" POLICY_TEST_GRAMMAR_STATUS=97 \
  make -C "$repo_root" -f "$tmp_root/Makefile" repository-policies-source \
    MAKE="make -f $tmp_root/Makefile" HARN_CMD="$tmp_root/harn" \
    > "$tmp_root/failed.log" 2>&1; then
  cat "$tmp_root/failed.log" >&2
  echo 'source policy sweep accepted a failing grammar check' >&2
  exit 1
fi
if [[ ! -f "$record" ]] || [[ $(cat "$record") != grammar-check ]]; then
  cat "$tmp_root/failed.log" >&2
  echo 'source policy sweep did not reach exactly one grammar check' >&2
  exit 1
fi
if ! grep -Eq '^FAIL +check-grammar-fitness$' "$tmp_root/failed.log"; then
  cat "$tmp_root/failed.log" >&2
  echo 'source policy sweep did not identify the grammar failure' >&2
  exit 1
fi

POLICY_TEST_RECORD="$record" POLICY_TEST_GRAMMAR_STATUS=0 \
  make -C "$repo_root" -f "$tmp_root/Makefile" repository-policies-source \
    MAKE="make -f $tmp_root/Makefile" HARN_CMD="$tmp_root/harn" \
    > "$tmp_root/passed.log" 2>&1
if [[ $(wc -l < "$record" | tr -d ' ') != 2 ]]; then
  echo 'passing source sweep did not reach the grammar check' >&2
  exit 1
fi
echo 'source_policy_grammar_test: ok'
