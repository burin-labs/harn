#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEMO_DIR="${ROOT}/.harn-runs/portal-demo"
GENERATE_ONLY=0
REFRESH=0

for arg in "$@"; do
  case "${arg}" in
    --generate-only) GENERATE_ONLY=1 ;;
    --refresh) REFRESH=1 ;;
    *)
      echo "Unknown argument: ${arg}" >&2
      exit 1
      ;;
  esac
done

if [[ ${REFRESH} -eq 0 ]] && [[ -f "${DEMO_DIR}/portal-success.json" ]] && [[ -f "${DEMO_DIR}/portal-replay.json" ]] && [[ -f "${DEMO_DIR}/portal-failed.json" ]]; then
  echo "Demo runs already exist in ${DEMO_DIR}. Reusing them."
else
  rm -rf "${DEMO_DIR}"
  mkdir -p "${DEMO_DIR}"

  echo "Building portal frontend..."
  (cd "${ROOT}" && npm run portal:build >/dev/null)

  echo "Generating demo runs in ${DEMO_DIR}..."
  (cd "${ROOT}" && cargo run --bin harn -- run examples/portal-demo.harn >/dev/null)
fi

echo "Demo runs ready:"
find "${DEMO_DIR}" -maxdepth 1 -name "*.json" -print | sort

if [[ ${GENERATE_ONLY} -eq 1 ]]; then
  echo
  echo "Open with:"
  echo "  cargo run --bin harn -- portal --dir .harn-runs/portal-demo --open false"
  exit 0
fi

echo
echo "Launching portal on demo dataset..."
cd "${ROOT}"
exec cargo run --bin harn -- portal --dir .harn-runs/portal-demo --open false
