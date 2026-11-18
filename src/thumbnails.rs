//! Bounded thumbnail service for the bottom preview strip.
//!
//! One standard-library worker thread handles at most one job at a time. The UI
//! supplies only the visible paths (plus its own margin) and a display
//! generation; this module owns the bounded metadata queue, the LRU pixel cache,
//! and cancellation. Results are borrowed from the prefetcher first, then fall
//! back to a still/first-frame decode. No full-resolution frame is retained.
//! Cache pixels survive context changes, but `get` only exposes entries the
//! worker has revalidated for the current context epoch.
//!
//! Callers poll [`Thumbnails::revision`]; there is no callback channel. The
//! worker and UI coordinate through a mutex and condition variable.
//!
//! Cancellation is cooperative. A decoder already inside a frame cannot be
//! interrupted by dropping its task, so [`Thumbnails::shutdown`] and context
//! changes only signal a flag and return. This module makes no OS scheduling
//! priority or whole-process memory claim.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use image::RgbaImage;

use crate::model::source::{SourceIdentity, SourceRef, source_identity};
use crate::model::{DecodePurpose, LoadedMedia, load_media_for_source};
use crate::preload::Prefetcher;

/// Longest thumbnail side, before aspect-ratio fitting.
const MAX_THUMB_WIDTH: u32 = 160;
/// Tallest thumbnail side, before aspect-ratio fitting.
const MAX_THUMB_HEIGHT: u32 = 120;
/// RGBA bytes retained by the thumbnail cache.
const CACHE_BUDGET_BYTES: u64 = 32 * 1024 * 1024;
/// Upper bound on path entries, independent of the byte budget.
const MAX_CACHE_ENTRIES: usize = 512;
/// How often an idle worker rechecks whether its handle vanished.
const WORKER_TICK: Duration = Duration::from_millis(100);

/// Injected decode function, replaced by deterministic fakes in tests.
type DecodeFn =
    dyn Fn(&SourceRef, &AtomicBool, DecodePurpose) -> Result<LoadedMedia, String> + Send + Sync;

struct CacheEntry {
    pixels: Arc<RgbaImage>,
    bytes: u64,
    last_used: u64,
    identity: SourceIdentity,
    /// Context epoch in which this entry was freshly validated or admitted.
    validated_epoch: u64,
}

/// LRU cache charged in actual RGBA bytes.
struct Cache {
    entries: HashMap<SourceRef, CacheEntry>,
    bytes: u64,
    budget: u64,
    clock: u64,
}

impl Cache {
    fn new(budget: u64) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            budget,
            clock: 0,
        }
    }

    /// Returns a cached thumbnail only when the current context epoch has
    /// validated it. Retained pixels may still be reused after validation.
    fn get(&mut self, path: &SourceRef, epoch: u64) -> Option<Arc<RgbaImage>> {
        let entry = self.entries.get_mut(path)?;
        if entry.validated_epoch != epoch {
            return None;
        }
        self.clock = self.clock.wrapping_add(1);
        entry.last_used = self.clock;
        Some(Arc::clone(&entry.pixels))
    }

    /// Marks a retained entry valid for `epoch`. Returns whether it changed.
    fn mark_validated(&mut self, path: &SourceRef, epoch: u64) -> bool {
        let entry = self.entries.get_mut(path);
        match entry {
            Some(entry) if entry.validated_epoch != epoch => {
                entry.validated_epoch = epoch;
                true
            }
            _ => false,
        }
    }

    fn identity(&self, path: &SourceRef) -> Option<SourceIdentity> {
        self.entries.get(path).map(|entry| entry.identity.clone())
    }

    fn remove(&mut self, path: &SourceRef) -> bool {
        let Some(entry) = self.entries.remove(path) else {
            return false;
        };
        self.bytes = self.bytes.saturating_sub(entry.bytes);
        true
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Inserts one thumbnail, preferring non-visible victims while over budget.
    /// Returns whether the path stayed cached and how many entries were evicted.
    fn insert(
        &mut self,
        path: SourceRef,
        identity: SourceIdentity,
        pixels: Arc<RgbaImage>,
        visible: &[SourceRef],
        validated_epoch: u64,
    ) -> (bool, usize) {
        let bytes = pixels.as_raw().len() as u64;
        if bytes == 0 || bytes > self.budget {
            return (false, 0);
        }

        self.remove(&path);
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        self.entries.insert(
            path.clone(),
            CacheEntry {
                pixels,
                bytes,
                last_used: clock,
                identity,
                validated_epoch,
            },
        );
        self.bytes = self.bytes.saturating_add(bytes);

        let evicted = self.evict_until(visible);
        (self.entries.contains_key(&path), evicted)
    }

    fn evict_until(&mut self, visible: &[SourceRef]) -> usize {
        let mut evicted = 0;
        while self.bytes > self.budget || self.entries.len() > MAX_CACHE_ENTRIES {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(path, entry)| {
                    (
                        visible.iter().any(|visible| visible == *path),
                        entry.last_used,
                    )
                })
                .map(|(path, _)| path.clone());
            let Some(victim) = victim else {
                break;
            };
            self.remove(&victim);
            evicted += 1;
        }
        evicted
    }
}

struct State {
    visible: Vec<SourceRef>,
    generation: u64,
    epoch: u64,
    allowed: bool,
    cancel: Arc<AtomicBool>,
    failed: HashSet<SourceRef>,
    cache: Cache,
}

struct Inner {
    state: Mutex<State>,
    wake: Condvar,
    shutdown: AtomicBool,
    revision: AtomicU64,
    prefetch: Prefetcher,
    decode: Arc<DecodeFn>,
    _worker: Mutex<Option<JoinHandle<()>>>,
    #[cfg(test)]
    busy: AtomicBool,
}

impl Inner {
    fn bump_revision(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    fn cancel_current(&self) {
        lock(&self.state).cancel.store(true, Ordering::Relaxed);
    }

    fn is_current(&self, epoch: u64) -> bool {
        let state = lock(&self.state);
        state.epoch == epoch && state.allowed && !self.shutdown.load(Ordering::Relaxed)
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Never joins: dropping the handle detaches a decoder that may still be
        // finishing an uninterruptible frame.
        self.shutdown.store(true, Ordering::Relaxed);
        lock(&self.state).cancel.store(true, Ordering::Relaxed);
        self.wake.notify_all();
    }
}

/// Shared thumbnail handle. Cloning is cheap and keeps the same worker.
#[derive(Clone)]
pub struct Thumbnails {
    inner: Arc<Inner>,
}

impl Thumbnails {
    /// Starts the bounded thumbnail worker.
    pub fn new(prefetch: Prefetcher) -> Self {
        Self::spawn(
            prefetch,
            Arc::new(load_media_for_source),
            CACHE_BUDGET_BYTES,
        )
    }

    fn spawn(prefetch: Prefetcher, decode: Arc<DecodeFn>, budget: u64) -> Self {
        let state = Mutex::new(State {
            visible: Vec::new(),
            generation: 0,
            epoch: 0,
            allowed: true,
            cancel: Arc::new(AtomicBool::new(false)),
            failed: HashSet::new(),
            cache: Cache::new(budget),
        });
        let inner = Arc::new(Inner {
            state,
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            revision: AtomicU64::new(0),
            prefetch,
            decode,
            _worker: Mutex::new(None),
            #[cfg(test)]
            busy: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&inner);
        let worker = thread::Builder::new()
            .name("oxeye-thumbnails".to_owned())
            .spawn(move || run(weak))
            .expect("spawn thumbnail worker");
        *lock(&inner._worker) = Some(worker);
        Self { inner }
    }

    /// Replaces the visible set. Pure memory coordination: no file I/O.
    ///
    /// The caller supplies visible paths plus its own margin only. Repeating the
    /// same paths and generation is a no-op so a UI refresh cannot reset the
    /// worker or starve a path. An empty set clears the CPU cache and cancels
    /// the current job.
    pub fn set_visible(&self, paths: Vec<PathBuf>, generation: u64) {
        self.set_visible_sources(
            paths.iter().map(|path| SourceRef::file(path)).collect(),
            generation,
        );
    }

    pub fn set_visible_sources(&self, paths: Vec<SourceRef>, generation: u64) {
        let mut state = lock(&self.inner.state);
        if state.visible == paths && state.generation == generation {
            return;
        }

        state.visible = paths;
        state.generation = generation;
        state.epoch = state.epoch.wrapping_add(1);
        state.failed.clear();
        state.cancel.store(true, Ordering::Relaxed);
        state.cancel = Arc::new(AtomicBool::new(false));

        let cleared = state.visible.is_empty() && !state.cache.entries.is_empty();
        if cleared {
            state.cache.clear();
        }
        drop(state);

        if cleared {
            self.inner.bump_revision();
        }
        self.inner.wake.notify_all();
    }

    /// Enables or suppresses new work. Disabling cancels the current job
    /// cooperatively; the UI only enables this when no foreground load,
    /// navigation, candidate, or active GIF needs the worker first.
    pub fn set_allowed(&self, allowed: bool) {
        let mut state = lock(&self.inner.state);
        if state.allowed == allowed {
            return;
        }
        state.allowed = allowed;
        if allowed {
            if state.cancel.load(Ordering::Relaxed) {
                state.cancel = Arc::new(AtomicBool::new(false));
            }
        } else {
            state.cancel.store(true, Ordering::Relaxed);
        }
        drop(state);
        self.inner.wake.notify_all();
    }

    /// Returns an already validated thumbnail. Pure cache lookup: no file I/O,
    /// no decode. A missing or not-yet-current-epoch entry is `None`; the UI
    /// draws its placeholder.
    pub fn get(&self, path: &Path) -> Option<Arc<RgbaImage>> {
        self.get_source(&SourceRef::file(path))
    }

    pub fn get_source(&self, source: &SourceRef) -> Option<Arc<RgbaImage>> {
        let mut state = lock(&self.inner.state);
        let epoch = state.epoch;
        state.cache.get(source, epoch)
    }

    /// Increases whenever the UI may need a new display: success, invalid file,
    /// or eviction. Poll this from a 100-250 ms UI timer.
    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Relaxed)
    }

    /// Stops the worker without waiting. Cancellation cannot interrupt a decoder
    /// already inside a frame; a result from that frame is discarded.
    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::Relaxed);
        self.inner.cancel_current();
        self.inner.wake.notify_all();
    }

    #[cfg(test)]
    fn with_decoder<F>(prefetch: Prefetcher, decode: F, budget: u64) -> Self
    where
        F: Fn(&Path, &AtomicBool, DecodePurpose) -> Result<LoadedMedia, String>
            + Send
            + Sync
            + 'static,
    {
        Self::spawn(
            prefetch,
            Arc::new(move |source, cancel, purpose| decode(source.path(), cancel, purpose)),
            budget,
        )
    }

    #[cfg(test)]
    fn worker_busy(&self) -> bool {
        self.inner.busy.load(Ordering::Relaxed)
    }
}

fn run(weak: Weak<Inner>) {
    let mut epoch = 0u64;
    let mut next = 0usize;

    loop {
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let state = lock(&inner.state);
        if inner.shutdown.load(Ordering::Relaxed) {
            return;
        }
        if state.epoch != epoch {
            epoch = state.epoch;
            next = 0;
        }
        if !state.allowed || state.visible.is_empty() || next >= state.visible.len() {
            let _ = inner.wake.wait_timeout(state, WORKER_TICK);
            continue;
        }
        if state.failed.contains(&state.visible[next]) {
            next += 1;
            continue;
        }

        let path = state.visible[next].clone();
        let cancel = Arc::clone(&state.cancel);
        drop(state);

        #[cfg(test)]
        inner.busy.store(true, Ordering::Relaxed);
        let consumed = process_one(&inner, &path, epoch, &cancel);
        #[cfg(test)]
        inner.busy.store(false, Ordering::Relaxed);

        // A cancelled or superseded path is retried only if the same epoch is
        // still current (for example, after the UI briefly disables the gate).
        if consumed {
            next += 1;
        }
    }
}

/// Processes one visible path. Returns whether its index can advance.
fn process_one(inner: &Arc<Inner>, path: &SourceRef, epoch: u64, cancel: &AtomicBool) -> bool {
    if cancel.load(Ordering::Relaxed) || !inner.is_current(epoch) {
        return false;
    }

    // Snapshot before any decode so a changed file is not admitted with old
    // pixels. Unreadable or non-regular files fail this epoch's placeholder.
    let Some(identity) = source_identity(path) else {
        lock(&inner.state).cache.remove(path);
        return fail(inner, path, epoch);
    };
    if cancel.load(Ordering::Relaxed) || !inner.is_current(epoch) {
        return false;
    }

    // Cache pixels survive a context change, but the worker revalidates the
    // file identity and only then marks the entry valid for the new epoch.
    let (validated, stale) = {
        let mut state = lock(&inner.state);
        // An old worker that lost its context must never validate pixels for a
        // newer epoch; the new worker revalidates from scratch.
        if state.epoch != epoch || cancel.load(Ordering::Relaxed) {
            return false;
        }
        match state.cache.identity(path) {
            Some(cached) if cached == identity => (state.cache.mark_validated(path, epoch), false),
            Some(_) => {
                state.cache.remove(path);
                (false, true)
            }
            None => (false, false),
        }
    };
    if validated {
        // Retained pixels became usable again without a second decode.
        inner.bump_revision();
        return true;
    }
    if stale {
        inner.bump_revision();
    }

    if cancel.load(Ordering::Relaxed) || !inner.is_current(epoch) {
        return false;
    }

    // Borrow a non-consuming prefetch result first. `peek` neither consumes the
    // entry nor counts a hit; the core prefetcher keeps its cache accounting.
    let media = match inner.prefetch.peek_source(path, cancel) {
        Some(media) => media,
        None => match (inner.decode)(path, cancel, DecodePurpose::Still) {
            Ok(media) => Arc::new(media),
            Err(_) => {
                if cancel.load(Ordering::Relaxed) || !inner.is_current(epoch) {
                    return false;
                }
                return fail(inner, path, epoch);
            }
        },
    };

    if cancel.load(Ordering::Relaxed) || !inner.is_current(epoch) {
        return false;
    }

    let Some(frame) = media.frames.first() else {
        drop(media);
        return fail(inner, path, epoch);
    };
    let pixels = thumbnail(&frame.pixels);
    // Release every full-resolution source/frame Arc before cache admission and
    // never keep one per thumbnail path.
    drop(media);

    if source_identity(path).as_ref() != Some(&identity) {
        return fail(inner, path, epoch);
    }
    if cancel.load(Ordering::Relaxed) || !inner.is_current(epoch) {
        return false;
    }

    admit(inner, path, epoch, identity, pixels)
}

fn fail(inner: &Inner, path: &SourceRef, epoch: u64) -> bool {
    let mut state = lock(&inner.state);
    let current = state.epoch == epoch;
    if current {
        // One placeholder per epoch: a bad source is not retried forever.
        state.failed.insert(path.clone());
    }
    drop(state);
    if current {
        inner.bump_revision();
    }
    true
}

fn admit(
    inner: &Inner,
    path: &SourceRef,
    epoch: u64,
    identity: SourceIdentity,
    pixels: RgbaImage,
) -> bool {
    let mut state = lock(&inner.state);
    if state.epoch != epoch || !state.allowed || inner.shutdown.load(Ordering::Relaxed) {
        return false;
    }
    let (admitted, evicted) = {
        let State { cache, visible, .. } = &mut *state;
        cache.insert(path.clone(), identity, Arc::new(pixels), visible, epoch)
    };
    drop(state);
    if admitted || evicted > 0 {
        inner.bump_revision();
    }
    true
}

/// Fits an RGBA frame into 160x120, preserving aspect ratio and never upscaling.
fn thumbnail(pixels: &RgbaImage) -> RgbaImage {
    let (width, height) = pixels.dimensions();
    if width == 0 || height == 0 || (width <= MAX_THUMB_WIDTH && height <= MAX_THUMB_HEIGHT) {
        return pixels.clone();
    }
    let (width, height) = thumbnail_dimensions(width, height);
    image::imageops::thumbnail(pixels, width, height)
}

fn thumbnail_dimensions(width: u32, height: u32) -> (u32, u32) {
    if width <= MAX_THUMB_WIDTH && height <= MAX_THUMB_HEIGHT {
        return (width, height);
    }

    let width = u64::from(width);
    let height = u64::from(height);
    if width * u64::from(MAX_THUMB_HEIGHT) >= height * u64::from(MAX_THUMB_WIDTH) {
        let scaled = (height * u64::from(MAX_THUMB_WIDTH) + width / 2) / width;
        (
            MAX_THUMB_WIDTH,
            scaled.clamp(1, u64::from(MAX_THUMB_HEIGHT)) as u32,
        )
    } else {
        let scaled = (width * u64::from(MAX_THUMB_HEIGHT) + height / 2) / height;
        (
            scaled.clamp(1, u64::from(MAX_THUMB_WIDTH)) as u32,
            MAX_THUMB_HEIGHT,
        )
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LoopMode, MediaFrame};
    use crate::settings::PrefetchSettings;
    use image::{Rgba, RgbaImage};
    use std::fs;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("oxeye-thumbnails-{}-{unique}", std::process::id()));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, b"fixture").expect("write fixture");
            path
        }

        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, bytes).expect("write fixture");
            path
        }

        fn png(&self, name: &str, pixels: &RgbaImage) -> PathBuf {
            let path = self.0.join(name);
            pixels
                .save_with_format(&path, image::ImageFormat::Png)
                .expect("write png fixture");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn empty_prefetch() -> Prefetcher {
        Prefetcher::new(PrefetchSettings {
            before: 0,
            after: 0,
            memory_mib: 0,
        })
    }

    fn media(path: &Path, width: u32, height: u32) -> LoadedMedia {
        LoadedMedia {
            source: SourceRef::file(path),
            path: path.to_path_buf(),
            frames: vec![MediaFrame {
                pixels: Arc::new(RgbaImage::from_pixel(
                    width,
                    height,
                    Rgba([10, 20, 30, 255]),
                )),
                delay: Duration::ZERO,
            }],
            loop_mode: LoopMode::Once,
        }
    }

    fn fixed(
        width: u32,
        height: u32,
    ) -> impl Fn(&Path, &AtomicBool, DecodePurpose) -> Result<LoadedMedia, String> + Send + Sync + 'static
    {
        move |path, _, _| Ok(media(path, width, height))
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let start = Instant::now();
        while !condition() {
            assert!(start.elapsed() < Duration::from_secs(5), "timed out");
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait_get(thumbs: &Thumbnails, path: &Path) -> Arc<RgbaImage> {
        let mut found = None;
        wait_until(|| {
            found = thumbs.get(path);
            found.is_some()
        });
        found.expect("thumbnail")
    }

    #[test]
    fn thumbnail_dimensions_fit_160x120_without_upscaling() {
        assert_eq!(thumbnail_dimensions(800, 600), (160, 120));
        assert_eq!(thumbnail_dimensions(1000, 100), (160, 16));
        assert_eq!(thumbnail_dimensions(100, 1000), (12, 120));
        assert_eq!(thumbnail_dimensions(80, 60), (80, 60));
        assert_eq!(thumbnail_dimensions(0, 10), (0, 10));

        let wide = RgbaImage::new(1000, 100);
        assert_eq!(thumbnail(&wide).dimensions(), (160, 16));
        let small = RgbaImage::new(80, 60);
        assert_eq!(thumbnail(&small).dimensions(), (80, 60));
    }

    #[test]
    fn archive_thumbnails_are_keyed_by_entry_and_revalidate_on_context_change() {
        use crate::model::source::{catalog_for, test_support::write_zip};
        use std::io::Cursor;
        let dir = TempDir::new();
        let path = dir.0.join("pages.cbz");
        let png = |width, height| {
            let mut bytes = Cursor::new(Vec::new());
            RgbaImage::from_pixel(width, height, Rgba([10, 20, 30, 255]))
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
            bytes.into_inner()
        };
        let wide = png(400, 200);
        let tall = png(200, 400);
        write_zip(&path, &[("a.png", &wide), ("b.png", &tall)]);
        let items = catalog_for(&path).unwrap().items;
        let thumbs = Thumbnails::new(empty_prefetch());
        thumbs.set_visible_sources(items.clone(), 1);
        wait_until(|| {
            thumbs.get_source(&items[0]).is_some() && thumbs.get_source(&items[1]).is_some()
        });
        assert_eq!(
            thumbs.get_source(&items[0]).unwrap().dimensions(),
            (160, 80)
        );
        assert_eq!(
            thumbs.get_source(&items[1]).unwrap().dimensions(),
            (60, 120)
        );
        thumbs.set_allowed(false);
        write_zip(&path, &[("a.png", &tall), ("b.png", &tall)]);
        let changed = catalog_for(&path).unwrap().items;
        thumbs.set_visible_sources(changed.clone(), 2);
        assert!(thumbs.get_source(&changed[0]).is_none());
        thumbs.set_allowed(true);
        wait_until(|| thumbs.get_source(&changed[0]).is_some());
        assert_eq!(
            thumbs.get_source(&changed[0]).unwrap().dimensions(),
            (60, 120)
        );
        thumbs.shutdown();
    }

    #[test]
    fn decoded_frames_are_shrunk_to_the_thumbnail_bound() {
        let dir = TempDir::new();
        let path = dir.file("large.bin");
        let thumbs = Thumbnails::with_decoder(empty_prefetch(), fixed(800, 600), 8 * 1024 * 1024);

        thumbs.set_visible(vec![path.clone()], 1);
        let pixels = wait_get(&thumbs, &path);
        assert_eq!(pixels.dimensions(), (160, 120));

        thumbs.shutdown();
    }

    #[test]
    fn gif_still_decode_keeps_only_the_first_frame() {
        let dir = TempDir::new();
        let path = dir.0.join("two-frames.gif");
        let file = fs::File::create(&path).expect("create gif");
        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        encoder
            .encode_frame(image::Frame::new(RgbaImage::from_pixel(
                40,
                30,
                Rgba([255, 0, 0, 255]),
            )))
            .expect("first frame");
        encoder
            .encode_frame(image::Frame::new(RgbaImage::from_pixel(
                40,
                30,
                Rgba([0, 0, 255, 255]),
            )))
            .expect("second frame");
        drop(encoder);

        let thumbs = Thumbnails::new(empty_prefetch());
        thumbs.set_visible(vec![path.clone()], 1);
        let pixels = wait_get(&thumbs, &path);
        assert_eq!(pixels.dimensions(), (40, 30));
        let pixel = pixels.get_pixel(20, 15);
        assert!(
            pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60,
            "expected the red first frame, got {pixel:?}"
        );

        thumbs.shutdown();
    }

    #[test]
    fn still_thumbnail_applies_exif_orientation() {
        let dir = TempDir::new();
        let mut source = RgbaImage::from_pixel(16, 8, Rgba([0, 0, 255, 255]));
        for y in 0..8 {
            for x in 0..8 {
                source.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode_image(&source)
            .expect("encode jpeg");
        let path = dir.write("oriented.jpg", &with_exif_orientation(jpeg, 6));

        let thumbs = Thumbnails::new(empty_prefetch());
        thumbs.set_visible(vec![path.clone()], 1);
        let pixels = wait_get(&thumbs, &path);
        assert_eq!(pixels.dimensions(), (8, 16));
        let top = pixels.get_pixel(4, 4);
        let bottom = pixels.get_pixel(4, 12);
        assert!(top[0] > 200 && top[2] < 60, "top {top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "bottom {bottom:?}");

        thumbs.shutdown();
    }

    #[test]
    fn background_source_pixels_are_released_before_cache_admission() {
        let dir = TempDir::new();
        let path = dir.file("release.bin");
        let source = Arc::new(Mutex::new(None::<Weak<RgbaImage>>));
        let slot = Arc::clone(&source);
        let decode = move |path: &Path, _: &AtomicBool, _: DecodePurpose| {
            let pixels = Arc::new(RgbaImage::from_pixel(640, 480, Rgba([1, 2, 3, 255])));
            *slot.lock().expect("weak slot") = Some(Arc::downgrade(&pixels));
            Ok(LoadedMedia {
                source: SourceRef::file(path),
                path: path.to_path_buf(),
                frames: vec![MediaFrame {
                    pixels,
                    delay: Duration::ZERO,
                }],
                loop_mode: LoopMode::Once,
            })
        };
        let thumbs = Thumbnails::with_decoder(empty_prefetch(), decode, 8 * 1024 * 1024);

        thumbs.set_visible(vec![path.clone()], 1);
        let pixels = wait_get(&thumbs, &path);
        assert_eq!(pixels.dimensions(), (160, 120));
        let weak = source.lock().expect("weak slot").take().expect("captured");
        assert!(
            weak.upgrade().is_none(),
            "full-resolution source was retained"
        );

        thumbs.shutdown();
    }

    #[test]
    fn peek_does_not_consume_or_count_a_cache_hit() {
        let dir = TempDir::new();
        let selected = dir.png(
            "selected.png",
            &RgbaImage::from_pixel(8, 8, Rgba([1, 1, 1, 255])),
        );
        let neighbor = dir.png(
            "neighbor.png",
            &RgbaImage::from_pixel(8, 8, Rgba([2, 2, 2, 255])),
        );
        let prefetch = Prefetcher::new(PrefetchSettings {
            before: 0,
            after: 1,
            memory_mib: 16,
        });
        prefetch.set_allowed(true);
        prefetch.update_context(
            vec![selected.clone(), neighbor.clone()],
            Some(selected),
            None,
        );
        wait_until(|| prefetch.stats().entries == 1);

        let before = prefetch.stats();
        let cancel = AtomicBool::new(false);
        let first = prefetch.peek(&neighbor, &cancel).expect("cached peek");
        let second = prefetch.peek(&neighbor, &cancel).expect("second peek");
        let after = prefetch.stats();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(after.entries, before.entries);
        assert_eq!(after.bytes, before.bytes);
        assert_eq!(after.hits, before.hits);

        drop((first, second));
        prefetch.shutdown();
    }

    #[test]
    fn hiding_rejects_a_late_decode_from_the_old_epoch() {
        let dir = TempDir::new();
        let path = dir.file("late.bin");
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = Arc::clone(&gate);
        let started = Arc::new(AtomicBool::new(false));
        let worker_started = Arc::clone(&started);
        let decode = move |path: &Path, _: &AtomicBool, _: DecodePurpose| {
            worker_started.store(true, Ordering::Relaxed);
            let (lock, wake) = &*worker_gate;
            let mut released = lock.lock().expect("gate");
            while !*released {
                released = wake.wait(released).expect("gate wait");
            }
            Ok(media(path, 320, 240))
        };
        let thumbs = Thumbnails::with_decoder(empty_prefetch(), decode, 8 * 1024 * 1024);

        thumbs.set_visible(vec![path.clone()], 1);
        wait_until(|| started.load(Ordering::Relaxed));

        // Hide the strip: empty paths clear the cache and start a new epoch.
        thumbs.set_visible(Vec::new(), 2);
        {
            let (lock, wake) = &*gate;
            *lock.lock().expect("gate") = true;
            wake.notify_all();
        }
        wait_until(|| !thumbs.worker_busy());
        assert!(thumbs.get(&path).is_none(), "stale result was admitted");

        thumbs.set_visible(vec![path.clone()], 3);
        let _ = wait_get(&thumbs, &path);
        let revision = thumbs.revision();
        thumbs.set_visible(Vec::new(), 4);
        assert!(thumbs.get(&path).is_none(), "hidden cache was not cleared");
        assert!(thumbs.revision() > revision);

        thumbs.shutdown();
    }

    #[test]
    fn disabling_the_gate_cancels_cooperatively_and_retries() {
        let dir = TempDir::new();
        let path = dir.file("retry.bin");
        let attempts = Arc::new(AtomicUsize::new(0));
        let decode_attempts = Arc::clone(&attempts);
        let decode = move |path: &Path, cancel: &AtomicBool, _: DecodePurpose| {
            let attempt = decode_attempts.fetch_add(1, Ordering::Relaxed);
            if attempt == 0 {
                while !cancel.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(1));
                }
                return Err(format!("{}: cancelled", path.display()));
            }
            Ok(media(path, 320, 240))
        };
        let thumbs = Thumbnails::with_decoder(empty_prefetch(), decode, 8 * 1024 * 1024);

        thumbs.set_visible(vec![path.clone()], 1);
        wait_until(|| attempts.load(Ordering::Relaxed) == 1);
        thumbs.set_allowed(false);
        wait_until(|| !thumbs.worker_busy());
        assert!(thumbs.get(&path).is_none());

        thumbs.set_allowed(true);
        let pixels = wait_get(&thumbs, &path);
        assert_eq!(pixels.dimensions(), (160, 120));
        assert!(attempts.load(Ordering::Relaxed) >= 2);

        thumbs.shutdown();
    }

    #[test]
    fn failed_source_is_marked_once_per_epoch() {
        let dir = TempDir::new();
        let path = dir.file("broken.bin");
        let attempts = Arc::new(AtomicUsize::new(0));
        let decode_attempts = Arc::clone(&attempts);
        let decode = move |_: &Path, _: &AtomicBool, _: DecodePurpose| {
            decode_attempts.fetch_add(1, Ordering::Relaxed);
            Err("broken".to_owned())
        };
        let thumbs = Thumbnails::with_decoder(empty_prefetch(), decode, 8 * 1024 * 1024);

        thumbs.set_visible(vec![path.clone(), path.clone()], 1);
        wait_until(|| !thumbs.worker_busy() && attempts.load(Ordering::Relaxed) == 1);
        assert!(thumbs.get(&path).is_none());

        // Same paths and generation: idempotent, no retry.
        thumbs.set_visible(vec![path.clone(), path.clone()], 1);
        thumbs.set_allowed(false);
        thumbs.set_allowed(true);
        wait_until(|| !thumbs.worker_busy());
        assert_eq!(attempts.load(Ordering::Relaxed), 1);

        // A new generation retries exactly once, then skips the duplicate.
        thumbs.set_visible(vec![path.clone(), path.clone()], 2);
        wait_until(|| !thumbs.worker_busy() && attempts.load(Ordering::Relaxed) == 2);
        assert_eq!(attempts.load(Ordering::Relaxed), 2);

        thumbs.shutdown();
    }

    #[test]
    fn one_bad_source_does_not_stop_the_visible_list() {
        let dir = TempDir::new();
        let broken = dir.file("broken.bin");
        let good = dir.file("good.bin");
        let bad_path = broken.clone();
        let decode = move |path: &Path, _: &AtomicBool, _: DecodePurpose| {
            if path == bad_path {
                Err("broken".to_owned())
            } else {
                Ok(media(path, 320, 240))
            }
        };
        let thumbs = Thumbnails::with_decoder(empty_prefetch(), decode, 8 * 1024 * 1024);

        thumbs.set_visible(vec![broken.clone(), good.clone()], 1);
        let pixels = wait_get(&thumbs, &good);
        assert_eq!(pixels.dimensions(), (160, 120));
        assert!(thumbs.get(&broken).is_none());

        thumbs.shutdown();
    }

    #[test]
    fn lru_evicts_at_the_byte_budget_boundary() {
        let dir = TempDir::new();
        let first = dir.file("a.bin");
        let second = dir.file("b.bin");
        let third = dir.file("c.bin");
        let thumb_bytes = 100 * 100 * 4;
        let budget = thumb_bytes * 2;
        let thumbs = Thumbnails::with_decoder(empty_prefetch(), fixed(100, 100), budget);

        thumbs.set_visible(vec![first.clone(), second.clone(), third.clone()], 1);
        wait_get(&thumbs, &third);

        {
            let state = lock(&thumbs.inner.state);
            assert_eq!(state.cache.entries.len(), 2);
            assert_eq!(state.cache.bytes, budget);
        }
        assert!(thumbs.get(&first).is_none(), "oldest entry was not evicted");
        assert!(thumbs.get(&second).is_some());
        assert!(thumbs.get(&third).is_some());
        assert_eq!(CACHE_BUDGET_BYTES, 32 * 1024 * 1024);

        thumbs.shutdown();
    }

    #[test]
    fn changed_file_is_not_served_until_a_fresh_decode_admits_it() {
        let dir = TempDir::new();
        let path = dir.file("changing.bin");
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = Arc::clone(&gate);
        let attempts = Arc::new(AtomicUsize::new(0));
        let decode_attempts = Arc::clone(&attempts);
        let decode = move |path: &Path, _: &AtomicBool, _: DecodePurpose| {
            let attempt = decode_attempts.fetch_add(1, Ordering::Relaxed);
            if attempt > 0 {
                let (lock, wake) = &*worker_gate;
                let mut released = lock.lock().expect("gate");
                while !*released {
                    released = wake.wait(released).expect("gate wait");
                }
            }
            Ok(media(path, if attempt == 0 { 400 } else { 200 }, 200))
        };
        let thumbs = Thumbnails::with_decoder(empty_prefetch(), decode, 8 * 1024 * 1024);

        thumbs.set_visible(vec![path.clone()], 1);
        assert_eq!(wait_get(&thumbs, &path).dimensions(), (160, 80));

        thumbs.set_allowed(false);
        fs::write(&path, b"changed fixture with a different length").expect("rewrite fixture");
        thumbs.set_visible(vec![path.clone()], 2);
        assert!(
            thumbs.get(&path).is_none(),
            "old-epoch pixels were served before revalidation"
        );

        thumbs.set_allowed(true);
        wait_until(|| attempts.load(Ordering::Relaxed) == 2);
        assert!(
            thumbs.get(&path).is_none(),
            "stale pixels were served while the fresh decode was pending"
        );

        {
            let (lock, wake) = &*gate;
            *lock.lock().expect("gate") = true;
            wake.notify_all();
        }
        let mut pixels = None;
        wait_until(|| {
            pixels = thumbs.get(&path);
            pixels
                .as_ref()
                .is_some_and(|pixels| pixels.dimensions() == (120, 120))
        });
        assert_eq!(
            pixels.expect("refreshed thumbnail").dimensions(),
            (120, 120)
        );
        assert_eq!(attempts.load(Ordering::Relaxed), 2);

        thumbs.shutdown();
    }

    #[test]
    fn unchanged_retained_entry_validates_without_second_decode() {
        let dir = TempDir::new();
        let path = dir.file("unchanged.bin");
        let attempts = Arc::new(AtomicUsize::new(0));
        let decode_attempts = Arc::clone(&attempts);
        let decode = move |path: &Path, _: &AtomicBool, _: DecodePurpose| {
            decode_attempts.fetch_add(1, Ordering::Relaxed);
            Ok(media(path, 400, 200))
        };
        let thumbs = Thumbnails::with_decoder(empty_prefetch(), decode, 8 * 1024 * 1024);

        thumbs.set_visible(vec![path.clone()], 1);
        assert_eq!(wait_get(&thumbs, &path).dimensions(), (160, 80));
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        let revision = thumbs.revision();

        thumbs.set_allowed(false);
        thumbs.set_visible(vec![path.clone()], 2);
        assert!(
            thumbs.get(&path).is_none(),
            "retained pixels were visible before current-epoch validation"
        );

        thumbs.set_allowed(true);
        let pixels = wait_get(&thumbs, &path);
        assert_eq!(pixels.dimensions(), (160, 80));
        assert_eq!(
            attempts.load(Ordering::Relaxed),
            1,
            "validation decoded the file again"
        );
        assert!(
            thumbs.revision() > revision,
            "validation did not request a redraw"
        );

        let epoch = lock(&thumbs.inner.state).epoch;
        let revision = thumbs.revision();
        thumbs.set_visible(vec![path.clone()], 2);
        assert_eq!(lock(&thumbs.inner.state).epoch, epoch);
        assert_eq!(thumbs.revision(), revision);
        assert_eq!(attempts.load(Ordering::Relaxed), 1);

        thumbs.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_source_never_serves_retained_pixels() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new();
        let path = dir.png(
            "locked.png",
            &RgbaImage::from_pixel(8, 8, Rgba([7, 7, 7, 255])),
        );
        let thumbs = Thumbnails::new(empty_prefetch());

        thumbs.set_visible(vec![path.clone()], 1);
        assert!(wait_get(&thumbs, &path).width() > 0);

        thumbs.set_allowed(false);
        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&path, permissions).expect("revoke read");
        if fs::File::open(&path).is_ok() {
            eprintln!("skipping unreadable test: process can read mode-000 files");
            let mut permissions = fs::metadata(&path).expect("metadata").permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(&path, permissions).expect("restore read");
            thumbs.shutdown();
            return;
        }

        thumbs.set_visible(vec![path.clone()], 2);
        assert!(thumbs.get(&path).is_none());
        let revision = thumbs.revision();
        thumbs.set_allowed(true);
        wait_until(|| thumbs.revision() > revision);
        assert!(thumbs.get(&path).is_none());

        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&path, permissions).expect("restore read");
        thumbs.shutdown();
    }

    #[test]
    fn deleted_or_non_regular_source_never_serves_retained_pixels() {
        let dir = TempDir::new();
        let path = dir.png(
            "gone.png",
            &RgbaImage::from_pixel(8, 8, Rgba([9, 9, 9, 255])),
        );
        let thumbs = Thumbnails::new(empty_prefetch());

        thumbs.set_visible(vec![path.clone()], 1);
        assert!(wait_get(&thumbs, &path).width() > 0);

        thumbs.set_allowed(false);
        fs::remove_file(&path).expect("delete fixture");
        thumbs.set_visible(vec![path.clone()], 2);
        assert!(thumbs.get(&path).is_none());
        let revision = thumbs.revision();
        thumbs.set_allowed(true);
        wait_until(|| thumbs.revision() > revision);
        assert!(thumbs.get(&path).is_none());

        dir.png(
            "gone.png",
            &RgbaImage::from_pixel(8, 8, Rgba([8, 8, 8, 255])),
        );
        thumbs.set_visible(vec![path.clone()], 3);
        assert!(wait_get(&thumbs, &path).width() > 0);

        thumbs.set_allowed(false);
        fs::remove_file(&path).expect("delete fixture");
        fs::create_dir(&path).expect("replace with directory");
        thumbs.set_visible(vec![path.clone()], 4);
        assert!(thumbs.get(&path).is_none());
        let revision = thumbs.revision();
        thumbs.set_allowed(true);
        wait_until(|| thumbs.revision() > revision);
        assert!(thumbs.get(&path).is_none());

        thumbs.shutdown();
    }

    fn with_exif_orientation(mut jpeg: Vec<u8>, orientation: u16) -> Vec<u8> {
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend_from_slice(&exif_tiff(orientation));
        let mut segment = vec![0xff, 0xe1];
        segment.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        segment.extend_from_slice(&exif);
        jpeg.splice(2..2, segment);
        jpeg
    }

    fn exif_tiff(orientation: u16) -> Vec<u8> {
        let mut exif = vec![0x49, 0x49, 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00];
        exif.extend_from_slice(&[0x01, 0x00]);
        exif.extend_from_slice(&[0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00]);
        exif.extend_from_slice(&orientation.to_le_bytes());
        exif.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        exif
    }
}
