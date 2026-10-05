#!/bin/sh
# Keeps a systemd-run synththing server up to date (docs/DEPLOY.md): if the
# newest GitHub release is newer than the installed binary, downloads its
# Linux build, checks it against the SHA-256 GitHub publishes for it,
# swaps it in and restarts the service. Run by synththing-update.timer;
# safe to run by hand. Optional: without the timer, update by hand. Needs
# curl and python3 (both standard on Ubuntu and Debian).
set -eu

REPO="Dissssy/synththing"
ASSET="synththing-linux-x86_64"
BINARY="/opt/synththing/synththing"
SERVICE="synththing"

installed=$("$BINARY" --version 2>/dev/null | awk '{print $2}')
# The newest release's version, and its Linux build's address and SHA-256.
read -r latest url expected <<EOF
$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | python3 -c '
import json, sys
release = json.load(sys.stdin)
asset = next((a for a in release.get("assets", []) if a["name"] == sys.argv[1]), None)
if asset is None:
    sys.exit("the newest release has no " + sys.argv[1])
digest = (asset.get("digest") or "").removeprefix("sha256:")
print(release["tag_name"].lstrip("v"), asset["browser_download_url"], digest or "-")
' "$ASSET")
EOF

newest=$(printf '%s\n%s\n' "$installed" "$latest" | sort -V | tail -n 1)
if [ "$installed" = "$latest" ] || [ "$newest" != "$latest" ]; then
    echo "up to date ($installed)"
    exit 0
fi
if [ "$expected" = "-" ]; then
    echo "the release has no checksum for $ASSET" >&2
    exit 1
fi

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
curl -fsSL "$url" -o "$tmp"
actual=$(sha256sum "$tmp" | awk '{print $1}')
if [ "$actual" != "$expected" ]; then
    echo "checksum mismatch: expected $expected, got $actual" >&2
    exit 1
fi

install -m 755 "$tmp" "$BINARY.new"
mv "$BINARY.new" "$BINARY"
systemctl restart "$SERVICE"
echo "updated $installed -> $latest"
