#!/usr/bin/env bash
# Resolve a ChromeDriver from the same Chrome build through Chrome for Testing.
# wasm-pack otherwise downloads the newest stable driver, which can race a
# staged browser rollout and fail before any Wasm test executes.

set -euo pipefail

if [[ -n "${HARN_CHROMEDRIVER:-}" ]]; then
  if [[ ! -x "$HARN_CHROMEDRIVER" ]]; then
    echo "HARN_CHROMEDRIVER is not executable: $HARN_CHROMEDRIVER" >&2
    exit 1
  fi
  printf '%s\n' "$HARN_CHROMEDRIVER"
  exit 0
fi

chrome_binary="${HARN_CHROME_BIN:-}"
platform=""
archive_binary="chromedriver"
case "$(uname -s):$(uname -m)" in
  Darwin:arm64)
    platform="mac-arm64"
    chrome_binary="${chrome_binary:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
    ;;
  Darwin:x86_64)
    platform="mac-x64"
    chrome_binary="${chrome_binary:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
    ;;
  Linux:x86_64)
    platform="linux64"
    if [[ -z "$chrome_binary" ]]; then
      chrome_binary="$(command -v google-chrome-stable || command -v google-chrome || command -v chromium || true)"
    fi
    ;;
  MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64|MINGW*:aarch64|MSYS*:aarch64|CYGWIN*:aarch64)
    platform="win64"
    archive_binary="chromedriver.exe"
    if [[ -z "$chrome_binary" ]]; then
      windows_candidates=(
        "${LOCALAPPDATA:-}/Google/Chrome/Application/chrome.exe"
        "/c/Program Files/Google/Chrome/Application/chrome.exe"
        "/c/Program Files (x86)/Google/Chrome/Application/chrome.exe"
      )
      for candidate in "${windows_candidates[@]}"; do
        if [[ -n "$candidate" && -x "$candidate" ]]; then
          chrome_binary="$candidate"
          break
        fi
      done
    fi
    ;;
  Linux:aarch64|Linux:arm64)
    echo "Chrome for Testing does not publish a Linux ARM64 ChromeDriver; set HARN_CHROMEDRIVER to a compatible local driver" >&2
    exit 1
    ;;
  *)
    echo "no official Chrome for Testing driver mapping for $(uname -s) $(uname -m); set HARN_CHROMEDRIVER" >&2
    exit 1
    ;;
esac

if [[ -z "$chrome_binary" || ! -x "$chrome_binary" ]]; then
  echo "Chrome was not found; set HARN_CHROME_BIN or HARN_CHROMEDRIVER" >&2
  exit 1
fi

for command_name in curl jq unzip; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "$command_name is required to resolve a version-matched ChromeDriver" >&2
    exit 1
  fi
done

chrome_version="$("$chrome_binary" --version | sed -E 's/^[^0-9]*([0-9]+\.[0-9]+\.[0-9]+\.[0-9]+).*$/\1/')"
chrome_build="${chrome_version%.*}"
if [[ ! "$chrome_build" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "could not parse Chrome version from $chrome_binary" >&2
  exit 1
fi

metadata_url="https://googlechromelabs.github.io/chrome-for-testing/known-good-versions-with-downloads.json"
resolved="$({ curl -fsSL "$metadata_url"; } | jq -r \
  --arg prefix "$chrome_build." \
  --arg platform "$platform" \
  '[.versions[]
    | select(.version | startswith($prefix))
    | . as $release
    | .downloads.chromedriver[]?
    | select(.platform == $platform)
    | [$release.version, .url]]
   | last
   | if . == null then empty else @tsv end')"

if [[ -z "$resolved" ]]; then
  echo "Chrome for Testing has no $platform driver for Chrome build $chrome_build; set HARN_CHROMEDRIVER" >&2
  exit 1
fi

driver_version="${resolved%%$'\t'*}"
driver_url="${resolved#*$'\t'}"
tools_root="${TMPDIR:-/tmp}/harn-tools/chromedriver-$driver_version-$platform"
driver_path="$tools_root/chromedriver-$platform/$archive_binary"
if [[ ! -x "$driver_path" ]]; then
  download_dir="$(mktemp -d "${TMPDIR:-/tmp}/harn-chromedriver.XXXXXX")"
  trap 'rm -rf "$download_dir"' EXIT
  curl -fsSL "$driver_url" -o "$download_dir/chromedriver.zip"
  mkdir -p "$tools_root"
  unzip -qo "$download_dir/chromedriver.zip" -d "$tools_root"
  chmod +x "$driver_path"
fi

driver_major="$($driver_path --version | sed -E 's/^ChromeDriver ([0-9]+).*/\1/')"
chrome_major="${chrome_version%%.*}"
if [[ "$driver_major" != "$chrome_major" ]]; then
  echo "resolved ChromeDriver $driver_major does not match Chrome $chrome_major" >&2
  exit 1
fi

printf '%s\n' "$driver_path"
