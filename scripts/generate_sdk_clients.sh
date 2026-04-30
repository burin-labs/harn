#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SPEC_PATH="${ROOT}/spec/openapi.yaml"
OUTPUT_DIR="${ROOT}/target/generated-sdks"
LANGUAGE="all"

PYTHON_GENERATOR_VERSION="${PYTHON_GENERATOR_VERSION:-0.28.3}"
TYPESCRIPT_GENERATOR_VERSION="${TYPESCRIPT_GENERATOR_VERSION:-0.97.0}"

usage() {
  cat <<'USAGE'
usage: scripts/generate_sdk_clients.sh [--language python|typescript|all] [--output-dir DIR]

Regenerates Harn Agents API SDK clients from spec/openapi.yaml.

Environment overrides:
  PYTHON_GENERATOR_VERSION      openapi-python-client version
  TYPESCRIPT_GENERATOR_VERSION  @hey-api/openapi-ts version
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --language)
      LANGUAGE="${2:-}"
      shift 2
      ;;
    --output-dir)
      OUTPUT_DIR="${2:-}"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$LANGUAGE" in
  python|typescript|all) ;;
  *)
    echo "--language must be python, typescript, or all" >&2
    exit 2
    ;;
esac

if [[ ! -f "$SPEC_PATH" ]]; then
  echo "missing OpenAPI spec: $SPEC_PATH" >&2
  exit 1
fi

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

harn_version() {
  awk '
    /^\[workspace.package\]/ { in_workspace_package = 1; next }
    /^\[/ { in_workspace_package = 0 }
    in_workspace_package && /^version = / {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' "$ROOT/Cargo.toml"
}

sdk_version() {
  local version major minor
  version="$(harn_version)"
  IFS=. read -r major minor _patch <<< "$version"
  echo "${major}.${minor}.0"
}

write_manifest() {
  local language="$1"
  local dir="$2"
  {
    echo "language=${language}"
    echo "harn_version=$(harn_version)"
    echo "sdk_version=$(sdk_version)"
    echo "openapi_spec=spec/openapi.yaml"
    echo "openapi_sha256=$(shasum -a 256 "$SPEC_PATH" | awk '{print $1}')"
    case "$language" in
      python) echo "generator=openapi-python-client@${PYTHON_GENERATOR_VERSION}" ;;
      typescript) echo "generator=@hey-api/openapi-ts@${TYPESCRIPT_GENERATOR_VERSION}" ;;
    esac
  } > "${dir}/harn-sdk-generation.txt"
}

generate_python() {
  local out="${OUTPUT_DIR}/python"
  local venv="${OUTPUT_DIR}/.venv-openapi-python-client"

  rm -rf "$out"
  mkdir -p "$out"

  if command -v uvx >/dev/null 2>&1; then
    uvx --from "openapi-python-client==${PYTHON_GENERATOR_VERSION}" \
      openapi-python-client generate \
      --path "$SPEC_PATH" \
      --output-path "$out" \
      --meta none \
      --overwrite \
      --fail-on-warning
  else
    python3 -m venv "$venv"
    "$venv/bin/python" -m pip install --upgrade pip
    "$venv/bin/python" -m pip install "openapi-python-client==${PYTHON_GENERATOR_VERSION}"
    PATH="$venv/bin:$PATH" "$venv/bin/openapi-python-client" generate \
      --path "$SPEC_PATH" \
      --output-path "$out" \
      --meta none \
      --overwrite \
      --fail-on-warning
  fi

  rm -rf "${out}/.ruff_cache"
  test -s "${out}/client.py"
  test -s "${out}/api/discovery/get_protocol_discovery.py"
  write_manifest "python" "$out"
  echo "generated Python SDK client in $out"
}

generate_typescript() {
  local out="${OUTPUT_DIR}/typescript"

  rm -rf "$out"
  mkdir -p "$out"

  (
    cd "$ROOT"
    npx --yes "@hey-api/openapi-ts@${TYPESCRIPT_GENERATOR_VERSION}" \
      -i ./spec/openapi.yaml \
      -o "$out" \
      -p @hey-api/typescript @hey-api/sdk @hey-api/client-fetch \
      --no-log-file
  )

  test -s "${out}/index.ts"
  test -s "${out}/sdk.gen.ts"
  test -s "${out}/types.gen.ts"
  write_manifest "typescript" "$out"
  echo "generated TypeScript SDK client in $out"
}

case "$LANGUAGE" in
  python) generate_python ;;
  typescript) generate_typescript ;;
  all)
    generate_python
    generate_typescript
    ;;
esac
