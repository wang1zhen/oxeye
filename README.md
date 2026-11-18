# Oxeye

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md)

A lightweight desktop image viewer built with Rust and egui. It focuses on fast folder browsing, large-image viewing, and a minimal cross-platform UI. Lightweight editing is planned.

## Features

- Fast folder browsing with a bottom preview strip.
- Read CBZ/ZIP, CBR/RAR, and CB7/7z archives as virtual folders without extraction.
- Large images shown with original detail, using tiled rendering.
- Zoom, pan, fit-to-window, actual size, 90° rotation, and fullscreen.
- Copy the current image to the clipboard.
- Right-to-left Book mode with paired pages, automatic wide-page detection, and `W` override.
- Resume reading progress in archives.
- Decoded neighbor preload for quicker navigation.
- Simple, uncluttered interface.

## Roadmap

Planned, not implemented: lightweight editing with crop, saved rotation, and multi-image collage. See [docs/roadmap.md](docs/roadmap.md) for the detailed roadmap and current status.

## Download

Installers are distributed through [GitHub Releases](https://github.com/wang1zhen/oxeye/releases).
Windows MSI is unsigned and may trigger SmartScreen. macOS DMG has only an ad-hoc
signature (no Developer ID/notarization): use right-click **Open** or allow it in System Settings if Gatekeeper blocks it.
Linux x86_64 packages target Ubuntu 22.04 / glibc 2.35 or newer; AppImage still needs
host OpenGL/EGL, X11/Wayland libraries, and a desktop portal.
Third-party notices: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Quick start

```sh
cargo run --release --locked -- path/to/image.png
```

Without a path, Oxeye opens an empty window. Use Open, `Ctrl`/`Cmd`+`O`, or drop a file onto the window.

## Formats

- Images: PNG, JPEG, WebP, BMP, ICO, and TIFF; GIF is the animation format.
- Archives: CBZ/ZIP, CBR/RAR, and CB7/7z are read as virtual folders without extracting files.
- Solid RAR/7z pages may require decoding earlier pages. Progress fingerprints use archive path, length, and modification time, so same-metadata replacements can be missed.

## Controls

| Action | Keys |
| --- | --- |
| Open | `Ctrl`+`O` / `Cmd`+`O` |
| Previous / next | `PgUp` / `PgDn` |
| Jump 10 files | `Up` / `Down` |
| Toggle Book mode | `M` |
| Toggle wide page | `W` |
| Fit / actual size | `1` or `9` / `0` |
| Zoom | `+`, `-`, or wheel |
| Copy image | `Ctrl`+`C` / `Cmd`+`C` |
| Rotate (display only) | `Ctrl`+`R` / `Ctrl`+`L` (`Cmd` on macOS) |
| Fullscreen | `F11` or `Alt`+`Enter` |
| Settings | `Ctrl`+`,` / `Cmd`+`,` |
| Quit | `Ctrl`+`W` / `Cmd`+`W` |

Full keyboard details: [docs/development.md](docs/development.md#keyboard-map).

## Build

Rust 1.95 or newer; tested with Rust 1.99. Oxeye runs on Linux, macOS, and Windows, and CI passes on all three platforms. Platform packages and build details are in [docs/development.md](docs/development.md#platform-prerequisites).
