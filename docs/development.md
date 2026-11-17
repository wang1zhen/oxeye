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
| `rfd` | `0.18.0` | Native file picker and error dialogs |
| `sys-locale` | `0.3.2` | One-time OS locale lookup |
| `serde`, `serde_json` | `1.0.229`, `1.0.154` | Settings persistence |
| `dirs` | `5.0.1` | User config directory |
| `tempfile` | `3.27.0` | Same-directory atomic settings save |

The package declares `rust-version = "1.95"`. Local validation used Rust 1.99;
1.95 itself was not exercised.

## Module map

- `main.rs` — eframe application, Glow renderer setup, actions, settings modal,
  navigation-queue orchestration, candidate/gesture handling.
- `model.rs` — bounded image reads, static decode, GIF preflight/composition,
  EXIF orientation, format detection, source limits.
- `preload.rs` — one-worker nearest-first decoded-neighbor cache, promotion,
  cancellation, Weak identity bookkeeping.
- `settings.rs` — schema v3, migration, validation, atomic save, locale
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
- `1`/`9` fit, `0` actual size, `+`/`-` zoom, `Ctrl`/`Cmd`+`R`/`L` rotate,
  `F11` or `Alt`+`Enter` fullscreen, and drag or `Alt`+drag pans.
- `Escape` closes Settings first, then cancels queued navigation, then leaves
  fullscreen. It never quits the app.

## Book layout and whole-spread transaction

- Source file order never changes. Book mode treats page 1 as a cover shown
  alone, then pairs right-even / left-odd pages, with a final single page when
  the count is odd. For six files: 1; right 2 / left 3; right 4 / left 5; 6.
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
  capped at 64 KiB, must use flat schema version 3, and every field is
  validated. Unknown fields and unsupported versions fall back to defaults with
  a warning.
- Version-1 and version-2 files migrate by preserving existing custom cache and
  reading values; missing newer fields take defaults. Loading never rewrites
  the file. A migrated file is written as v3 only on an explicit save.
- Fields: `before`, `after`, `memory_mib`, `thumbnails_visible`,
  `reading_mode`, `comic_arrow_direction`, and `language`.
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
  provided, and the README is not translated to Japanese.

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

Final status (2026-11-17, Linux/Rust 1.99):

- `cargo fmt --all -- --check`: pass.
- `cargo check --locked --all-targets`: pass.
- `cargo test --locked --all-targets`: **200 pass, 2 ignored**.
- `cargo clippy --locked --all-targets -- -D warnings`: pass.
- `cargo build --locked --release`: pass from a clean target in **1m42s**;
  incremental **38.49s**. The stripped Linux binary was **15,416,760 bytes
  (~14.7 MiB)**; this size is Linux-specific.
- Explicit `real_input_images` + `real_input_preload`: pass in **17.32s**.
- Ignored `target/` retains only local acceptance evidence (about 8.7 MB).

Full acceptance commands:

```sh
cargo fmt --all -- --check
CARGO_BUILD_JOBS=2 cargo check --locked --all-targets
CARGO_BUILD_JOBS=2 cargo test --locked --all-targets
CARGO_BUILD_JOBS=2 cargo clippy --locked --all-targets -- -D warnings
CARGO_BUILD_JOBS=2 cargo build --locked --release
```

The two ignored real-input tests are explicit:

```sh
export OXEYE_TEST_IMAGE_ROOT=/path/to/local/images
CARGO_BUILD_JOBS=2 cargo test --locked real_input_images -- --ignored --nocapture
CARGO_BUILD_JOBS=2 cargo test --locked real_input_preload -- --ignored --nocapture
```

Native evidence from this rewrite used app-only captures by exact PID/UUID,
never the full desktop, with private files kept under ignored
`target/acceptance/`. It covered a large image, a Book spread, dual
finite+infinite and infinite+infinite spreads whose page regions changed,
English/Simplified Chinese/Japanese and Automatic-Japanese screenshots, and CJK
glyphs. Preview-strip visible and hidden states were also captured.
Final release smoke used the same app-only method with no global input: a
6720×4480 image displayed within about two seconds at 13% with populated
previews, and Book mode in Japanese with two infinite GIFs changed both
left/right page regions across captures. Settings-modal and input-driven
behavior remain unit/source evidence only because no safe targeted Wayland
input was available. No images were copied or uploaded, and test windows were
closed afterwards.

## Platform prerequisites

- Linux: OpenGL/EGL, Wayland or X11, and a desktop portal. CI installs
  `build-essential`, `pkg-config`, xkbcommon (core/X11), Wayland, X11/XCB
  (including render/shape/xfixes), Mesa GL, and Mesa EGL packages.
- Windows: the MSVC Rust toolchain and Windows SDK.
- macOS: Xcode command line tools with Apple's deprecated OpenGL. The Metal
  toolchain is not used.

## CI

- `.github/workflows/ci.yml` runs format, check, tests, strict Clippy, and a
  release build on Ubuntu, macOS, and Windows.
- CI has passed on all three platforms; native interactive verification remains
  Linux-only.

## Known limits

- No OS-level decode priority and no interruptible single static decode.
- File identity is length + modification time; same-metadata replacements and
  metadata-identical symlink retargets can be missed.
- Per-image, thumbnail, and prefetch bounds are separate and are not a
  process-wide RSS cap.
- Renderer upload errors cannot be recovered by retry.
- Raw technical diagnostics, file names, paths, and format/key names remain
  untranslated.
- Traditional Chinese is not translated, and the README has no Japanese
  version.
- Native Windows/macOS runs and MSRV 1.95 remain unverified; local checks used
  Rust 1.99.
