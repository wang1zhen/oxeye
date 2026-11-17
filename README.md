# Oxeye

[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md)

A lightweight desktop image viewer built with Rust and egui. It focuses on fast folder browsing, large-image viewing, and a minimal cross-platform UI. Lightweight editing is planned.

## Features

- Fast folder browsing with a bottom preview strip.
- Large images shown with original detail, using tiled rendering.
- Zoom, pan, fit-to-window, actual size, 90° rotation, and fullscreen.
- Right-to-left Book mode with paired pages.
- Decoded neighbor preload for quicker navigation.
- Simple, uncluttered interface.

## Roadmap

Planned, not implemented: lightweight editing with crop, saved rotation, and multi-image collage.

## Quick start

```sh
cargo run --release --locked -- path/to/image.png
```

Without a path, Oxeye opens an empty window. Use Open, `Ctrl`/`Cmd`+`O`, or drop a file onto the window.

## Controls

| Action | Keys |
| --- | --- |
| Open | `Ctrl`+`O` / `Cmd`+`O` |
| Previous / next | `PgUp` / `PgDn` |
| Jump 10 files | `Up` / `Down` |
| Toggle Book mode | `M` |
| Fit / actual size | `1` or `9` / `0` |
| Zoom | `+`, `-`, or wheel |
| Rotate (display only) | `Ctrl`+`R` / `Ctrl`+`L` (`Cmd` on macOS) |
| Fullscreen | `F11` or `Alt`+`Enter` |
| Settings | `Ctrl`+`,` / `Cmd`+`,` |
| Quit | `Ctrl`+`W` / `Cmd`+`W` |

Full keyboard details: [docs/development.md](docs/development.md#keyboard-map).

## Build

Rust 1.95 or newer; tested with Rust 1.99. Oxeye runs on Linux, macOS, and Windows, and CI passes on all three platforms. Platform packages and build details are in [docs/development.md](docs/development.md#platform-prerequisites).
