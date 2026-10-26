#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/harn-published-runtime-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$root/scripts/lib/candidate_archive_contract.sh"
mkdir -p "$scratch/repo" "$scratch/bin" "$scratch/payload" "$scratch/cache/downloads/1.2.3"
git init -q -b main "$scratch/repo"
git -C "$scratch/repo" config commit.gpgSign false
git -C "$scratch/repo" config maintenance.auto false
git -C "$scratch/repo" config gc.auto 0
git -C "$scratch/repo" -c user.name=Test -c user.email=test@example.com commit -q --allow-empty -m source
export PUBLICATION_SOURCE
PUBLICATION_SOURCE="$(git -C "$scratch/repo" rev-parse HEAD)"
git -C "$scratch/repo" -c tag.gpgSign=false tag v1.2.3
export PUBLICATION_TARGETS PUBLICATION_TRACE PUBLICATION_RUNTIME_ARCHIVE RUNTIME_EXEC_TRACE
PUBLICATION_TARGETS="$(candidate_archive_expected_targets_json)"
PUBLICATION_TRACE="$scratch/github-calls"
RUNTIME_EXEC_TRACE="$scratch/runtime-calls"
PUBLICATION_RUNTIME_ARCHIVE="$scratch/cache/downloads/1.2.3/harn-x86_64-unknown-linux-gnu.tar.xz"
cp "$root/scripts/tests/fixtures/release_publication_github.py" "$scratch/bin/gh"
chmod +x "$scratch/bin/gh"
cat >"$scratch/payload/harn" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == 'version --json' ]] || exit 67
printf '%s\n' version >> "$RUNTIME_EXEC_TRACE"
jq -n --arg source "${RUNTIME_REPORTED_SOURCE:-$PUBLICATION_SOURCE}" \
  --arg version "${RUNTIME_REPORTED_VERSION:-1.2.3}" \
  '{ok:true,data:{version:$version,source_revision:$source}}'
EOF
chmod +x "$scratch/payload/harn"
cp "$scratch/payload/harn" "$scratch/bin/harn"
tar -cJf "$PUBLICATION_RUNTIME_ARCHIVE" -C "$scratch/payload" harn
checksum="$(sha256_file "$PUBLICATION_RUNTIME_ARCHIVE")"
binary_sha="$(sha256_file "$scratch/bin/harn")"
jq -n --arg path "$scratch/bin/harn" --arg checksum "$checksum" --arg binary_sha "$binary_sha" \
  '{schema_version:"harn-bootstrap-v1",version:"1.2.3",target:"x86_64-unknown-linux-gnu",
    binary_path:$path,binary_sha256:$binary_sha,checksum:$checksum,
    source:"https://github.com/fixture/harn/releases/download/v1.2.3/harn-x86_64-unknown-linux-gnu.tar.xz"}' \
  >"$scratch/receipt.json"
export GH_REPO=fixture/harn
export PATH="$scratch/bin:$PATH"
cd "$scratch/repo"
verify() {
  bash "$root/scripts/verify_published_harn_runtime.sh" "$scratch/receipt.json" \
    "$scratch/cache" "$PUBLICATION_SOURCE" 1.2.3
}
verify >"$scratch/proof.json"
jq -e --arg source "$PUBLICATION_SOURCE" --arg sha "$binary_sha" '
  .schema == "harn.published_orchestration_runtime.v1" and
  .source == $source and .binary_sha256 == $sha
' "$scratch/proof.json" >/dev/null
[[ "$(cat "$RUNTIME_EXEC_TRACE")" == version ]]
grep -q '^attestation verify$' "$PUBLICATION_TRACE"
rm "$RUNTIME_EXEC_TRACE"

# Empty and malformed authority must refuse before executing any installed tool.
for PUBLICATION_CASE in empty-attestation attestation-failure wrong-source wrong-workflow \
  wrong-run wrong-attempt wrong-subject incomplete-index draft-release unsigned-source \
  nonpublisher-index nonpublisher-archive wrong-archive-digest http-failure; do
  export PUBLICATION_CASE
  if verify >"$scratch/refusal.log" 2>&1; then
    echo "FAIL: accepted $PUBLICATION_CASE" >&2
    exit 1
  fi
  [[ ! -e "$RUNTIME_EXEC_TRACE" ]]
done
unset PUBLICATION_CASE

# Archive and receipt agreement alone does not prove the reached CLI identity.
for identity in source version; do
  if [[ "$identity" == source ]]; then
    export RUNTIME_REPORTED_SOURCE=0000000000000000000000000000000000000000
  else
    export RUNTIME_REPORTED_VERSION=9.9.9
  fi
  if verify >"$scratch/refusal.log" 2>&1; then
    echo "FAIL: accepted wrong reported $identity" >&2
    exit 1
  fi
  [[ "$(cat "$RUNTIME_EXEC_TRACE")" == version ]]
  rm "$RUNTIME_EXEC_TRACE"
  unset RUNTIME_REPORTED_SOURCE RUNTIME_REPORTED_VERSION
done

cp "$PUBLICATION_RUNTIME_ARCHIVE" "$scratch/archive.saved"
printf 'tampered' >>"$PUBLICATION_RUNTIME_ARCHIVE"
if verify >"$scratch/refusal.log" 2>&1; then
  echo 'FAIL: accepted modified cached archive' >&2
  exit 1
fi
[[ ! -e "$RUNTIME_EXEC_TRACE" ]]
mv "$scratch/archive.saved" "$PUBLICATION_RUNTIME_ARCHIVE"

# A receipt forged to match a substituted executable cannot borrow an authentic
# archive's provenance, even though the receipt and binary agree with each other.
printf '\n# substituted\n' >>"$scratch/bin/harn"
changed_sha="$(sha256_file "$scratch/bin/harn")"
jq --arg sha "$changed_sha" '.binary_sha256 = $sha' "$scratch/receipt.json" >"$scratch/changed.json"
mv "$scratch/changed.json" "$scratch/receipt.json"
if verify >"$scratch/refusal.log" 2>&1; then
  echo 'FAIL: substituted executable borrowed archive authority' >&2
  exit 1
fi
[[ ! -e "$RUNTIME_EXEC_TRACE" ]]

# The immutable tag and requested source are separate load-bearing inputs.
if bash "$root/scripts/verify_published_harn_runtime.sh" "$scratch/receipt.json" \
  "$scratch/cache" 0000000000000000000000000000000000000000 1.2.3 \
  >"$scratch/refusal.log" 2>&1; then
  echo 'FAIL: mismatched tag source accepted' >&2
  exit 1
fi
[[ ! -e "$RUNTIME_EXEC_TRACE" ]]
echo 'PASS: source-bound published runtime and pre-execution refusal controls'
