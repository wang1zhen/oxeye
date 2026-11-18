//! Bounded, nearest-first speculative prefetch cache for decoded neighbors.
//!
//! One standard-library worker thread performs at most one speculative decode
//! at a time. The cache is keyed by the absolute selected path (symlinks are
//! not resolved) and invalidated by file length plus modification time. The UI
//! owns all policy calls; this module owns accounting, ordering, promotion,
//! cancellation, and the worker lifecycle.

use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::model::source::{SourceIdentity, SourceRef, source_identity};
use crate::model::{DecodePurpose, LoadedMedia, load_media_for_source};
use crate::settings::PrefetchSettings;

/// How often a foreground join rechecks its cancellation flag.
const JOIN_TICK: Duration = Duration::from_millis(10);

/// Injected decode function, replaced by deterministic fakes in tests.
type DecodeFn = dyn Fn(&SourceRef, &AtomicBool, DecodePurpose) -> Result<Arc<LoadedMedia>, String>
    + Send
    + Sync;

/// Guard for one foreground navigation transaction.
///
/// A top-level [`Prefetcher::foreground_requested_source`] advances the generation and
/// returns this token. Partner-page promotion with a stale token is rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForegroundToken(u64);

/// Cheap snapshot of what the UI currently cares about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: u64,
    pub loading: bool,
    pub hits: u64,
}

/// One decoded image together with the original identity it was verified
/// against. Cache, reservation, joined handoff, and delivery all share this
/// envelope so old pixels are never stamped with fresh metadata.
#[derive(Clone)]
struct DecodedEntry {
    media: Arc<LoadedMedia>,
    identity: SourceIdentity,
    purpose: DecodePurpose,
}

/// A cached decoded image and the bytes it charges.
struct CacheEntry {
    decoded: DecodedEntry,
    bytes: u64,
}

/// A speculative decode request. Its epoch ties the result to one context.
#[derive(Clone)]
struct Job {
    id: u64,
    epoch: u64,
    path: SourceRef,
    purpose: DecodePurpose,
    cancel: Arc<AtomicBool>,
}

/// The single decode currently running.
struct Inflight {
    job: Job,
    /// Foreground loads waiting on this job.
    waiters: usize,
    /// A same-path foreground request has claimed this job.
    promoted: bool,
    /// Completed result kept until every waiter has taken it.
    done: Option<Result<DecodedEntry, String>>,
}

/// A decoded `Arc` claimed by the latest foreground request.
///
/// It leaves cache accounting immediately and stays outside the budget until
/// `load` consumes it, because it is foreground ownership, not a cache alias.
struct ForegroundReservation {
    path: SourceRef,
    decoded: DecodedEntry,
}

/// Result of trying to join a same-path in-flight decode.
enum JoinOutcome {
    Delivered(Arc<LoadedMedia>),
    Cancelled,
    Failed(String),
    NotInflight,
}

/// Non-owning identity memory for the current/previous delivered image.
///
/// It never keeps decoded pixels alive; a former-current candidate can only be
/// upgraded while another owner (UI/cache/reservation) still holds the `Arc`.
#[derive(Clone)]
struct RememberedEntry {
    media: Weak<LoadedMedia>,
    identity: SourceIdentity,
    purpose: DecodePurpose,
}

impl RememberedEntry {
    fn from_entry(entry: &DecodedEntry) -> Self {
        Self {
            media: Arc::downgrade(&entry.media),
            identity: entry.identity.clone(),
            purpose: entry.purpose,
        }
    }

    fn upgrade(&self, displayed: &Arc<LoadedMedia>) -> Option<DecodedEntry> {
        let media = self.media.upgrade()?;
        Arc::ptr_eq(&media, displayed).then_some(DecodedEntry {
            media,
            identity: self.identity.clone(),
            purpose: self.purpose,
        })
    }
}

#[derive(Default)]
struct State {
    settings: PrefetchSettings,
    cache: HashMap<SourceRef, CacheEntry>,
    bytes: u64,
    hits: u64,
    epoch: u64,
    purpose: DecodePurpose,
    siblings: Vec<SourceRef>,
    selected: Option<SourceRef>,
    /// Explicitly visible pages, at most two. Strong and never cache-charged.
    displayed: Vec<Arc<LoadedMedia>>,
    /// Non-owning original identities for the current displayed pages.
    displayed_identities: Vec<RememberedEntry>,
    /// One delivered decoded identity, non-owning.
    trusted: Option<RememberedEntry>,
    /// Second-most-recent delivery identity, non-owning.
    previous: Option<RememberedEntry>,
    allowed: bool,
    inflight: Option<Inflight>,
    foreground: Option<ForegroundReservation>,
    /// Latest source passed to [`Prefetcher::foreground_requested_source`].
    foreground_target: Option<SourceRef>,
    /// Generation of the current top-level foreground transaction.
    foreground_generation: u64,
    failed: HashSet<SourceRef>,
    next_id: u64,
}

impl State {
    fn new(settings: PrefetchSettings) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }
}

/// Snapshot handed to the worker without holding the lock during file I/O.
struct Demand {
    epoch: u64,
    purpose: DecodePurpose,
    settings: PrefetchSettings,
    selected: Option<SourceRef>,
    order: Vec<SourceRef>,
    failed: HashSet<SourceRef>,
    cached: HashSet<SourceRef>,
    visible: Vec<SourceRef>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    shutdown: AtomicBool,
    decode: Arc<DecodeFn>,
    #[cfg(test)]
    handoff_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Shared {
    fn notify(&self) {
        self.wake.notify_all();
    }

    fn stop(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        {
            let mut state = lock(&self.state);
            state.foreground_generation = state.foreground_generation.wrapping_add(1);
            state.foreground = None;
            if let Some(inflight) = &state.inflight {
                inflight.job.cancel.store(true, Ordering::Relaxed);
            }
        }
        self.notify();
    }
}

struct Inner {
    shared: Arc<Shared>,
    _worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Never joins: an uninterruptible single decode may still be running.
        self.shared.stop();
    }
}

/// Handle shared by the UI and its background loader.
#[derive(Clone)]
pub struct Prefetcher {
    inner: Arc<Inner>,
}

impl Prefetcher {
    /// Starts the speculative worker with the given limits.
    pub fn new(settings: PrefetchSettings) -> Self {
        Self::spawn(
            settings,
            Arc::new(|path, cancel, purpose| {
                load_media_for_source(path, cancel, purpose).map(Arc::new)
            }),
        )
    }

    fn spawn(settings: PrefetchSettings, decode: Arc<DecodeFn>) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::new(settings)),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            decode,
            #[cfg(test)]
            handoff_hook: Mutex::new(None),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("oxeye-prefetch".to_owned())
            .spawn(move || run(worker_shared))
            .expect("spawn prefetch worker");
        Self {
            inner: Arc::new(Inner {
                shared,
                _worker: Mutex::new(Some(worker)),
            }),
        }
    }

    /// Starts one top-level foreground transaction and returns its guard.
    ///
    /// The latest request supersedes any older reservation. A cached image
    /// is moved out of the cache into a one-slot foreground reservation so a
    /// subsequent `update_context` cannot prune it before `load` runs. A
    /// same-path in-flight decode is preserved and may also be promoted by
    /// [`Self::load_source`]; a different in-flight decode is asked to stop.
    #[cfg(test)]
    pub fn foreground_requested(&self, path: &Path) -> ForegroundToken {
        self.foreground_requested_source(&SourceRef::file(path))
    }

    pub fn foreground_requested_source(&self, source: &SourceRef) -> ForegroundToken {
        let absolute = source.clone();
        let mut state = lock(&self.inner.shared.state);
        state.foreground_generation = state.foreground_generation.wrapping_add(1);
        let token = ForegroundToken(state.foreground_generation);
        common_request_mutation(&mut state, &absolute);
        drop(state);
        self.inner.shared.notify();
        token
    }

    /// Promotes the second page of the transaction that owns `token`.
    ///
    /// The token check and the reservation/target/in-flight mutation happen
    /// under one lock. A stale token returns `false` without mutating state, so
    /// an old Book worker cannot drop or cancel the new transaction's page.
    /// Valid partner promotion does not advance the generation.
    #[cfg(test)]
    pub fn promote_if_current(&self, path: &Path, token: ForegroundToken) -> bool {
        self.promote_source_if_current(&SourceRef::file(path), token)
    }

    pub fn promote_source_if_current(&self, source: &SourceRef, token: ForegroundToken) -> bool {
        let absolute = source.clone();
        let mut state = lock(&self.inner.shared.state);
        if state.foreground_generation != token.0
            || self.inner.shared.shutdown.load(Ordering::Relaxed)
        {
            return false;
        }
        common_request_mutation(&mut state, &absolute);
        drop(state);
        self.inner.shared.notify();
        true
    }

    /// Loads `path`, promoting a cache entry or an in-flight same-path decode.
    ///
    /// This blocks and must run on a background thread only. `cancel` is the
    /// caller's own cancellation flag; stale results still need to be rejected
    /// by the caller's revision check.
    #[cfg(test)]
    pub fn load(&self, path: &Path, cancel: &AtomicBool) -> Result<Arc<LoadedMedia>, String> {
        self.load_source(&SourceRef::file(path), cancel)
    }

    pub fn load_source(
        &self,
        source: &SourceRef,
        cancel: &AtomicBool,
    ) -> Result<Arc<LoadedMedia>, String> {
        let absolute = source.clone();
        if cancel.load(Ordering::Relaxed) {
            return Err(format!("{}: cancelled", absolute.display()));
        }

        if let Some(media) = self.take_reserved(&absolute) {
            return Ok(media);
        }
        if let Some(media) = self.take_cached(&absolute) {
            return Ok(media);
        }
        match self.join_or_decode(&absolute, cancel) {
            JoinOutcome::Delivered(media) => return Ok(media),
            JoinOutcome::Failed(error) => return Err(error),
            JoinOutcome::Cancelled | JoinOutcome::NotInflight => {}
        }

        if cancel.load(Ordering::Relaxed) || self.inner.shared.shutdown.load(Ordering::Relaxed) {
            return Err(format!("{}: cancelled", absolute.display()));
        }
        self.decode_fresh(&absolute, cancel)
    }

    /// Test-only single-display compatibility wrapper around [`Self::update_context_many`].
    #[cfg(test)]
    pub fn update_context(
        &self,
        siblings: Vec<PathBuf>,
        selected: Option<PathBuf>,
        displayed: Option<Arc<LoadedMedia>>,
    ) {
        self.update_context_many(siblings, selected, displayed.into_iter().collect());
    }

    /// Replaces the current neighborhood and the explicitly visible pages.
    ///
    /// `displayed` holds the one or two pages the UI currently shows. All of
    /// them are excluded from cache accounting, and speculative prefetch is
    /// computed outside their file span. Pure coordination: no file I/O and no
    /// decoding happen on this call.
    #[cfg(test)]
    pub fn update_context_many(
        &self,
        siblings: Vec<PathBuf>,
        selected: Option<PathBuf>,
        displayed: Vec<Arc<LoadedMedia>>,
    ) {
        self.update_context_many_sources(
            siblings.iter().map(|path| SourceRef::file(path)).collect(),
            selected.as_deref().map(SourceRef::file),
            displayed,
        );
    }

    pub fn update_context_many_sources(
        &self,
        siblings: Vec<SourceRef>,
        selected: Option<SourceRef>,
        displayed: Vec<Arc<LoadedMedia>>,
    ) {
        let mut state = lock(&self.inner.shared.state);
        if state.siblings == siblings
            && state.selected == selected
            && same_displayed_list(&state.displayed, &displayed)
        {
            let old_identities = std::mem::take(&mut state.displayed_identities);
            state.displayed = displayed;
            rebuild_displayed_identities(&mut state, &old_identities);
            if !foreground_is_current(&state) {
                state.foreground = None;
            }
            return;
        }

        let old_displayed = std::mem::take(&mut state.displayed);
        let old_identities = std::mem::take(&mut state.displayed_identities);

        state.siblings = siblings;
        state.selected = selected;
        state.displayed = displayed;
        rebuild_displayed_identities(&mut state, &old_identities);
        state.epoch = state.epoch.wrapping_add(1);
        state.failed.clear();
        if !foreground_is_current(&state) {
            state.foreground = None;
        }
        rebalance(&mut state);
        let order = retention_order(&state);
        let epoch = state.epoch;
        let purpose = state.purpose;
        let selected = state.selected.clone();
        let foreground_target = state.foreground_target.clone();
        if let Some(inflight) = &mut state.inflight {
            if inflight.job.purpose != purpose {
                inflight.job.cancel.store(true, Ordering::Relaxed);
            } else if inflight.promoted {
                let wanted = selected.as_ref() == Some(&inflight.job.path)
                    || foreground_target.as_ref() == Some(&inflight.job.path);
                if !wanted {
                    inflight.promoted = false;
                    inflight.job.cancel.store(true, Ordering::Relaxed);
                }
            } else if inflight.waiters == 0 {
                if order.contains(&inflight.job.path) {
                    inflight.job.epoch = epoch;
                } else {
                    inflight.job.cancel.store(true, Ordering::Relaxed);
                }
            }
        }

        // Formerly visible pages can re-enter the cache using their original
        // non-owning identity. No file I/O here: `load` revalidates freshness.
        let candidates: Vec<DecodedEntry> = old_displayed
            .iter()
            .filter_map(|media| {
                if state.displayed.iter().any(|new| same_media(new, media))
                    || state.selected.as_ref() == Some(&media.source)
                {
                    return None;
                }
                let entry = old_identities
                    .iter()
                    .find_map(|remembered| remembered.upgrade(media))
                    .or_else(|| remembered_entry(&state, media))?;
                (entry.purpose == state.purpose).then_some(entry)
            })
            .collect();
        drop(state);

        for entry in candidates {
            let path = entry.media.source.clone();
            let mut state = lock(&self.inner.shared.state);
            if state.epoch == epoch {
                admit(&mut state, &path, entry);
            }
        }
        self.inner.shared.notify();
    }

    /// Switches the decode policy for new speculative and foreground work.
    ///
    /// Clears decoded state that belongs to the old policy and cancels every
    /// in-flight decode, including a promoted one. Currently displayed pages
    /// stay visible: they remain the UI's last good image during the switch.
    pub fn set_purpose(&self, purpose: DecodePurpose) {
        let mut state = lock(&self.inner.shared.state);
        if state.purpose == purpose {
            return;
        }
        state.purpose = purpose;
        state.foreground_generation = state.foreground_generation.wrapping_add(1);
        state.epoch = state.epoch.wrapping_add(1);
        state.cache.clear();
        state.bytes = 0;
        state.foreground = None;
        state.failed.clear();
        if let Some(inflight) = &state.inflight {
            inflight.job.cancel.store(true, Ordering::Relaxed);
        }
        drop(state);
        self.inner.shared.notify();
    }

    /// A cache-only dimension hint: no freshness I/O, promotion, or hit accounting.
    pub fn cached_dimensions(&self, source: &SourceRef) -> Option<(u32, u32)> {
        let state = lock(&self.inner.shared.state);
        let frame = state.cache.get(source)?.decoded.media.frames.first()?;
        Some(frame.pixels.dimensions())
    }

    /// Non-consuming cache/current/reservation lookup for the thumbnail worker.
    ///
    /// Background use only. Returns a borrowed `Arc` without promoting,
    /// reserving, recording a hit, or decoding; the caller must resize and drop
    /// it promptly. Returns `None` when the file is no longer fresh or was never
    /// recorded with an original identity.
    #[cfg(test)]
    pub fn peek(&self, path: &Path, cancel: &AtomicBool) -> Option<Arc<LoadedMedia>> {
        self.peek_source(&SourceRef::file(path), cancel)
    }

    pub fn peek_source(&self, source: &SourceRef, cancel: &AtomicBool) -> Option<Arc<LoadedMedia>> {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let absolute = source.clone();
        let found = {
            let state = lock(&self.inner.shared.state);
            let cache = state
                .cache
                .get(&absolute)
                .map(|entry| entry.decoded.clone());
            let reserved = state
                .foreground
                .as_ref()
                .filter(|reservation| reservation.path == absolute)
                .map(|reservation| reservation.decoded.clone());
            let displayed = state
                .displayed
                .iter()
                .find(|media| media.source == absolute)
                .and_then(|media| {
                    state
                        .displayed_identities
                        .iter()
                        .find_map(|remembered| remembered.upgrade(media))
                });
            cache.or(reserved).or(displayed)
        }?;
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if !readable_fresh(&absolute, &found.identity) {
            return None;
        }
        Some(found.media)
    }

    /// Enables or suppresses new speculative decodes.
    ///
    /// A valid promoted same-path request is never cancelled here.
    pub fn set_allowed(&self, allowed: bool) {
        let mut state = lock(&self.inner.shared.state);
        state.allowed = allowed;
        if !allowed
            && let Some(inflight) = &state.inflight
            && !inflight.promoted
            && inflight.waiters == 0
        {
            inflight.job.cancel.store(true, Ordering::Relaxed);
        }
        self.inner.shared.notify();
    }

    /// Applies new limits, pruning immediately and cancelling stale work.
    pub fn configure(&self, settings: PrefetchSettings) {
        if settings.validate().is_err() {
            return;
        }
        let mut state = lock(&self.inner.shared.state);
        if state.settings == settings {
            return;
        }
        state.settings = settings;
        state.epoch = state.epoch.wrapping_add(1);
        state.failed.clear();
        rebalance(&mut state);
        if let Some(inflight) = &state.inflight
            && !inflight.promoted
            && inflight.waiters == 0
        {
            inflight.job.cancel.store(true, Ordering::Relaxed);
        }
        self.inner.shared.notify();
    }

    pub fn stats(&self) -> CacheStats {
        let state = lock(&self.inner.shared.state);
        CacheStats {
            entries: state.cache.len(),
            bytes: state.bytes,
            loading: state.inflight.is_some(),
            hits: state.hits,
        }
    }

    /// Stops the worker without waiting for a running decode. Idempotent.
    pub fn shutdown(&self) {
        self.inner.shared.stop();
    }

    fn take_cached(&self, path: &SourceRef) -> Option<Arc<LoadedMedia>> {
        let cached = {
            let state = lock(&self.inner.shared.state);
            state
                .cache
                .get(path)
                .filter(|entry| entry.decoded.purpose == state.purpose)
                .map(|entry| entry.decoded.clone())
        }?;
        let fresh = readable_fresh(path, &cached.identity);

        let mut state = lock(&self.inner.shared.state);
        let matching = state.cache.get(path).is_some_and(|entry| {
            entry.decoded.purpose == state.purpose
                && Arc::ptr_eq(&entry.decoded.media, &cached.media)
        });
        if matching {
            if fresh {
                let entry = state.cache.remove(path).expect("cache entry");
                state.bytes = state.bytes.saturating_sub(entry.bytes);
                state.hits += 1;
                // Promoted to foreground ownership; do not re-prefetch it.
                state.failed.insert(path.clone());
                remember_delivery(&mut state, &entry.decoded);
                return Some(entry.decoded.media);
            }
            let entry = state.cache.remove(path).expect("cache entry");
            state.bytes = state.bytes.saturating_sub(entry.bytes);
        }
        None
    }

    /// Consumes the latest foreground reservation if it is still fresh.
    fn take_reserved(&self, path: &SourceRef) -> Option<Arc<LoadedMedia>> {
        let reserved = {
            let state = lock(&self.inner.shared.state);
            state
                .foreground
                .as_ref()
                .filter(|reservation| {
                    &reservation.path == path && reservation.decoded.purpose == state.purpose
                })
                .map(|reservation| reservation.decoded.clone())
        }?;
        let fresh = readable_fresh(path, &reserved.identity);

        let mut state = lock(&self.inner.shared.state);
        let matching = state.foreground.as_ref().is_some_and(|reservation| {
            &reservation.path == path
                && reservation.decoded.purpose == state.purpose
                && Arc::ptr_eq(&reservation.decoded.media, &reserved.media)
        });
        if fresh && matching {
            let reservation = state.foreground.take().expect("reserved entry");
            state.hits += 1;
            state.failed.insert(path.clone());
            remember_delivery(&mut state, &reservation.decoded);
            return Some(reservation.decoded.media);
        }
        if !fresh && matching {
            state.foreground = None;
        }
        None
    }

    /// Decodes `path` in the foreground and records its verified identity.
    fn decode_fresh(
        &self,
        path: &SourceRef,
        cancel: &AtomicBool,
    ) -> Result<Arc<LoadedMedia>, String> {
        let identity = source_identity(path);
        let purpose = lock(&self.inner.shared.state).purpose;
        if cancel.load(Ordering::Relaxed) {
            return Err(format!("{}: cancelled", path.display()));
        }
        lock(&self.inner.shared.state).failed.insert(path.clone());
        let media = (self.inner.shared.decode)(path, cancel, purpose)?;
        if cancel.load(Ordering::Relaxed) || self.inner.shared.shutdown.load(Ordering::Relaxed) {
            return Err(format!("{}: cancelled", path.display()));
        }
        // All file I/O stays outside the shared mutex.
        let verified = identity.filter(|identity| readable_fresh(path, identity));
        #[cfg(test)]
        if let Some(hook) = lock(&self.inner.shared.handoff_hook).take() {
            hook();
        }
        let mut state = lock(&self.inner.shared.state);
        if cancel.load(Ordering::Relaxed) || self.inner.shared.shutdown.load(Ordering::Relaxed) {
            return Err(format!("{}: cancelled", path.display()));
        }
        if state.purpose != purpose {
            return Err(format!(
                "{}: cancelled: decode policy changed",
                path.display()
            ));
        }
        if let Some(identity) = verified {
            let entry = DecodedEntry {
                media: Arc::clone(&media),
                identity,
                purpose,
            };
            remember_delivery(&mut state, &entry);
        }
        Ok(media)
    }

    /// Attaches to a same-path in-flight job.
    fn join_or_decode(&self, path: &SourceRef, cancel: &AtomicBool) -> JoinOutcome {
        let shared = &self.inner.shared;
        let id = {
            let mut state = lock(&shared.state);
            let id = {
                let purpose = state.purpose;
                let Some(inflight) = state.inflight.as_mut().filter(|inflight| {
                    &inflight.job.path == path && inflight.job.purpose == purpose
                }) else {
                    return JoinOutcome::NotInflight;
                };
                inflight.promoted = true;
                inflight.waiters += 1;
                inflight.job.id
            };
            state.failed.insert(path.clone());
            id
        };

        let mut state = lock(&shared.state);
        loop {
            if shared.shutdown.load(Ordering::Relaxed) {
                detach(&mut state, id, false);
                shared.notify();
                return JoinOutcome::Cancelled;
            }
            if cancel.load(Ordering::Relaxed) {
                detach(&mut state, id, false);
                shared.notify();
                return JoinOutcome::Cancelled;
            }
            let Some(inflight) = state
                .inflight
                .as_ref()
                .filter(|inflight| inflight.job.id == id)
            else {
                return JoinOutcome::NotInflight;
            };
            if let Some(result) = &inflight.done {
                if inflight.job.purpose != state.purpose {
                    detach(&mut state, id, true);
                    shared.notify();
                    return JoinOutcome::NotInflight;
                }
                let result = result.clone();
                detach(&mut state, id, true);
                shared.notify();
                drop(state);
                #[cfg(test)]
                if let Some(hook) = lock(&shared.handoff_hook).take() {
                    hook();
                }
                return match result {
                    Ok(entry) => {
                        if !readable_fresh(&entry.media.source, &entry.identity) {
                            return JoinOutcome::NotInflight;
                        }
                        let mut state = lock(&shared.state);
                        // The hook and freshness check ran unlocked; the policy
                        // may have changed while they did.
                        if state.purpose != entry.purpose {
                            return JoinOutcome::NotInflight;
                        }
                        if cancel.load(Ordering::Relaxed) || shared.shutdown.load(Ordering::Relaxed)
                        {
                            return JoinOutcome::Cancelled;
                        }
                        remember_delivery(&mut state, &entry);
                        JoinOutcome::Delivered(entry.media)
                    }
                    Err(error) if error.ends_with("cancelled") => JoinOutcome::Cancelled,
                    Err(error) => JoinOutcome::Failed(error),
                };
            }
            if inflight.waiters == 0 && !inflight.promoted {
                return JoinOutcome::Cancelled;
            }
            state = match shared.wake.wait_timeout(state, JOIN_TICK) {
                Ok((state, _)) => state,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    /// Test-only: wait for the worker to exit after [`Self::shutdown`].
    #[cfg(test)]
    fn join_worker(&self) {
        if let Some(worker) = lock(&self.inner._worker).take() {
            let _ = worker.join();
        }
    }

    /// Test-only: run `hook` at an unlocked validation boundary.
    ///
    /// A joined result calls it before freshness is revalidated; a fresh
    /// foreground decode calls it after validation, before reacquiring state.
    /// Tests use it to hold the boundary and exercise UI coordination.
    #[cfg(test)]
    fn set_handoff_hook(&self, hook: impl FnOnce() + Send + 'static) {
        *lock(&self.inner.shared.handoff_hook) = Some(Box::new(hook));
    }
}

/// Shared foreground request mutation, called with the state lock held.
fn common_request_mutation(state: &mut State, absolute: &SourceRef) {
    state.foreground_target = Some(absolute.clone());
    if state
        .foreground
        .as_ref()
        .is_some_and(|reservation| &reservation.path != absolute)
    {
        state.foreground = None;
    }
    if state.foreground.is_none()
        && let Some(entry) = state.cache.remove(absolute)
    {
        state.bytes = state.bytes.saturating_sub(entry.bytes);
        state.foreground = Some(ForegroundReservation {
            path: absolute.clone(),
            decoded: entry.decoded,
        });
    }
    state.failed.insert(absolute.clone());
    if let Some(inflight) = &mut state.inflight {
        if &inflight.job.path == absolute {
            inflight.promoted = true;
        } else {
            inflight.promoted = false;
            inflight.job.cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn run(shared: Arc<Shared>) {
    while let Some(demand) = wait_for_demand(&shared) {
        let mut failed = Vec::new();
        let mut chosen = None;
        for path in &demand.order {
            if demand.failed.contains(path)
                || demand.cached.contains(path)
                || demand.visible.iter().any(|visible| visible == path)
            {
                continue;
            }
            match source_identity(path) {
                Some(identity) => {
                    chosen = Some((path.clone(), identity));
                    break;
                }
                None => failed.push(path.clone()),
            }
        }

        let mut state = lock(&shared.state);
        for path in failed {
            if state.epoch == demand.epoch {
                state.failed.insert(path);
            }
        }
        let (path, identity) = match chosen {
            Some(chosen) => chosen,
            None => continue,
        };
        let order = speculation_order(&state).unwrap_or_default();
        let still_wanted = !shared.shutdown.load(Ordering::Relaxed)
            && state.epoch == demand.epoch
            && state.purpose == demand.purpose
            && state.allowed
            && state.inflight.is_none()
            && state.settings == demand.settings
            && state.selected == demand.selected
            && order.contains(&path)
            && !state.cache.contains_key(&path)
            && !state.failed.contains(&path)
            && !state.displayed.iter().any(|media| media.source == path);
        if !still_wanted {
            continue;
        }

        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        let cancel = Arc::new(AtomicBool::new(false));
        state.inflight = Some(Inflight {
            job: Job {
                id,
                epoch: demand.epoch,
                path: path.clone(),
                purpose: demand.purpose,
                cancel: Arc::clone(&cancel),
            },
            waiters: 0,
            promoted: false,
            done: None,
        });
        drop(state);

        let result = execute(&shared, &path, identity, &cancel, demand.purpose);
        finish(&shared, id, path, result);
    }
}

fn wait_for_demand(shared: &Shared) -> Option<Demand> {
    let mut state = lock(&shared.state);
    loop {
        if shared.shutdown.load(Ordering::Relaxed) {
            return None;
        }
        if state.allowed
            && state.inflight.is_none()
            && state.settings.memory_mib > 0
            && (state.settings.before > 0 || state.settings.after > 0)
            && let Some(order) = speculation_order(&state)
        {
            let visible: Vec<SourceRef> = state
                .displayed
                .iter()
                .map(|media| media.source.clone())
                .collect();
            let needed = order.iter().any(|path| {
                !state.cache.contains_key(path)
                    && !state.failed.contains(path)
                    && !visible.contains(path)
            });
            if needed {
                return Some(Demand {
                    epoch: state.epoch,
                    purpose: state.purpose,
                    settings: state.settings.clone(),
                    selected: state.selected.clone(),
                    order,
                    failed: state.failed.clone(),
                    cached: state.cache.keys().cloned().collect(),
                    visible,
                });
            }
        }
        state = match shared.wake.wait(state) {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
    }
}

fn execute(
    shared: &Shared,
    path: &SourceRef,
    identity: SourceIdentity,
    cancel: &AtomicBool,
    purpose: DecodePurpose,
) -> Result<DecodedEntry, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err(format!("{}: cancelled", path.display()));
    }
    if !readable_fresh(path, &identity) {
        return Err(format!("{}: file changed while preloading", path.display()));
    }
    let media = (shared.decode)(path, cancel, purpose)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(format!("{}: cancelled", path.display()));
    }
    if !readable_fresh(path, &identity) {
        return Err(format!("{}: file changed while preloading", path.display()));
    }
    Ok(DecodedEntry {
        media,
        identity,
        purpose,
    })
}

fn finish(shared: &Shared, id: u64, path: SourceRef, result: Result<DecodedEntry, String>) {
    let mut state = lock(&shared.state);
    let Some(mut inflight) = state.inflight.take() else {
        return;
    };
    if inflight.job.id != id {
        state.inflight = Some(inflight);
        return;
    }
    if shared.shutdown.load(Ordering::Relaxed) {
        shared.notify();
        return;
    }
    if inflight.job.purpose != state.purpose {
        // Waiters observe the missing job and retry under the new policy.
        shared.notify();
        return;
    }
    if inflight.waiters > 0 {
        inflight.done = Some(result);
        state.inflight = Some(inflight);
        shared.notify();
        return;
    }

    match result {
        Ok(entry) => {
            if inflight.promoted {
                // The foreground requested this exact path and no waiter has
                // consumed it yet: keep the verified result as a zero-charge
                // reservation until `load` (or a newer request) takes it.
                let keep = state
                    .foreground_target
                    .as_ref()
                    .is_none_or(|target| target == &path)
                    && state
                        .foreground
                        .as_ref()
                        .is_none_or(|reservation| reservation.path == path);
                if keep {
                    state.foreground = Some(ForegroundReservation {
                        path: path.clone(),
                        decoded: entry,
                    });
                }
                state.failed.insert(path);
            } else if inflight.job.epoch == state.epoch && !admit(&mut state, &path, entry) {
                state.failed.insert(path);
            }
        }
        Err(_) => {
            if inflight.job.epoch == state.epoch || inflight.promoted {
                state.failed.insert(path);
            }
        }
    }
    shared.notify();
}

/// Removes a waiting caller; optionally consumes its stored result.
fn detach(state: &mut State, id: u64, took_result: bool) {
    let Some(mut inflight) = state.inflight.take() else {
        return;
    };
    if inflight.job.id != id {
        state.inflight = Some(inflight);
        return;
    }
    inflight.waiters = inflight.waiters.saturating_sub(1);
    if took_result {
        if inflight.waiters == 0 {
            inflight.done = None;
        }
        if inflight.waiters > 0 {
            state.inflight = Some(inflight);
        }
        return;
    }
    if inflight.waiters > 0 {
        state.inflight = Some(inflight);
        return;
    }
    inflight.promoted = false;
    let epoch = inflight.job.epoch;
    match inflight.done.take() {
        Some(Ok(entry)) => {
            if epoch == state.epoch
                && inflight.job.purpose == state.purpose
                && entry.purpose == state.purpose
            {
                let path = inflight.job.path.clone();
                if !admit(state, &path, entry) {
                    state.failed.insert(path);
                }
            }
        }
        Some(Err(_)) => {}
        None => {
            // Still decoding; keep the job so cancellation can reach it.
            state.inflight = Some(inflight);
        }
    }
}

#[cfg(test)]
pub(crate) fn neighbor_order(
    siblings: &[PathBuf],
    selected: Option<&Path>,
    settings: &PrefetchSettings,
) -> Vec<PathBuf> {
    let Some(selected) = selected else {
        return Vec::new();
    };
    let Some(index) = siblings.iter().position(|path| path == selected) else {
        return Vec::new();
    };
    neighbor_order_span(siblings, (index, index), settings)
}

/// Nearest-first files strictly outside the visible span.
fn neighbor_order_span<T: Clone>(
    siblings: &[T],
    (first, last): (usize, usize),
    settings: &PrefetchSettings,
) -> Vec<T> {
    let mut order = Vec::new();
    for distance in 1..=settings.before.max(settings.after) {
        if distance <= settings.after
            && let Some(path) = siblings.get(last + distance)
        {
            order.push(path.clone());
        }
        if distance <= settings.before
            && let Some(path) = first.checked_sub(distance).and_then(|i| siblings.get(i))
        {
            order.push(path.clone());
        }
    }
    order
}

fn selected_index(state: &State) -> Option<usize> {
    let selected = state.selected.as_ref()?;
    state.siblings.iter().position(|path| path == selected)
}

/// Committed visible span, when every displayed path is still in this folder.
fn committed_span(state: &State) -> Option<(usize, usize)> {
    let mut span: Option<(usize, usize)> = None;
    for media in &state.displayed {
        let Some(index) = state.siblings.iter().position(|path| path == &media.source) else {
            continue;
        };
        span = Some(match span {
            None => (index, index),
            Some((first, last)) => (first.min(index), last.max(index)),
        });
    }
    span
}

/// Span that may start a speculative decode, or `None` when it is unsafe.
///
/// With no explicit display yet, the selected file is the span. Otherwise the
/// selected file must fall inside the committed visible span; a pending
/// mismatch disables new speculation instead of guessing.
fn speculation_span(state: &State) -> Option<(usize, usize)> {
    let selected_index = selected_index(state)?;
    if state.displayed.is_empty() {
        return Some((selected_index, selected_index));
    }
    let (first, last) = committed_span(state)?;
    (first <= selected_index && selected_index <= last).then_some((first, last))
}

fn speculation_order(state: &State) -> Option<Vec<SourceRef>> {
    speculation_span(state).map(|span| neighbor_order_span(&state.siblings, span, &state.settings))
}

/// Neighborhood retained in cache, independent of launch permission.
///
/// A pending selected file that is already known keeps its own single-file
/// range while old pages are still displayed. Unknown folders retain nothing,
/// so stale entries from the previous directory cannot survive.
fn retention_order(state: &State) -> Vec<SourceRef> {
    let Some(selected_index) = selected_index(state) else {
        return Vec::new();
    };
    let span = if state.displayed.is_empty() {
        (selected_index, selected_index)
    } else {
        match committed_span(state) {
            Some((first, last)) if first <= selected_index && selected_index <= last => {
                (first, last)
            }
            // Pending navigation: retain around the selected file until the
            // new spread commits. New speculation still stays disabled.
            Some(_) => (selected_index, selected_index),
            None => return Vec::new(),
        }
    };
    neighbor_order_span(&state.siblings, span, &state.settings)
}

fn rebalance(state: &mut State) {
    let order = retention_order(state);
    let selected = state.selected.clone();
    let stale: Vec<SourceRef> = state
        .cache
        .iter()
        .filter(|(path, entry)| {
            !order.contains(path)
                || selected.as_ref() == Some(path)
                || state
                    .displayed
                    .iter()
                    .any(|media| Arc::ptr_eq(media, &entry.decoded.media) || media.source == **path)
        })
        .map(|(path, _)| path.clone())
        .collect();
    for path in stale {
        remove_entry(state, &path);
    }
    enforce_budget(state, &order);
}

/// Inserts a decoded neighbor if it is in range, not an alias, and fits.
fn admit(state: &mut State, path: &SourceRef, decoded: DecodedEntry) -> bool {
    if state.settings.memory_mib == 0 || (state.settings.before == 0 && state.settings.after == 0) {
        return false;
    }
    if decoded.purpose != state.purpose {
        return false;
    }
    if state.selected.as_ref() == Some(path) {
        return false;
    }
    if state.cache.contains_key(path) {
        return false;
    }
    if state
        .displayed
        .iter()
        .any(|displayed| Arc::ptr_eq(displayed, &decoded.media) || &displayed.source == path)
    {
        return false;
    }
    if state
        .cache
        .values()
        .any(|entry| Arc::ptr_eq(&entry.decoded.media, &decoded.media))
    {
        return false;
    }
    let order = retention_order(state);
    if !order.iter().any(|candidate| candidate == path) {
        return false;
    }
    let bytes = media_bytes(&decoded.media);
    let budget = state.settings.memory_mib.saturating_mul(1024 * 1024);
    if bytes == 0 || bytes > budget {
        return false;
    }
    state
        .cache
        .insert(path.clone(), CacheEntry { decoded, bytes });
    state.bytes += bytes;
    enforce_budget(state, &order);
    state.cache.contains_key(path)
}

fn enforce_budget(state: &mut State, order: &[SourceRef]) {
    let budget = state.settings.memory_mib.saturating_mul(1024 * 1024);
    for path in order.iter().rev() {
        if state.bytes <= budget {
            return;
        }
        remove_entry(state, path);
    }
    if state.bytes > budget {
        state.cache.clear();
        state.bytes = 0;
    }
}

fn remove_entry(state: &mut State, path: &SourceRef) {
    if let Some(entry) = state.cache.remove(path) {
        state.bytes = state.bytes.saturating_sub(entry.bytes);
    }
}

fn media_bytes(media: &LoadedMedia) -> u64 {
    media
        .frames
        .iter()
        .map(|frame| frame.pixels.as_raw().len() as u64)
        .sum()
}

fn same_media(left: &Arc<LoadedMedia>, right: &Arc<LoadedMedia>) -> bool {
    Arc::ptr_eq(left, right) || left.source == right.source
}

fn same_displayed_list(left: &[Arc<LoadedMedia>], right: &[Arc<LoadedMedia>]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| same_media(left, right))
}

/// Keeps the latest foreground reservation across context churn.
///
/// It survives while it is the latest requested target and either that target
/// is selected or old pages are still displayed (a spread transition).
fn foreground_is_current(state: &State) -> bool {
    state.foreground.as_ref().is_some_and(|reservation| {
        state.foreground_target.as_ref() == Some(&reservation.path)
            && (state.selected.as_ref() == Some(&reservation.path) || !state.displayed.is_empty())
    })
}

fn rebuild_displayed_identities(state: &mut State, old: &[RememberedEntry]) {
    let mut next = Vec::new();
    for media in &state.displayed {
        let found = old
            .iter()
            .chain(state.trusted.iter())
            .chain(state.previous.iter())
            .find(|remembered| {
                remembered
                    .media
                    .upgrade()
                    .is_some_and(|candidate| Arc::ptr_eq(&candidate, media))
            })
            .cloned();
        if let Some(found) = found {
            next.push(found);
        }
    }
    state.displayed_identities = next;
}

#[cfg(test)]
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn remember_delivery(state: &mut State, entry: &DecodedEntry) {
    let same = state.trusted.as_ref().is_some_and(|trusted| {
        trusted
            .media
            .upgrade()
            .is_some_and(|media| Arc::ptr_eq(&media, &entry.media))
    });
    if !same {
        state.previous = state.trusted.take();
    }
    state.trusted = Some(RememberedEntry::from_entry(entry));
}

fn remembered_entry(state: &State, displayed: &Arc<LoadedMedia>) -> Option<DecodedEntry> {
    state
        .trusted
        .as_ref()
        .and_then(|entry| entry.upgrade(displayed))
        .or_else(|| {
            state
                .previous
                .as_ref()
                .and_then(|entry| entry.upgrade(displayed))
        })
}

fn readable_fresh(path: &SourceRef, identity: &SourceIdentity) -> bool {
    source_identity(path).as_ref() == Some(identity)
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
    use image::{Rgba, RgbaImage};
    use std::fs;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::time::{Instant, SystemTime};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("oxeye-preload-{}-{unique}", std::process::id()));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, b"fixture").expect("write fixture");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn media(path: &Path, width: u32, height: u32, frames: usize) -> LoadedMedia {
        LoadedMedia {
            source: SourceRef::file(path),
            frames: (0..frames)
                .map(|_| MediaFrame {
                    pixels: Arc::new(RgbaImage::from_pixel(width, height, Rgba([1, 2, 3, 255]))),
                    delay: Duration::ZERO,
                })
                .collect(),
            loop_mode: LoopMode::Once,
        }
    }

    fn fixed(
        width: u32,
        height: u32,
        frames: usize,
    ) -> impl Fn(&Path, &AtomicBool) -> Result<Arc<LoadedMedia>, String> + Send + Sync + 'static
    {
        move |path, _| Ok(Arc::new(media(path, width, height, frames)))
    }

    fn with_fake<F>(settings: PrefetchSettings, decode: F) -> Prefetcher
    where
        F: Fn(&Path, &AtomicBool) -> Result<Arc<LoadedMedia>, String> + Send + Sync + 'static,
    {
        with_purpose_fake(settings, move |path, cancel, _| decode(path, cancel))
    }

    fn with_purpose_fake<F>(settings: PrefetchSettings, decode: F) -> Prefetcher
    where
        F: Fn(&Path, &AtomicBool, DecodePurpose) -> Result<Arc<LoadedMedia>, String>
            + Send
            + Sync
            + 'static,
    {
        Prefetcher::spawn(
            settings,
            Arc::new(move |source, cancel, purpose| decode(source.path(), cancel, purpose)),
        )
    }

    fn settings(before: usize, after: usize, memory_mib: u64) -> PrefetchSettings {
        PrefetchSettings {
            before,
            after,
            memory_mib,
        }
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let start = Instant::now();
        while !condition() {
            assert!(start.elapsed() < Duration::from_secs(5), "timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn cached_paths(prefetcher: &Prefetcher) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = lock(&prefetcher.inner.shared.state)
            .cache
            .keys()
            .map(|source| source.path().to_path_buf())
            .collect();
        paths.sort();
        paths
    }

    fn weak_retains(state: &State, path: &Path) -> bool {
        [state.trusted.as_ref(), state.previous.as_ref()]
            .into_iter()
            .flatten()
            .any(|entry| {
                entry
                    .media
                    .upgrade()
                    .is_some_and(|media| media.source == SourceRef::file(path))
            })
    }

    #[test]
    fn cached_neighbor_dimensions_do_not_consume_pixels_or_change_accounting() {
        let dir = TempDir::new();
        let first = dir.file("a.png");
        let neighbor = dir.file("b.png");
        let prefetch = with_fake(settings(0, 1, 1), fixed(16, 32, 2));
        prefetch.update_context(
            vec![first.clone(), neighbor.clone()],
            Some(first.clone()),
            None,
        );
        prefetch.set_allowed(true);
        wait_until(|| prefetch.stats().entries == 1);
        prefetch.set_allowed(false);
        let source = SourceRef::file(&neighbor);
        let before = prefetch.stats();
        let owners = {
            let state = lock(&prefetch.inner.shared.state);
            Arc::strong_count(&state.cache[&source].decoded.media)
        };
        fs::remove_file(&neighbor).unwrap();
        assert_eq!(prefetch.cached_dimensions(&source), Some((16, 32)));
        assert_eq!(prefetch.cached_dimensions(&SourceRef::file(&first)), None);
        assert_eq!(prefetch.stats(), before);
        let state = lock(&prefetch.inner.shared.state);
        assert_eq!(
            Arc::strong_count(&state.cache[&source].decoded.media),
            owners
        );
        drop(state);
        prefetch.shutdown();
    }

    #[test]
    fn cached_archive_dimensions_never_open_or_validate_the_archive() {
        use crate::model::source::ArchiveKind;
        let dir = TempDir::new();
        let path = dir.0.join("never-created.cbz");
        let source = SourceRef::Archive {
            archive: path.clone(),
            archive_len: 123,
            archive_mtime_secs: 0,
            archive_mtime_nanos: 0,
            kind: ArchiveKind::Zip,
            index: 9,
            name: "page.png".into(),
            size: 0,
            crc: 0,
        };
        let mut decoded = media(&path, 4, 8, 1);
        decoded.source = source.clone();
        let prefetch = Prefetcher::new(settings(0, 0, 0));
        {
            let mut state = lock(&prefetch.inner.shared.state);
            state.bytes = media_bytes(&decoded);
            let bytes = state.bytes;
            state.cache.insert(
                source.clone(),
                CacheEntry {
                    decoded: DecodedEntry {
                        media: Arc::new(decoded),
                        identity: SourceIdentity {
                            len: 123,
                            modified: SystemTime::UNIX_EPOCH,
                            entry: Some(source.clone()),
                        },
                        purpose: DecodePurpose::Animated,
                    },
                    bytes,
                },
            );
        }
        let before = prefetch.stats();
        assert!(!path.exists());
        assert_eq!(prefetch.cached_dimensions(&source), Some((4, 8)));
        assert_eq!(prefetch.stats(), before);
        assert!(!path.exists());
        prefetch.shutdown();
    }

    #[test]
    fn order_is_nearest_first_without_wrapping() {
        let siblings: Vec<PathBuf> = ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let selected = siblings[2].clone();
        assert_eq!(
            neighbor_order(&siblings, Some(&selected), &settings(2, 2, 1)),
            ["d", "b", "e", "a"].map(PathBuf::from).to_vec()
        );
        assert_eq!(
            neighbor_order(&siblings, Some(&selected), &settings(1, 2, 1)),
            ["d", "b", "e"].map(PathBuf::from).to_vec()
        );
        let first = siblings[0].clone();
        assert_eq!(
            neighbor_order(&siblings, Some(&first), &settings(9, 2, 1)),
            ["b", "c"].map(PathBuf::from).to_vec()
        );
        let last = siblings[4].clone();
        assert_eq!(
            neighbor_order(&siblings, Some(&last), &settings(2, 9, 1)),
            ["d", "c"].map(PathBuf::from).to_vec()
        );
        assert!(neighbor_order(&siblings, None, &settings(2, 2, 1)).is_empty());
        assert!(neighbor_order(&siblings, Some(&selected), &settings(0, 0, 1)).is_empty());
    }

    #[test]
    fn archive_entries_have_distinct_cache_keys_and_revalidate_same_path_replacements() {
        use crate::model::source::{catalog_for, test_support::write_zip};
        use std::io::Cursor;
        let dir = TempDir::new();
        let path = dir.0.join("pages.cbz");
        let png = |color| {
            let mut bytes = Cursor::new(Vec::new());
            RgbaImage::from_pixel(4, 3, Rgba(color))
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
            bytes.into_inner()
        };
        let red = png([255, 0, 0, 255]);
        let blue = png([0, 0, 255, 255]);
        write_zip(&path, &[("a.png", &red), ("b.png", &blue)]);
        let items = catalog_for(&path).unwrap().items;
        let prefetch = Prefetcher::new(settings(1, 1, 1));
        prefetch.update_context_many_sources(items.clone(), Some(items[0].clone()), Vec::new());
        prefetch.set_allowed(true);
        wait_until(|| prefetch.stats().entries == 1);
        prefetch.set_allowed(false);
        let cancel = AtomicBool::new(false);
        assert!(prefetch.peek_source(&items[0], &cancel).is_none());
        let cached = prefetch.peek_source(&items[1], &cancel).unwrap();
        prefetch.foreground_requested_source(&items[1]);
        let loaded = prefetch.load_source(&items[1], &cancel).unwrap();
        assert!(Arc::ptr_eq(&loaded, &cached));
        assert_eq!(loaded.source, items[1]);
        assert_eq!(prefetch.stats().hits, 1);
        // Readmit this delivered page as a neighbor, then change the archive
        // without changing the entry's central metadata.
        prefetch.update_context_many_sources(
            items.clone(),
            Some(items[1].clone()),
            vec![loaded.clone()],
        );
        let primary = prefetch.load_source(&items[0], &cancel).unwrap();
        prefetch.update_context_many_sources(items.clone(), Some(items[0].clone()), vec![primary]);
        assert_eq!(prefetch.stats().entries, 1);
        let old = source_identity(&items[1]).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old.modified + Duration::from_secs(5))
            .unwrap();
        assert!(prefetch.peek_source(&items[1], &cancel).is_none());
        assert!(prefetch.load_source(&items[1], &cancel).is_err());
        let refreshed = catalog_for(&path).unwrap().items;
        let fresh = prefetch.load_source(&refreshed[1], &cancel).unwrap();
        assert!(!Arc::ptr_eq(&loaded, &fresh));
        assert_eq!(
            prefetch.stats().hits,
            1,
            "stale archive cache was not a hit"
        );
        write_zip(&path, &[("a.png", &red), ("b.png", &red)]);
        assert!(prefetch.load_source(&items[1], &cancel).is_err());
        let new_items = catalog_for(&path).unwrap().items;
        let changed = prefetch.load_source(&new_items[1], &cancel).unwrap();
        assert_eq!(
            *changed.frames[0].pixels.get_pixel(0, 0),
            Rgba([255, 0, 0, 255])
        );
        prefetch.shutdown();
    }

    #[test]
    fn count_and_byte_budget_keep_nearest_entries() {
        let dir = TempDir::new();
        let siblings = vec![
            dir.file("a.png"),
            dir.file("b.png"),
            dir.file("c.png"),
            dir.file("d.png"),
            dir.file("e.png"),
        ];
        let prefetcher = with_fake(settings(2, 2, 1), fixed(300, 300, 1));
        prefetcher.update_context(siblings.clone(), Some(siblings[2].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 2);
        thread::sleep(Duration::from_millis(30));
        let stats = prefetcher.stats();
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.bytes, 2 * 300 * 300 * 4);
        assert!(!stats.loading);
        assert_eq!(
            cached_paths(&prefetcher),
            [siblings[1].clone(), siblings[3].clone()]
        );
    }

    #[test]
    fn budget_boundary_keeps_exactly_fitting_entries_and_skips_one_byte_margin() {
        let dir = TempDir::new();
        let siblings = vec![
            dir.file("a.png"),
            dir.file("b.png"),
            dir.file("c.png"),
            dir.file("d.png"),
            dir.file("e.png"),
        ];

        // 256x256 RGBA = 262,144 bytes; four exactly fill the 1 MiB budget.
        let exact = with_fake(settings(2, 2, 1), fixed(256, 256, 1));
        exact.update_context(siblings.clone(), Some(siblings[2].clone()), None);
        exact.set_allowed(true);
        wait_until(|| exact.stats().entries == 4);
        assert_eq!(exact.stats().bytes, 1024 * 1024);

        // 257x257 leaves no room for the fourth neighbor.
        let over = with_fake(settings(2, 2, 1), fixed(257, 257, 1));
        over.update_context(siblings.clone(), Some(siblings[2].clone()), None);
        over.set_allowed(true);
        wait_until(|| over.stats().entries == 3);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(over.stats().entries, 3);
        assert_eq!(over.stats().bytes, 3 * 257 * 257 * 4);
        assert!(over.stats().bytes <= 1024 * 1024);
    }

    #[test]
    fn gif_charges_all_frames_and_duplicate_arc_is_not_charged_twice() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let shared = Arc::new(media(&a, 100, 100, 3));
        let shared_for_decoder = Arc::clone(&shared);
        let prefetcher = with_fake(settings(0, 1, 1), move |_path, _| {
            Ok(Arc::clone(&shared_for_decoder))
        });
        let siblings = vec![a, dir.file("b.png")];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        let stats = prefetcher.stats();
        assert_eq!(stats.bytes, 100 * 100 * 4 * 3);

        let c = dir.file("c.png");
        let siblings = vec![siblings[0].clone(), siblings[1].clone(), c];
        let shared = Arc::new(media(&siblings[0], 100, 100, 3));
        let shared_for_decoder = Arc::clone(&shared);
        let prefetcher = with_fake(settings(0, 2, 1), move |_path, _| {
            Ok(Arc::clone(&shared_for_decoder))
        });
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(prefetcher.stats().entries, 1);
        assert_eq!(prefetcher.stats().bytes, 100 * 100 * 4 * 3);
    }

    #[test]
    fn hit_promotes_arc_out_of_cache() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        prefetcher.set_allowed(false);
        let cached = lock(&prefetcher.inner.shared.state)
            .cache
            .get(&SourceRef::file(&b))
            .map(|entry| Arc::clone(&entry.decoded.media))
            .expect("cached neighbor");

        let cancel = AtomicBool::new(false);
        let loaded = prefetcher.load(&b, &cancel).expect("cache hit");
        assert!(Arc::ptr_eq(&loaded, &cached));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let stats = prefetcher.stats();
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.bytes, 0);
        assert_eq!(stats.hits, 1);
        prefetcher.update_context(siblings, Some(b), Some(loaded));
        assert_eq!(prefetcher.stats().entries, 0);
    }

    #[test]
    fn mtime_change_invalidates_cache() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        prefetcher.set_allowed(false);
        let cached = lock(&prefetcher.inner.shared.state)
            .cache
            .get(&SourceRef::file(&b))
            .map(|entry| Arc::clone(&entry.decoded.media))
            .expect("cached neighbor");

        let modified = SystemTime::now() + Duration::from_secs(2);
        fs::OpenOptions::new()
            .write(true)
            .open(&b)
            .unwrap()
            .set_modified(modified)
            .unwrap();

        let loaded = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("reload after mtime change");
        assert!(!Arc::ptr_eq(&loaded, &cached));
        assert_eq!(count.load(Ordering::Relaxed), 2);
        assert_eq!(prefetcher.stats().entries, 0);
        assert_eq!(prefetcher.stats().hits, 0);
    }

    #[test]
    fn deleted_neighbor_is_uncacheable() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let prefetcher = with_fake(settings(0, 1, 1), fixed(10, 10, 1));
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        prefetcher.set_allowed(false);
        fs::remove_file(&b).unwrap();
        let loaded = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("ordinary load after deletion");
        assert_eq!(loaded.source, SourceRef::file(&b));
        assert_eq!(prefetcher.stats().entries, 0);
        assert_eq!(prefetcher.stats().hits, 0);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_path_is_the_cache_identity() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let target = dir.file("real-image");
        let link = dir.0.join("link.png");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let prefetcher = with_fake(settings(0, 1, 1), fixed(10, 10, 1));
        let siblings = vec![a, link.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        assert_eq!(cached_paths(&prefetcher), [absolute(&link)]);
    }

    #[test]
    fn same_path_foreground_joins_one_decode_and_survives_set_allowed_false() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            if let Some(tx) = entered_tx.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            if let Some(rx) = release_rx.lock().unwrap().take() {
                rx.recv().unwrap();
            }
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let loader = prefetcher.clone();
        let path = b.clone();
        let joined = thread::spawn(move || loader.load(&path, &AtomicBool::new(false)));
        wait_until(|| {
            lock(&prefetcher.inner.shared.state)
                .inflight
                .as_ref()
                .is_some_and(|inflight| inflight.waiters > 0)
        });
        prefetcher.set_allowed(false);
        release_tx.send(()).unwrap();
        let loaded = joined.join().unwrap().expect("promoted load");
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(loaded.source, SourceRef::file(&b));
        assert_eq!(prefetcher.stats().entries, 0);
        prefetcher.update_context(siblings, Some(b), Some(loaded));
        thread::sleep(Duration::from_millis(30));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(prefetcher.stats().entries, 0);
    }

    #[test]
    fn set_allowed_false_cancels_speculative_decode() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            if let Some(tx) = entered_tx.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            if let Some(rx) = release_rx.lock().unwrap().take() {
                rx.recv().unwrap();
            }
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        prefetcher.set_allowed(false);
        release_tx.send(()).unwrap();
        wait_until(|| !prefetcher.stats().loading);
        assert_eq!(prefetcher.stats().entries, 0);
    }

    #[test]
    fn foreground_request_for_another_path_cancels_speculation() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let c = dir.file("c.png");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            if let Some(tx) = entered_tx.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            if let Some(rx) = release_rx.lock().unwrap().take() {
                rx.recv().unwrap();
            }
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b, c];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        prefetcher.foreground_requested(&siblings[2]);
        release_tx.send(()).unwrap();
        wait_until(|| !prefetcher.stats().loading);
        assert_eq!(prefetcher.stats().entries, 0);
    }

    #[test]
    fn same_path_foreground_request_preserves_decode() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            if let Some(tx) = entered_tx.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            if let Some(rx) = release_rx.lock().unwrap().take() {
                rx.recv().unwrap();
            }
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        prefetcher.foreground_requested(&b);
        prefetcher.set_allowed(false);
        release_tx.send(()).unwrap();
        wait_until(|| !prefetcher.stats().loading);
        assert!(lock(&prefetcher.inner.shared.state).foreground.is_some());
        let loaded = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("promoted completion reservation");
        assert_eq!(loaded.source, SourceRef::file(&b));
        assert_eq!(prefetcher.stats().entries, 0);
        assert_eq!(prefetcher.stats().hits, 1);
    }

    #[test]
    fn stale_context_discards_result() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            if let Some(tx) = entered_tx.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            if let Some(rx) = release_rx.lock().unwrap().take() {
                rx.recv().unwrap();
            }
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        prefetcher.update_context(Vec::new(), None, None);
        release_tx.send(()).unwrap();
        wait_until(|| !prefetcher.stats().loading);
        assert_eq!(prefetcher.stats().entries, 0);
    }

    #[test]
    fn settings_shrink_prunes_old_neighbors() {
        let dir = TempDir::new();
        let siblings = vec![
            dir.file("a.png"),
            dir.file("b.png"),
            dir.file("c.png"),
            dir.file("d.png"),
            dir.file("e.png"),
        ];
        let prefetcher = with_fake(settings(2, 2, 1), fixed(300, 300, 1));
        prefetcher.update_context(siblings.clone(), Some(siblings[2].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 2);
        prefetcher.configure(settings(0, 1, 1));
        wait_until(|| prefetcher.stats().entries == 1);
        assert_eq!(prefetcher.stats().bytes, 300 * 300 * 4);
        assert_eq!(cached_paths(&prefetcher), [siblings[3].clone()]);
        prefetcher.configure(settings(0, 0, 0));
        assert_eq!(prefetcher.stats().entries, 0);
        assert_eq!(prefetcher.stats().bytes, 0);
    }

    #[test]
    fn oversized_neighbor_does_not_block_ordinary_load() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 600, 600, 1)))
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| count.load(Ordering::Relaxed) == 1 && !prefetcher.stats().loading);
        assert_eq!(prefetcher.stats().entries, 0);
        let loaded = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("ordinary load");
        assert_eq!(loaded.frames.len(), 1);
        assert_eq!(count.load(Ordering::Relaxed), 2);
        assert_eq!(prefetcher.stats().entries, 0);
    }

    #[test]
    fn nothing_is_decoded_while_not_allowed() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(1, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(count.load(Ordering::Relaxed), 0);
        assert_eq!(prefetcher.stats().entries, 0);
    }

    #[test]
    fn ui_sequence_promotes_cached_arc_before_context_update() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        prefetcher.set_allowed(false);
        let cached = lock(&prefetcher.inner.shared.state)
            .cache
            .get(&SourceRef::file(&b))
            .map(|entry| Arc::clone(&entry.decoded.media))
            .expect("cached neighbor");

        // Exact main.rs order: foreground_requested, set_allowed(false),
        // set selected, update_context, then background load.
        prefetcher.foreground_requested(&b);
        prefetcher.set_allowed(false);
        prefetcher.update_context(siblings, Some(b.clone()), None);
        let stats = prefetcher.stats();
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.bytes, 0);
        assert!(lock(&prefetcher.inner.shared.state).foreground.is_some());

        let loaded = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("reserved foreground load");
        assert!(
            Arc::ptr_eq(&loaded, &cached),
            "foreground consumed the cached Arc"
        );
        assert_eq!(count.load(Ordering::Relaxed), 1, "no second decode");
        assert_eq!(prefetcher.stats().hits, 1);
        assert_eq!(prefetcher.stats().entries, 0);
        assert_eq!(prefetcher.stats().bytes, 0);
    }

    #[test]
    fn latest_foreground_reservation_replaces_the_previous_one() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let d = dir.file("d.png");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(0, 2, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b.clone(), d.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 2);
        prefetcher.set_allowed(false);
        let (cached_b, cached_d) = {
            let state = lock(&prefetcher.inner.shared.state);
            (
                Arc::clone(&state.cache[&SourceRef::file(&b)].decoded.media),
                Arc::clone(&state.cache[&SourceRef::file(&d)].decoded.media),
            )
        };

        prefetcher.foreground_requested(&b);
        prefetcher.foreground_requested(&d);
        assert_eq!(
            prefetcher.stats().bytes,
            0,
            "reservations are not cache budget"
        );

        let stale_b = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("abandoned request decodes normally");
        assert!(!Arc::ptr_eq(&stale_b, &cached_b));
        assert_eq!(count.load(Ordering::Relaxed), 3);

        let loaded_d = prefetcher
            .load(&d, &AtomicBool::new(false))
            .expect("latest reservation");
        assert!(Arc::ptr_eq(&loaded_d, &cached_d));
        assert_eq!(count.load(Ordering::Relaxed), 3);
        assert_eq!(prefetcher.stats().hits, 1);
    }

    #[test]
    fn context_mismatch_and_staleness_drop_reservation() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(1, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a.clone(), b.clone()];
        prefetcher.update_context(siblings.clone(), Some(a.clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        prefetcher.set_allowed(false);
        let cached = lock(&prefetcher.inner.shared.state).cache[&SourceRef::file(&b)]
            .decoded
            .media
            .clone();

        prefetcher.foreground_requested(&b);
        prefetcher.update_context(siblings.clone(), Some(a.clone()), None);
        assert!(lock(&prefetcher.inner.shared.state).foreground.is_none());
        let changed = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("decode after dropping reservation");
        assert!(!Arc::ptr_eq(&changed, &cached));
        assert_eq!(count.load(Ordering::Relaxed), 2);

        prefetcher.update_context(vec![b.clone(), a.clone()], Some(a), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        prefetcher.set_allowed(false);
        let cached = lock(&prefetcher.inner.shared.state).cache[&SourceRef::file(&b)]
            .decoded
            .media
            .clone();
        prefetcher.foreground_requested(&b);
        let modified = SystemTime::now() + Duration::from_secs(2);
        fs::OpenOptions::new()
            .write(true)
            .open(&b)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let loaded = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("stale reservation decodes");
        assert!(!Arc::ptr_eq(&loaded, &cached));
        assert_eq!(count.load(Ordering::Relaxed), 4);
        assert_eq!(prefetcher.stats().hits, 0);
        assert!(lock(&prefetcher.inner.shared.state).foreground.is_none());
    }

    #[test]
    fn promoted_completion_before_waiter_is_reserved_and_delivered_once() {
        use std::sync::Mutex as StdMutex;
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let captured = Arc::new(StdMutex::new(Vec::<Arc<LoadedMedia>>::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let counter = Arc::clone(&count);
        let captured_for_decoder = Arc::clone(&captured);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            let decoded = Arc::new(media(path, 10, 10, 1));
            captured_for_decoder
                .lock()
                .unwrap()
                .push(Arc::clone(&decoded));
            if counter.fetch_add(1, Ordering::Relaxed) == 0 {
                if let Some(tx) = entered_tx.lock().unwrap().take() {
                    tx.send(()).unwrap();
                }
                if let Some(rx) = release_rx.lock().unwrap().take() {
                    rx.recv().unwrap();
                }
            }
            Ok(decoded)
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        prefetcher.foreground_requested(&b);
        prefetcher.set_allowed(false);
        prefetcher.update_context(siblings, Some(b.clone()), None);
        release_tx.send(()).unwrap();
        wait_until(|| !prefetcher.stats().loading);
        assert!(
            lock(&prefetcher.inner.shared.state).foreground.is_some(),
            "promoted completion must become a reservation"
        );

        let first = captured.lock().unwrap()[0].clone();
        let loaded = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("consume promoted reservation");
        assert!(Arc::ptr_eq(&loaded, &first));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(prefetcher.stats().hits, 1);
        assert_eq!(prefetcher.stats().entries, 0);
        assert_eq!(prefetcher.stats().bytes, 0);
    }

    #[test]
    fn cancelled_promoted_job_retries_fresh_once() {
        use std::sync::Mutex as StdMutex;
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let captured = Arc::new(StdMutex::new(Vec::<Arc<LoadedMedia>>::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let counter = Arc::clone(&count);
        let captured_for_decoder = Arc::clone(&captured);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, cancel| {
            let decoded = Arc::new(media(path, 10, 10, 1));
            captured_for_decoder
                .lock()
                .unwrap()
                .push(Arc::clone(&decoded));
            if counter.fetch_add(1, Ordering::Relaxed) == 0 {
                if let Some(tx) = entered_tx.lock().unwrap().take() {
                    tx.send(()).unwrap();
                }
                if let Some(rx) = release_rx.lock().unwrap().take() {
                    rx.recv().unwrap();
                }
                // Simulate the model checking cancellation on return.
                if cancel.load(Ordering::Relaxed) {
                    return Err(format!("{}: cancelled", path.display()));
                }
            }
            Ok(decoded)
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let other = dir.file("c.png");
        prefetcher.foreground_requested(&other);
        prefetcher.foreground_requested(&b);
        prefetcher.set_allowed(false);
        let loader = prefetcher.clone();
        let path = b.clone();
        let joined = thread::spawn(move || loader.load(&path, &AtomicBool::new(false)));
        wait_until(|| {
            lock(&prefetcher.inner.shared.state)
                .inflight
                .as_ref()
                .is_some_and(|inflight| inflight.waiters > 0)
        });
        release_tx.send(()).unwrap();
        let loaded = joined
            .join()
            .unwrap()
            .expect("retry after cancelled joined completion");
        assert_eq!(count.load(Ordering::Relaxed), 2);
        let decoded = captured.lock().unwrap();
        assert!(Arc::ptr_eq(&loaded, &decoded[1]));
        assert!(!Arc::ptr_eq(&loaded, &decoded[0]));
    }

    #[test]
    fn cancelled_caller_returns_promptly_while_joining() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            if let Some(tx) = entered_tx.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            if let Some(rx) = release_rx.lock().unwrap().take() {
                rx.recv().unwrap();
            }
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        prefetcher.foreground_requested(&b);

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_loader = Arc::clone(&cancel);
        let loader = prefetcher.clone();
        let path = b.clone();
        let joined = thread::spawn(move || loader.load(&path, &cancel_for_loader));
        wait_until(|| {
            lock(&prefetcher.inner.shared.state)
                .inflight
                .as_ref()
                .is_some_and(|inflight| inflight.waiters > 0)
        });
        let start = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        let result = joined.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(result.unwrap_err().contains("cancelled"));
        release_tx.send(()).unwrap();
        prefetcher.shutdown();
        prefetcher.join_worker();
    }

    #[test]
    fn joined_result_is_revalidated_after_decoder_finishes() {
        use std::sync::Mutex as StdMutex;
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let captured = Arc::new(StdMutex::new(Vec::<Arc<LoadedMedia>>::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let counter = Arc::clone(&count);
        let captured_for_decoder = Arc::clone(&captured);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            let decoded = Arc::new(media(path, 10, 10, 1));
            captured_for_decoder
                .lock()
                .unwrap()
                .push(Arc::clone(&decoded));
            if counter.fetch_add(1, Ordering::Relaxed) == 0 {
                if let Some(tx) = entered_tx.lock().unwrap().take() {
                    tx.send(()).unwrap();
                }
                if let Some(rx) = release_rx.lock().unwrap().take() {
                    rx.recv().unwrap();
                }
            }
            Ok(decoded)
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let change_path = b.clone();
        prefetcher.set_handoff_hook(move || {
            let modified = SystemTime::now() + Duration::from_secs(3);
            fs::OpenOptions::new()
                .write(true)
                .open(&change_path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        });
        let loader = prefetcher.clone();
        let path = b.clone();
        let joined = thread::spawn(move || loader.load(&path, &AtomicBool::new(false)));
        wait_until(|| {
            lock(&prefetcher.inner.shared.state)
                .inflight
                .as_ref()
                .is_some_and(|inflight| inflight.waiters > 0)
        });
        release_tx.send(()).unwrap();
        let loaded = joined.join().unwrap().expect("fresh reload after join");
        assert_eq!(count.load(Ordering::Relaxed), 2);
        let decoded = captured.lock().unwrap();
        assert!(Arc::ptr_eq(&loaded, &decoded[1]));
        assert!(!Arc::ptr_eq(&loaded, &decoded[0]));
    }

    #[test]
    fn detach_dropped_completed_result_does_not_cache_old_epoch() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let identity = source_identity(&SourceRef::file(&b)).unwrap();
        let build = || {
            let mut state = State::new(settings(1, 1, 1));
            state.siblings = vec![SourceRef::file(&a), SourceRef::file(&b)];
            state.selected = Some(SourceRef::file(&a));
            state.inflight = Some(Inflight {
                job: Job {
                    id: 9,
                    epoch: 5,
                    path: SourceRef::file(&b),
                    purpose: DecodePurpose::Animated,
                    cancel: Arc::new(AtomicBool::new(false)),
                },
                waiters: 1,
                promoted: false,
                done: Some(Ok(DecodedEntry {
                    media: Arc::new(media(&b, 10, 10, 1)),
                    identity: identity.clone(),
                    purpose: DecodePurpose::Animated,
                })),
            });
            state
        };

        let mut same_epoch = build();
        same_epoch.epoch = 5;
        detach(&mut same_epoch, 9, false);
        assert!(
            same_epoch.cache.contains_key(&SourceRef::file(&b)),
            "eligible at done epoch"
        );

        let mut next_epoch = build();
        next_epoch.epoch = 6;
        detach(&mut next_epoch, 9, false);
        assert!(next_epoch.cache.is_empty(), "stale epoch must not readmit");
        assert!(next_epoch.inflight.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_cache_and_reservation_are_not_used() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        prefetcher.set_allowed(false);

        let permissions = fs::metadata(&b).unwrap().permissions();
        fs::set_permissions(&b, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&b).is_ok() {
            fs::set_permissions(&b, permissions).unwrap();
            return; // running as root; permission loss cannot be exercised
        }
        let loaded = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("ordinary decode after cache loses read permission");
        assert_eq!(loaded.source, SourceRef::file(&b));
        assert_eq!(count.load(Ordering::Relaxed), 2);
        assert_eq!(prefetcher.stats().entries, 0);
        assert_eq!(prefetcher.stats().hits, 0);
        fs::set_permissions(&b, permissions).unwrap();

        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        prefetcher.set_allowed(false);
        prefetcher.foreground_requested(&b);
        assert!(lock(&prefetcher.inner.shared.state).foreground.is_some());

        let permissions = fs::metadata(&b).unwrap().permissions();
        fs::set_permissions(&b, fs::Permissions::from_mode(0o000)).unwrap();
        let loaded = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("ordinary decode after reservation loses read permission");
        assert_eq!(loaded.source, SourceRef::file(&b));
        assert_eq!(count.load(Ordering::Relaxed), 2);
        assert_eq!(prefetcher.stats().hits, 0);
        assert!(lock(&prefetcher.inner.shared.state).foreground.is_none());
        fs::set_permissions(&b, permissions).unwrap();
    }

    #[test]
    fn previous_current_is_readmitted_when_returning() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(1, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a.clone(), b.clone()];
        prefetcher.set_allowed(false);
        prefetcher.update_context(siblings.clone(), Some(a.clone()), None);
        let media_a = prefetcher
            .load(&a, &AtomicBool::new(false))
            .expect("load a");
        prefetcher.update_context(siblings.clone(), Some(a.clone()), Some(media_a.clone()));
        prefetcher.update_context(siblings.clone(), Some(b.clone()), Some(media_a.clone()));
        let media_b = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("load b");
        assert_eq!(count.load(Ordering::Relaxed), 2);

        prefetcher.update_context(siblings.clone(), Some(b.clone()), Some(media_b.clone()));
        assert_eq!(cached_paths(&prefetcher), [absolute(&a)]);
        assert_eq!(prefetcher.stats().bytes, 10 * 10 * 4);

        let expected = Arc::clone(&media_a);
        drop(media_a);
        prefetcher.foreground_requested(&a);
        prefetcher.set_allowed(false);
        prefetcher.update_context(siblings.clone(), Some(a.clone()), Some(media_b.clone()));
        let returned = prefetcher
            .load(&a, &AtomicBool::new(false))
            .expect("a should be readmitted");
        assert!(Arc::ptr_eq(&returned, &expected));
        assert_eq!(count.load(Ordering::Relaxed), 2);
        assert_eq!(prefetcher.stats().hits, 1);
        prefetcher.update_context(siblings, Some(a), Some(returned));
    }

    #[test]
    fn previous_current_readmission_requires_fresh_identity_range_and_budget() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");

        // Changed identity: old pixels must not be stamped with new metadata.
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let changed = with_fake(settings(1, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a.clone(), b.clone()];
        changed.set_allowed(false);
        changed.update_context(siblings.clone(), Some(a.clone()), None);
        let media_a = changed.load(&a, &AtomicBool::new(false)).unwrap();
        changed.update_context(siblings.clone(), Some(a.clone()), Some(media_a.clone()));
        let modified = SystemTime::now() + Duration::from_secs(4);
        fs::OpenOptions::new()
            .write(true)
            .open(&a)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let media_b = Arc::new(media(&b, 10, 10, 1));
        changed.update_context(siblings.clone(), Some(b.clone()), Some(media_b.clone()));
        changed.update_context(siblings, Some(b.clone()), Some(media_b));
        // UI-side admission uses the original identity and performs no I/O, so
        // the stale entry may be cached until access.
        assert_eq!(changed.stats().entries, 1);
        let stale = changed.stats();
        let returned = changed
            .load(&a, &AtomicBool::new(false))
            .expect("changed A reloads");
        assert!(!Arc::ptr_eq(&returned, &media_a));
        assert_eq!(count.load(Ordering::Relaxed), 2);
        assert_eq!(changed.stats().hits, stale.hits);
        assert_eq!(changed.stats().entries, 0);

        // Unreadable at access: the cached entry is rejected and reloaded.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let count = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&count);
            let unreadable = with_fake(settings(1, 1, 1), move |path, _| {
                counter.fetch_add(1, Ordering::Relaxed);
                Ok(Arc::new(media(path, 10, 10, 1)))
            });
            let siblings = vec![a.clone(), b.clone()];
            unreadable.set_allowed(false);
            unreadable.update_context(siblings.clone(), Some(a.clone()), None);
            let media_a = unreadable.load(&a, &AtomicBool::new(false)).unwrap();
            unreadable.update_context(siblings.clone(), Some(a.clone()), Some(media_a.clone()));
            let media_b = Arc::new(media(&b, 10, 10, 1));
            unreadable.update_context(siblings.clone(), Some(b.clone()), Some(media_b.clone()));
            unreadable.update_context(siblings, Some(b.clone()), Some(media_b));
            assert_eq!(unreadable.stats().entries, 1);
            let permissions = fs::metadata(&a).unwrap().permissions();
            fs::set_permissions(&a, fs::Permissions::from_mode(0o000)).unwrap();
            if fs::File::open(&a).is_ok() {
                fs::set_permissions(&a, permissions).unwrap();
            } else {
                let returned = unreadable.load(&a, &AtomicBool::new(false)).unwrap();
                assert!(!Arc::ptr_eq(&returned, &media_a));
                assert_eq!(count.load(Ordering::Relaxed), 2);
                assert_eq!(unreadable.stats().entries, 0);
                fs::set_permissions(&a, permissions).unwrap();
            }
        }

        // Out of range: no neighbor slots.
        let out_of_range = with_fake(settings(0, 0, 1), fixed(10, 10, 1));
        let siblings = vec![a.clone(), b.clone()];
        out_of_range.set_allowed(false);
        out_of_range.update_context(siblings.clone(), Some(a.clone()), None);
        let media_a = out_of_range.load(&a, &AtomicBool::new(false)).unwrap();
        out_of_range.update_context(siblings.clone(), Some(a.clone()), Some(media_a.clone()));
        out_of_range.update_context(siblings, Some(b.clone()), Some(media_a.clone()));
        out_of_range.update_context(
            vec![b.clone()],
            Some(b.clone()),
            Some(Arc::new(media(&b, 10, 10, 1))),
        );
        assert_eq!(out_of_range.stats().entries, 0);

        // Over budget: one A image exceeds the 1 MiB budget.
        let over_budget = with_fake(settings(0, 1, 1), fixed(600, 600, 1));
        let siblings = vec![a.clone(), b.clone()];
        over_budget.set_allowed(false);
        over_budget.update_context(siblings.clone(), Some(a.clone()), None);
        let media_a = over_budget.load(&a, &AtomicBool::new(false)).unwrap();
        over_budget.update_context(siblings.clone(), Some(a), Some(media_a.clone()));
        over_budget.update_context(siblings.clone(), Some(b.clone()), Some(media_a));
        over_budget.update_context(
            siblings,
            Some(b.clone()),
            Some(Arc::new(media(&b, 10, 10, 1))),
        );
        assert_eq!(over_budget.stats().entries, 0);
    }

    #[test]
    fn shutdown_wakes_a_joining_foreground() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            if let Some(tx) = entered_tx.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            if let Some(rx) = release_rx.lock().unwrap().take() {
                rx.recv().unwrap();
            }
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b.clone()];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        prefetcher.foreground_requested(&b);

        let loader = prefetcher.clone();
        let path = b.clone();
        let joined = thread::spawn(move || loader.load(&path, &AtomicBool::new(false)));
        wait_until(|| {
            lock(&prefetcher.inner.shared.state)
                .inflight
                .as_ref()
                .is_some_and(|inflight| inflight.waiters > 0)
        });
        let start = Instant::now();
        prefetcher.shutdown();
        let result = joined.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(result.is_err());
        release_tx.send(()).unwrap();
        prefetcher.join_worker();
    }

    #[test]
    #[ignore = "requires OXEYE_TEST_IMAGE_ROOT with real images"]
    fn real_input_preload() {
        fn discover_folder(root: &Path) -> Option<Vec<PathBuf>> {
            let mut queue = vec![root.to_path_buf()];
            let mut visited = 0usize;
            while let Some(directory) = queue.pop() {
                visited += 1;
                if visited > 256 {
                    break;
                }
                let Ok(entries) = fs::read_dir(&directory) else {
                    continue;
                };
                let mut images = Vec::new();
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        if queue.len() < 256 {
                            queue.push(path);
                        }
                    } else if path.is_file() && crate::model::supported_path(&path) {
                        images.push(path);
                    }
                }
                if images.len() < 3 {
                    continue;
                }
                images.sort();
                if let Ok(siblings) = crate::model::sibling_images(&images[1])
                    && siblings.len() >= 3
                {
                    return Some(siblings);
                }
            }
            None
        }

        let Some(root) = std::env::var_os("OXEYE_TEST_IMAGE_ROOT").map(PathBuf::from) else {
            eprintln!("OXEYE_TEST_IMAGE_ROOT is not set; skipping real_input_preload");
            return;
        };
        let siblings = discover_folder(&root)
            .unwrap_or_else(|| panic!("no folder with at least 3 images under {}", root.display()));
        let selected = siblings[siblings.len() / 2].clone();
        let budget_mib = 128;
        let prefetcher = Prefetcher::new(PrefetchSettings {
            before: 1,
            after: 1,
            memory_mib: budget_mib,
        });
        prefetcher.update_context(siblings.clone(), Some(selected.clone()), None);
        prefetcher.set_allowed(true);

        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline {
            let stats = prefetcher.stats();
            if !stats.loading && stats.entries > 0 {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let stats = prefetcher.stats();
        assert!(stats.entries > 0, "no neighbor cached: {stats:?}");
        assert!(stats.entries <= 2, "range exceeded: {stats:?}");
        assert!(
            stats.bytes <= budget_mib * 1024 * 1024,
            "prefetch bytes exceed budget: {stats:?}"
        );

        let (cached_path, cached_media) = {
            let state = lock(&prefetcher.inner.shared.state);
            let (path, entry) = state.cache.iter().next().expect("cached neighbor");
            (path.path().to_path_buf(), Arc::clone(&entry.decoded.media))
        };
        let hits_before = prefetcher.stats().hits;
        prefetcher.foreground_requested(&cached_path);
        prefetcher.set_allowed(false);
        prefetcher.update_context(siblings.clone(), Some(cached_path.clone()), None);
        let loaded = prefetcher
            .load(&cached_path, &AtomicBool::new(false))
            .expect("cached neighbor through the UI sequence");
        assert!(Arc::ptr_eq(&loaded, &cached_media));
        assert!(prefetcher.stats().hits > hits_before);
        prefetcher.shutdown();

        let opener = Prefetcher::new(PrefetchSettings {
            before: 1,
            after: 1,
            memory_mib: 0,
        });
        opener.update_context(siblings, Some(selected.clone()), None);
        opener.set_allowed(true);
        let opened = opener
            .load(&selected, &AtomicBool::new(false))
            .expect("zero prefetch budget still opens normally");
        assert!(!opened.frames.is_empty());
        assert_eq!(opener.stats().entries, 0);
        opener.shutdown();
    }

    #[test]
    fn weak_trusted_does_not_retain_abandoned_delivery_after_disable() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let prefetcher = with_fake(settings(1, 1, 1), fixed(10, 10, 1));
        let siblings = vec![a.clone(), b.clone()];
        prefetcher.set_allowed(false);
        prefetcher.update_context(siblings.clone(), Some(a.clone()), None);
        let media_a = prefetcher.load(&a, &AtomicBool::new(false)).unwrap();
        prefetcher.update_context(siblings.clone(), Some(a.clone()), Some(media_a.clone()));
        prefetcher.update_context(siblings.clone(), Some(b.clone()), Some(media_a.clone()));
        let media_b = prefetcher.load(&b, &AtomicBool::new(false)).unwrap();
        prefetcher.update_context(siblings.clone(), Some(b.clone()), Some(media_b.clone()));
        prefetcher.update_context(siblings, Some(a.clone()), Some(media_a.clone()));

        drop(media_b);
        prefetcher.configure(settings(0, 0, 0));
        let state = lock(&prefetcher.inner.shared.state);
        assert!(state.cache.is_empty());
        assert_eq!(state.bytes, 0);
        assert!(
            !weak_retains(&state, &b),
            "abandoned delivered B must not stay strongly retained"
        );
    }

    #[test]
    fn rejected_previous_current_leaves_no_strong_owner() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let prefetcher = with_fake(settings(0, 0, 1), fixed(10, 10, 1));
        let siblings = vec![a.clone(), b.clone()];
        prefetcher.set_allowed(false);
        prefetcher.update_context(siblings.clone(), Some(a.clone()), None);
        let media_a = prefetcher.load(&a, &AtomicBool::new(false)).unwrap();
        prefetcher.update_context(siblings.clone(), Some(a.clone()), Some(media_a.clone()));
        prefetcher.update_context(siblings.clone(), Some(b.clone()), Some(media_a.clone()));
        let media_b = Arc::new(media(&b, 10, 10, 1));
        prefetcher.update_context(siblings, Some(b), Some(media_b));
        assert_eq!(prefetcher.stats().entries, 0, "out of range cannot admit");

        drop(media_a);
        let state = lock(&prefetcher.inner.shared.state);
        assert!(
            !weak_retains(&state, &a),
            "rejected former current must not be strongly retained"
        );
    }

    #[cfg(unix)]
    #[test]
    fn update_context_performs_no_file_io() {
        let dir = TempDir::new();
        let fifo = dir.0.join("fifo.png");
        let Ok(status) = std::process::Command::new("mkfifo").arg(&fifo).status() else {
            return;
        };
        if !status.success() {
            return;
        }
        let other = dir.file("other.png");
        let prefetcher = with_fake(settings(1, 1, 1), fixed(10, 10, 1));
        prefetcher.set_allowed(false);
        let fifo_media = Arc::new(media(&fifo, 10, 10, 1));
        let identity = SourceIdentity {
            len: 1,
            modified: SystemTime::UNIX_EPOCH,
            entry: None,
        };
        {
            let mut state = lock(&prefetcher.inner.shared.state);
            state.siblings = vec![SourceRef::file(&fifo), SourceRef::file(&other)];
            state.selected = Some(SourceRef::file(&other));
            state.displayed = vec![Arc::clone(&fifo_media)];
            state.trusted = Some(RememberedEntry::from_entry(&DecodedEntry {
                media: Arc::clone(&fifo_media),
                identity,
                purpose: DecodePurpose::Animated,
            }));
            state.epoch = 1;
        }

        let (done_tx, done_rx) = mpsc::channel();
        let loader = prefetcher.clone();
        let siblings = vec![fifo.clone(), other.clone()];
        let other_media = Arc::new(media(&other, 10, 10, 1));
        let caller = thread::spawn(move || {
            loader.update_context(siblings, Some(other), Some(other_media));
            done_tx.send(()).ok();
        });
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("update_context must not perform file I/O");
        caller.join().unwrap();
        // A readable check would reject this non-regular path; admission means
        // update_context used only the original identity and bounds.
        assert!(cached_paths(&prefetcher).contains(&absolute(&fifo)));
    }

    #[test]
    fn purpose_switch_clears_cache_and_does_not_cross_policies() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let prefetcher = with_purpose_fake(settings(0, 1, 1), |path, _, purpose| {
            let frames = match purpose {
                DecodePurpose::Animated => 3,
                DecodePurpose::Still => 1,
            };
            Ok(Arc::new(media(path, 10, 10, frames)))
        });
        let siblings = vec![a.clone(), b.clone()];
        prefetcher.update_context_many(siblings.clone(), Some(a.clone()), Vec::new());
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        assert_eq!(
            lock(&prefetcher.inner.shared.state).cache[&SourceRef::file(&b)]
                .decoded
                .media
                .frames
                .len(),
            3
        );

        let displayed = Arc::new(media(&a, 10, 10, 3));
        prefetcher.update_context_many(
            siblings.clone(),
            Some(a.clone()),
            vec![Arc::clone(&displayed)],
        );
        prefetcher.set_purpose(DecodePurpose::Still);
        assert_eq!(prefetcher.stats().entries, 0);
        assert_eq!(prefetcher.stats().bytes, 0);
        let state = lock(&prefetcher.inner.shared.state);
        assert_eq!(
            state.displayed.len(),
            1,
            "old rendered page stays visible during the switch"
        );
        assert!(Arc::ptr_eq(&state.displayed[0], &displayed));
        drop(state);
        prefetcher.set_allowed(false);
        let still = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("still load");
        assert_eq!(still.frames.len(), 1);
        assert_eq!(still.frames[0].delay, Duration::ZERO);

        prefetcher.set_purpose(DecodePurpose::Animated);
        let animated = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("animated load");
        assert_eq!(animated.frames.len(), 3);
    }

    #[test]
    fn purpose_change_cancels_inflight_and_promoted_results() {
        use std::sync::Mutex as StdMutex;
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let decoded = Arc::new(StdMutex::new(Vec::<(DecodePurpose, usize)>::new()));
        let captured = Arc::clone(&decoded);
        let prefetcher = with_purpose_fake(settings(0, 1, 1), move |path, cancel, purpose| {
            let frames = match purpose {
                DecodePurpose::Animated => 3,
                DecodePurpose::Still => 1,
            };
            let media = Arc::new(media(path, 10, 10, frames));
            captured.lock().unwrap().push((purpose, frames));
            if counter.fetch_add(1, Ordering::Relaxed) == 0 {
                if let Some(tx) = entered_tx.lock().unwrap().take() {
                    tx.send(()).unwrap();
                }
                if let Some(rx) = release_rx.lock().unwrap().take() {
                    rx.recv().unwrap();
                }
                if cancel.load(Ordering::Relaxed) {
                    return Err(format!("{}: cancelled", path.display()));
                }
            }
            Ok(media)
        });
        let siblings = vec![a.clone(), b.clone()];
        prefetcher.update_context_many(siblings, Some(a), Vec::new());
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        prefetcher.foreground_requested(&b);

        prefetcher.set_purpose(DecodePurpose::Still);
        prefetcher.set_allowed(false);
        release_tx.send(()).unwrap();
        wait_until(|| !prefetcher.stats().loading);
        assert!(lock(&prefetcher.inner.shared.state).foreground.is_none());
        assert_eq!(prefetcher.stats().entries, 0);

        let loaded = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("retry under new policy");
        assert_eq!(loaded.frames.len(), 1);
        assert_eq!(count.load(Ordering::Relaxed), 2);
        assert_eq!(
            decoded.lock().unwrap().as_slice(),
            [(DecodePurpose::Animated, 3), (DecodePurpose::Still, 1)]
        );
    }

    #[test]
    fn joined_still_handoff_rechecks_purpose_after_unlocked_validation() {
        use std::sync::Mutex as StdMutex;
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let count = Arc::new(AtomicUsize::new(0));
        let decoded = Arc::new(StdMutex::new(
            Vec::<(DecodePurpose, Arc<LoadedMedia>)>::new(),
        ));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let counter = Arc::clone(&count);
        let captured = Arc::clone(&decoded);
        let prefetcher = with_purpose_fake(settings(0, 1, 1), move |path, cancel, purpose| {
            let frames = match purpose {
                DecodePurpose::Animated => 3,
                DecodePurpose::Still => 1,
            };
            let media = Arc::new(media(path, 10, 10, frames));
            captured.lock().unwrap().push((purpose, Arc::clone(&media)));
            if counter.fetch_add(1, Ordering::Relaxed) == 0 {
                if let Some(tx) = entered_tx.lock().unwrap().take() {
                    tx.send(()).unwrap();
                }
                if let Some(rx) = release_rx.lock().unwrap().take() {
                    rx.recv().unwrap();
                }
                if cancel.load(Ordering::Relaxed) {
                    return Err(format!("{}: cancelled", path.display()));
                }
            }
            Ok(media)
        });
        let siblings = vec![a.clone(), b.clone()];
        prefetcher.set_purpose(DecodePurpose::Still);
        prefetcher.update_context_many(siblings, Some(a), Vec::new());
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let loader = prefetcher.clone();
        let path = b.clone();
        let joined = thread::spawn(move || loader.load(&path, &AtomicBool::new(false)));
        wait_until(|| {
            lock(&prefetcher.inner.shared.state)
                .inflight
                .as_ref()
                .is_some_and(|inflight| inflight.waiters > 0)
        });
        prefetcher.set_allowed(false);
        let hook_prefetcher = prefetcher.clone();
        prefetcher.set_handoff_hook(move || hook_prefetcher.set_purpose(DecodePurpose::Animated));
        release_tx.send(()).unwrap();

        let loaded = joined
            .join()
            .unwrap()
            .expect("fallback decodes under the new policy");
        assert_eq!(loaded.frames.len(), 3, "must not deliver the old Still Arc");
        assert_eq!(count.load(Ordering::Relaxed), 2);
        let captured = decoded.lock().unwrap();
        assert_eq!(captured[0].0, DecodePurpose::Still);
        assert_eq!(captured[0].1.frames.len(), 1);
        assert_eq!(captured[1].0, DecodePurpose::Animated);
        assert!(Arc::ptr_eq(&loaded, &captured[1].1));
        assert!(!Arc::ptr_eq(&loaded, &captured[0].1));
    }

    #[test]
    fn ui_coordination_finishes_while_freshness_validation_boundary_is_held() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let prefetcher = with_fake(settings(0, 1, 1), fixed(10, 10, 1));
        prefetcher.set_allowed(false);
        prefetcher.update_context_many(vec![a.clone()], Some(a.clone()), Vec::new());

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        prefetcher.set_handoff_hook(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });

        let loader = prefetcher.clone();
        let path = a.clone();
        let joined = thread::spawn(move || loader.load(&path, &AtomicBool::new(false)));
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("background validation boundary");

        let (done_tx, done_rx) = mpsc::channel();
        let ui = prefetcher.clone();
        let ui_thread = thread::spawn(move || {
            let stats = ui.stats();
            ui.set_allowed(false);
            ui.set_purpose(DecodePurpose::Still);
            done_tx.send(stats).unwrap();
        });
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("UI coordination must not wait for background validation");
        ui_thread.join().unwrap();

        release_tx.send(()).unwrap();
        assert!(
            joined.join().unwrap().is_err(),
            "policy changed during the fresh decode"
        );
        assert_eq!(prefetcher.stats().entries, 0);
    }

    #[test]
    fn pending_selected_context_retains_cache_until_spread_commits() {
        let dir = TempDir::new();
        let siblings: Vec<PathBuf> = ["a", "b", "c", "d", "e"]
            .into_iter()
            .map(|name| dir.file(name))
            .collect();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(1, 2, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let a_display = Arc::new(media(&siblings[0], 10, 10, 1));
        prefetcher.set_allowed(false);
        prefetcher.update_context_many(
            siblings.clone(),
            Some(siblings[0].clone()),
            vec![Arc::clone(&a_display)],
        );
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 2);
        assert_eq!(
            cached_paths(&prefetcher),
            [siblings[1].clone(), siblings[2].clone()]
        );
        assert_eq!(count.load(Ordering::Relaxed), 2);
        let (b_cached, c_cached) = {
            let state = lock(&prefetcher.inner.shared.state);
            (
                Arc::clone(&state.cache[&SourceRef::file(&siblings[1])].decoded.media),
                Arc::clone(&state.cache[&SourceRef::file(&siblings[2])].decoded.media),
            )
        };

        prefetcher.set_allowed(false);
        prefetcher.foreground_requested(&siblings[1]);
        prefetcher.update_context_many(
            siblings.clone(),
            Some(siblings[1].clone()),
            vec![a_display],
        );
        assert_eq!(
            cached_paths(&prefetcher),
            [siblings[2].clone()],
            "pending B keeps C instead of clearing the committed cache"
        );

        let b_loaded = prefetcher
            .load(&siblings[1], &AtomicBool::new(false))
            .expect("B reservation survives pending context");
        assert!(Arc::ptr_eq(&b_loaded, &b_cached));
        prefetcher.foreground_requested(&siblings[2]);
        let c_loaded = prefetcher
            .load(&siblings[2], &AtomicBool::new(false))
            .expect("C reservation survives pending context");
        assert!(Arc::ptr_eq(&c_loaded, &c_cached));
        assert_eq!(count.load(Ordering::Relaxed), 2, "no extra decode");

        let selected = siblings[1].clone();
        prefetcher.update_context_many(
            siblings,
            Some(selected),
            vec![Arc::clone(&b_loaded), Arc::clone(&c_loaded)],
        );
        let stats = prefetcher.stats();
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.bytes, 0);
        let state = lock(&prefetcher.inner.shared.state);
        assert!(Arc::ptr_eq(&state.displayed[0], &b_loaded));
        assert!(Arc::ptr_eq(&state.displayed[1], &c_loaded));
    }

    #[test]
    fn dual_visible_pages_are_excluded_and_prefetch_outside_their_span() {
        let dir = TempDir::new();
        let siblings: Vec<PathBuf> = ["a", "b", "c", "d", "e", "f"]
            .into_iter()
            .map(|name| dir.file(name))
            .collect();
        let c = Arc::new(media(&siblings[2], 10, 10, 1));
        let d = Arc::new(media(&siblings[3], 10, 10, 1));

        let prefetcher = with_fake(settings(1, 1, 1), fixed(10, 10, 1));
        prefetcher.update_context_many(
            siblings.clone(),
            Some(siblings[2].clone()),
            vec![Arc::clone(&c), Arc::clone(&d)],
        );
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 2);
        assert_eq!(
            cached_paths(&prefetcher),
            [siblings[1].clone(), siblings[4].clone()]
        );
        assert_eq!(prefetcher.stats().bytes, 2 * 10 * 10 * 4);
        assert!(
            !lock(&prefetcher.inner.shared.state)
                .cache
                .contains_key(&SourceRef::file(&siblings[2]))
        );
        assert!(
            !lock(&prefetcher.inner.shared.state)
                .cache
                .contains_key(&SourceRef::file(&siblings[3]))
        );

        // Two forward slots start after the second visible page, so the
        // current pages do not consume the ahead count.
        let wide = with_fake(settings(0, 2, 1), fixed(10, 10, 1));
        wide.update_context_many(siblings.clone(), Some(siblings[2].clone()), vec![c, d]);
        wide.set_allowed(true);
        wait_until(|| wide.stats().entries == 2);
        assert_eq!(
            cached_paths(&wide),
            [siblings[4].clone(), siblings[5].clone()]
        );
    }

    #[test]
    fn formerly_visible_dual_pages_readmit_from_original_identity() {
        let dir = TempDir::new();
        let siblings: Vec<PathBuf> = ["a", "b", "c", "d", "e", "f", "g"]
            .into_iter()
            .map(|name| dir.file(name))
            .collect();
        let prefetcher = with_fake(settings(2, 1, 1), fixed(10, 10, 1));
        prefetcher.set_allowed(false);
        prefetcher.update_context_many(siblings.clone(), Some(siblings[2].clone()), Vec::new());
        let c = prefetcher
            .load(&siblings[2], &AtomicBool::new(false))
            .unwrap();
        let d = prefetcher
            .load(&siblings[3], &AtomicBool::new(false))
            .unwrap();
        prefetcher.update_context_many(
            siblings.clone(),
            Some(siblings[2].clone()),
            vec![Arc::clone(&c), Arc::clone(&d)],
        );

        let e = Arc::new(media(&siblings[4], 10, 10, 1));
        let f = Arc::new(media(&siblings[5], 10, 10, 1));
        prefetcher.update_context_many(siblings.clone(), Some(siblings[4].clone()), vec![e, f]);
        assert_eq!(
            cached_paths(&prefetcher),
            [siblings[2].clone(), siblings[3].clone()],
            "both former visible pages re-enter outside the new span"
        );
        let state = lock(&prefetcher.inner.shared.state);
        assert!(Arc::ptr_eq(
            &state.cache[&SourceRef::file(&siblings[2])].decoded.media,
            &c
        ));
        assert!(Arc::ptr_eq(
            &state.cache[&SourceRef::file(&siblings[3])].decoded.media,
            &d
        ));
    }

    #[test]
    fn second_page_handoff_survives_primary_selected_page() {
        use std::sync::Mutex as StdMutex;
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let d = dir.file("d.png");
        let count = Arc::new(AtomicUsize::new(0));
        let captured = Arc::new(StdMutex::new(Vec::<Arc<LoadedMedia>>::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let counter = Arc::clone(&count);
        let captured_for_decoder = Arc::clone(&captured);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            let decoded = Arc::new(media(path, 10, 10, 1));
            captured_for_decoder
                .lock()
                .unwrap()
                .push(Arc::clone(&decoded));
            if counter.fetch_add(1, Ordering::Relaxed) == 0 {
                if let Some(tx) = entered_tx.lock().unwrap().take() {
                    tx.send(()).unwrap();
                }
                if let Some(rx) = release_rx.lock().unwrap().take() {
                    rx.recv().unwrap();
                }
            }
            Ok(decoded)
        });
        let siblings = vec![a.clone(), b.clone(), d];
        prefetcher.update_context_many(siblings.clone(), Some(a), Vec::new());
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        prefetcher.foreground_requested(&b);
        let a_media = Arc::new(media(&siblings[0], 10, 10, 1));
        let selected = siblings[0].clone();
        prefetcher.update_context_many(siblings, Some(selected), vec![a_media]);
        release_tx.send(()).unwrap();
        wait_until(|| !prefetcher.stats().loading);
        assert!(
            lock(&prefetcher.inner.shared.state).foreground.is_some(),
            "second page reservation must survive a different selected page"
        );

        let first = captured.lock().unwrap()[0].clone();
        let loaded = prefetcher
            .load(&b, &AtomicBool::new(false))
            .expect("consume promoted second page");
        assert!(Arc::ptr_eq(&loaded, &first));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(prefetcher.stats().hits, 1);
    }

    #[test]
    fn stale_foreground_token_cannot_drop_the_new_reservation() {
        let dir = TempDir::new();
        let b = dir.file("b.png");
        let c = dir.file("c.png");
        let e = dir.file("e.png");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(0, 2, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![b.clone(), c.clone(), e.clone()];
        prefetcher.set_allowed(false);
        prefetcher.update_context_many(siblings, Some(b.clone()), Vec::new());
        prefetcher.set_allowed(true);
        wait_until(|| {
            let stats = prefetcher.stats();
            stats.entries == 2 && !stats.loading
        });
        let e_cached = {
            let state = lock(&prefetcher.inner.shared.state);
            Arc::clone(&state.cache[&SourceRef::file(&e)].decoded.media)
        };
        prefetcher.set_allowed(false);

        // The old Book transaction owns the generation before the new click.
        let old_token = prefetcher.foreground_requested(&b);
        let (go_tx, go_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let loader = prefetcher.clone();
        let partner = c.clone();
        let old_partner = thread::spawn(move || {
            go_rx.recv().unwrap();
            done_tx
                .send(loader.promote_if_current(&partner, old_token))
                .unwrap();
        });

        let new_token = prefetcher.foreground_requested(&e);
        assert_ne!(old_token, new_token);
        {
            let state = lock(&prefetcher.inner.shared.state);
            let reservation = state.foreground.as_ref().expect("new E reservation");
            assert_eq!(reservation.path, SourceRef::file(&e));
            assert!(Arc::ptr_eq(&reservation.decoded.media, &e_cached));
        }

        go_tx.send(()).unwrap();
        assert!(
            !done_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "old partner must not own the current generation"
        );
        old_partner.join().unwrap();
        {
            let state = lock(&prefetcher.inner.shared.state);
            let reservation = state.foreground.as_ref().expect("E reservation survives");
            assert_eq!(reservation.path, SourceRef::file(&e));
            assert!(Arc::ptr_eq(&reservation.decoded.media, &e_cached));
        }

        let loaded = prefetcher
            .load(&e, &AtomicBool::new(false))
            .expect("load the new reservation");
        assert!(Arc::ptr_eq(&loaded, &e_cached));
        assert_eq!(count.load(Ordering::Relaxed), 2, "no additional decode");
    }

    #[test]
    fn current_foreground_token_promotes_the_partner_page() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let c = dir.file("c.png");
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a.clone(), b.clone(), c.clone()];
        prefetcher.set_allowed(false);
        prefetcher.update_context_many(siblings.clone(), Some(b.clone()), Vec::new());
        prefetcher.set_allowed(true);
        wait_until(|| {
            let stats = prefetcher.stats();
            stats.entries == 1 && !stats.loading
        });
        let c_cached = {
            let state = lock(&prefetcher.inner.shared.state);
            Arc::clone(&state.cache[&SourceRef::file(&c)].decoded.media)
        };
        prefetcher.set_allowed(false);

        let token = prefetcher.foreground_requested(&b);
        assert!(prefetcher.promote_if_current(&c, token));
        {
            let state = lock(&prefetcher.inner.shared.state);
            assert_eq!(state.foreground_generation, token.0);
            assert!(Arc::ptr_eq(
                &state.foreground.as_ref().unwrap().decoded.media,
                &c_cached
            ));
        }

        // The primary selected page differs from the promoted partner, but the
        // transaction token and old displayed page keep the reservation.
        let old_page = Arc::new(media(&siblings[0], 10, 10, 1));
        prefetcher.update_context_many(siblings, Some(b), vec![old_page]);
        assert!(prefetcher.promote_if_current(&c, token));
        let loaded = prefetcher
            .load(&c, &AtomicBool::new(false))
            .expect("partner reservation");
        assert!(Arc::ptr_eq(&loaded, &c_cached));
        assert_eq!(count.load(Ordering::Relaxed), 1, "partner reused the cache");
    }

    #[test]
    fn purpose_switch_invalidates_the_foreground_token() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let prefetcher = with_fake(settings(0, 1, 1), fixed(10, 10, 1));
        prefetcher.set_allowed(false);
        prefetcher.update_context_many(vec![a.clone(), b.clone()], Some(a), Vec::new());
        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        prefetcher.set_allowed(false);

        let old_token = prefetcher.foreground_requested(&b);
        assert!(lock(&prefetcher.inner.shared.state).foreground.is_some());
        prefetcher.set_purpose(DecodePurpose::Still);
        assert!(
            !prefetcher.promote_if_current(&b, old_token),
            "policy change must stale the old transaction"
        );
        let state = lock(&prefetcher.inner.shared.state);
        assert!(state.foreground.is_none());
        assert_ne!(state.foreground_generation, old_token.0);
    }

    #[test]
    fn peek_is_nonconsuming_fresh_and_policy_agnostic() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let prefetcher = with_purpose_fake(settings(0, 1, 1), |path, _, purpose| {
            let frames = match purpose {
                DecodePurpose::Animated => 3,
                DecodePurpose::Still => 1,
            };
            Ok(Arc::new(media(path, 10, 10, frames)))
        });
        let siblings = vec![a.clone(), b.clone()];
        prefetcher.set_allowed(false);
        prefetcher.update_context_many(siblings.clone(), Some(a.clone()), Vec::new());
        let a_media = prefetcher.load(&a, &AtomicBool::new(false)).unwrap();
        prefetcher.update_context_many(
            siblings.clone(),
            Some(a.clone()),
            vec![Arc::clone(&a_media)],
        );

        prefetcher.set_allowed(true);
        wait_until(|| prefetcher.stats().entries == 1);
        let before = prefetcher.stats();
        let cached = {
            let state = lock(&prefetcher.inner.shared.state);
            Arc::clone(&state.cache[&SourceRef::file(&b)].decoded.media)
        };

        let cancel = AtomicBool::new(false);
        let peeked_current = prefetcher.peek(&a, &cancel).expect("current page");
        assert!(Arc::ptr_eq(&peeked_current, &a_media));
        let peeked_cache = prefetcher.peek(&b, &cancel).expect("cached neighbor");
        assert!(Arc::ptr_eq(&peeked_cache, &cached));
        let after = prefetcher.stats();
        assert_eq!(
            (after.entries, after.bytes, after.hits),
            (before.entries, before.bytes, before.hits)
        );
        assert!(
            lock(&prefetcher.inner.shared.state)
                .cache
                .contains_key(&SourceRef::file(&b))
        );

        prefetcher.set_allowed(false);
        prefetcher.foreground_requested(&b);
        let reserved = prefetcher.peek(&b, &cancel).expect("reserved neighbor");
        assert!(Arc::ptr_eq(&reserved, &cached));
        assert!(lock(&prefetcher.inner.shared.state).foreground.is_some());
        assert!(prefetcher.peek(&b, &AtomicBool::new(true)).is_none());

        let modified = SystemTime::now() + Duration::from_secs(5);
        fs::OpenOptions::new()
            .write(true)
            .open(&b)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert!(
            prefetcher.peek(&b, &cancel).is_none(),
            "stale reservation must not be handed out"
        );
        assert!(
            lock(&prefetcher.inner.shared.state).foreground.is_some(),
            "peek must not consume the reservation"
        );
    }

    #[test]
    fn shutdown_is_nonblocking_and_clones_keep_the_worker_alive() {
        let dir = TempDir::new();
        let a = dir.file("a.png");
        let b = dir.file("b.png");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(Some(release_rx));
        let entered_tx = Mutex::new(Some(entered_tx));
        let prefetcher = with_fake(settings(0, 1, 1), move |path, _| {
            if let Some(tx) = entered_tx.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            if let Some(rx) = release_rx.lock().unwrap().take() {
                rx.recv().unwrap();
            }
            Ok(Arc::new(media(path, 10, 10, 1)))
        });
        let siblings = vec![a, b];
        prefetcher.update_context(siblings.clone(), Some(siblings[0].clone()), None);
        prefetcher.set_allowed(true);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let clone = prefetcher.clone();
        drop(prefetcher);
        assert!(!clone.inner.shared.shutdown.load(Ordering::Relaxed));

        let start = Instant::now();
        clone.shutdown();
        assert!(start.elapsed() < Duration::from_millis(100));
        release_tx.send(()).unwrap();
        clone.join_worker();
        assert!(!clone.stats().loading);
    }
}
