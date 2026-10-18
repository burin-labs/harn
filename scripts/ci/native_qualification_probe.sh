#!/usr/bin/env bash
# Measure both permission and refusal through the enforcing host adapter.
set -euo pipefail
test -x /usr/bin/sandbox-exec
scratch=$(mktemp -d "${TMPDIR:-/tmp}/harn-confinement.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
profile='(version 1)(deny default)(allow process-exec)(allow file-read*)'
/usr/bin/sandbox-exec -p "$profile" /usr/bin/true
if /usr/bin/sandbox-exec -p "$profile" /usr/bin/touch "$scratch/forbidden"; then
  echo '::error::Process confinement allowed the forbidden write.' >&2
  exit 1
fi
test ! -e "$scratch/forbidden"
echo 'Process confinement: allowed execution and refused write measured.'
