#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/check_sdk_release_artifacts.sh"
workflow="$repo_root/.github/workflows/sdk-codegen.yml"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

fail() {
  echo "check_sdk_release_artifacts_test: $*" >&2
  exit 1
}

write_manifest() {
  local dir="$1"
  local language="$2"
  mkdir -p "$dir"
  cat >"${dir}/harn-sdk-generation.txt" <<EOF
language=${language}
harn_version=0.10.116
sdk_version=0.10.0
openapi_spec=spec/openapi.yaml
openapi_sha256=279739bbe7aff1242dcdb68d6061776277927701b8e0da3fd1d80120b200557a
EOF
}

require_line() {
  local expected="$1"
  if ! grep -Fqx -- "${expected}" "${workflow}"; then
    fail "missing SDK codegen workflow contract: ${expected}"
  fi
}

if "$script" >/dev/null 2>"$tmp_root/usage.err"; then
  fail "expected usage failure without a mode"
fi
grep -q "usage:" "$tmp_root/usage.err" || fail "usage stderr missing"

both="$tmp_root/both"
write_manifest "$both/python" python
write_manifest "$both/typescript" typescript
if ! out="$("$script" --dir "$both")"; then
  fail "expected success when both language manifests are present"
fi
grep -q "both SDK language artifacts are present" <<<"$out" \
  || fail "success output missing: $out"

python_only="$tmp_root/python-only"
write_manifest "$python_only/python" python
if "$script" --dir "$python_only" >/dev/null 2>"$tmp_root/python-only.err"; then
  fail "expected failure when the TypeScript artifact is missing"
fi
grep -q "typescript" "$tmp_root/python-only.err" \
  || fail "missing-typescript stderr incorrect"

typescript_only="$tmp_root/typescript-only"
write_manifest "$typescript_only/typescript" typescript
if "$script" --dir "$typescript_only" >/dev/null 2>"$tmp_root/typescript-only.err"; then
  fail "expected failure when the Python artifact is missing"
fi
grep -q "python" "$tmp_root/typescript-only.err" \
  || fail "missing-python stderr incorrect"

empty_dir="$tmp_root/empty"
mkdir -p "$empty_dir"
if "$script" --dir "$empty_dir" >/dev/null 2>"$tmp_root/empty.err"; then
  fail "expected failure when both language artifacts are missing"
fi
grep -q "python" "$tmp_root/empty.err" || fail "empty-dir stderr omitted python"
grep -q "typescript" "$tmp_root/empty.err" \
  || fail "empty-dir stderr omitted typescript"

incomplete="$tmp_root/incomplete"
write_manifest "$incomplete/python" python
mkdir -p "$incomplete/typescript"
printf 'language=typescript\nharn_version=0.10.116\n' \
  >"$incomplete/typescript/harn-sdk-generation.txt"
if "$script" --dir "$incomplete" >/dev/null 2>"$tmp_root/incomplete.err"; then
  fail "expected failure when openapi_sha256 is missing"
fi
grep -q "openapi_sha256" "$tmp_root/incomplete.err" \
  || fail "incomplete-manifest stderr omitted openapi_sha256"

sha_named="$tmp_root/sha-named"
write_manifest "$sha_named/harn-sdk-python-3ed08af50c27aadf3960c08193b4b9a12a7ea43e" python
write_manifest "$sha_named/harn-sdk-typescript-3ed08af50c27aadf3960c08193b4b9a12a7ea43e" typescript
if ! "$script" --dir "$sha_named" >/dev/null; then
  fail "expected success for SHA-named artifact directories"
fi

mkdir -p "$tmp_root/bin"
cat >"$tmp_root/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" > "${FAKE_GH_ARGS:?}"
case "${1:-} ${2:-}" in
  "release view")
    cat "${FAKE_GH_ASSETS:?}"
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 99
    ;;
esac
EOF
chmod +x "$tmp_root/bin/gh"

cat >"$tmp_root/complete-assets.json" <<'EOF'
["harn-sdk-python.tar.gz","harn-sdk-typescript.tar.gz","harn-x86_64-unknown-linux-gnu.tar.gz"]
EOF
# The script reads newline-separated names from --jq '.assets[].name'.
cat >"$tmp_root/complete-assets.txt" <<'EOF'
harn-sdk-python.tar.gz
harn-sdk-typescript.tar.gz
harn-x86_64-unknown-linux-gnu.tar.gz
EOF

if ! out="$(
  FAKE_GH_ARGS="$tmp_root/gh-args" \
  FAKE_GH_ASSETS="$tmp_root/complete-assets.txt" \
    "$script" --release v0.10.117 --repo burin-labs/harn --gh-bin "$tmp_root/bin/gh"
)"; then
  fail "expected success when both GitHub release SDK assets exist"
fi
grep -q "both SDK language artifacts are attached to v0.10.117" <<<"$out" \
  || fail "release success output missing: $out"
grep -q "release view v0.10.117 --repo burin-labs/harn" "$tmp_root/gh-args" \
  || fail "gh invocation missing tag or repo"

cat >"$tmp_root/python-assets.txt" <<'EOF'
harn-sdk-python.tar.gz
harn-x86_64-unknown-linux-gnu.tar.gz
EOF
if FAKE_GH_ARGS="$tmp_root/gh-args-missing" \
  FAKE_GH_ASSETS="$tmp_root/python-assets.txt" \
  "$script" --release v0.10.116 --repo burin-labs/harn --gh-bin "$tmp_root/bin/gh" \
  >/dev/null 2>"$tmp_root/release-missing.err"; then
  fail "expected failure when the TypeScript GitHub release asset is missing"
fi
grep -q "harn-sdk-typescript.tar.gz" "$tmp_root/release-missing.err" \
  || fail "missing-release-asset stderr incorrect"

require_line '    if: always() && !cancelled()'
if ! grep -Fq 'scripts/check_sdk_release_artifacts.sh --dir' "$workflow"; then
  fail "SDK codegen workflow no longer runs the fail-closed directory check"
fi
if ! grep -Fq 'harn-sdk-python.tar.gz' "$workflow"; then
  fail "SDK codegen workflow no longer attaches the Python release asset"
fi
if ! grep -Fq 'harn-sdk-typescript.tar.gz' "$workflow"; then
  fail "SDK codegen workflow no longer attaches the TypeScript release asset"
fi
if ! grep -Fq 'check_sdk_release_artifacts.sh --release' "$workflow"; then
  fail "SDK codegen workflow no longer fail-closes on missing GitHub release SDK assets"
fi

echo "check_sdk_release_artifacts_test: ok"
