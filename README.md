# Oxeye

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md)

A lightweight desktop image viewer and editor built with Rust and egui. It focuses on fast folder browsing, large-image viewing, quick crop/rotate/flip edits, and a minimal cross-platform UI.

## Features

- Fast folder browsing with a bottom preview strip.
- Read CBZ/ZIP, CBR/RAR, and CB7/7z archives as virtual folders without extraction.
- Large images shown with original detail, using tiled rendering.
- Zoom, pan, fit-to-window, actual size, 90° rotation, and fullscreen.
- Lightweight, non-destructive editing: crop with aspect presets, rotate, flip, and undo. The preview updates before anything is written.
- Overwrite the original with explicit confirmation, or use Save As for PNG/JPEG/WebP/BMP. Writes are atomic; a failed save leaves the original untouched.
- Copy the current image to the clipboard.
- Right-to-left Book mode with paired pages, automatic wide-page detection, and `W` override.
- Resume reading progress in archives.
- Decoded neighbor preload for quicker navigation.
- Simple, uncluttered interface.

## Editing and saving

- Editing supports single static PNG, JPEG, WebP, and BMP images, including archive members (saved as a copy only). GIF, TIFF, and ICO stay read-only; Oxeye rejects editing them instead of silently flattening an animation or multi-page file to one still frame.
- Enter editing with `C`; the current display rotation is baked in once, and exiting restores the original image and direction. Crop presets are Free, 1:1, 4:3, 3:2, 16:9, and Original.
- `Ctrl`/`Cmd`+`S` overwrites the original only after a confirmation that names the full path and target format. `Ctrl`/`Cmd`+`Shift`+`S` writes a copy; an unknown extension is an error, not a guessed format.
- Overwriting uses a same-directory temporary file, `fsync`, and an atomic rename. Cancelling or failing a save leaves the original bytes and permissions unchanged.
- EXIF, ICC/XMP metadata, and animation are not preserved. JPEG is re-encoded lossily (quality defaults to 90); WebP is written lossless and may become larger.

## Roadmap

Released in 0.3.0: lightweight editing, atomic overwrite, and Save As. Still planned: template-based collage and animated GIF editing. See [docs/roadmap.md](docs/roadmap.md) for details and current status.

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

- Writable after editing: PNG, JPEG, WebP, and BMP.
- Read-only: GIF (animation), TIFF, and ICO. They cannot be edited or overwritten in this version; animated GIF editing is not silently downgraded to a still frame.
- Archives: CBZ/ZIP, CBR/RAR, and CB7/7z are read as virtual folders without extracting files. Members can be edited into a new copy via Save As, but cannot be overwritten in place.
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
| Rotate | `Ctrl`+`R` / `Ctrl`+`L` (`Cmd` on macOS); display rotation while viewing, image rotation while editing |
| Enter editing and crop | `C` |
| Apply / cancel crop | `Enter` / `Esc` |
| Flip horizontally / vertically | `X` / `Y` |
| Undo edit | `Ctrl`+`Z` / `Cmd`+`Z` |
| Save (overwrite, with confirmation) | `Ctrl`+`S` / `Cmd`+`S` |
| Save As | `Ctrl`+`Shift`+`S` / `Cmd`+`Shift`+`S` |
| Fullscreen | `F11` or `Alt`+`Enter` |
| Settings | `Ctrl`+`,` / `Cmd`+`,` |
| Quit | `Ctrl`+`W` / `Cmd`+`W` |

`Esc` closes Settings, then cancels a crop selection, then leaves editing, then cancels queued navigation, then leaves fullscreen. Full keyboard details: [docs/development.md](docs/development.md#keyboard-map).

## Build

Rust 1.95 or newer; tested with Rust 1.99. Oxeye runs on Linux, macOS, and Windows, and CI passes on all three platforms. Platform packages and build details are in [docs/development.md](docs/development.md#platform-prerequisites).
