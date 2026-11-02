#!/usr/bin/env bash
# Run CircleCI on demand (docs/CI.md). Nothing runs on push; this starts a pipeline.
#
#   tools/ci.sh check [branch]     build, Rust tests, clippy, Chromium browser journeys
#   tools/ci.sh full  [branch]     the check plus WebKit, Firefox and the long runs
#   tools/ci.sh status [number]    the workflows of a pipeline (default: the latest)
#   tools/ci.sh release vX.Y.Z     build, test and package a release from tag vX.Y.Z (pushed);
#                                  publishes only after approval in CircleCI (docs/RELEASING.md)
#
# branch defaults to the current git branch, which must be pushed.
#
# Needs CIRCLECI_TOKEN (a personal API token: CircleCI → User settings → Personal API Tokens).
# Projects connected through CircleCI's GitHub OAuth app use the slug gh/alephgrid/alephgrid
# (the default). Projects connected through the CircleCI GitHub App need
#   CIRCLECI_PROJECT_SLUG=circleci/<organization id>/<project id>
#   CIRCLECI_DEFINITION_ID=<pipeline definition id>
# both shown under Project settings.

set -euo pipefail

API=https://circleci.com/api/v2
SLUG=${CIRCLECI_PROJECT_SLUG:-gh/alephgrid/alephgrid}
need() {
  : "${CIRCLECI_TOKEN:?set CIRCLECI_TOKEN to a CircleCI personal API token (see docs/CI.md)}"
  command -v jq >/dev/null || { echo "needs jq" >&2; exit 1; }
}

call() { # method path [json]
  curl -fsS -X "$1" "$API$2" -H "Circle-Token: $CIRCLECI_TOKEN" -H "Content-Type: application/json" ${3:+-d "$3"}
}

app_url() { # pipeline number
  case "$SLUG" in
    gh/*) echo "https://app.circleci.com/pipelines/github/${SLUG#gh/}/$1" ;;
    *) echo "https://app.circleci.com/pipelines/$SLUG/$1" ;;
  esac
}

case "${1:-}" in
  check|full)
    need
    branch=${2:-$(git rev-parse --abbrev-ref HEAD)}
    if [ "$1" = check ]; then params='{"run_check":true}'; else params='{"run_full":true}'; fi
    if [ -n "${CIRCLECI_DEFINITION_ID:-}" ]; then
      body=$(jq -n --arg d "$CIRCLECI_DEFINITION_ID" --arg b "$branch" --argjson p "$params" \
        '{definition_id:$d, config:{branch:$b}, checkout:{branch:$b}, parameters:$p}')
      reply=$(call POST "/project/$SLUG/pipeline/run" "$body")
    else
      body=$(jq -n --arg b "$branch" --argjson p "$params" '{branch:$b, parameters:$p}')
      reply=$(call POST "/project/$SLUG/pipeline" "$body")
    fi
    number=$(jq -r '.number' <<<"$reply")
    echo "Started $1 on $branch: pipeline $number"
    app_url "$number"
    ;;
  release)
    need
    tag=${2:?usage: tools/ci.sh release vX.Y.Z}
    [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "a release tag looks like v0.1.0" >&2; exit 2; }
    # Release exactly the commit this checkout tagged: the tag on origin must name the same
    # commit as the local tag, and that commit must be on main. CircleCI builds (and runs
    # the release config of) whatever the remote tag names when it starts.
    # An annotated tag is listed twice; "^{}" is the commit it names.
    remote_commit=$(git ls-remote origin "refs/tags/$tag" "refs/tags/$tag^{}" | awk '{c=$1} /\^\{\}$/{p=$1} END{print (p ? p : c)}')
    [ -n "$remote_commit" ] || { echo "tag $tag is not on origin; push it first (docs/RELEASING.md)" >&2; exit 1; }
    local_commit=$(git rev-parse --verify --quiet "$tag^{commit}") || { echo "tag $tag is not in this checkout; git fetch --tags" >&2; exit 1; }
    [ "$remote_commit" = "$local_commit" ] || { echo "tag $tag on origin is $remote_commit, here it is $local_commit: not releasing" >&2; exit 1; }
    git fetch --quiet origin main
    git merge-base --is-ancestor "$local_commit" origin/main || { echo "tag $tag ($local_commit) is not on main: not releasing" >&2; exit 1; }
    echo "Releasing $tag at $local_commit"
    params=$(jq -n --arg v "${tag#v}" '{run_release: true, release_version: $v}')
    if [ -n "${CIRCLECI_DEFINITION_ID:-}" ]; then
      body=$(jq -n --arg d "$CIRCLECI_DEFINITION_ID" --arg t "$tag" --argjson p "$params" '{definition_id:$d, config:{tag:$t}, checkout:{tag:$t}, parameters:$p}')
      reply=$(call POST "/project/$SLUG/pipeline/run" "$body")
    else
      body=$(jq -n --arg t "$tag" --argjson p "$params" '{tag:$t, parameters:$p}')
      reply=$(call POST "/project/$SLUG/pipeline" "$body")
    fi
    number=$(jq -r '.number' <<<"$reply")
    echo "Started release $tag: pipeline $number. Approve \"approve-publish\" there once the draft release looks right."
    app_url "$number"
    ;;
  status)
    need
    if [ -n "${2:-}" ]; then
      id=$(call GET "/project/$SLUG/pipeline/$2" | jq -r '.id'); number=$2
    else
      latest=$(call GET "/project/$SLUG/pipeline" | jq '.items[0]')
      id=$(jq -r '.id' <<<"$latest"); number=$(jq -r '.number' <<<"$latest")
    fi
    echo "Pipeline $number"
    call GET "/pipeline/$id/workflow" | jq -r '.items[] | "  \(.name): \(.status)"'
    app_url "$number"
    ;;
  *)
    sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
