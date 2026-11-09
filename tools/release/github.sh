#!/usr/bin/env bash
# The GitHub release of a version (docs/RELEASING.md). Needs GITHUB_TOKEN (contents: write).
#
#   tools/release/github.sh draft VERSION DIST   create (or replace) the draft release vVERSION
#                                                with every file in DIST; drafts are private
#   tools/release/github.sh publish VERSION      make the draft public (after approval)
#
# The notes are CHANGELOG.md's "## VERSION" section.
set -euo pipefail

repo=${GITHUB_REPOSITORY:-alephgrid/alephgrid}
api=https://api.github.com/repos/$repo
: "${GITHUB_TOKEN:?set GITHUB_TOKEN (a token with contents: write on $repo)}"
gh() { curl -fsS -H "Authorization: Bearer $GITHUB_TOKEN" -H "Accept: application/vnd.github+json" -H "X-GitHub-Api-Version: 2022-11-28" "$@"; }

mode=$1 version=$2 tag="v$2"
release_id() { gh "$api/releases?per_page=100" | jq -r --arg t "$tag" '[.[] | select(.tag_name == $t)][0].id // empty'; }

case "$mode" in
  draft)
    dist=$3
    notes=$(awk -v v="$version" '$0 ~ "^## " { if (found) exit; if (index($0, "## " v) == 1) { found = 1; next } } found' CHANGELOG.md)
    [ -n "$notes" ] || { echo "CHANGELOG.md has no \"## $version\" section" >&2; exit 1; }
    old=$(release_id)
    if [ -n "$old" ]; then
      draft=$(gh "$api/releases/$old" | jq -r .draft)
      [ "$draft" = true ] || { echo "$tag is already published; not touching it" >&2; exit 1; }
      gh -X DELETE "$api/releases/$old" >/dev/null
    fi
    body=$(jq -n --arg t "$tag" --arg n "Alephgrid $version" --arg b "$notes" '{tag_name: $t, name: $n, body: $b, draft: true}')
    id=$(gh -X POST "$api/releases" -d "$body" | jq -r .id)
    for f in "$dist"/*; do
      gh -X POST -H "Content-Type: application/octet-stream" --data-binary @"$f" \
        "https://uploads.github.com/repos/$repo/releases/$id/assets?name=$(basename "$f")" >/dev/null
      echo "uploaded $(basename "$f")"
    done
    echo "draft release $tag ready: https://github.com/$repo/releases (drafts are visible only to maintainers)"
    ;;
  publish)
    id=$(release_id)
    [ -n "$id" ] || { echo "no release $tag" >&2; exit 1; }
    gh -X PATCH "$api/releases/$id" -d '{"draft": false, "make_latest": "true"}' | jq -r '"published " + .html_url'
    ;;
  *)
    sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
