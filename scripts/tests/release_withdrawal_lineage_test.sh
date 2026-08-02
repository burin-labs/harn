#!/usr/bin/env bash
# Exercise the real release-metadata verifier against an immutable tag on a
# divergent commit. The withdrawal is valid only for the exact tagged commit
# and a HEAD descended from the last published release paperwork.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
harn_bin="${HARN_BIN:-}"
if [[ -z "$harn_bin" ]]; then
  harn_bin="$("$repo_root"/scripts/harn_bin.sh --print)"
fi

tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
fixture_repo="$tmp_root/repo"
mkdir -p \
  "$fixture_repo/.github" \
  "$fixture_repo/spec/acp-registry/harn" \
  "$fixture_repo/spec/protocol-artifacts"

cat > "$fixture_repo/Cargo.toml" <<'EOF'
[workspace]

[workspace.package]
version = "0.10.51"
EOF
cat > "$fixture_repo/CHANGELOG.md" <<'EOF'
# Changelog

## v0.10.51

- Withdrawn candidate.

## v0.10.50

- Last published release.
EOF
cat > "$fixture_repo/spec/protocol-artifacts/manifest.json" <<'EOF'
{"artifactVersion":"0.10.51"}
EOF
cat > "$fixture_repo/spec/acp-registry/harn/agent.json" <<'EOF'
{
  "version": "0.10.50",
  "distribution": {
    "binary": {
      "linux": {
        "archive": "https://example.test/releases/download/v0.10.50/harn.tar.gz"
      }
    }
  }
}
EOF

write_registry() {
  local tag_commit="$1"
  cat > "$fixture_repo/.github/release-withdrawals.json" <<EOF
{
  "schema_version": 1,
  "releases": [
    {
      "version": "0.10.51",
      "tag_commit": "$tag_commit",
      "distribution_version": "0.10.50",
      "replacement": "0.10.52",
      "reason": "invalid_tagged_candidate"
    }
  ]
}
EOF
}

write_registry 0000000000000000000000000000000000000000
git -C "$fixture_repo" init -q -b main
git -C "$fixture_repo" config user.name "Harn Release Test"
git -C "$fixture_repo" config user.email "release-test@example.invalid"
git -C "$fixture_repo" add .
git -C "$fixture_repo" -c commit.gpgsign=false commit -q -m "Release v0.10.50"
git -C "$fixture_repo" checkout -q --orphan withdrawn-candidate
git -C "$fixture_repo" -c commit.gpgsign=false commit -q -m "Invalid v0.10.51 candidate"
git -C "$fixture_repo" tag v0.10.51
tag_commit="$(git -C "$fixture_repo" rev-list -n 1 v0.10.51)"
git -C "$fixture_repo" checkout -q main
write_registry "$tag_commit"
git -C "$fixture_repo" add .github/release-withdrawals.json
git -C "$fixture_repo" -c commit.gpgsign=false commit -q -m "Record v0.10.51 withdrawal"

# Prove the standalone publish-tools layout resolves the new sibling module.
release_tools="$tmp_root/release-tools"
"$repo_root/scripts/stage_release_tools.sh" "$release_tools"
(
  cd "$fixture_repo"
  "$harn_bin" run \
    --read-only-root "$release_tools" \
    "$release_tools/release_metadata.harn" \
    -- current --root "$fixture_repo"
) >"$tmp_root/standalone.out" 2>"$tmp_root/standalone.err"
sandbox_notice="sandbox active; extra read-only root: $(cd "$release_tools" && pwd -P)"
if [[ "$(<"$tmp_root/standalone.out")" != "0.10.51" ]]; then
  echo "FAIL: standalone release-tools metadata command did not resolve" >&2
  cat "$tmp_root/standalone.out" "$tmp_root/standalone.err" >&2
  exit 1
fi
if [[ "$(<"$tmp_root/standalone.err")" != "$sandbox_notice" ]]; then
  echo "FAIL: standalone release-tools metadata command emitted unexpected stderr" >&2
  cat "$tmp_root/standalone.err" >&2
  exit 1
fi

run_verifier() {
  local case_name="$1"
  (
    cd "$fixture_repo"
    "$harn_bin" run \
      --read-only-root "$repo_root" \
      "$repo_root/scripts/verify_release_metadata.harn"
  ) >"$tmp_root/$case_name.out" 2>"$tmp_root/$case_name.err"
}

run_verifier valid
sandbox_notice="sandbox active; extra read-only root: $repo_root"
if [[ "$(<"$tmp_root/valid.out")" != "verified release metadata for v0.10.51" ]]; then
  echo "FAIL: exact divergent-tag withdrawal did not verify" >&2
  cat "$tmp_root/valid.out" "$tmp_root/valid.err" >&2
  exit 1
fi
if [[ "$(<"$tmp_root/valid.err")" != "$sandbox_notice" ]]; then
  echo "FAIL: exact divergent-tag withdrawal emitted unexpected stderr" >&2
  cat "$tmp_root/valid.err" >&2
  exit 1
fi

wrong_prefix="a"
if [[ "${tag_commit:0:1}" == "$wrong_prefix" ]]; then
  wrong_prefix="b"
fi
wrong_tag="$wrong_prefix${tag_commit:1}"
write_registry "$wrong_tag"
if run_verifier wrong-tag; then
  echo "FAIL: mismatched immutable tag commit passed" >&2
  exit 1
fi
head_commit="$(git -C "$fixture_repo" rev-parse HEAD)"
expected_wrong_tag="error: current version 0.10.51 is already tagged at $tag_commit, but HEAD ($head_commit) does not include that commit and has no \`Release v0.10.51\` commit of its own; either bump the version or rebase onto the tagged commit before continuing
$sandbox_notice"
if [[ "$(<"$tmp_root/wrong-tag.err")" != "$expected_wrong_tag" ]]; then
  echo "FAIL: mismatched-tag diagnostic drifted" >&2
  cat "$tmp_root/wrong-tag.err" >&2
  exit 1
fi

write_registry "$tag_commit"
git -C "$fixture_repo" checkout -q --orphan unrelated-history
git -C "$fixture_repo" add .
git -C "$fixture_repo" -c commit.gpgsign=false commit -q -m "Unrelated history"
if run_verifier unrelated; then
  echo "FAIL: withdrawal bypassed last-published lineage" >&2
  exit 1
fi
expected_lineage="error: withdrawal for v0.10.51 matches immutable tag commit $tag_commit, but HEAD has no \`Release v0.10.50\` lineage commit
$sandbox_notice"
if [[ "$(<"$tmp_root/unrelated.err")" != "$expected_lineage" ]]; then
  echo "FAIL: withdrawal-lineage diagnostic drifted" >&2
  cat "$tmp_root/unrelated.err" >&2
  exit 1
fi

mv "$fixture_repo/.github/release-withdrawals.json" "$tmp_root/release-withdrawals.json"
if run_verifier missing-registry; then
  echo "FAIL: missing withdrawal registry passed verification" >&2
  exit 1
fi
expected_missing_registry="error: release withdrawal registry is missing: .github/release-withdrawals.json
$sandbox_notice"
if [[ "$(<"$tmp_root/missing-registry.err")" != "$expected_missing_registry" ]]; then
  echo "FAIL: missing-registry diagnostic drifted" >&2
  cat "$tmp_root/missing-registry.err" >&2
  exit 1
fi

echo "release_withdrawal_lineage_test: ok"
