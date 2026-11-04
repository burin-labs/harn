#!/usr/bin/env bash
# Fail unless the account running this job is isolated from the host's other users and from signing material.
#
# Runs as the first step of the owned macOS job. Each probe attempts something the account must not be able to
# do and fails the job if it succeeds, so isolation is checked on every run rather than once.
#
# What it can show: the account cannot list or read another user's home (runner credentials, keychains, config),
# holds no code-signing identity in any keychain it can search, is not an administrator and cannot use sudo,
# cannot read other users' process environments, and carries no secret-shaped environment variable.
# What it cannot show: the absence of a macOS or kernel privilege-escalation bug, or of a secret stored somewhere
# no probe names.
set -uo pipefail

failures=0
fail() { echo "ISOLATION FAILURE: $*" >&2; failures=$((failures + 1)); }
pass() { echo "isolated: $*"; }

self="$(id -un)"

# 1. Not an administrator, no sudo.
if id -Gn | tr ' ' '\n' | grep -qx admin; then fail "$self is in the admin group"; else pass "not in the admin group"; fi
if sudo -n true >/dev/null 2>&1; then fail "$self can use sudo without a password"; else pass "no passwordless sudo"; fi

# 2. Other users' homes are closed. Every other local account with a home under /Users.
others=0
for home in /Users/*; do
  [[ -d "$home" ]] || continue
  owner="$(stat -f %Su "$home")"
  [[ "$owner" == "$self" || "$home" == /Users/Shared ]] && continue
  others=$((others + 1))
  if ls "$home" >/dev/null 2>&1; then fail "can list $home (owner $owner)"; else pass "cannot list $home"; fi
  for secret in actions-runner/.credentials actions-runner/.credentials_rsaparams actions-runner/.runner \
                Library/Keychains/login.keychain-db .ssh .config .aws .netrc .npmrc .docker; do
    if [[ -e "$home/$secret" ]] && cat "$home/$secret" >/dev/null 2>&1; then fail "can read $home/$secret"; fi
  done
done
echo "other user homes checked: $others"

# 3. No code-signing identity is reachable through any keychain this account searches.
identities="$(security find-identity -v -p codesigning 2>&1 | grep -Eo '^ *[0-9]+ valid identities found' | grep -Eo '[0-9]+' | head -1)"
if [[ "${identities:-x}" == 0 ]]; then pass "no code-signing identities in the keychain search list"; else fail "code-signing identities visible: ${identities:-unreadable}"; fi
if security find-generic-password -a notarytool >/dev/null 2>&1; then fail "a notarization credential is readable"; else pass "no notarization credential"; fi

# 4. Other users' process environments are not readable.
leaked=0
while read -r pid user; do
  [[ "$user" == "$self" || "$user" == root ]] && continue
  if ps -E -p "$pid" -o command= 2>/dev/null | grep -qE '(GITHUB_TOKEN|ACTIONS_RUNTIME_TOKEN|ACTIONS_ID_TOKEN|NOTARY|APPLE_)='; then leaked=1; fi
done < <(ps -axo pid=,user= 2>/dev/null)
if [[ "$leaked" == 0 ]]; then pass "no other user's process environment shows runner tokens"; else fail "another user's process environment is readable"; fi

# 5. No secret-shaped variable is present in this job's own environment beyond the job token the runner supplies.
secrets="$(env | cut -d= -f1 | grep -Ei '(SECRET|PASSWORD|NOTARY|SIGNING|APPLE_ID|PRIVATE_KEY|API_KEY)' || true)"
if [[ -z "$secrets" ]]; then pass "no secret-shaped environment variables"; else fail "secret-shaped variables present: $(echo "$secrets" | tr '\n' ' ')"; fi

if (( failures > 0 )); then
  echo "runner isolation: ${failures} failure(s); refusing to run pull-request code" >&2
  exit 1
fi
echo "runner isolation: all probes held"
