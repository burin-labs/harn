#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
installer="$repo_root/.github/actions/apt-install/install.sh"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/harn-apt-install-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT

mkdir -p "$fixture_root/bin" "$fixture_root/runner" "$fixture_root/sources"
calls="$fixture_root/calls"
printf 'deb https://packages.microsoft.com/ubuntu/24.04/prod noble main\n' \
  >"$fixture_root/sources/microsoft.list"

cat >"$fixture_root/bin/sudo" <<'SCRIPT'
#!/usr/bin/env bash
exec "$@"
SCRIPT

cat >"$fixture_root/bin/timeout" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$APT_TEST_CALLS"
if [[ "$*" == *" apt-get "*" update" ]] && [[ ! -e "$APT_TEST_FIRST_UPDATE_FAILED" ]]; then
  : >"$APT_TEST_FIRST_UPDATE_FAILED"
  exit 124
fi
while [[ "$1" == --* || "$1" == *s ]]; do
  if [[ "$1" == "--kill-after=5s" || "$1" == "--signal=TERM" || "$1" == *s ]]; then
    shift
  else
    break
  fi
done
exec "$@"
SCRIPT

cat >"$fixture_root/bin/apt-get" <<'SCRIPT'
#!/usr/bin/env bash
printf 'apt-get %s\n' "$*" >>"$APT_TEST_CALLS"
exit 0
SCRIPT

cat >"$fixture_root/bin/mold" <<'SCRIPT'
#!/usr/bin/env bash
printf 'mold 2.0\n'
SCRIPT

chmod +x "$fixture_root/bin/"*

output="$({
  PATH="$fixture_root/bin:$PATH" \
    APT_PACKAGES=mold \
    CHECK_COMMAND='' \
    RUNNER_TEMP="$fixture_root/runner" \
    APT_SOURCES_DIR="$fixture_root/sources" \
    APT_TEST_CALLS="$calls" \
    APT_TEST_FIRST_UPDATE_FAILED="$fixture_root/first-update-failed" \
    bash "$installer"
} 2>&1)"

if [[ "$output" != *"apt-get update failed or timed out"* ]]; then
  printf 'expected timeout recovery warning, got:\n%s\n' "$output" >&2
  exit 1
fi

update_calls="$(grep -c ' apt-get .* update' "$calls")"
if [[ "$update_calls" -ne 2 ]]; then
  printf 'expected two bounded update attempts, got %s\n' "$update_calls" >&2
  exit 1
fi

if [[ ! -e "$fixture_root/runner/disabled-apt-sources/microsoft.list" ]]; then
  printf 'failed update did not quarantine the optional Microsoft source\n' >&2
  exit 1
fi

if ! grep -q -- '--kill-after=5s 40s apt-get' "$calls"; then
  printf 'update command was not bounded by the owned timeout:\n' >&2
  cat "$calls" >&2
  exit 1
fi

if ! grep -q -- 'Acquire::http::Timeout=15' "$calls"; then
  printf 'apt HTTP timeout was not configured:\n' >&2
  cat "$calls" >&2
  exit 1
fi

if ! grep -q -- ' apt-get .* install -y --no-install-recommends mold' "$calls"; then
  printf 'package installation did not follow recovery:\n' >&2
  cat "$calls" >&2
  exit 1
fi

printf 'apt install action tests passed\n'
