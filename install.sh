#!/bin/sh
# Harn installer.
#
# Usage:
#   curl -fsSL https://harnlang.com/install.sh | sh
#
# Environment variables:
#   HARN_VERSION         pin to a specific release tag, e.g. v0.8.19.
#                        Defaults to the latest GitHub release.
#   HARN_INSTALL_DIR     install destination directory. When unset, the
#                        script tries $XDG_BIN_DIR, $HOME/bin,
#                        $HOME/.local/bin, then $HOME/.harn/bin.
#   HARN_NO_VERIFY       set to 1 to skip SHA256 checksum verification
#                        (not recommended).
#   HARN_NO_MODIFY_PATH  set to 1 to suppress PATH update guidance.
#
# The script downloads a signed tarball from the GitHub release matching
# your OS/arch, verifies it against the release's SHA256SUMS manifest,
# and installs the `harn`, `harn-dap`, and `harn-lsp` binaries.

set -eu

REPO="burin-labs/harn"
RELEASES_URL="https://github.com/${REPO}/releases"
API_URL="https://api.github.com/repos/${REPO}/releases"

bold=""
dim=""
red=""
yellow=""
green=""
reset=""
if [ -t 1 ] && [ -t 2 ] && command -v tput >/dev/null 2>&1; then
  if [ "$(tput colors 2>/dev/null || echo 0)" -ge 8 ]; then
    bold="$(tput bold)"
    dim="$(tput dim)"
    red="$(tput setaf 1)"
    yellow="$(tput setaf 3)"
    green="$(tput setaf 2)"
    reset="$(tput sgr0)"
  fi
fi

info()  { printf '%s%s%s\n' "$dim"   "$1" "$reset"; }
note()  { printf '%s%s%s\n' "$bold"  "$1" "$reset"; }
warn()  { printf '%s%s%s\n' "$yellow" "$1" "$reset" >&2; }
ok()    { printf '%s%s%s\n' "$green" "$1" "$reset"; }
die()   { printf '%serror:%s %s\n' "$red" "$reset" "$1" >&2; exit 1; }

require() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

require uname
require tar
require mkdir
require mktemp

# Detect a downloader. curl is preferred; fall back to wget so the
# script works on minimal Linux images.
if command -v curl >/dev/null 2>&1; then
  DOWNLOADER="curl"
elif command -v wget >/dev/null 2>&1; then
  DOWNLOADER="wget"
else
  die "neither curl nor wget is available"
fi

download_to() {
  # download_to URL DEST  — fails loudly on HTTP errors.
  url="$1"
  dest="$2"
  if [ "$DOWNLOADER" = "curl" ]; then
    curl --fail --silent --show-error --location --output "$dest" "$url"
  else
    wget --quiet --output-document="$dest" "$url"
  fi
}

# Detect OS + arch.
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
  darwin)
    case "$ARCH" in
      arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
      x86_64|amd64)  TARGET="x86_64-apple-darwin" ;;
      *) die "unsupported macOS architecture: $ARCH" ;;
    esac
    ;;
  linux)
    case "$ARCH" in
      x86_64|amd64) TARGET="x86_64-unknown-linux-gnu" ;;
      aarch64|arm64) TARGET="aarch64-unknown-linux-gnu" ;;
      *) die "unsupported Linux architecture: $ARCH" ;;
    esac
    ;;
  msys*|mingw*|cygwin*)
    die "Windows installs use harn-x86_64-pc-windows-msvc.zip from ${RELEASES_URL}"
    ;;
  *)
    die "unsupported OS: $OS"
    ;;
esac

# Resolve version. HARN_VERSION pins a specific tag; otherwise we follow
# the redirect on /releases/latest, which is cheaper and more reliable
# than parsing the JSON API.
VERSION="${HARN_VERSION:-}"
if [ -z "$VERSION" ]; then
  info "Resolving latest release..."
  if [ "$DOWNLOADER" = "curl" ]; then
    VERSION="$(curl --silent --location --head --write-out '%{url_effective}' \
      --output /dev/null "${RELEASES_URL}/latest" 2>/dev/null | sed 's#.*/tag/##')"
  else
    VERSION="$(wget --quiet --max-redirect=5 --server-response --spider \
      "${API_URL}/latest" 2>&1 | grep -i '^  *location:' | tail -1 \
      | sed 's#.*/tag/##' | tr -d '\r')"
  fi
  if [ -z "$VERSION" ]; then
    # Fall back to the JSON API.
    tmp_json="$(mktemp)"
    trap 'rm -f "$tmp_json"' EXIT
    if download_to "${API_URL}/latest" "$tmp_json"; then
      VERSION="$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$tmp_json" | head -1)"
    fi
    rm -f "$tmp_json"
    trap - EXIT
  fi
  [ -n "$VERSION" ] || die "could not determine latest release tag"
fi
case "$VERSION" in
  v[0-9]*) ;;
  [0-9]*) VERSION="v${VERSION}" ;;
  *) die "invalid version: $VERSION (expected v0.0.0)" ;;
esac

ASSET="harn-${TARGET}.tar.gz"
ASSET_URL="${RELEASES_URL}/download/${VERSION}/${ASSET}"
CHECKSUMS_URL="${RELEASES_URL}/download/${VERSION}/SHA256SUMS"

note "Installing harn ${VERSION} for ${TARGET}"

# Resolve install dir per precedence: HARN_INSTALL_DIR > XDG_BIN_DIR >
# $HOME/bin (when existing & on PATH) > $HOME/.local/bin (when existing
# & on PATH) > $HOME/.harn/bin (created lazily).
in_path() {
  case ":${PATH:-}:" in
    *":$1:"*) return 0 ;;
    *) return 1 ;;
  esac
}

INSTALL_DIR=""
PATH_HINT=""
if [ -n "${HARN_INSTALL_DIR:-}" ]; then
  INSTALL_DIR="$HARN_INSTALL_DIR"
elif [ -n "${XDG_BIN_DIR:-}" ]; then
  INSTALL_DIR="$XDG_BIN_DIR"
elif [ -n "${HOME:-}" ] && [ -d "$HOME/bin" ] && in_path "$HOME/bin"; then
  INSTALL_DIR="$HOME/bin"
elif [ -n "${HOME:-}" ] && [ -d "$HOME/.local/bin" ] && in_path "$HOME/.local/bin"; then
  INSTALL_DIR="$HOME/.local/bin"
else
  [ -n "${HOME:-}" ] || die "HOME is unset; pass HARN_INSTALL_DIR explicitly"
  INSTALL_DIR="$HOME/.harn/bin"
  if ! in_path "$INSTALL_DIR"; then
    PATH_HINT="$INSTALL_DIR"
  fi
fi

mkdir -p "$INSTALL_DIR" || die "cannot create $INSTALL_DIR"
[ -w "$INSTALL_DIR" ] || die "$INSTALL_DIR is not writable"

# Stage download in a temp dir so a failed install never leaves a
# half-written binary on PATH.
TMPDIR_BASE="${TMPDIR:-/tmp}"
WORKDIR="$(mktemp -d "${TMPDIR_BASE}/harn-install.XXXXXX")"
cleanup() { rm -rf "$WORKDIR"; }
trap cleanup EXIT INT TERM HUP

info "Downloading ${ASSET}"
download_to "$ASSET_URL" "$WORKDIR/$ASSET" \
  || die "failed to download $ASSET_URL"

if [ "${HARN_NO_VERIFY:-0}" = "1" ]; then
  warn "SHA256 verification skipped (HARN_NO_VERIFY=1)"
else
  info "Verifying checksum"
  if ! download_to "$CHECKSUMS_URL" "$WORKDIR/SHA256SUMS" 2>/dev/null; then
    warn "SHA256SUMS not published for $VERSION; skipping verification"
  else
    expected="$(awk -v name="$ASSET" '$2 == name || $2 == "*"name { print $1 }' \
      "$WORKDIR/SHA256SUMS" | head -1)"
    [ -n "$expected" ] || die "no checksum for $ASSET in SHA256SUMS"
    if command -v sha256sum >/dev/null 2>&1; then
      actual="$(sha256sum "$WORKDIR/$ASSET" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
      actual="$(shasum -a 256 "$WORKDIR/$ASSET" | awk '{print $1}')"
    else
      die "neither sha256sum nor shasum is available; set HARN_NO_VERIFY=1 to skip"
    fi
    [ "$expected" = "$actual" ] || die "checksum mismatch: expected $expected, got $actual"
  fi
fi

info "Extracting"
tar -xzf "$WORKDIR/$ASSET" -C "$WORKDIR"

for bin in harn harn-dap harn-lsp; do
  [ -f "$WORKDIR/$bin" ] || continue
  install -m 0755 "$WORKDIR/$bin" "$INSTALL_DIR/$bin"
done

ok "Installed harn ${VERSION} to ${INSTALL_DIR}"

if [ -n "$PATH_HINT" ] && [ "${HARN_NO_MODIFY_PATH:-0}" != "1" ]; then
  echo
  note "Add ${PATH_HINT} to your PATH:"
  # shellcheck disable=SC2016  # $PATH must stay literal in the printed snippet
  printf '  %sexport PATH="%s:$PATH"%s\n' "$bold" "$PATH_HINT" "$reset"
  echo
  info "Then restart your shell or run \`hash -r\`."
fi

if [ "$INSTALL_DIR/harn" != "$(command -v harn 2>/dev/null || true)" ] \
  && [ -z "$PATH_HINT" ]; then
  warn "harn is installed at $INSTALL_DIR/harn but another version may be ahead of it on PATH"
fi

echo
info "Run \`harn quickstart\` to set up a project, or \`harn --help\` to explore."
