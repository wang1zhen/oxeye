# Roadmap

## Released

**v0.3.0** — 2026-11-18

- Lightweight non-destructive editing for static PNG/JPEG/WebP/BMP: crop with
  aspect presets, rotate, flip, undo, and live preview through the existing
  tiled candidate pipeline.
- Overwrite-first saving with an explicit confirmation (full path, format,
  metadata loss), plus Save As. Writes use a same-directory temp file, `fsync`,
  inherited permissions, and atomic persist; failure leaves the original intact.
- Format whitelist for writing: GIF/TIFF/ICO stay read-only, while archive
  members can be edited but only saved as a copy.
- Overwrite cache invalidation chain (`Prefetcher::forget_paths` → page
  dimensions → thumbnail generation → reload at turns=0).
- Closing the window waits for a running image save; the edit session restores
  the original frame and display rotation on exit.
- CI for commit 8dffff8 passed on Linux, macOS, and Windows (run 44321681029).
  Packaging dry-run 44321689650 uploaded all four 0.3.0 installers (deb
  5,450,296 B; AppImage 8,317,432 B; DMG 11,259,563 B; MSI 4,608,000 B). The
  macOS job needed two reruns because `macos-latest` was not assigned for 15
  minutes each time; the third attempt passed.
- Tag workflow 44512526910 succeeded on Linux, macOS, and Windows and created a
  draft Release with deb 5,451,076 B / AppImage 8,317,432 B / DMG 11,259,547 B /
  MSI 4,603,904 B plus `SHA256SUMS`; all four hashes verified locally.
- The Release is still a draft. Native visual and interactive acceptance of the
  editing UI remains unverified; Windows is unsigned and macOS is ad-hoc
  signed.

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

## Next: collage and animated GIF editing

Planned, not implemented:

- Template-based collage: 2/3/4-cell and custom column layouts, gap and
  background color, per-cell fit/crop/rotate, then export.
- Animated GIF editing: an indexed-frame pipeline with quantization and frame
  differencing. The intended reference flow is ImageMagick
  `CoalesceImages` → `SetImageType(PaletteType)` → `OptimizeImageLayers`.
  It is not in 0.3.0 because palette quality, output size, and implementation
  complexity need a dedicated pass; shipping a poor re-encode would violate the
  "no silent downgrade" rule.

Keep the editor non-destructive by default: original files are not modified
unless the user explicitly chooses overwrite, and writes are atomic.

## Deliberately out of scope

- Full layer editor, filters/color grading, video, cloud sync, plugin system.
- Automatic updates, file association registration.
- CBR writing/compression, encrypted archives, PDF, nested archives.
- Traditional Chinese remains unsupported; the UI and README are available in
  English, Simplified Chinese, and Japanese.

## Known limits

- GIF, TIFF, ICO, and archive members cannot be edited; GIF animation editing is
  not silently downgraded to a still frame.
- Template collage is deferred.
- Editing drops EXIF, ICC/XMP, and animation. JPEG overwrite is a lossy
  re-encode; WebP is lossless-only and may grow.
- The atomic write does not fsync the parent directory, so a power loss right
  after rename may not guarantee persistence.
- macOS Unicode normalization (NFC/NFD) differences can defeat path identity
  checks when the source disappeared during editing.
- Large-image tile switching and undo replay can stall briefly.
- Archive reading is sequential; solid RAR/7z pages may require decoding
  predecessors. No arbitrary random access within a solid block.
- Length+mtime archive fingerprints can miss same-metadata replacements.
- Per-image, thumbnail, preload and progress limits are not a whole-process
  RSS cap.
- Windows/macOS CI passes, but native GUI interaction on those platforms is
  not verified.
- Clipboard backend success cannot be detected; failed copies are silent.
