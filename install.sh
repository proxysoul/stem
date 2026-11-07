#!/bin/sh
set -eu
REPO="proxysoul/stem"
DIR="${STEM_INSTALL_DIR:-$HOME/.local/bin}"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) TARGET=aarch64-unknown-linux-gnu ;;
  *) echo "stem: no prebuilt binary for $(uname -s) $(uname -m); build from source with: cargo install --git https://github.com/$REPO stem" >&2; exit 1 ;;
esac
URL="https://github.com/$REPO/releases/${STEM_VERSION:+download/$STEM_VERSION}${STEM_VERSION:-latest/download}/stem-$TARGET.tar.gz"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
curl -fsSL "$URL" -o "$TMP/stem.tar.gz"
curl -fsSL "$URL.sha256" -o "$TMP/stem.tar.gz.sha256"
EXPECTED=$(cut -d' ' -f1 "$TMP/stem.tar.gz.sha256")
ACTUAL=$( (shasum -a 256 "$TMP/stem.tar.gz" 2>/dev/null || sha256sum "$TMP/stem.tar.gz") | cut -d' ' -f1)
[ "$EXPECTED" = "$ACTUAL" ] || { echo "stem: checksum mismatch, not installing" >&2; exit 1; }
tar xzf "$TMP/stem.tar.gz" -C "$TMP"
mkdir -p "$DIR"
install -m 755 "$TMP/stem" "$DIR/stem"
echo "stem installed to $DIR/stem"
case ":$PATH:" in *":$DIR:"*) ;; *) echo "add $DIR to your PATH" ;; esac
