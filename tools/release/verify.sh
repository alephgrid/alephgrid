#!/usr/bin/env bash
# Check a release's artifacts against the tagged source before the draft release is made
# (docs/RELEASING.md). Run from the repository root:
#
#   tools/release/verify.sh VERSION DIST
#
# Fails unless DIST holds, for every release target, alephgrid-VERSION-TARGET.tar.gz with a
# matching .sha256, holding exactly the binary, LICENSE, NOTICE, THIRD-PARTY-NOTICES.md,
# README.md and CHANGELOG.md in a folder of the same name, with the notices and licence
# identical to this checkout's; a CycloneDX SBOM (.cdx.json, with a matching .sha256) for
# every target and for the web bundle; and, on Linux x86_64, the x86_64 binary reports
# VERSION.
set -euo pipefail

version=${1:?usage: tools/release/verify.sh VERSION DIST} dist=${2:?usage: tools/release/verify.sh VERSION DIST}
targets="x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu aarch64-apple-darwin x86_64-apple-darwin"
repo=$(pwd)
fail() { echo "release check failed: $*" >&2; exit 1; }
digest() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{print $1}'; }
checksum() { # file: its .sha256 names it and matches
  [ -f "$1" ] || fail "missing $1"
  [ -f "$1.sha256" ] || fail "missing $1.sha256"
  local want named
  read -r want named < "$1.sha256"
  [ "$named" = "$1" ] || fail "$1.sha256 names \"$named\""
  [ "$(digest "$1")" = "$want" ] || fail "$1 does not match $1.sha256"
}

cd "$dist"
for t in $targets; do
  name="alephgrid-$version-$t"
  archive="$name.tar.gz"
  checksum "$archive"
  expected=$(printf '%s\n' alephgrid CHANGELOG.md LICENSE NOTICE README.md THIRD-PARTY-NOTICES.md)
  found=$(tar -tzf "$archive" | sed -e "s|^$name/||" -e '/^$/d' | sort)
  [ "$found" = "$expected" ] || fail "$archive holds: $(echo $found) (expected: $(echo $expected))"
  for f in LICENSE NOTICE THIRD-PARTY-NOTICES.md; do
    tar -xzOf "$archive" "$name/$f" | cmp -s - "$repo/$f" || fail "$archive: $f differs from the tagged source"
  done
  echo "ok  $archive"
done

for t in $targets web; do
  sbom="alephgrid-$version-$t.cdx.json"
  checksum "$sbom"
  node -e 'const b = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    if (b.bomFormat !== "CycloneDX" || !Array.isArray(b.components) || b.components.length === 0) { console.error(process.argv[1] + " is not a CycloneDX SBOM with components"); process.exit(1); }' "$sbom" \
    || fail "$sbom"
  echo "ok  $sbom"
done

if [ "$(uname -s)-$(uname -m)" = "Linux-x86_64" ]; then
  work=$(mktemp -d)
  tar -xzf "alephgrid-$version-x86_64-unknown-linux-gnu.tar.gz" -C "$work"
  reported=$("$work/alephgrid-$version-x86_64-unknown-linux-gnu/alephgrid" --version)
  rm -rf "$work"
  [ "$reported" = "alephgrid $version" ] || fail "the x86_64 Linux binary reports \"$reported\""
  echo "ok  the x86_64 Linux binary reports $version"
else
  echo "--  the binary version is checked on Linux x86_64 only (here: $(uname -s)-$(uname -m))"
fi
echo "release artifacts for $version verified"
