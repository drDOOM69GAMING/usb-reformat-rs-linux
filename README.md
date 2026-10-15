# usb-reformat-linux

[![CI](https://github.com/drDOOM69GAMING/usb-reformat-rs-linux/actions/workflows/ci.yml/badge.svg)](https://github.com/drDOOM69GAMING/usb-reformat-rs-linux/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/drDOOM69GAMING/usb-reformat-rs-linux?sort=semver)](https://github.com/drDOOM69GAMING/usb-reformat-rs-linux/releases)
[![Downloads](https://img.shields.io/github/downloads/drDOOM69GAMING/usb-reformat-rs-linux/total)](https://github.com/drDOOM69GAMING/usb-reformat-rs-linux/releases)
[![License](https://img.shields.io/github/license/drDOOM69GAMING/usb-reformat-rs-linux)](LICENSE)
[![Rust](https://img.shields.io/badge/language-Rust-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![GTK4](https://img.shields.io/badge/GUI-GTK4-4A8CF7?logo=gtk&logoColor=white)](https://www.gtk.org/)
[![Platform](https://img.shields.io/badge/OS-Linux-FCC624?logo=linux&logoColor=black)](#requirements)
[![Last commit](https://img.shields.io/github/last-commit/drDOOM69GAMING/usb-reformat-rs-linux)](https://github.com/drDOOM69GAMING/usb-reformat-rs-linux/commits/main)
[![Issues](https://img.shields.io/github/issues/drDOOM69GAMING/usb-reformat-rs-linux)](https://github.com/drDOOM69GAMING/usb-reformat-rs-linux/issues)
[![Stars](https://img.shields.io/github/stars/drDOOM69GAMING/usb-reformat-rs-linux)](https://github.com/drDOOM69GAMING/usb-reformat-rs-linux/stargazers)
[![Commit activity](https://img.shields.io/github/commit-activity/m/drDOOM69GAMING/usb-reformat-rs-linux)](https://github.com/drDOOM69GAMING/usb-reformat-rs-linux/graphs/commit-activity)

A Linux port of the original Windows utility with the same clean, Windows 98 style GUI. It wipes, repartitions and formats USB sticks and fixed disks. All privileged operations run through `pkexec`, so the main app stays unprivileged and safe.

This tool only manages disks. It never backs anything up, never images anything, and never restores anything.

## Download

Prebuilt x86_64 binaries are on the [Releases](https://github.com/drDOOM69GAMING/usb-reformat-rs-linux/releases) page:

```sh
curl -LO https://github.com/drDOOM69GAMING/usb-reformat-rs-linux/releases/latest/download/usb-reformat
chmod +x usb-reformat
./usb-reformat
```

The binary is self-contained: the music and the NTFS formatter are both baked in, so one file is all you need.

## Features

- GTK4 GUI with a Windows 98 style theme, starting in dark mode with a one-click switch back to the classic light palette.
- Lists physical disks (not every partition as a separate entry).
- Flags system disks as SYSTEM and blocks wiping the disk that backs `/`.
- Detects SSDs (via `lsblk`, `rota` and bus) and blocks full (multi-pass) wipe. USB sticks are spared from that rule.
- Asks for a second confirmation before wiping fixed (non-removable) disks.
- Supports MBR (DOS) partition tables with single partition layouts.
- Formats to exFAT, FAT32, NTFS or ext4. NTFS works even on a machine without `ntfsprogs`, because the formatter is carried inside the binary.
- Verifies the resulting layout after formatting.
- Pause and cancel a long wipe gracefully.
- Real progress from the worker, with machine-readable markers in the log so the GUI can stay in sync.
- Built-in chiptune player (optional, enabled by default): the tracker modules are baked into the binary, so a lone executable still has music. A `music/` folder beside the program overrides them. Can be disabled with `--no-default-features`.
- CLI: `--list` (read-only), `--status` (check required tools), `--help`. The `--run` family of flags is only used by the GUI when spawning the privileged worker via `pkexec`.

## Requirements

On CachyOS/Arch Linux you need:

- `pkexec` (polkit)
- `lsblk`, `findmnt`, `wipefs`, `sfdisk`, `blockdev` (util-linux)
- `mkfs.exfat` (exfatprogs), `mkfs.vfat` (dosfstools), `mkfs.ext4` (e2fsprogs)
- `udisksctl` (udisks2)
- GTK4 4.24 (runtime and development headers)
- Optional (for music): ALSA/PulseAudio/PipeWire and the `rodio` backend
- Optional tools not strictly required but nice: `partprobe`/`blkid`

One command covers the formatting and authorisation tools:

```sh
sudo pacman -S --needed exfatprogs dosfstools e2fsprogs util-linux polkit
```

NTFS is the exception. It normally needs `mkntfs` from `ntfsprogs`, but this build carries its own copy, so NTFS formatting works with nothing extra installed. A system `ntfsprogs` is preferred when it is present, so installing it is still worthwhile if you want the newest version.

## Build

```sh
cd usb-reformat-linux
cargo build --release
```

To build without GUI or without music (CLI only, no GTK/rodio dependencies):

```sh
cargo build --no-default-features --release
```

## Usage

Run the GUI normally:

```sh
./target/release/usb-reformat
```

The GUI will prompt for `pkexec` authorisation when you confirm a wipe/format operation. It never runs the worker as your user.

Read-only commands:

```sh
./target/release/usb-reformat --list
./target/release/usb-reformat --status
```

## Safety notes

- The disk backing `/` (the system root disk) is refused.
- Full wipe is blocked on SSDs (except for removable USB sticks). Choose the quick "no pass" option for SSDs when appropriate.
- Fixed disks require an extra "Type WIPE to confirm" step.
- The wipe writes with `O_DIRECT` where the device supports it, so bytes go straight to the drive instead of piling up in the page cache. On a filesystem that does not support it, the buffered path is used but flushed every few megabytes, so the backlog stays small and a slow or failing drive cannot stall the whole machine.
- The worker monitors `pause` and `cancel` flag files in a control directory, so you can pause/resume or cancel. The wipe checks those flags between chunks, so a drive that has stopped responding can delay the stop until the current write returns.
- If a drive is unplugged or fails mid-wipe, the write is reported with the likely cause (unplugged, or full, which usually points to a fake capacity stick) instead of hanging.
- Log files are created with mode 0644 in a temp directory so the unprivileged GUI can read them while the privileged worker writes.

## Music

The collection is compiled into the executable, so a single copied file plays music with no folder beside it. Anything on disk takes priority over the baked-in copy:

1. `USB_REFORMAT_MUSIC` environment variable. When set it is authoritative: an empty or missing directory means silence, not a fall back.
2. A `music/` directory beside the executable, then up to two parent directories. This is what lets `target/release/usb-reformat` find the `music/` folder at the repository root, whatever directory you launch it from.
3. `music/` in the current working directory
4. `~/.local/share/usb-reformat/music/`
5. `usb-reformat/music/` under each entry in `XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`)

Drop tracker files (`.xm`, `.it`, `.mod`, `.s3m`) into one of those directories to swap the soundtrack without rebuilding. `build.rs` bakes the contents of `music/` into the binary, so a checkout without that directory builds a silent player rather than a broken one.

## Bundled components

`build.rs` bakes two things into the executable:

- The tracker modules in `music/`, so the player works from a single file.
- `tools/ntfs/mkntfs` and its `libntfs-3g.so.91`, so NTFS can be created without `ntfsprogs` installed.

The NTFS formatter is only unpacked by the privileged worker, into a private directory that root created with `mkdtemp` (mode 0700). Nothing on a user writable path is ever executed as root. A system `mkfs.ntfs` or `mkntfs` is always preferred when one exists, so the bundled copy is a fallback rather than the default.

`mkntfs` and `libntfs-3g.so.91` come from [ntfs-3g](https://github.com/tuxera/ntfs-3g) and are GPL-2.0-or-later; they are redistributed here unmodified. Delete `tools/ntfs` and rebuild if you would rather depend on the system package.

## Credits

Based on the original [usb-reformat-rs](https://github.com/drDOOM69GAMING/usb-reformat-rs) (Windows). Tracker playback uses [xmrs](https://github.com/silvioq/xmrs) and [xmrsplayer](https://github.com/silvioq/xmrsplayer), rendered through [rodio](https://github.com/RustAudio/rodio).

## License

MIT. See [LICENSE](LICENSE) for details.