#!/bin/sh
# Replace the bundled NTFS formatter with the versions from the current packages.
#
# Downloads (does not install) ntfsprogs and libntfs-3g, extracts mkntfs and its
# library, and drops them beside this script.
set -eu

here=$(dirname "$0")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cd "$tmp"

for url in $(pacman -Sp ntfsprogs ntfs-3g 2>/dev/null); do
    echo "downloading $(basename "$url")"
    curl -sSLO "$url"
done

for package in *.pkg.tar.*; do
    tar --zstd -xf "$package"
done

cp usr/bin/mkntfs "$here/mkntfs"
cp usr/lib/libntfs-3g.so.91 "$here/libntfs-3g.so.91"
chmod 0755 "$here/mkntfs"
chmod 0644 "$here/libntfs-3g.so.91"

echo "updated $here"
echo "now rebuild so build.rs picks up the new files"
