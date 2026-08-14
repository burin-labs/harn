#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

bounded=$(make -C "$repo_root" -n conformance HARN_CONFORMANCE_JOBS=3)
if ! grep -Fq 'test conformance --parallel --jobs 3 --timeout 60000' <<<"$bounded"; then
  echo "conformance target ignored HARN_CONFORMANCE_JOBS" >&2
  printf '%s\n' "$bounded" >&2
  exit 1
fi

overridden=$(
  make -C "$repo_root" -n conformance \
    HARN_CONFORMANCE_JOBS=3 \
    HARN_CONFORMANCE_TIMEOUT_MS=90000
)
if ! grep -Fq 'test conformance --parallel --jobs 3 --timeout 90000' <<<"$overridden"; then
  echo "conformance target ignored HARN_CONFORMANCE_TIMEOUT_MS" >&2
  printf '%s\n' "$overridden" >&2
  exit 1
fi

automatic=$(make -C "$repo_root" -n conformance HARN_CONFORMANCE_JOBS=)
if grep -Eq -- '--jobs([=[:space:]]|$)' <<<"$automatic"; then
  echo "conformance target emitted an empty worker limit" >&2
  printf '%s\n' "$automatic" >&2
  exit 1
fi

echo "conformance worker and timeout budget propagation passed"
