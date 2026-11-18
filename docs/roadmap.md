# Roadmap

## Released

**v0.2.0** — 2026-11-18

- Virtual archive reading for CBZ/ZIP, CBR/RAR, and CB7/7z without extracting to
  disk. RAR uses pure-Rust `rars`; no UnRAR.
- `progress.json` v1 resume keyed by archive fingerprint, with per-page
  Auto/Single/Spread overrides and a bounded recent-book store.
- Dynamic ComicLayout: single cover, automatic wide pages, and `W` manual
  override. Settings schema v4 adds `auto_wide`.
- Third-party notices packaged with deb, AppImage, DMG, and MSI.
- Dry-run workflow 44252633961 and tag workflow 44254035119 succeeded; CI for
  commit c5a4d99 passed on Linux, macOS, and Windows.
- Assets: deb 5,339,784 B; AppImage 8,194,552 B; DMG 10,799,039 B; MSI
  4,464,640 B; SHA256SUMS verified all four.
- Release: https://github.com/wang1zhen/oxeye/releases/tag/v0.2.0
- Native archive/wide-page visual and interactive acceptance remains
  unverified; Windows is unsigned and macOS is ad-hoc signed.

**v0.1.0** — 2026-11-17

- Rust + egui/eframe viewer.
- Folder browsing, tiled large images, zoom/pan/fit/actual, rotation, fullscreen.
- Clipboard copy, Book mode, preview strip, bounded preload, settings.
- GIF animation, English/Simplified Chinese/Japanese UI.
- Windows MSI, macOS universal DMG, Linux deb/AppImage.
- Release: https://github.com/wang1zhen/oxeye/releases/tag/v0.1.0

## Next: lightweight editing

Planned, not implemented:

- Non-destructive crop with common aspect ratios.
- Save rotation/flip as a copy; optional overwrite with explicit confirmation.
- Export PNG/JPEG/WebP with quality selection.
- Multi-select from the preview strip and template-based collage (2/3/4/custom
  columns), with fit/crop/gap/background and export.

Keep the editor non-destructive by default: original files are not modified
unless the user explicitly chooses overwrite, and writes are atomic.

## Deliberately out of scope

- Full layer editor, filters/color grading, video, cloud sync, plugin system.
- Automatic updates, file association registration.
- CBR writing/compression, encrypted archives, PDF, nested archives.
- Traditional Chinese remains unsupported; the UI and README are available in
  English, Simplified Chinese, and Japanese.

## Known limits

- Archive reading is sequential; solid RAR/7z pages may require decoding
  predecessors. No arbitrary random access within a solid block.
- Length+mtime archive fingerprints can miss same-metadata replacements.
- Per-image, thumbnail, preload and progress limits are not a whole-process
  RSS cap.
- Windows/macOS CI passes, but native GUI interaction on those platforms is
  not verified.
- Clipboard backend success cannot be detected; failed copies are silent.
