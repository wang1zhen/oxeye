# Changelog

## 0.3.0 — 2026-11-18

- Lightweight, non-destructive editing for single static PNG/JPEG/WebP/BMP
  images: crop with Free/1:1/4:3/3:2/16:9/Original presets, 90° rotate, flip,
  and undo (up to 16 steps).
- Entering editing bakes the current display rotation into the preview once;
  exiting restores the original frame and direction. Previews reuse the normal
  tiled candidate pipeline.
- Save strategy: overwrite-first with an explicit confirmation naming the full
  path and target format, plus Save As through the native file dialog. Ctrl/Cmd+S
  overwrites; Ctrl/Cmd+Shift+S writes a copy.
- Format whitelist for writing: PNG, JPEG, WebP, BMP. GIF, TIFF, ICO, and
  archive members are read-only; editing is rejected instead of silently
  flattening animation or multi-page files.
- Atomic writes: same-directory temporary file, `sync_all`, inherited
  permissions, then atomic persist. Cancel or failure leaves the original bytes
  and permissions unchanged.
- Successful overwrite invalidates caches and reloads at turns=0:
  `Prefetcher::forget_paths`, page dimensions, thumbnails, then a fresh decode.
  Closing the window waits for a running image save.
- Settings schema remains v4 (editing state is transient).

### Limits

- GIF, TIFF, and ICO cannot be edited in this version. Animated GIF editing is
  not silently downgraded to a still frame; it is not implemented yet.
- Template-based collage is deferred.
- EXIF, ICC/XMP metadata, and animation are not preserved.
- WebP is written lossless only; photographic sources may become larger.
- JPEG overwrite is a lossy re-encode (default quality 90).
- The atomic write does not fsync the parent directory; a power loss immediately
  after rename may not guarantee persistence.
- macOS filesystem Unicode normalization (NFC/NFD) differences can make the same
  path look different when the source was deleted during editing.
- Reloading a large image tiles synchronously; undo replays from the base image,
  so both may stall briefly on very large images.
- Native visual and interactive acceptance of the editing UI remains unverified.

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
