# Roadmap

## Released

**v0.1.0** — 2026-11-17

- Rust + egui/eframe viewer.
- Folder browsing, tiled large images, zoom/pan/fit/actual, rotation, fullscreen.
- Clipboard copy, Book mode, preview strip, bounded preload, settings.
- GIF animation, English/Simplified Chinese/Japanese UI.
- Windows MSI, macOS universal DMG, Linux deb/AppImage.
- Release: https://github.com/wang1zhen/oxeye/releases/tag/v0.1.0

## In `main`, not yet released

Pushed after v0.1.0:

- `5a2b7ad` — virtual `SourceRef` data source and ZIP/CBZ core.
- `cad4d75` — ZIP/CBZ UI integration.
- `3464403` — CBR/RAR and CB7/7z support.

Current verified state: 244 tests pass, 2 ignored; fmt/check/strict Clippy pass.
Archive reads use in-memory entry streams, bounded outputs and archive
fingerprints; no extraction to disk. RAR uses pure-Rust `rars`, 7z uses
`sevenz-rust2`, ZIP uses `zip`; no UnRAR/C++.

Not yet released as v0.2.0, and not visually accepted on a live display.

## Next: C3 comic reading polish (paused)

Paused because the available subagent model quotas were exhausted. No C3 code
has been written.

Planned:

1. **Progress store**
   - Separate `progress.json` in the user config directory.
   - Key by archive path + length/mtime fingerprint; store entry index/name.
   - Atomic save, bounded read, 200-book cap, corrupt file does not auto-save.
   - Resume on open; fingerprint changes ignore stale progress.
2. **Smart wide pages**
   - Cover page single; landscape/wide pages shown alone; otherwise pairs.
   - Manual per-page override (`W` candidate): Auto / Single / Spread.
   - Probe dimensions lazily for current/neighbor pages only.
   - Settings toggle for automatic wide-page detection.
3. **C3 acceptance**
   - Synthetic CBZ/CB7 and checked-in RAR fixtures; no private assets.
   - Book/queue/prefetch/thumbnail regressions.
   - Automated tests first; native visual verification when a display is
     available.

Then bump to **v0.2.0**, update all three READMEs, changelog, third-party
notices and installers.

## Later: lightweight editing

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
- Traditional Chinese and a Japanese README were not added in v0.1.0; the UI
  itself supports Japanese.

## Known limits

- Archive reading is sequential; solid RAR/7z pages may require decoding
  predecessors. No arbitrary random access within a solid block.
- Length+mtime archive fingerprints can miss same-metadata replacements.
- Per-image, thumbnail, preload and progress limits are not a whole-process
  RSS cap.
- Windows/macOS CI passes, but native GUI interaction on those platforms is
  not verified.
- Clipboard backend success cannot be detected; failed copies are silent.
