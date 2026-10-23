#!/usr/bin/env bash
# Pinned language servers for harn-hostlib's rename fixtures, which run with
# HARN_EXT_LSP_FIXTURES_REQUIRED=1 so a missing server fails instead of skipping.
set -euo pipefail

npm install --global --no-audit --no-fund \
  typescript@5.9.3 typescript-language-server@6.0.2 pyright@1.1.414
GOTOOLCHAIN=auto go install golang.org/x/tools/gopls@v0.24.0
echo "$(go env GOPATH)/bin" >> "$GITHUB_PATH"
