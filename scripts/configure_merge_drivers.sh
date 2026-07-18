#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# `harn-generated` is wired to `true` so noisy generated files (the language
# spec mirrors and syntax keywords) never produce conflict
# markers during merge. The driver succeeds without writing %A, so git keeps
# the current side and trusts the user (or a follow-up hook) to regenerate
# from the authoring source.
#
# WARNING: this is only safe when something else regenerates the mirror
# afterwards. The pre-commit hook handles plain commits; the
# `.githooks/post-rewrite` hook handles single-commit rebases; the
# pre-push generated-artifact checks (including `make check-language-spec`)
# are the final guard before CI. Without one of
# those, a rebase silently drops regenerated updates.
git config merge.harn-generated.name "Keep current generated file during merge; regenerate after merge"
git config merge.harn-generated.driver true

echo "Configured Harn merge drivers"
