#!/usr/bin/env bash
# Package one release binary (docs/RELEASING.md):
#
#   tools/release/package.sh VERSION TARGET BINARY OUTDIR
#
# writes OUTDIR/alephgrid-VERSION-TARGET.tar.gz (the binary, LICENSE, README.md, CHANGELOG.md
# in a folder of the same name) and its .sha256. The binary must report VERSION.
set -euo pipefail

version=$1 target=$2 bin=$3 out=$4
# CROSS=1: built for another CPU than this machine's, so it cannot be run here; the version
# is checked where it can run (the smoke test of a native build of the same commit).
if [ -z "${CROSS:-}" ]; then reported=$("$bin" --version 2>/dev/null || true); else reported="alephgrid $version"; fi
if [ "$reported" != "alephgrid $version" ]; then
  echo "the binary reports \"$reported\", expected \"alephgrid $version\"" >&2
  exit 1
fi

name="alephgrid-$version-$target"
work=$(mktemp -d)
mkdir -p "$work/$name" "$out"
cp "$bin" "$work/$name/alephgrid"
chmod 755 "$work/$name/alephgrid"
cp LICENSE README.md CHANGELOG.md "$work/$name/"
tar -C "$work" -czf "$out/$name.tar.gz" "$name"
rm -rf "$work"
cd "$out"
if command -v sha256sum >/dev/null; then sha256sum "$name.tar.gz"; else shasum -a 256 "$name.tar.gz"; fi > "$name.tar.gz.sha256"
echo "packaged $out/$name.tar.gz"
