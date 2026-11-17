# Oxeye

[English](README.md) | [简体中文](README.zh-CN.md)

Oxeye is a lightweight, read-only desktop image viewer built with Rust and egui/eframe. It opens images from a folder and never changes the files.

## Features

- Single-page reading or right-to-left book spreads with paired pages.
- Large-image viewing without losing original detail.
- GIF animation in Single mode and on both Book pages with independent clocks.
- `P` play/pause for GIFs and `M` to toggle Single/Book mode.
- Zoom, pan, fit-to-window, actual size, 90° rotation, and fullscreen.
- Decoded neighbor preload and a bottom preview strip with clickable thumbnails.
- English, Simplified Chinese, and Japanese, with Automatic following the OS locale.

## Quick start

```sh
cargo run --release --locked -- path/to/image.png
```

Without a path, Oxeye opens an empty window. Use Open, `Ctrl`/`Cmd`+`O`, or drop a file onto the window.

## Controls

| Action | Keys |
| --- | --- |
| Open | `Ctrl`+`O` / `Cmd`+`O` |
| Previous / next | `PgUp` / `PgDn` (in Book: previous/next spread) |
| Jump 10 files | `Up` / `Down` (in Book: keep spreads aligned) |
| Left / Right | Single: pan horizontally when possible, otherwise previous/next. Book: always navigate spreads in the configured direction. |
| Space | Pan down if possible; otherwise Single next / Book next spread |
| Toggle Single / Book | `M` |
| Fit / actual size | `1` or `9` / `0` |
| Zoom | `+`, `-`, wheel, or `Ctrl`+wheel at the pointer |
| Rotate clockwise / counterclockwise | `Ctrl`+`R` / `Ctrl`+`L` (`Cmd` on macOS) |
| Fullscreen | `F11` or `Alt`+`Enter` |
| Play / pause GIFs | `P` |
| Settings | `Ctrl`+`,` / `Cmd`+`,` |
| Back | `Escape`: closes Settings first, then cancels queued navigation, then leaves fullscreen |
| Quit | `Ctrl`+`W` / `Cmd`+`W` |
| Pan the image | Drag, or `Alt`+drag |

Browsing stops at the first and last image. More shortcuts and aliases: [docs/development.md](docs/development.md#keyboard-map).

## Book mode

Choose Reading mode in Settings: “Single image” or “Book · right to left”. In Book mode, “Book: next spread” selects “Right arrow →” (default) or “← Left arrow”; Apply saves settings; changing Reading mode reloads the containing view. Consecutive pages are paired right-to-left: for six files, page 1 is alone, then right 2 / left 3, right 4 / left 5, and page 6 alone. Opening any page shows its containing spread. Fit, Actual size, zoom, pan, and rotation apply to both pages. GIF pages animate independently on each side.

## Preview strip

A bottom strip shows nearby thumbnails by default. Click one to jump; the current page or spread is highlighted and scrolled into view. Use Hide previews / Show previews in the toolbar or Settings. Thumbnails use a small separate 32 MiB cache at up to 160×120.

## Settings and preload

- Defaults are 5 previous, 5 next, and 256 MiB. Settings adjusts both counts and the budget. Apply applies immediately and saves to the user config directory; Cancel keeps the current settings, and save errors are shown.
- Settings also controls Language (Automatic/English/简体中文/日本語), Reading mode, Book direction, and preview visibility. Automatic follows the OS locale at startup. Restore defaults changes only the draft until Apply.
- Neighbors load nearest first. Counts and budget both limit what is kept. A count of 0 disables preloading on that side; a budget of 0 disables all preloading.
- The budget covers decoded pixels only. The current image(s), GPU textures, and temporary decode buffers are outside it. GIF playback pauses new preload.

A successful save notice clears after about two seconds; errors stay until dismissed. See [docs/development.md](docs/development.md) for freshness and exact cache rules.

## Formats

PNG, JPEG, WebP, BMP, ICO, and TIFF display as static images. GIF is the only supported animation; animated WebP shows its first frame. Detection follows file contents, but the selected path still needs a supported extension. Video, archives, and smart spread detection are not implemented.

## Limits

- Each image may use at most 256 MiB for encoded input, decoded data, and final output, with sources up to 65,535 pixels per side within those bounds.
- A GIF may keep at most 256 MiB of decoded frames and 2,048 frames. Two visible Book GIFs can therefore reach about 512 MiB.
- GPU tiles are capped at the smaller of 4,096 pixels per side and the renderer's maximum texture size.
- Preview thumbnails use a separate 32 MiB cache and are resized to at most 160×120.
- Preload, preview, and per-image limits are separate. Together they are not a whole-process memory or RSS cap.

## Read-only

Oxeye only reads images. It does not save, rename, delete, or rewrite them, and rotation is not persisted.

## Requirements

- Rust 1.95 or newer; tested with Rust 1.99.
- Linux: OpenGL/EGL, Wayland or X11, and a desktop portal.
- Windows: the MSVC Rust toolchain.
- macOS: Xcode command line tools and Apple's deprecated OpenGL.

Oxeye targets Linux, macOS, and Windows. Linux is the primary tested platform; macOS and Windows CI is configured but unverified. Details: [docs/development.md](docs/development.md#platform-prerequisites).

## Development

```sh
cargo test --locked --all-targets
```

Formatting, check, tests, Clippy, and release builds run in [.github/workflows/ci.yml](.github/workflows/ci.yml). See [docs/development.md](docs/development.md) for architecture and remaining unverified items.

Current validation: 200 tests pass (2 ignored); format, check, and strict Clippy pass; a Linux release smoke covered a 6720×4480 image, populated previews, and dual infinite GIF Book playback. Windows and macOS CI remains unrun.
