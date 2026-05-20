#!/usr/bin/env bash
set -uo pipefail
set -x

root="$(cd "$(dirname "$0")/.." && pwd)"
exe="$root/src-tauri/target/release/scan2bim-converter"
[ -f "$exe" ] || { echo "FAIL: Tauri release binary not found at $exe" >&2; ls -la "$root/src-tauri/target/release/" || true; exit 1; }

version="$(node -p "require('$root/package.json').version")"
stage="$root/dist/Scan2BIM-Converter-$version-macOS-arm64"
out_archive="$stage.zip"

rm -rf "$stage" "$out_archive"
mkdir -p "$stage/binaries"

echo "=== bin layout going in ==="
ls -la "$root/binaries-mac/" || true
ls -la "$root/binaries-mac/pdal/" 2>/dev/null || echo "no pdal dir"

cp "$exe" "$stage/"
chmod +x "$stage/scan2bim-converter"

if [ -f "$root/binaries-mac/PotreeConverter" ]; then
  cp "$root/binaries-mac/PotreeConverter" "$stage/binaries/"
  chmod +x "$stage/binaries/PotreeConverter"
else
  echo "WARN: no PotreeConverter binary found under binaries-mac/"
fi

[ -d "$root/binaries-mac/resources" ] && cp -R "$root/binaries-mac/resources" "$stage/binaries/" || true
[ -d "$root/binaries-mac/licenses" ]  && cp -R "$root/binaries-mac/licenses"  "$stage/binaries/" || true

mkdir -p "$stage/binaries/pdal"
if [ -d "$root/binaries-mac/pdal/bin" ]; then
  cp -R "$root/binaries-mac/pdal/bin" "$stage/binaries/pdal/"
fi
if [ -d "$root/binaries-mac/pdal/lib" ]; then
  cp -R "$root/binaries-mac/pdal/lib" "$stage/binaries/pdal/"
fi
if [ -d "$root/binaries-mac/pdal/share" ]; then
  cp -R "$root/binaries-mac/pdal/share" "$stage/binaries/pdal/"
fi

cat > "$stage/README.txt" <<EOF
Scan2BIM Converter $version (macOS ARM64)

To run:  open the folder and double-click "scan2bim-converter".

UNSIGNED BUILD - macOS Gatekeeper will block the first launch with
a "cannot be opened because the developer cannot be verified"
warning. To bypass once:

  - Right-click "scan2bim-converter" in Finder and choose "Open"
  - Confirm in the dialog
  - After this it runs normally on every future launch

Or run from a terminal:
  xattr -dr com.apple.quarantine "Scan2BIM-Converter-$version-macOS-arm64"
  ./scan2bim-converter

Portable build - move the whole folder anywhere. binaries/ holds
the PDAL and PotreeConverter binaries the app uses.

Tested on macOS 14 (Sonoma) on Apple Silicon.

(c) Sohail Saifi for Patrick Staeding
EOF

ditto -c -k --sequesterRsrc --keepParent "$stage" "$out_archive"

if [ -f "$out_archive" ]; then
  size_mb=$(du -m "$out_archive" | cut -f1)
  echo
  echo "DONE: $out_archive (${size_mb} MB)"
else
  echo "FAIL: ditto did not produce $out_archive" >&2
  exit 1
fi
