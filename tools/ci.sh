#!/usr/bin/env bash
# Run CircleCI on demand (docs/CI.md). Nothing runs on push; this starts a pipeline.
#
#   tools/ci.sh check [branch]     build, Rust tests, clippy, Chromium browser journeys
#   tools/ci.sh full  [branch]     the check plus WebKit, Firefox and the long runs
#   tools/ci.sh status [number]    the workflows of a pipeline (default: the latest)
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
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
