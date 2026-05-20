#!/usr/bin/env bash
set -uo pipefail
set -x

root="$(cd "$(dirname "$0")/.." && pwd)"
exe="$root/src-tauri/target/release/scan2bim-converter"
[ -f "$exe" ] || { echo "FAIL: Tauri release binary not found at $exe" >&2; ls -la "$root/src-tauri/target/release/" || true; exit 1; }

version="$(node -p "require('$root/package.json').version")"
stage="$root/dist/Scan2BIM-Converter-$version-Linux-x64"
out_archive="$stage.tar.gz"

rm -rf "$stage" "$out_archive"
mkdir -p "$stage/binaries"

echo "=== STAGE: bin layout going in ==="
ls -la "$root/binaries-linux/" || true
ls -la "$root/binaries-linux/pdal/" 2>/dev/null || echo "no pdal dir"

cp "$exe" "$stage/"
chmod +x "$stage/scan2bim-converter"

if [ -f "$root/binaries-linux/PotreeConverter" ]; then
  cp "$root/binaries-linux/PotreeConverter" "$stage/binaries/"
  chmod +x "$stage/binaries/PotreeConverter"
else
  echo "WARN: no PotreeConverter binary found under binaries-linux/"
fi

# Copy ALL shared libs from binaries-linux/ root (laszip etc.) so the
# dynamic linker finds them next to PotreeConverter. cp -L dereferences
# symlinks so versioned files are copied as their target content.
for f in "$root"/binaries-linux/*.so "$root"/binaries-linux/*.so.*; do
  [ -f "$f" ] && cp -L "$f" "$stage/binaries/" || true
done

[ -d "$root/binaries-linux/resources" ] && cp -R "$root/binaries-linux/resources" "$stage/binaries/" || true
[ -d "$root/binaries-linux/licenses" ]  && cp -R "$root/binaries-linux/licenses"  "$stage/binaries/" || true

mkdir -p "$stage/binaries/pdal"
if [ -d "$root/binaries-linux/pdal/bin" ]; then
  cp -R "$root/binaries-linux/pdal/bin" "$stage/binaries/pdal/"
fi
if [ -d "$root/binaries-linux/pdal/lib" ]; then
  cp -R "$root/binaries-linux/pdal/lib" "$stage/binaries/pdal/"
fi
if [ -d "$root/binaries-linux/pdal/share" ]; then
  cp -R "$root/binaries-linux/pdal/share" "$stage/binaries/pdal/"
fi

# 4. README
cat > "$stage/README.txt" <<EOF
Scan2BIM Converter $version (Linux x86_64)

To run:  ./scan2bim-converter

Portable build, no installer. Move the whole
"Scan2BIM-Converter-$version-Linux-x64" folder anywhere and the
app keeps working. binaries/ holds the PDAL and PotreeConverter
binaries the app uses.

Requirements: glibc 2.35+ (Ubuntu 22.04 or newer). WebKitGTK is
required for the Tauri web view: on Ubuntu/Debian install
  sudo apt-get install libwebkit2gtk-4.1-0

(c) Sohail Saifi for Patrick Staeding
EOF

tar -C "$root/dist" -czf "$out_archive" "$(basename "$stage")"

size_mb=$(du -m "$out_archive" | cut -f1)
echo
echo "DONE: $out_archive (${size_mb} MB)"
echo "Test: tar -xzf $out_archive && cd $(basename "$stage") && ./scan2bim-converter"
