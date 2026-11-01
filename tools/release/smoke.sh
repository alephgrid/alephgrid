#!/usr/bin/env bash
# Smoke-test a release binary before anything is published (docs/RELEASING.md):
#
#   tools/release/smoke.sh BINARY VERSION
#
# starts it on an empty data directory, checks the version it serves, creates a workbook,
# imports a CSV into it, reads the rows back, and stops it.
set -euo pipefail

bin=$1 version=$2
dir=$(mktemp -d)
port=$(( 20000 + RANDOM % 20000 ))
cleanup() { [ -n "${pid:-}" ] && kill "$pid" 2>/dev/null; rm -rf "$dir"; }
trap cleanup EXIT

"$bin" --data-dir "$dir/data" --port "$port" > "$dir/log" 2>&1 &
pid=$!
url="http://127.0.0.1:$port"
for _ in $(seq 1 100); do curl -fs "$url/api/config" >/dev/null 2>&1 && break; sleep 0.2; done
config=$(curl -fs "$url/api/config") || { cat "$dir/log"; echo "the server did not start" >&2; exit 1; }
echo "$config" | grep -q "\"version\":\"$version\"" || { echo "unexpected config: $config" >&2; exit 1; }

# The UI is embedded: the home page is served.
curl -fs "$url/" | grep -qi "<html" || { echo "the embedded UI is missing" >&2; exit 1; }

id=$(curl -fs -X POST "$url/api/workbooks" -H 'content-type: application/json' -d '{"name":"smoke"}' | sed -E 's/.*"id":"([^"]+)".*/\1/')
[ -n "$id" ] || { echo "could not create a workbook" >&2; exit 1; }
printf 'city,people\nPune,7\nOslo,3\n' > "$dir/cities.csv"
curl -fs -X POST "$url/api/workbooks/$id/import?filename=cities.csv" --data-binary @"$dir/cities.csv" -H 'content-type: text/csv' > "$dir/import.json" \
  || { cat "$dir/log"; echo "the CSV import failed" >&2; exit 1; }
grep -q '"row_count":2' "$dir/import.json" || { echo "unexpected import result: $(cat "$dir/import.json")" >&2; exit 1; }
echo "smoke test passed: $("$bin" --version)"
