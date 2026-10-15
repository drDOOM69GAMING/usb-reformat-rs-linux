# Music

This directory contains tracker module files (`.xm`, `.it`, `.mod`, `.s3m`) that the built-in chiptune player will pick up automatically.

These tracks are included for ambiance. The GUI's music toggle can enable/disable playback. If you want to use a different music set, you can override with the `USB_REFORMAT_MUSIC` environment variable to point to another directory (and if set, only that directory is searched).

The app will auto-play music if any valid modules are found at startup when the default features include `music`.