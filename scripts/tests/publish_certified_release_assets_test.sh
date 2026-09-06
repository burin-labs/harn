#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export FIXTURE_DIR="$tmp"
mkdir "$tmp/bin" "$tmp/assets"
# shellcheck source=scripts/lib/candidate_archive_contract.sh
source "$root/scripts/lib/candidate_archive_contract.sh"
for name in "${EXPECTED_RELEASE_ARCHIVES[@]}" SHA256SUMS release-assets.json; do
  printf 'verified bytes %s' "$name" > "$tmp/assets/$name"
done
printf notes > "$tmp/notes"
cat > "$tmp/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FIXTURE_DIR/calls"
case "$*" in
  'api '*'/git/ref/tags/'*) echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ;;
  'api '*'/git/tags/'*) echo 1111111111111111111111111111111111111111 ;;
  'api --paginate --slurp '*) cat "$FIXTURE_DIR/releases.json" ;;
  'release create '*) printf '[[{"tag_name":"v0.10.131","assets":[]}]]' > "$FIXTURE_DIR/releases.json" ;;
  'release upload '*)
    [[ "$*" != *clobber* ]]
    name="$(basename "$4")"
    digest="sha256:$(shasum -a 256 "$4" | awk '{print $1}')"
    if [[ "${RACE:-false}" == true ]]; then digest=sha256:conflicting; fi
    jq --arg name "$name" --arg digest "$digest" '.[0][0].assets += [{name:$name,digest:$digest}]' "$FIXTURE_DIR/releases.json" > "$FIXTURE_DIR/next.json"
    mv "$FIXTURE_DIR/next.json" "$FIXTURE_DIR/releases.json"
    [[ "${RACE:-false}" != true ]]
    ;;
  'release edit '*) ;;
  *) echo "unexpected mutation: $*" >&2; exit 90 ;;
esac
SH
chmod +x "$tmp/bin/gh"
export PATH="$tmp/bin:$PATH"
publish() { "$root/scripts/publish_certified_release_assets.sh" burin-labs/harn v0.10.131 1111111111111111111111111111111111111111 "$tmp/assets" "$tmp/notes" false true; }
printf '[[]]' > "$tmp/releases.json"
publish
[[ "$(grep -c '^release upload ' "$tmp/calls")" == 7 ]]
publish
[[ "$(grep -c '^release upload ' "$tmp/calls")" == 7 ]]
printf '[[{"tag_name":"v0.10.131","assets":[{"name":"harn-sdk-python.tar.gz","digest":"sha256:sdk-python"},{"name":"harn-sdk-typescript.tar.gz","digest":"sha256:sdk-typescript"}]}]]' > "$tmp/releases.json"
printf '' > "$tmp/calls"
publish
[[ "$(grep -c '^release upload ' "$tmp/calls")" == 7 ]]
[[ "$(jq '[.[0][0].assets[] | select(.name | startswith("harn-sdk-"))] | length' "$tmp/releases.json")" == 2 ]]
printf '[[{"tag_name":"v0.10.131","assets":[{"name":"SHA256SUMS","digest":"sha256:conflicting"}]}]]' > "$tmp/releases.json"
printf '' > "$tmp/calls"
if publish > "$tmp/refusal" 2>&1; then echo 'FAIL: accepted conflicting metadata' >&2; exit 1; fi
if grep -q '^release ' "$tmp/calls"; then echo 'FAIL: mutated conflicting release' >&2; exit 1; fi
printf '[[{"tag_name":"v0.10.131","assets":[]}]]' > "$tmp/releases.json"
printf '' > "$tmp/calls"
export RACE=true
if publish > "$tmp/refusal" 2>&1; then echo 'FAIL: accepted conflicting concurrent upload' >&2; exit 1; fi
if grep -Eq 'clobber|delete|^release edit ' "$tmp/calls"; then echo 'FAIL: replaced bytes or finalized after conflicting upload' >&2; exit 1; fi
echo 'publication create-only, idempotency and conflict controls passed'
