#!/usr/bin/env bash
# Tells the people whose feedback led to what a release fixed or added
# (docs/feedback-intake.md, "What came of it"). Run by release.yml once the
# release is out, for the tag being released: the pull requests merged
# since the tag before it, the issues each one closed as done, the feedback
# numbers on those issues (intake writes "Feedback #12, #14" in the issue
# and its comments), all sent to the official server's intake, which
# notifies those who asked to hear. A bug is "fixed", anything else "added".
#
# Needs GH_TOKEN, INTAKE_TOKEN, GITHUB_REPOSITORY and the tag as $1; the
# server is $SERVER (the official one by default). DRY_RUN=1 only prints.
set -euo pipefail

tag=$1
version=${tag#v}
server=${SERVER:-https://synththing.p51.nl}

previous=$(git describe --tags --abbrev=0 --match 'v*' "$tag^" 2>/dev/null || true)
if [ -z "$previous" ]; then
  echo "no release before $tag, so nothing to tell"
  exit 0
fi

query='query($owner: String!, $name: String!, $pr: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $pr) {
      closingIssuesReferences(first: 25) {
        nodes {
          number
          stateReason
          labels(first: 30) { nodes { name } }
          body
          comments(first: 100) { nodes { body } }
        }
      }
    }
  }
}'

# The feedback numbers written in some text: "feedback #12", "Feedback #12, #14 and #15".
numbers() {
  grep -oiE 'feedback #[0-9]+(( *, *| *(and|&) *)#[0-9]+)*' | grep -oE '[0-9]+' | sort -un || true
}

prs=$(git log --merges --format=%s "$previous..$tag" | sed -n 's/^Merge pull request #\([0-9]*\).*/\1/p' | sort -un)
for pr in $prs; do
  issues=$(gh api graphql -F owner="${GITHUB_REPOSITORY%/*}" -F name="${GITHUB_REPOSITORY#*/}" -F pr="$pr" -f query="$query" \
    --jq '.data.repository.pullRequest.closingIssuesReferences.nodes[] | select(.stateReason == "COMPLETED") | @json')
  while IFS= read -r issue; do
    [ -n "$issue" ] || continue
    number=$(jq -r .number <<<"$issue")
    outcome=$(jq -r 'if any(.labels.nodes[]; .name == "bug") then "fixed" else "added" end' <<<"$issue")
    feedback=$(jq -r '[.body] + [.comments.nodes[].body] | join("\n")' <<<"$issue" | numbers | jq -sc .)
    if [ "$feedback" = "[]" ]; then
      continue
    fi
    body=$(jq -nc --argjson feedback "$feedback" --arg outcome "$outcome" --arg version "$version" \
      '{feedback: $feedback, outcome: $outcome, version: $version}')
    echo "#$number (via #$pr): $body"
    if [ -n "${DRY_RUN:-}" ]; then
      continue
    fi
    status=$(curl -sS -o reply.json -w '%{http_code}' -X POST "$server/api/v1/intake/resolved" \
      -H "Authorization: Bearer $INTAKE_TOKEN" -H "Content-Type: application/json" --data "$body")
    if [ "$status" = 200 ]; then
      echo "  told $(jq -r .told reply.json)"
    else
      # (The server may not have this release yet; the release isn't held back for it.)
      echo "::warning::couldn't tell the senders of #$number's feedback ($status): $(cat reply.json)"
    fi
  done <<<"$issues"
done
