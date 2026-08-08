#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/bin"
cat > "$TMP/bin/npx" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" > "${FAKE_NPX_ARGS:?}"
while [[ $# -gt 0 ]]; do
  if [[ "$1" == "-o" ]]; then
    out="$2"
    mkdir -p "$out"
    printf '// generated\n' | tee \
      "$out/index.ts" \
      "$out/sdk.gen.ts" \
      "$out/types.gen.ts" >/dev/null
    exit 0
  fi
  shift
done
echo "missing -o argument" >&2
exit 1
EOF
chmod +x "$TMP/bin/npx"

PATH="$TMP/bin:$PATH" \
FAKE_NPX_ARGS="$TMP/npx-args" \
TYPESCRIPT_GENERATOR_VERSION="0.97.0-test" \
TYPESCRIPT_VERSION="6.0.3-test" \
  "$ROOT/scripts/generate_sdk_clients.sh" \
    --language typescript \
    --output-dir "$TMP/output"

expected_args="$(cat <<'EOF'
--yes
--package
@hey-api/openapi-ts@0.97.0-test
--package
typescript@6.0.3-test
openapi-ts
-i
./spec/openapi.yaml
-o
__OUTPUT__
-p
@hey-api/typescript
@hey-api/sdk
@hey-api/client-fetch
--no-log-file
EOF
)"
expected_args="${expected_args/__OUTPUT__/$TMP\/output\/typescript}"
[[ "$(cat "$TMP/npx-args")" == "$expected_args" ]]

manifest="$TMP/output/typescript/harn-sdk-generation.txt"
grep -Fxq 'generator=@hey-api/openapi-ts@0.97.0-test' "$manifest"
grep -Fxq 'typescript=typescript@6.0.3-test' "$manifest"
