# Development notes

Current architecture and acceptance notes for Oxeye's Rust + egui/eframe
implementation. The dependency graph is egui/eframe only. Dependencies are
pinned by `Cargo.lock`; no vendored sources are required.

## Pinned versions

| Crate | Version | Notes |
| --- | --- | --- |
| `eframe`, `egui`, `egui_glow`, `epaint` | `0.37.0` | Application shell and immediate-mode UI |
| `glow` | `0.17.0` | OpenGL renderer backend used by eframe |
| `image` | `0.25.10` | Decoding, EXIF orientation, limits |
| `gif` | `0.14.2` | GIF loop metadata and frame counting |
| `zip` | `9.7.0` | CBZ/ZIP virtual archive reading |
| `rars` | `0.13.0` | Pure-Rust CBR/RAR reading; no UnRAR |
| `sevenz-rust2` | `0.25.0` | CB7/7z virtual archive reading |
| `rfd` | `0.18.0` | Native file picker and error dialogs |
| `sys-locale` | `0.3.2` | One-time OS locale lookup |
| `serde`, `serde_json` | `1.0.229`, `1.0.154` | Settings persistence |
| `dirs` | `5.0.1` | User config directory |
| `tempfile` | `3.27.0` | Same-directory atomic settings and image saves |

The package declares `rust-version = "1.95"`. Release packaging builds with Rust
1.95.0, exercising compilation; local checks and the full test suite use Rust
1.99. Full test-suite execution on 1.95 remains unverified.

## Module map

- `main.rs` — eframe application, Glow renderer setup, actions, settings modal,
  navigation-queue orchestration, candidate/gesture handling.
- `model.rs` — bounded image reads, static decode, GIF preflight/composition,
  EXIF orientation, format detection, source limits.
- `edit.rs` — framework-free edit session: base/preview, bounded op stack,
  crop/rotate/flip pixels, aspect fitting, memory-budget validation.
- `export.rs` — framework-free writable-format detection, PNG/JPEG/WebP/BMP
  encoding, atomic overwrite, permission inheritance.
- `source.rs` — virtual `SourceRef` files/archive entries, ZIP/RAR/7z catalogs,
  archive fingerprints, and bounded entry reads.
- `progress.rs` — `progress.json` v1 resume store and per-page layout overrides.
- `comic.rs` — dynamic Book layout with cover, wide-page, and manual overrides.
- `preload.rs` — one-worker nearest-first decoded-neighbor cache, promotion,
  cancellation, Weak identity bookkeeping.
- `settings.rs` — schema v4, migration, validation, atomic save, locale
  resolution.
- `navigation.rs` — framework-free file navigation math and the sequential
  navigation queue.
- `ui_state.rs` — selected index, horizontal direction, and spread coverage
  helpers.
- `thumbnails.rs` — bounded preview worker and still-thumbnail cache.
- `thumb_ui.rs` — framework-free preview-strip layout and reveal math.
- `spread_playback.rs` — independent GIF clocks for up to two Book pages.
- `viewer.rs` — framework-free tile layout, projection, and playback math.
- `ui_text.rs` — English, Simplified Chinese, and Japanese catalogs.

## Decoding and GIF composition

- The selected path is made absolute without resolving symlinks. Navigation
  keeps the selected file's directory and stored name.
- Encoded files are read into a bounded snapshot before decoder construction,
  because some decoders copy their whole input before honoring limits. The
  supported-extension filter applies to the selected path; format dispatch
  follows file contents.
- Static decodes set dimensions/allocator limits, inspect EXIF orientation, and
  validate the oriented dimensions and final RGBA size. Static media becomes
  one zero-delay frame in `LoopMode::Once`.
- GIF preflight uses the `gif` crate directly to read loop metadata and count
  frames without trusting the container. The compositor then produces full
  logical-screen RGBA frames. Zero delays normalize to 10 ms. A GIF is limited
  to 2,048 frames and 256 MiB of retained RGBA; exceeding either limit rejects
  the whole animation instead of truncating it.
- Cancellation is checked before/after decode and between GIF frames. A single
  static decode cannot be interrupted mid-flight. These are per-image bounds,
  not a whole-process memory guarantee.

## Keyboard map

`src/main.rs` is the source of truth; `README.md` mirrors the user-facing
bindings.

- `Ctrl`/`Cmd`+`O` opens, `Ctrl`/`Cmd`+`,` opens Settings, and `Ctrl`/`Cmd`+`W`
  closes the window. On macOS the command modifier substitutes for Ctrl on
  those bindings, plus rotate `R`/`L`.
- `PgUp`/`PgDn` choose previous/next image, or previous/next spread in Book
  mode. `Up`/`Down` jump 10 files, clamp at the ends, and normalize to a spread
  in Book mode. `Home`/`End` jump to the first/last image.
- `Left`/`Right`: Single mode pans horizontally when possible, otherwise goes
  previous/next. Book mode always navigates spreads in the configured
  direction. `Space` pans down when possible, otherwise Single next / Book next
  spread.
- `M` toggles Single/Book (without Shift); `P` controls GIF playback.
- `Ctrl`/`Cmd`+`C` copies the current image. Single mode writes the current
  full-resolution displayed frame with display rotation; GIFs copy the current
  frame. Book mode writes both visible pages left-to-right with a transparent
  gap, capped at 256 MiB; if the composite exceeds the cap it falls back to the
  selected page with a localized notice.
- `1`/`9` fit, `0` actual size, `+`/`-` zoom, `Ctrl`/`Cmd`+`R`/`L` rotate,
  `F11` or `Alt`+`Enter` fullscreen, and drag or `Alt`+drag pans. While editing,
  `R`/`L` rotate the image itself (an edit op), not the display.
- `C` enters editing and starts crop mode; in edit mode it starts crop again
  without creating a new session. `Enter` applies the crop, `Esc` cancels it.
- `X`/`Y` flip horizontally/vertically; `Ctrl`/`Cmd`+`Z` undoes the last edit.
  `Ctrl`/`Cmd`+`S` opens the overwrite confirmation; `Ctrl`/`Cmd`+`Shift`+`S`
  opens Save As. Enter never confirms the overwrite dialog; only the button does.
- `Escape` closes Settings first, then cancels a crop selection, then leaves the
  edit session (discarding unsaved edits), then cancels queued navigation, then
  leaves fullscreen. It never quits the app.
- During an edit session, navigation, preview-strip clicks, Book `M`, wide `W`,
  playback `P`, and drag-and-drop opening are ignored. Zoom/pan, `Ctrl`/`Cmd`+`C`
  (copies the committed preview), fullscreen, and closing remain available.

## Archive sources

- A `SourceRef` is either a filesystem file or an archive member carrying
  archive path, length/mtime fingerprint, format kind, entry index/name/size,
  and CRC. Archive names are labels, never filesystem paths.
- `source.rs` recognizes `.cbz`/`.zip`, `.cbr`/`.rar`, and `.cb7`/`.7z`.
  `zip 9.7.0`, pure-Rust `rars 0.13.0`, and `sevenz-rust2 0.25.0` scan image
  entries without extracting anything to disk. No UnRAR or native RAR code is
  linked.
- Catalog limits: at most 20,000 image entries, 16 MiB of archive metadata,
  256 MiB of archive work/output/workspace, and a 128 MiB RAR dictionary.
  Extracted entry bytes are also bounded by the model's per-image read cap.
- Reads reopen the archive, verify the fingerprint and entry identity, and
  stream the entry into the bounded decoder. Solid RAR/7z members may require
  earlier pages to be decoded; there is no arbitrary random access inside a
  solid block.
- Fingerprints are archive path + length + modification time. A replacement
  preserving both can miss changes; entry metadata checks catch most but not
  all unchanged-member substitutions.

## Third-party notices

`[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)` lists the archive crates shipped in release binaries:
`zip` (MIT), `rars` (Apache-2.0), and `sevenz-rust2` (Apache-2.0). It points to
the RAR fixture provenance and license under `tests/fixtures/archives/`; no
UnRAR or native RAR code is included. The application LICENSE remains MIT. The
notice file is packaged in deb/AppImage docs, DMG `Resources`, and the MSI as a
separate single-file component.

## Reading progress

- `progress.json` version 1 lives in the user config directory. Reads are
  capped at 1 MiB, saves are atomic and serialized, and the store keeps at most
  200 recently read books.
- Records store a native path unit plus archive fingerprint, entry index/name,
  last-read time, and per-page `Auto`/`Single`/`Spread` overrides. Opening an
  archive resumes the stored entry when the fingerprint still matches; ordinary
  folder images do not resume a stored position.
- A fingerprint change ignores stale resume data and removes old overrides.
  Corrupt or oversized progress files warn and are not auto-saved until the
  user acts on a valid store.
- `W` cycles the current page Auto → Single → Spread → Auto, persists the
  override, and refreshes the Book layout. The cover remains single even with a
  forced Spread.

## Book layout and whole-spread transaction

- Source file order never changes. Book mode treats page 1 as a cover shown
  alone, then pairs right-even / left-odd pages, with a final single page when
  the count is odd. For six files: 1; right 2 / left 3; right 4 / left 5; 6.
- Layout is dynamic. `auto_wide` (default on) shows a page alone when it is
  wider than tall and `width * 5 > height * 6`; otherwise pages pair. `W`
  overrides the current page for the next layout. Dimension probes are lazy and
  limited to the current/neighbor range; the cover is always single.
- Every entry point normalizes the current file to its containing spread.
  Opening a page lands on that spread and keeps the clicked page as the anchor.
- The joint canvas top-aligns pages with different heights and keeps a shared
  gap. Fit/Actual size, zoom, pan, and rotation apply to the spread as one
  canvas.
- A Book preparation loads both visible pages before committing. The previous
  good pages stay visible until the whole spread is ready; partial or failed
  page work does not replace them. Candidate and GPU retirement follow the
  whole-spread transaction, not individual page uploads.
- Both Book pages may animate GIFs with independent clocks. Switching reading
  modes does not freeze Book GIFs.

## Spread playback

- Each visible page owns a `Playback` built from its `PageTiming`. Static pages
  have no clock, and invalid page indices fall back to page 0.
- Finite loop metadata means additional repetitions after the first
  play-through. Pause preserves elapsed positions; resume continues only
  unfinished clocks, so a finished page stays on its last frame. `restart_all`
  explicitly recreates every animated clock from frame 0.
- The group reports per-page frames, the earliest next deadline, whether any
  page is playing, and whether every animated page has finished. `P` controls
  the group in Book mode and the single animation in Single mode.

## Editing and export

- Editing is a temporary session over one static frame. The current display
  rotation is baked into the session base once on entry (`0/90/180/270` quarter
  turns), and `rotation` is zeroed so crop math always works in displayed
  orientation. Exiting restores the original media, tiled frame, and full
  rotation; the baked base is never treated as the on-disk original.
- The session keeps `base`, a bounded operation stack (16 ops), and a
  materialized `preview`. `apply` materializes the new op; `undo` pops and
  replays the prefix from `base`. Memory uses N = one full RGBA buffer: `apply`
  holds `base` + old preview + output (3N); `undo` holds base + replay input +
  output (3N), or 4N if an external holder keeps the old preview. These are
  session buffers, not a process RSS cap.
- Previews are wrapped as a single-frame `LoadedMedia` and committed through the
  existing `prepare_frame` → candidate → `commit_candidate` pipeline, so
  textures, dimensions, fit/pan, fullscreen, and normal viewing paths behave like
  ordinary images. Preview commits use `loaded=false`: they do not record
  reading progress or page dimensions.
- Overwrite/Save As use the pure `export.rs` API. The writable whitelist is
  PNG/JPEG/WebP/BMP; GIF/TIFF/ICO are rejected for editing instead of silently
  flattening. Archive members can be edited but only saved as a copy, never
  written back into the archive. Overwrite is same-directory temp file +
  `sync_all` + inherited permissions + atomic persist; cancel or failure leaves
  the original bytes unchanged.
- `Ctrl`/`Cmd`+`S` always opens a confirmation (full path, format, EXIF/ICC and
  animation loss). Enter never confirms; the pressed-Enter events are stripped
  before the modal's buttons are built. JPEG quality is adjustable (40–100,
  default 90). WebP is lossless-only and warned as potentially larger.
- Saving runs on one worker thread and is polled by the UI; a second save is
  refused while one is running. Closing the window is deferred while an image
  save runs and is re-requested only after a successful result. Save As onto the
  frozen source path is routed through the overwrite transaction; an unknown
  extension is an error.
- After a successful overwrite the disk is the only truth:
  `Prefetcher::forget_paths` drops cached pixels/dimensions (and marks in-flight
  decodes forgotten/epoch-invalid), the saved page index is removed from
  `page_dimensions`, thumbnail generation advances and caches clear, then the
  file is re-decoded and reloaded at turns=0. Applying the pre-edit rotation here
  would rotate the already-saved pixels a second time.
- Book restore after editing uses the existing `load` path; a Book spread is
  rebuilt as a whole at turns=0, so only the edited page is guaranteed to carry
  the intended orientation. A layout-correction reload carries the turns of the
  just-prepared frame; ordinary navigation still loads at 0.
- Known limits: the atomic write does not fsync the parent directory; macOS
  NFC/NFD normalization is outside path identity checks; large-image tile
  switching and undo replay can stall; native visual/interactive acceptance of
  the editing overlay, toolbar, and dialogs is still unverified.

## Prefetch

- Defaults are 5 previous, 5 next, and a 256 MiB decoded-pixel budget. Counts
  are 0–50 per direction; the budget is 0–2,048 MiB. Zero counts disable that
  side, and a zero budget disables prefetch entirely.
- Fill order is nearest first, alternating next and previous at each distance,
  without folder wrap. Counts and decoded bytes both limit retention.
- Both visible Book pages are excluded from speculative ownership, as are the
  current Single image, candidates, GPU textures, temporary decode buffers, and
  the foreground reservation. The budget is not an RSS cap.
- One worker performs at most one speculative decode. Results carry the
  original length + modification-time identity. Cache, reservation, and joined
  handoff use strong decoded entries; delivered/previous identity memory is
  Weak, so abandoned history cannot retain pixels.
- Foreground-token validation and reservation/target promotion are atomic under
  the prefetch state mutex. Whole-spread candidate commit and GPU retirement
  remain UI-thread responsibilities outside that mutex.
- GIF playback pauses new speculative and thumbnail work. Metadata identity is
  path + length + modification time; a replacement preserving both can be
  missed.

## Settings

- Settings are stored at `dirs::config_dir()/oxeye/settings.json`. The file is
  capped at 64 KiB, must use flat schema version 4, and every field is
  validated. Unknown fields and unsupported versions fall back to defaults with
  a warning.
- Version-1 through version-3 files migrate by preserving existing custom cache
  and reading values; missing newer fields take defaults, including
  `auto_wide = true`. Loading never rewrites the file. A migrated file is
  written as v4 only on an explicit save.
- Fields: `before`, `after`, `memory_mib`, `thumbnails_visible`,
  `reading_mode`, `comic_arrow_direction`, `auto_wide`, and `language`.
- Saves validate first, write and sync a same-directory `tempfile`, then persist
  atomically. Serialization prevents an older result from winning, and a failed
  save keeps the previous file.
- Apply persists immediately; Cancel discards the draft; Restore defaults edits
  the draft only. Changing Reading mode reloads the containing view; changing
  Book direction only saves the setting. A successful save notice clears after
  about two seconds behind a generation guard; errors stay until dismissed. The
  delayed window-close gate waits for the serialized save.

## Thumbnails

- The preview strip requests only visible cells plus a small margin. The cache
  budget is 32 MiB with at most 512 entries; thumbnails are at most 160×120 and
  never upscale.
- Thumbnails are still images: borrow a prefetched still entry first, otherwise
  decode a still/first frame. No full-resolution frame is retained.
- Cache pixels may survive context changes, but `get` exposes only entries the
  worker revalidated for the current epoch; invalid or missing entries return a
  placeholder instead of stale pixels.
- Display textures cover only visible cells and are explicitly retired. There is
  no numeric GPU byte budget. This is separate from prefetch and is not RSS.

## Language

- `ui_text.rs` provides app-authored catalogs for English, Simplified Chinese,
  and Japanese. File names, paths, format names, key names, and raw technical
  diagnostics remain untranslated.
- `Automatic` resolves the OS locale once at startup with `sys-locale`; there is
  no live language watch. `en*` maps to English, `ja*` to Japanese, and `zh`,
  `zh-CN`, `zh-SG`, or `zh-Hans*` to Simplified Chinese.
- `zh-Hant` and unsupported locales map to English. Traditional Chinese is not
  provided. The README is available in English, Simplified Chinese, and Japanese.

## Tiling and GPU limits

- Source images are capped at 65,535 pixels per side and 256 MiB each for
  encoded input, decoded data, and final RGBA.
- Display textures use at most `min(4_096, renderer.max_texture_side)` pixels
  per side, with a 1-pixel filtering gutter. This is a texture bound, not a
  source cap; large images keep full resolution.
- Only visible cells upload textures, and retired textures are released
  explicitly. Renderer upload errors are reported but cannot be recovered by a
  retry path.
- The startup renderer is eframe's Glow/OpenGL backend. Linux requires
  OpenGL/EGL plus Wayland or X11; macOS uses Apple's deprecated OpenGL; Windows
  uses the MSVC toolchain.

## Verification

Final v0.3.0 status (2026-11-18, Linux/Rust 1.99; CI and dry-run on 2026-11-19):

- `cargo fmt --all -- --check`: pass.
- `CARGO_BUILD_JOBS=2 cargo check --locked --all-targets`: pass.
- `CARGO_BUILD_JOBS=2 cargo test --locked --all-targets`: **366 pass, 2 ignored**.
- `CARGO_BUILD_JOBS=2 cargo clippy --locked --all-targets -- -D warnings`: pass.
- `CARGO_BUILD_JOBS=2 cargo build --locked --release`: pass (1m 40s).
- `cargo metadata --no-deps --format-version 1 --locked`: pass (Cargo.lock at 0.3.0).
- `bash -n packaging/linux/package.sh`, `bash -n packaging/macos/package.sh`,
  `plistlib` parse of `packaging/macos/Info.plist` (both version keys `0.3.0`),
  and `python3` XML parse of `packaging/windows/main.wxs`: pass.
- CI for commit `8dffff8` passed on Linux, macOS, and Windows
  (run 44321681029).
- Packaging dry-run 44321689650 succeeded and uploaded all four installers. The
  macOS job needed two `--failed` reruns because `macos-latest` was not assigned
  for 15 minutes each time (queued, then cancelled with no steps and no runner);
  the third attempt ran and passed. This was runner availability, not a script
  failure.
- The tag workflow and draft Release have **not** been run, so no published
  0.3.0 assets or `SHA256SUMS` exist yet.
- Native visual/interactive acceptance of the editing toolbar, crop overlay,
  save confirmation, and Save As dialog remains unverified.

Full acceptance commands:

```sh
cargo fmt --all -- --check
CARGO_BUILD_JOBS=2 cargo check --locked --all-targets
CARGO_BUILD_JOBS=2 cargo test --locked --all-targets
CARGO_BUILD_JOBS=2 cargo clippy --locked --all-targets -- -D warnings
cargo metadata --no-deps --format-version 1 --locked
bash -n packaging/linux/package.sh
python3 -c "import xml.dom.minidom as m; m.parse('packaging/windows/main.wxs')"
CARGO_BUILD_JOBS=2 cargo build --locked --release
```

The two ignored real-input tests are explicit:

```sh
export OXEYE_TEST_IMAGE_ROOT=/path/to/local/images
CARGO_BUILD_JOBS=2 cargo test --locked real_input_images -- --ignored --nocapture
CARGO_BUILD_JOBS=2 cargo test --locked real_input_preload -- --ignored --nocapture
```

Earlier native evidence for shared viewer behavior used app-only captures by
exact PID/UUID, never the full desktop, with private files under ignored
`target/acceptance/`. It covered a large image, a Book spread, dual
finite+infinite and infinite+infinite spreads whose page regions changed,
English/Simplified Chinese/Japanese and Automatic-Japanese screenshots, CJK
glyphs, and visible/hidden preview strips. Settings-modal and input-driven
behavior remain unit/source evidence only because no safe targeted Wayland input
was available. Archive and wide-page native visual acceptance for v0.3.0 is not
recorded here, and the new editing UI has only unit/state-machine evidence. No
images were copied or uploaded, and test windows were closed afterwards.

## Platform prerequisites

- Linux: OpenGL/EGL, Wayland or X11, and a desktop portal. CI installs
  `build-essential`, `pkg-config`, xkbcommon (core/X11), Wayland, X11/XCB
  (including render/shape/xfixes), Mesa GL, and Mesa EGL packages.
- Windows: the MSVC Rust toolchain and Windows SDK.
- macOS: Xcode command line tools with Apple's deprecated OpenGL. The Metal
  toolchain is not used.

## Packaging v0.3.0

Release builds use Rust **1.95.0** with `--locked`; install that toolchain before
running a packaging script. Run each script on its native OS from a checkout
containing `assets/icons/oxeye-{1024,512,256}.png` and `assets/icons/oxeye.ico`.
Build tools and installer definitions are separate from application source.

| Platform | Command | Tools |
| --- | --- | --- |
| Linux x86_64 | `bash packaging/linux/package.sh` | Ubuntu 22.04, cargo-deb **3.8.0**, appimagetool **1.9.1**, type2-runtime **20251108**, curl, dpkg tools |
| macOS universal | `bash packaging/macos/package.sh` | Xcode CLI tools: lipo, sips, iconutil, codesign, hdiutil; arm64 + x86_64 Rust targets |
| Windows x64 | `pwsh -File packaging/windows/package.ps1` | MSVC/Windows SDK (including dumpbin), .NET SDK **8.0.408** in CI, WiX **7.0.1**, WixToolset.UI.wixext **7.0.1** |

Scripts keep cargo-deb, WiX executables, and AppImage downloads in `.tools/`;
AppImage tool/runtime downloads are SHA-256 checked. Windows tooling explicitly accepts
the user-approved WiX 7 EULA/OSMF terms with `--acceptEula wix7`. WiX 7 retains
the `http://wixtoolset.org/schemas/v4/wxs` XML namespace; this is not WiX 3
authoring. Keep the checked-in MSI UpgradeCode unchanged between releases.

Outputs in `dist/`:

- `oxeye-0.3.0-windows-x86_64.msi` — per-machine Program Files install, Start
  Menu shortcut, MIT license UI, and separate single-file third-party notices
  component. Static CRT build; dumpbin checks for VC++ runtime imports.
- `oxeye-0.3.0-macos-universal.dmg` — `Oxeye.app` plus `/Applications` link,
  macOS 11.0 minimum, ad-hoc signed; `Resources/` contains `LICENSE` and
  `THIRD_PARTY_NOTICES.md`; **no Developer ID signing or notarization**.
- `oxeye-0.3.0-linux-amd64.deb` — binary, desktop entry, icons, copyright,
  `/usr/share/doc/oxeye/THIRD_PARTY_NOTICES.md`, and explicit
  GL/EGL/X11/Wayland/fontconfig/D-Bus/Zenity/portal dependencies.
- `oxeye-0.3.0-linux-x86_64.AppImage` — thin AppDir with LICENSE and third-party
  notices under `usr/share/doc/oxeye`; preserves the caller's working directory
  for relative image paths. Needs host graphics and portal libraries; does not
  bundle graphics drivers or promise arbitrary-distro compatibility.

Linux binaries use Ubuntu **22.04 / glibc 2.35** as their build baseline.
Windows is unsigned and may trigger SmartScreen. macOS Gatekeeper may require
right-click **Open** or approval in System Settings; ad-hoc signing is not
Developer ID signing. No file associations or macOS file-open Apple Events
are registered.

### Installer verification and release flow

v0.3.0 packaging definitions include `THIRD_PARTY_NOTICES.md` in the deb and
AppImage docs, the DMG's `Resources`, and the MSI as a separate single-file
component. This avoids a multi-file auto-GUID component.

Local v0.3.0 checks passed: `bash -n packaging/linux/package.sh`, `bash -n
packaging/macos/package.sh`, `plistlib` parsing of `packaging/macos/Info.plist`,
Python XML parsing of `packaging/windows/main.wxs`, and
`cargo metadata --no-deps --format-version 1 --locked`. The v0.3.0 packaging
dry-run 44321689650 then succeeded and uploaded all four installers:

- `oxeye-0.3.0-linux-amd64.deb` — **5,450,296 B**
- `oxeye-0.3.0-linux-x86_64.AppImage` — **8,317,432 B**
- `oxeye-0.3.0-macos-universal.dmg` — **11,259,563 B**
- `oxeye-0.3.0-windows-x86_64.msi` — **4,608,000 B**

These are dry-run artifact sizes, not published assets; the tag workflow, the
`SHA256SUMS` file, and the draft Release have **not** been created. Interactive
installer UI, real macOS Intel hardware, Gatekeeper/SmartScreen approval, and
actual GPU/hardware behavior remain unverified; Windows is unsigned and macOS is
ad-hoc signed. Native visual and interactive acceptance of the editing UI is
also unverified.

`.github/workflows/release.yml` builds on Ubuntu 22.04, macOS, and Windows with
pinned Rust/tools/Actions. It checks deb metadata/install/remove and bundled
notices, AppImage extract-and-run under Xvfb without FUSE plus notices, DMG
integrity/mount/universal architectures/ad-hoc signature plus notices, and MSI
silent install/uninstall/Start Menu/executable DLL imports plus notices.
Installer execution does not replace interactive GUI or real-hardware checks; no
macOS Intel hardware run, Gatekeeper/SmartScreen approval, or installer UI
verification has been performed.

Use `workflow_dispatch` for the v0.3.0 dry-run; it uploads Actions artifacts
**without creating a Release**. After successful smoke tests and independent
review, push `v0.3.0` for the tag-only publish job. The tag must match
Cargo.toml. The publish job requires exactly four installers, writes
`SHA256SUMS`, and runs `gh release create --verify-tag --draft --generate-notes`;
it never publishes automatically. Check downloads with
`sha256sum --check SHA256SUMS` (Linux), `shasum -a 256` (macOS), or
`Get-FileHash -Algorithm SHA256` (PowerShell). Only after user review should a
maintainer run `gh release edit v0.3.0 --draft=false`.

## CI

- `.github/workflows/ci.yml` runs format, check, tests, strict Clippy, and a
  release build on Ubuntu, macOS, and Windows.
- CI has passed on all three platforms; native interactive verification remains
  Linux-only.

## Known limits

- GIF, TIFF, ICO, and archive members cannot be edited; animated GIF editing is
  not silently downgraded to a still frame.
- Template collage is deferred. EXIF, ICC/XMP, and animation are not preserved.
- JPEG overwrite is a lossy re-encode; WebP is lossless-only and may grow.
- The atomic write does not fsync the parent directory, so a power loss right
  after rename may not guarantee persistence.
- macOS Unicode normalization (NFC/NFD) differences can defeat path identity
  checks when the source disappeared during editing.
- Large-image tile switching and undo replay can stall briefly.
- No OS-level decode priority and no interruptible single static decode.
- File identity is length + modification time; same-metadata replacements and
  metadata-identical symlink retargets can be missed.
- Per-image, thumbnail, and prefetch bounds are separate and are not a
  process-wide RSS cap.
- Archive reading is sequential; solid RAR/7z pages may require decoding
  predecessors. No arbitrary random access exists inside a solid block.
- Archive fingerprints are length + modification time; same-metadata
  replacements can be missed. Encrypted and nested archives are unsupported.
- Renderer upload errors cannot be recovered by retry.
- Clipboard backend success cannot be detected; a failed copy is not
  distinguishable from a successful one at the UI level.
- Raw technical diagnostics, file names, paths, and format/key names remain
  untranslated.
- Traditional Chinese is not translated. The README is available in English,
  Simplified Chinese, and Japanese.
- Native GUI runs on Windows/macOS and full test-suite execution on MSRV 1.95
  remain unverified; local checks used Rust 1.99 and release builds used 1.95.0.
