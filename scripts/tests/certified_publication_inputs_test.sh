#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export INPUT_PROMOTE_ONLY=true EVENT_NAME=workflow_dispatch REF_TYPE=branch REF_NAME=main
export INPUT_TAG=v0.10.131 PROMOTION_RUN=123 PROMOTION_MANIFEST_ID=456
export PROMOTION_SOURCE=1111111111111111111111111111111111111111
export PROMOTION_ARCHIVE_POLICY=2222222222222222222222222222222222222222
export PROMOTION_POLICY=3333333333333333333333333333333333333333
export GITHUB_SHA="$PROMOTION_POLICY"
export PROMOTION_MANIFEST_DIGEST=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
reject() {
  if "$@" > "$tmp/refusal" 2>&1; then
    echo "FAIL: accepted invalid publication intent: $*" >&2
    exit 1
  fi
}
validator="$root/scripts/validate_release_promotion_inputs.sh"
"$validator"
reject env PROMOTION_RUN= "$validator"
reject env PROMOTION_SOURCE=bad "$validator"
reject env PROMOTION_POLICY="$PROMOTION_SOURCE" "$validator"
reject env PROMOTION_MANIFEST_DIGEST=sha256:bad "$validator"
reject env INPUT_PROMOTE_ONLY=false "$validator"
reject env FORCE_REBUILD=true "$validator"
reject env INPUT_CANDIDATE_ONLY=true "$validator"
reject env INPUT_TARGETS=x86_64-unknown-linux-gnu "$validator"
reject env REF_NAME=other "$validator"

# Execute the workflow's actual resolver. No build dispatch or asset discovery
# may be reached when the complete certified promotion tuple is selected.
mkdir -p "$tmp/resolve/scripts/lib" "$tmp/resolve/.github" "$tmp/bin"
cp "$root/scripts/validate_release_promotion_inputs.sh" "$tmp/resolve/scripts/"
cp "$root/scripts/lib/release_version.sh" "$tmp/resolve/scripts/lib/"
cp "$root/scripts/release_contract.env" "$tmp/resolve/scripts/"
cp "$root/.github/release-runner-policy.json" "$tmp/resolve/.github/"
cat > "$tmp/resolve/scripts/verify_release_tag_main_ancestry.sh" <<'SH'
#!/usr/bin/env bash
[[ "$*" == '--tag v0.10.131' ]]
SH
cat > "$tmp/bin/git" <<'SH'
#!/usr/bin/env bash
case "$1" in
  rev-parse) echo "$PROMOTION_SOURCE" ;;
  ls-remote) printf '%s\trefs/tags/v0.10.131\n' "$PROMOTION_SOURCE" ;;
  *) exit 91 ;;
esac
SH
cat > "$tmp/bin/gh" <<'SH'
#!/usr/bin/env bash
echo 'FAIL: promotion tried discovery or dispatch' >&2
exit 92
SH
chmod +x "$tmp/bin/git" "$tmp/bin/gh" "$tmp/resolve/scripts/verify_release_tag_main_ancestry.sh"
awk '/        id: resolve/{found=1} found && /        run: \|/{body=1;next} body && /^  [^ ]/{exit} body{print substr($0,11)}' "$root/.github/workflows/build-release-binaries.yml" > "$tmp/resolve.sh"
(
  cd "$tmp/resolve"
  export PATH="$tmp/bin:$PATH" GITHUB_OUTPUT="$tmp/outputs" GITHUB_STEP_SUMMARY="$tmp/summary"
  export INPUT_TARGETS='' INPUT_LEGACY_PROVENANCE_OVERRIDE='' INPUT_BENCHMARK_CARGO_BLOAT=false
  export INPUT_BENCHMARK_ONLY=false INPUT_BENCHMARK_SOURCE_REF='' INPUT_BENCHMARK_SOURCE_SHA=''
  export INPUT_CANDIDATE_SOURCE_REF='' INPUT_CANDIDATE_SOURCE_SHA='' INPUT_CANDIDATE_ONLY=false
  export INPUT_WARM_CACHE_ONLY=false FORCE_REBUILD=false
  bash -eu "$tmp/resolve.sh"
)
grep -Fxq should_build_binaries=false "$tmp/outputs"
grep -Fxq should_finalize_release=true "$tmp/outputs"
grep -Fxq build_mode=promote "$tmp/outputs"
grep -Fxq '[]' "$tmp/outputs"

mkdir "$tmp/repo"
git -C "$tmp/repo" init -q
git -C "$tmp/repo" -c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false commit --allow-empty -qm candidate
candidate="$(git -C "$tmp/repo" rev-parse HEAD)"
git -C "$tmp/repo" tag v0.10.131
git -C "$tmp/repo" -c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false commit --allow-empty -qm current-policy
policy="$(git -C "$tmp/repo" rev-parse HEAD)"
export EXPECTED_SOURCE_SHA="$candidate" EXPECTED_POLICY_SHA="$policy" GITHUB_SHA="$policy"
cd "$tmp/repo"
publication="$root/scripts/validate_release_publication_inputs.sh"
"$publication"
reject env EXPECTED_SOURCE_SHA="$policy" "$publication"
reject env EXPECTED_POLICY_SHA="$candidate" "$publication"
reject env INPUT_TAG=v0.10.132 "$publication"
reject env INPUT_TAG=main "$publication"
echo 'certified publication input controls passed (candidate != current main)'
