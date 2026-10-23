#!/bin/sh
# Keeps a systemd-run synththing server up to date (docs/DEPLOY.md): if the
# newest GitHub release is newer than the installed binary, downloads its
# Linux build, checks it against the SHA-256 GitHub publishes for it,
# swaps it in and restarts the service. Also keeps the front page in
# WEBSITE (if that folder exists) at the newest release's
# deploy/website/index.html, and its docs in WEBSITE/docs. Run by
# synththing-update.timer; safe to run by hand. Optional: without the timer, update by hand. Needs
# curl and python3 (both standard on Ubuntu and Debian).
set -eu

REPO="Dissssy/synththing"
ASSET="synththing-linux-x86_64"
BINARY="/opt/synththing/synththing"
SERVICE="synththing"
WEBSITE="/var/www/synththing"
# (An admin's "update now", from the server: synththing-update.path.)
rm -f /var/lib/synththing/update.request

installed=$("$BINARY" --version 2>/dev/null | awk '{print $2}')
release=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest")
# asset NAME: the newest release's version, then NAME's address and
# SHA-256 ("-" for each if the release doesn't have it).
asset() {
    printf '%s' "$release" | python3 -c '
import json, sys
release = json.load(sys.stdin)
asset = next((a for a in release.get("assets", []) if a["name"] == sys.argv[1]), {})
digest = (asset.get("digest") or "").removeprefix("sha256:")
print(release["tag_name"].lstrip("v"), asset.get("browser_download_url") or "-", digest or "-")
' "$1"
}
read -r latest url expected <<EOF
$(asset "$ASSET")
EOF

# The docs (book.toml), from the release's synththing-docs.tar.gz, into
# WEBSITE/docs, swapped in whole once it's downloaded and checked.
update_docs() {
    read -r _ docs_url docs_sha <<EOF
$(asset synththing-docs.tar.gz)
EOF
    if [ "$docs_sha" = "-" ] || [ "$docs_sha" = "$(cat "$WEBSITE/docs/.sha256" 2>/dev/null)" ]; then
        return 0
    fi
    dir=$(mktemp -d)
    curl -fsSL "$docs_url" -o "$dir/docs.tar.gz" &&
        [ "$(sha256sum "$dir/docs.tar.gz" | awk '{print $1}')" = "$docs_sha" ] &&
        mkdir "$dir/docs" && tar -xzf "$dir/docs.tar.gz" -C "$dir/docs" &&
        echo "$docs_sha" > "$dir/docs/.sha256" && chmod -R a+rX "$dir/docs" &&
        rm -rf "$WEBSITE/docs.old" && { [ ! -d "$WEBSITE/docs" ] || mv "$WEBSITE/docs" "$WEBSITE/docs.old"; } &&
        mv "$dir/docs" "$WEBSITE/docs" && rm -rf "$WEBSITE/docs.old" &&
        echo "docs updated to $latest"
    status=$?
    rm -rf "$dir"
    return $status
}

# The front page and docs, as of the newest release (a failure here leaves
# the old ones and doesn't hold up the server's own update).
if [ -d "$WEBSITE" ]; then
    page=$(mktemp)
    if curl -fsSL "https://raw.githubusercontent.com/$REPO/v$latest/deploy/website/index.html" -o "$page"; then
        if ! cmp -s "$page" "$WEBSITE/index.html"; then
            install -m 644 "$page" "$WEBSITE/index.html"
            echo "front page updated to $latest"
        fi
    else
        echo "no front page in $latest" >&2
    fi
    rm -f "$page"
    update_docs || echo "couldn't update the docs to $latest" >&2
fi

newest=$(printf '%s\n%s\n' "$installed" "$latest" | sort -V | tail -n 1)
if [ "$installed" = "$latest" ] || [ "$newest" != "$latest" ]; then
    echo "up to date ($installed)"
    exit 0
fi
if [ "$url" = "-" ]; then
    echo "the newest release has no $ASSET" >&2
    exit 1
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
