#!/usr/bin/env bash
# Administrator-installed literal namespace attachments; CI never loads them.
set -euo pipefail

readonly profile=/etc/apparmor.d/harn-ci-e2e-userns
readonly receipt=/etc/harn/ci-e2e-namespace.json
readonly parser=/usr/sbin/apparmor_parser
readonly schema=harn.ci_e2e_namespace.v1

fail() {
  echo "Missing Linux namespace confinement prerequisite: $*" >&2
  exit 1
}

attachment() {
  local value="$1" resolved
  [[ "$value" =~ ^/[A-Za-z0-9_./-]+$ && "/$value/" != *'/../'* ]] ||
    fail 'launcher attachments must be absolute literal paths'
  resolved="$(realpath -m -- "$value")"
  [[ "$resolved" =~ ^/[A-Za-z0-9_./-]+$ ]] ||
    fail 'resolved launcher attachments must be literal paths'
  printf '%s\n' "$resolved"
}

trusted_path() {
  local path="$1" owner mode
  while :; do
    [[ -e "$path" && ! -L "$path" ]] || fail "missing or symbolic prerequisite: $path"
    owner="$(stat -c %u -- "$path")"
    mode="$(stat -c %a -- "$path")"
    if [[ "$owner" != 0 ]] || (( (8#$mode & 8#022) != 0 )); then
      fail "prerequisite is not root-owned and protected: $path"
    fi
    [[ "$path" != / ]] || break
    path="$(dirname -- "$path")"
  done
}

install_profile() {
  [[ "$(id -u)" == 0 ]] || fail 'installation requires the host administrator'
  (( $# >= 1 && $# <= 64 )) || fail 'install needs one to 64 exact launcher paths'
  trusted_path "$parser"
  trusted_path /etc
  if [[ ! -e /etc/harn && ! -L /etc/harn ]]; then
    install -d -o root -g root -m 755 /etc/harn
  fi
  trusted_path /etc/harn
  trusted_path /etc/apparmor.d
  local paths=() path selector generated manifest='' digest
  for path in "$@"; do paths+=("$(attachment "$path")"); done
  selector="$(IFS=,; echo "${paths[*]}")"
  if (( ${#paths[@]} > 1 )); then selector="{$selector}"; fi
  generated="$(mktemp /etc/apparmor.d/.harn-netns-XXXXXX)"
  trap 'rm -f -- "$generated" "$manifest"' EXIT
  manifest="$(mktemp /etc/harn/.harn-netns-XXXXXX)"
  printf 'abi <abi/4.0>,\nprofile harn-ci-e2e-userns "%s" flags=(unconfined) {\n  userns,\n}\n' \
    "$selector" > "$generated"
  digest="$(sha256sum "$generated")"
  digest="${digest%% *}"
  printf '%s\n' "${paths[@]}" | jq -Rsc --arg schema "$schema" --arg hash "$digest" \
    '{schema:$schema, launchers:(split("\n")[:-1]), profileSha256:$hash}' > "$manifest"
  # Root loads only generated literal attachments. No CI-controlled profile
  # input and no sudoers rule are involved.
  "$parser" --replace "$generated"
  chmod 644 "$generated" "$manifest"
  mv -f -- "$generated" "$profile"
  mv -f -- "$manifest" "$receipt"
  trap - EXIT
  echo "Installed namespace grant for ${#paths[@]} exact paths; no runner sudo privilege added."
}

verify_profile() {
  [[ $# == 1 ]] || fail 'verify needs the selected launcher path'
  trusted_path "$profile"
  trusted_path "$receipt"
  [[ -f "$profile" && -f "$receipt" ]] || fail 'installation files must be regular files'
  local launcher digest
  launcher="$(attachment "$1")"
  digest="$(sha256sum "$profile")"
  digest="${digest%% *}"
  jq -e --arg schema "$schema" --arg launcher "$launcher" --arg hash "$digest" \
    '.schema == $schema and (.launchers | type == "array") and
     (.launchers | index($launcher) != null) and .profileSha256 == $hash' \
    "$receipt" > /dev/null || fail "no matching installed attachment for $launcher"
  echo 'Root-owned namespace prerequisite present; the required E2E case must prove the loaded grant.'
}

case "${1:-}" in
  install) shift; install_profile "$@" ;;
  verify) shift; verify_profile "$@" ;;
  *) fail 'usage: e2e_namespace_profile.sh install PATH... | verify PATH' ;;
esac
