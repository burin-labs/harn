#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: scripts/verify_release_tag_main_ancestry.sh --tag vX.Y.Z[-PRERELEASE] [--repo PATH]

Verify that an immutable remote release tag selects the genuine matching
Release squash commit on origin/main. The command is read-only with respect to
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

git -C "$repo" fetch --quiet --no-tags origin main "$tag_target"
main_head="$(git -C "$repo" rev-parse --verify 'origin/main^{commit}')"
if ! git -C "$repo" merge-base --is-ancestor "$tag_target" "$main_head"; then
  echo "error: origin/$tag targets $tag_target, which is not reachable from origin/main $main_head" >&2
  exit 1
fi

read -r -a ancestry <<<"$(git -C "$repo" rev-list --parents -n 1 "$tag_target")"
if ((${#ancestry[@]} != 2)); then
  echo "error: origin/$tag target must be a one-parent squash commit on main" >&2
  exit 1
fi
parent="${ancestry[1]}"
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

echo "verified origin/$tag -> $tag_target: matching Release v$version squash commit on origin/main"
