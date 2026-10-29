#!/usr/bin/env bash
# Pinned language servers for harn-hostlib's rename fixtures, which run with
# HARN_EXT_LSP_FIXTURES_REQUIRED=1 so a missing server fails instead of skipping.
set -euo pipefail

# The npm registry and Go module proxy are the only network reads in this step;
# a transient failure there is the only way it flakes, so retry each install a
# bounded number of times and still fail loudly when every attempt fails.
attempts="${HARN_LSP_INSTALL_ATTEMPTS:-3}"

with_retry() {
  local attempt=1
  until "$@"; do
    if ((attempt >= attempts)); then
      echo "::error::'$*' failed after ${attempts} attempts" >&2
      return 1
    fi
    echo "attempt ${attempt}/${attempts} failed for '$*'; retrying" >&2
    sleep $((attempt * 5))
    attempt=$((attempt + 1))
  done
}

with_retry npm install --global --no-audit --no-fund \
  typescript@5.9.3 typescript-language-server@6.0.2 pyright@1.1.414
GOTOOLCHAIN=auto with_retry go install golang.org/x/tools/gopls@v0.24.0
echo "$(go env GOPATH)/bin" >> "$GITHUB_PATH"
