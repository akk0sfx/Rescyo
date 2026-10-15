#!/bin/sh
# Builds HFS+, APFS and DMG test images with macOS's own tools. Runs only on macOS, only on
# files under OUT: every image is created from a folder, attached as a virtual disk backed by
# that file (never a physical disk), changed through the macOS driver, and detached.
#
# Usage: corpus/apple/build.sh OUT
set -eu
OUT=${1:?usage: build.sh OUT}
[ "$(uname)" = Darwin ] || { echo "macOS only" >&2; exit 2; }
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
SRC="$OUT/src"
rm -rf "$SRC" "$OUT"/*.img "$OUT"/*.dmg "$OUT/mnt"
mkdir -p "$SRC" "$OUT/mnt"
python3 "$(dirname "$0")/files.py" "$SRC" "$OUT/files.json"

image() { # name fs
    hdiutil create -quiet -size 32m -fs "$2" -volname RESCYO -layout NONE -format UDRW -srcfolder "$SRC" "$OUT/$1.dmg"
    hdiutil attach -quiet -nobrowse -noautoopen -mountpoint "$OUT/mnt" "$OUT/$1.dmg"
    rm "$OUT/mnt/trash/deleted.bin"
    sync
    hdiutil detach -quiet "$OUT/mnt"
    mv "$OUT/$1.dmg" "$OUT/$1.img"
}
image hfs "HFS+"
image apfs APFS

# The HFS+ image in every DMG encoding, for the DMG reader.
for fmt in UDRO UDZO UDBZ ULFO ULMO; do
    hdiutil convert -quiet "$OUT/hfs.img" -format "$fmt" -o "$OUT/hfs-$fmt.dmg"
done
rm -rf "$SRC" "$OUT/mnt"
ls -la "$OUT"
