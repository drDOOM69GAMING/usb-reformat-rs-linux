# Bundled NTFS formatter

`mkntfs` and `libntfs-3g.so.91` in this directory are copied unmodified from the
Arch/CachyOS `ntfsprogs` and `libntfs-3g` packages (2026.9.28). `build.rs` bakes
both into the executable, and `src/ntfstool.rs` unpacks them into a private root
owned directory (mode 0700) when the system has no `mkntfs` of its own.

Why carry a copy: this program is often copied around as a single file, and
`mkntfs` normally lives in a system package. Baking it in means the NTFS option
always works. A system `mkfs.ntfs` or `mkntfs` is preferred at runtime when one
exists, so the bundled copy is a fallback rather than the default.

`mkntfs` links against `libntfs-3g.so.91`, so both files are needed. It is run
with `LD_LIBRARY_PATH` pointing at the directory it was unpacked into.

## Licence

ntfs-3g is GPL-2.0-or-later. These binaries are redistributed unmodified.
Source: <https://github.com/tuxera/ntfs-3g>

Delete this directory and rebuild if you would rather depend on the system
package; NTFS then reports a clear "install ntfsprogs" message instead.

## Refreshing the copy

`./fetch.sh` downloads the current packages and replaces the two files.
