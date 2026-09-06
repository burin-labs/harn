#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$root/scripts/lib/candidate_archive_contract.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export FIXTURE_DIR="$tmp"
mkdir "$tmp/bin" "$tmp/files"
source_sha=1111111111111111111111111111111111111111
policy=2222222222222222222222222222222222222222
run=123
manifest_id=456
printf '[]' > "$tmp/inventory.json"
printf '{}' > "$tmp/entries.json"
add_inventory() {
  local id="$1" name="$2" digest="$3"
  jq --argjson id "$id" --arg name "$name" --arg digest "$digest" --arg policy "$policy" \
    '. + [{id:$id,name:$name,digest:$digest,expired:false,workflow_run:{id:123,head_sha:$policy}}]' \
    "$tmp/inventory.json" > "$tmp/next.json"
  mv "$tmp/next.json" "$tmp/inventory.json"
}
id=500
for archive in "${EXPECTED_RELEASE_ARCHIVES[@]}"; do
  target="$(target_for_archive "$archive")"
  printf 'candidate bytes %s' "$target" > "$tmp/files/$archive"
  (cd "$tmp/files" && zip -q "$tmp/$id.zip" "$archive")
  add_inventory "$id" "harn-$target" "sha256:$(sha256_file "$tmp/$id.zip")"
  signing=not_applicable notarization=not_applicable
  if [[ "$target" == *apple-darwin ]]; then signing=signed; notarization=notarized; fi
  jq --arg target "$target" --arg archive "$archive" --arg digest "$(sha256_file "$tmp/files/$archive")" --arg signing "$signing" --arg notarization "$notarization" \
    '. + {($target):{archive:$archive,sha256:$digest,signingStatus:$signing,notarizationStatus:$notarization,attestationIdentity:"https://harnlang.com/attestations/release-archive/v1",runId:"123",runAttempt:"1"}}' \
    "$tmp/entries.json" > "$tmp/next.json"
  mv "$tmp/next.json" "$tmp/entries.json"
  id=$((id+1))
done
manifest_name="candidate-archive-manifest-$source_sha"
jq -n --arg source "$source_sha" --arg policy "$policy" --slurpfile entries "$tmp/entries.json" \
  '{schemaVersion:"harn.candidate_archive_manifest.v1",sourceCommit:$source,policyRevision:$policy,runId:"123",runAttempt:"1",archives:$entries[0]}' > "$tmp/files/$manifest_name.json"
(cd "$tmp/files" && zip -q "$tmp/$manifest_id.zip" "$manifest_name.json")
digest="sha256:$(sha256_file "$tmp/$manifest_id.zip")"
add_inventory "$manifest_id" "$manifest_name" "$digest"
jq -n --arg policy "$policy" '{id:123,event:"workflow_dispatch",path:".github/workflows/build-release-binaries.yml",head_branch:"main",head_sha:$policy,status:"completed",conclusion:"success",run_attempt:1}' > "$tmp/run.json"
cat > "$tmp/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  'api --paginate --slurp '*'/artifacts?per_page=100') jq '[{artifacts:.}]' "$FIXTURE_DIR/inventory.json" ;;
  'api '*'/actions/artifacts/'*'/zip') id="${2%/zip}"; cat "$FIXTURE_DIR/${id##*/}.zip" ;;
  'api '*'/actions/runs/123') cat "$FIXTURE_DIR/run.json" ;;
  *) echo "unexpected API action $*" >&2; exit 90 ;;
esac
SH
chmod +x "$tmp/bin/gh"
export PATH="$tmp/bin:$PATH"
downloader="$root/scripts/download_candidate_archive_promotion.sh"
invoke() { "$downloader" burin-labs/harn "$run" "$manifest_id" "$digest" "$source_sha" "$policy" "$tmp/output-$1"; }
reject() { if invoke "$1" > "$tmp/refusal" 2>&1; then echo "FAIL: accepted $1" >&2; exit 1; fi; }
invoke valid
for archive in "${EXPECTED_RELEASE_ARCHIVES[@]}"; do cmp "$tmp/files/$archive" "$tmp/output-valid/$archive"; done
cp "$tmp/run.json" "$tmp/good-run.json"
jq '.status="in_progress" | .conclusion=null' "$tmp/good-run.json" > "$tmp/run.json"
reject pending
cp "$tmp/good-run.json" "$tmp/run.json"
cp "$tmp/inventory.json" "$tmp/good-inventory.json"
jq '.[0].expired=true' "$tmp/good-inventory.json" > "$tmp/inventory.json"
reject expired
jq '. + [.[0]]' "$tmp/good-inventory.json" > "$tmp/inventory.json"
reject ambiguous
jq 'map(select(.id != 500))' "$tmp/good-inventory.json" > "$tmp/inventory.json"
reject missing
cp "$tmp/good-inventory.json" "$tmp/inventory.json"
source_sha=3333333333333333333333333333333333333333
reject wrong_source
source_sha=1111111111111111111111111111111111111111
printf corruption >> "$tmp/500.zip"
reject corrupt_archive
echo 'candidate download proof controls passed; zero workflow dispatches'
