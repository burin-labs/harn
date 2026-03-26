#!/bin/sh
set -e

# Detect OS and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  darwin)
    case "$ARCH" in
      arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
      x86_64) TARGET="x86_64-apple-darwin" ;;
      *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
    esac
    ;;
  linux)
    case "$ARCH" in
      x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
      *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
    esac
    ;;
  *) echo "Unsupported OS: $OS"; exit 1 ;;
esac

# Get latest release
REPO="burin-labs/harn"
LATEST=$(curl -sL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')

if [ -z "$LATEST" ]; then
  echo "Could not determine latest release"
  exit 1
fi

URL="https://github.com/$REPO/releases/download/$LATEST/harn-$TARGET.tar.gz"
echo "Downloading harn $LATEST for $TARGET..."

TMPDIR=$(mktemp -d)
curl -sL "$URL" -o "$TMPDIR/harn.tar.gz"
tar xzf "$TMPDIR/harn.tar.gz" -C "$TMPDIR"

INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
echo "Installing to $INSTALL_DIR/harn..."
install -m 755 "$TMPDIR/harn" "$INSTALL_DIR/harn"
rm -rf "$TMPDIR"

echo "harn $LATEST installed successfully!"
echo "Run 'harn --version' to verify."
