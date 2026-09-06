#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: scripts/verify_release_tag_main_ancestry.sh --tag vX.Y.Z[-PRERELEASE] [--repo PATH]

Verify that an immutable remote release tag selects a genuine matching release
on main or a trusted signed release candidate based on main. The command is read-only with respect to
the remote and does not trust ambient local tag refs.
EOF
  exit 2
}

tag=""
repo="."
while (($# > 0)); do
  case "$1" in
    --tag)
      tag="${2:-}"
      shift 2
      ;;
    --repo)
      repo="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage
      ;;
  esac
done

if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]]; then
  echo "error: expected canonical release tag, got '${tag:-<empty>}'" >&2
  exit 2
fi
if [[ ! -d "$repo/.git" && ! -f "$repo/.git" ]]; then
  echo "error: not a git worktree: $repo" >&2
  exit 2
fi

remote_rows="$(
  git -C "$repo" ls-remote --tags origin "refs/tags/$tag" "refs/tags/$tag^{}"
)"
tag_object="$(awk -v ref="refs/tags/$tag" '$2 == ref {print $1}' <<<"$remote_rows")"
tag_target="$(awk -v ref="refs/tags/$tag^{}" '$2 == ref {print $1}' <<<"$remote_rows")"
if [[ ! "$tag_object" =~ ^[0-9a-f]{40}$ || ! "$tag_target" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: origin/$tag is missing or is not an annotated tag resolving to one exact commit" >&2
  exit 1
fi

git -C "$repo" fetch --quiet --no-tags origin main "$tag_object"
main_head="$(git -C "$repo" rev-parse --verify 'origin/main^{commit}')"
candidate=false
if ! git -C "$repo" merge-base --is-ancestor "$tag_target" "$main_head"; then
  # Trust policy comes from main, never from the unmerged candidate being judged.
  # Verify the remote tag object itself so an ambient local tag cannot supply
  # a signature for different bytes. The signed marker is durable certification
  # authority after the transient release-certify ref has been cleaned up.
  policy_file="$(mktemp "${TMPDIR:-/tmp}/harn-release-signers.XXXXXX")"
  trap 'rm -f "$policy_file"' EXIT
  tag_body="$(git -C "$repo" cat-file tag "$tag_object")"
  # A GPG signature can verify against a local keyring without consulting the
  # SSH allowlist. The candidate contract admits only SSH release identities.
  if [[ "$(grep -c '^-----BEGIN SSH SIGNATURE-----$' <<<"$tag_body" || true)" != 1 ]] ||
      grep -q '^-----BEGIN PGP SIGNATURE-----$' <<<"$tag_body" ||
      ! git -C "$repo" show "$main_head:.github/release-bot-allowed-signers" >"$policy_file" ||
      ! git -C "$repo" -c "gpg.ssh.allowedSignersFile=$policy_file" verify-tag "$tag_object" >/dev/null 2>&1; then
    echo "error: origin/$tag is not reachable from origin/main and has no trusted candidate signature" >&2
    exit 1
  fi
  metadata="$(sed '/^-----BEGIN SSH SIGNATURE-----/,$d' <<<"$tag_body" | awk '/^Harn-Release-Candidate:/ {print}')"
  if [[ "$metadata" != "Harn-Release-Candidate: $tag_target" ]]; then
    echo "error: origin/$tag signed candidate metadata does not name exactly $tag_target" >&2
    exit 1
  fi
  certify_ref="refs/heads/release-certify/$tag_target"
  certify_rows="$(git -C "$repo" ls-remote origin "$certify_ref")"
  if [[ -n "$certify_rows" && "$(awk '{print $1}' <<<"$certify_rows")" != "$tag_target" ]]; then
    echo "error: origin/$tag candidate certification ref moved" >&2
    exit 1
  fi
  candidate=true
fi

read -r -a ancestry <<<"$(git -C "$repo" rev-list --parents -n 1 "$tag_target")"
if ((${#ancestry[@]} != 2)); then
  echo "error: origin/$tag target must be a one-parent squash commit on main" >&2
  exit 1
fi
parent="${ancestry[1]}"
if [[ "$candidate" == true ]] && ! git -C "$repo" merge-base --is-ancestor "$parent" "$main_head"; then
  echo "error: origin/$tag candidate parent is not reachable from origin/main" >&2
  exit 1
fi
version="$(git -C "$repo" show "$tag_target:Cargo.toml" | awk -F'"' '/^version = "/ {print $2; exit}')"
parent_version="$(git -C "$repo" show "$parent:Cargo.toml" | awk -F'"' '/^version = "/ {print $2; exit}')"
if [[ -z "$version" || "v$version" != "$tag" ]]; then
  echo "error: origin/$tag target reports workspace version '${version:-<missing>}'" >&2
  exit 1
fi
if [[ "$parent_version" == "$version" ]]; then
  echo "error: origin/$tag target did not introduce workspace version $version" >&2
  exit 1
fi
subject="$(git -C "$repo" show -s --format=%s "$tag_target")"
if [[ ! "$subject" =~ ^Release\ v${version//./\.}([[:space:]]+\(#[0-9]+\))?$ ]]; then
  echo "error: origin/$tag target is not the matching Release squash commit (subject: $subject)" >&2
  exit 1
fi

echo "verified origin/$tag -> $tag_target: matching Release v$version (trusted candidate=$candidate)"
