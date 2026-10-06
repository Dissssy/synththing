#!/bin/sh
# Move the official publisher to a new key, on the official server's VPS
# (synththing.p51.nl's own setup: not something a self-hosted server needs).
#
# Asks for the current publisher secret key (not shown as it's typed), has
# the official authority rotate it to a new key (the official scripts move
# to it, the old key stops working, apps follow), then hands the new secret
# to GitHub's SYNTHTHING_PUBLISH_KEY secret with `gh` if it's installed and
# signed in, or else leaves it in a file only root can read. Either way:
# keep the new key somewhere safe too (Bitwarden).
#
#   sh /opt/synththing/rotate-publisher.sh
set -eu

BINARY=/opt/synththing/synththing
SERVER=https://synththing.p51.nl
REPO=Dissssy/synththing
OUT=/root/synththing-publish.key

printf 'Current publisher secret key (hex, not shown): '
stty -echo
read -r OLD
stty echo
printf '\n'

umask 077
NEW=$(SYNTHTHING_PUBLISH_KEY="$OLD" "$BINARY" publisher-rotate --server "$SERVER")
OLD=

if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    printf '%s' "$NEW" | gh secret set SYNTHTHING_PUBLISH_KEY -R "$REPO"
    echo "GitHub's SYNTHTHING_PUBLISH_KEY is the new key now."
    echo "Keep it somewhere safe too: it's in $OUT until you delete it."
fi
printf '%s\n' "$NEW" > "$OUT"
NEW=
echo "The new secret key is in $OUT (only root can read it)."
echo "Put it in GitHub (Settings > Secrets and variables > Actions > SYNTHTHING_PUBLISH_KEY) if gh didn't,"
echo "and in Bitwarden, then: shred -u $OUT"
