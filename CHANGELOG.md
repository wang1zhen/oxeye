# Changelog

## 0.2.0 — 2026-11-18

- Read CBZ/ZIP, CBR/RAR, and CB7/7z archives as virtual image folders without
  extracting files to disk. RAR support uses pure-Rust `rars`; no UnRAR.
- Resume reading progress from `progress.json`, keyed by archive fingerprint,
  with a bounded recent-book store and atomic saves.
- Dynamic ComicLayout: single cover, automatic wide pages, and `W` to cycle the
  current page through Auto / Single / Spread.
- New `auto_wide` setting in schema v4, with v1–v3 migration.
- Package third-party notices in deb, AppImage, DMG, and MSI installers.

### Limits

- Archive reading is sequential; solid RAR/7z pages can require decoding
  predecessors. There is no arbitrary random access inside a solid block.
- Archive progress uses a length+mtime fingerprint; same-metadata replacements
  can miss. Encrypted and nested archives are not supported.
- Windows is unsigned (SmartScreen may warn); macOS is ad-hoc signed without
  Developer ID or notarization (Gatekeeper may block opening).
- Native installer UI and real macOS Intel hardware remain unverified; published
  release artifacts require the tag-only workflow.

## 0.1.0 — 2026-11-17

- Initial read-only image viewer: folder navigation, preview strip, tiled
  large-image rendering, zoom/pan, display rotation, fullscreen, and clipboard copy.
- Right-to-left Book mode, animated GIF playback, bounded neighbor preloading,
  persistent settings, and English/Simplified Chinese/Japanese UI.
- Installer definitions for Windows x64 MSI, macOS universal DMG, and Linux
  x86_64 deb/AppImage; MIT license.

### Limits

- No editing, saved rotation, automatic updates, or OS file-association setup.
- Windows is unsigned (SmartScreen may warn); macOS is ad-hoc signed, without
  Developer ID or notarization (Gatekeeper may block opening).
- Linux baseline: Ubuntu 22.04 / glibc 2.35. AppImage needs host OpenGL/EGL,
  X11/Wayland libraries, and a desktop portal; it is not a fully bundled runtime.
- Native installer smoke tests passed in the three-platform release dry-run
  (`workflow_dispatch` run 44086841142); artifacts are not publicly released.
