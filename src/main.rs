//! Oxeye: a small read-only image viewer built on egui/eframe.

mod comic;
mod edit;
mod export;
mod model;
mod navigation;
mod preload;
mod progress;
mod settings;
mod spread_playback;
mod thumb_ui;
mod thumbnails;
mod ui_state;
mod ui_text;
mod viewer;

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, FontId, Frame as EguiFrame, Key, Layout, Margin,
    Modifiers, PointerButton, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions,
    Theme, ThemePreference, Vec2, ViewportCommand,
};
use image::RgbaImage;
use settings::AppSettings;
#[cfg(test)]
use settings::PrefetchSettings;
use settings::{Language, resolve_language};
use spread_playback::{PageTiming, SpreadPlayback};

#[cfg(test)]
use comic::is_wide;
use comic::{ComicLayout, comic_layout};
use model::DecodePurpose;
use model::source::{SourceRef, catalog_for, source_identity};
use navigation::{FixedLayout, Navigation, PageLayout};
use preload::{CacheStats, ForegroundToken, Prefetcher};
use progress::{PageOverride, ProgressStore};
use settings::{ComicArrowDirection, ReadingMode};
use thumb_ui::{
    CELL_WIDTH, STRIP_HEIGHT, cell_at, clamp_offset, request_range, reveal_offset, visible_range,
};
use thumbnails::Thumbnails;
use ui_state::{
    Feedback, FeedbackKind, NavigationFlow, NavigationTicket, horizontal_navigation,
    restore_defaults,
};
use viewer::{Playback, TiledFrame, intersects, prepare_tiles, spread_layout};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Previous,
    Next,
    JumpFiles(isize),
    Pan { vertical: bool, forward: bool },
    ZoomIn,
    ZoomOut,
    Fit,
    Actual,
    RotateLeft,
    RotateRight,
    Playback,
    Fullscreen,
    Escape,
    Open,
    Copy,
    First,
    Last,
    Close,
    Settings,
    ToggleMode,
    WideToggle,
    Edit,
    UndoEdit,
    FlipHorizontal,
    FlipVertical,
    SaveOverwrite,
    SaveAs,
    CropApply,
    CropCancel,
    EditExit,
}

/// Maps one key press to one viewer action. Keeping this pure makes key order
/// testable without a window.
fn key_action(key: Key, modifiers: Modifiers) -> Option<Action> {
    if modifiers.command && !modifiers.alt {
        return match key {
            Key::S if modifiers.shift => Some(Action::SaveAs),
            Key::S => Some(Action::SaveOverwrite),
            Key::Z => Some(Action::UndoEdit),
            Key::O => Some(Action::Open),
            Key::C => Some(Action::Copy),
            Key::W => Some(Action::Close),
            Key::R => Some(Action::RotateRight),
            Key::L => Some(Action::RotateLeft),
            Key::Comma => Some(Action::Settings),
            Key::Plus | Key::Equals => Some(Action::ZoomIn),
            Key::Minus => Some(Action::ZoomOut),
            _ => None,
        };
    }
    if modifiers.command || modifiers.alt {
        return (modifiers.alt && key == Key::Enter).then_some(Action::Fullscreen);
    }
    match key {
        Key::PageDown => Some(Action::Next),
        Key::PageUp => Some(Action::Previous),
        Key::ArrowLeft => Some(Action::Pan {
            vertical: false,
            forward: false,
        }),
        Key::ArrowRight => Some(Action::Pan {
            vertical: false,
            forward: true,
        }),
        Key::ArrowUp => Some(Action::JumpFiles(-10)),
        Key::ArrowDown => Some(Action::JumpFiles(10)),
        Key::Space => Some(Action::Pan {
            vertical: true,
            forward: true,
        }),
        Key::Home => Some(Action::First),
        Key::End => Some(Action::Last),
        Key::Escape => Some(Action::Escape),
        Key::F11 => Some(Action::Fullscreen),
        Key::Plus | Key::Equals => Some(Action::ZoomIn),
        Key::Minus => Some(Action::ZoomOut),
        Key::Num1 | Key::Num9 => Some(Action::Fit),
        Key::Num0 => Some(Action::Actual),
        Key::P => Some(Action::Playback),
        Key::M if !modifiers.shift => Some(Action::ToggleMode),
        Key::W if modifiers == Modifiers::NONE => Some(Action::WideToggle),
        Key::C if modifiers == Modifiers::NONE => Some(Action::Edit),
        Key::X if modifiers == Modifiers::NONE => Some(Action::FlipHorizontal),
        Key::Y if modifiers == Modifiers::NONE => Some(Action::FlipVertical),
        Key::Enter => Some(Action::CropApply),
        _ => None,
    }
}

fn action_allows_repeat(action: Action) -> bool {
    !matches!(
        action,
        Action::ToggleMode
            | Action::WideToggle
            | Action::Edit
            | Action::UndoEdit
            | Action::FlipHorizontal
            | Action::FlipVertical
            | Action::SaveOverwrite
            | Action::SaveAs
            | Action::CropApply
            | Action::CropCancel
            | Action::EditExit
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsKey {
    Next,
    Previous,
    Decrease,
    Increase,
    Activate,
    Cancel,
    CloseWindow,
}

fn settings_key_action(key: Key, modifiers: Modifiers) -> Option<SettingsKey> {
    if modifiers.command && key == Key::W {
        return Some(SettingsKey::CloseWindow);
    }
    if modifiers.command || modifiers.alt {
        return None;
    }
    match key {
        Key::Tab if modifiers.shift => Some(SettingsKey::Previous),
        Key::Tab => Some(SettingsKey::Next),
        Key::ArrowLeft | Key::ArrowDown | Key::Minus => Some(SettingsKey::Decrease),
        Key::ArrowRight | Key::ArrowUp | Key::Plus | Key::Equals => Some(SettingsKey::Increase),
        Key::Enter | Key::Space => Some(SettingsKey::Activate),
        Key::Escape => Some(SettingsKey::Cancel),
        _ => None,
    }
}

const SETTING_BEFORE: usize = 0;
const SETTING_AFTER: usize = 1;
const SETTING_MEMORY: usize = 2;
const SETTING_PREVIEWS: usize = 3;
const SETTING_READING: usize = 4;
const SETTING_AUTO_WIDE: usize = 5;
const SETTING_ARROW: usize = 6;
const SETTING_LANGUAGE: usize = 7;
const SETTING_RESTORE: usize = 8;
const SETTING_CANCEL: usize = 9;
const SETTING_APPLY: usize = 10;
const SETTING_COUNT: usize = 11;

/// Reading progress is small; one write after the page settles is enough.
const PROGRESS_SAVE_DELAY: Duration = Duration::from_millis(500);

fn next_settings_control(current: usize, backwards: bool) -> usize {
    (current + if backwards { SETTING_COUNT - 1 } else { 1 }) % SETTING_COUNT
}

fn step_value(value: u64, increase: bool, step: u64, maximum: u64) -> u64 {
    if increase {
        value.saturating_add(step).min(maximum)
    } else {
        value.saturating_sub(step)
    }
}

fn pause_controllers(
    playback: &mut Option<Playback>,
    spread: &mut Option<SpreadPlayback>,
    mode: ReadingMode,
    now: Duration,
) {
    if mode == ReadingMode::Book {
        if let Some(spread) = spread {
            spread.pause(now);
        }
    } else if let Some(playback) = playback {
        playback.pause(now);
    }
}

fn animation_available(
    mode: ReadingMode,
    spread_present: bool,
    frame_counts: impl IntoIterator<Item = usize>,
) -> bool {
    let mut frame_counts = frame_counts.into_iter();
    match mode {
        ReadingMode::Book => spread_present && frame_counts.any(|count| count > 1),
        ReadingMode::Single => frame_counts.next().is_some_and(|count| count > 1),
    }
}

fn animation_changes(targets: &[usize], committed: &[usize]) -> Vec<(usize, usize)> {
    targets
        .iter()
        .enumerate()
        .filter(|(page, target)| committed.get(*page).copied() != Some(**target))
        .map(|(page, target)| (page, *target))
        .collect()
}

fn animation_frame_is_current(prepared: usize, target: usize, committed: usize) -> bool {
    prepared == target && target != committed
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupPlaybackAction {
    Pause,
    Resume,
    Restart,
}

fn group_playback_action(any_playing: bool, all_finished: bool) -> GroupPlaybackAction {
    if any_playing {
        GroupPlaybackAction::Pause
    } else if all_finished {
        GroupPlaybackAction::Restart
    } else {
        GroupPlaybackAction::Resume
    }
}

fn spread_any_playing(spread: &Option<SpreadPlayback>, now: Duration) -> bool {
    spread
        .as_ref()
        .is_some_and(|spread| spread.any_playing(now))
}

fn page_timing(media: &model::LoadedMedia) -> Option<PageTiming> {
    (media.frames.len() > 1).then(|| PageTiming {
        frame_count: media.frames.len(),
        delays: media.frames.iter().map(|frame| frame.delay).collect(),
        loop_mode: media.loop_mode,
    })
}

fn thumbnail_poll_delay(stable_polls: u8, missing_visible: bool) -> Option<Duration> {
    if stable_polls < 2 {
        Some(Duration::from_millis(250))
    } else if missing_visible {
        Some(Duration::from_secs(1))
    } else {
        None
    }
}

fn toggled_reading_mode(mode: ReadingMode) -> ReadingMode {
    if mode == ReadingMode::Single {
        ReadingMode::Book
    } else {
        ReadingMode::Single
    }
}

fn cycled_language(language: Language) -> Language {
    match language {
        Language::Automatic => Language::English,
        Language::English => Language::SimplifiedChinese,
        Language::SimplifiedChinese => Language::Japanese,
        Language::Japanese => Language::Automatic,
    }
}

fn toggled_arrow_direction(direction: ComicArrowDirection) -> ComicArrowDirection {
    if direction == ComicArrowDirection::RightNext {
        ComicArrowDirection::LeftNext
    } else {
        ComicArrowDirection::RightNext
    }
}

fn step_draft_setting(control: usize, increase: bool, draft: &mut AppSettings) {
    match control {
        SETTING_BEFORE => {
            draft.prefetch.before =
                step_value(draft.prefetch.before as u64, increase, 1, 50) as usize
        }
        SETTING_AFTER => {
            draft.prefetch.after = step_value(draft.prefetch.after as u64, increase, 1, 50) as usize
        }
        SETTING_MEMORY => {
            draft.prefetch.memory_mib = step_value(draft.prefetch.memory_mib, increase, 64, 2048)
        }
        SETTING_PREVIEWS => draft.thumbnails_visible = !draft.thumbnails_visible,
        SETTING_READING => draft.reading_mode = toggled_reading_mode(draft.reading_mode),
        SETTING_AUTO_WIDE => draft.auto_wide = !draft.auto_wide,
        SETTING_ARROW => {
            draft.comic_arrow_direction = toggled_arrow_direction(draft.comic_arrow_direction)
        }
        SETTING_LANGUAGE => draft.language = cycled_language(draft.language),
        _ => {}
    }
}

fn reading_mode_changed(active: &AppSettings, draft: &AppSettings) -> bool {
    active.reading_mode != draft.reading_mode
}

/// Keep the file the user last chose when switching layouts, not just the
/// spread anchor that Book navigation stores as `selected`.
fn mode_target(
    current_file: &Option<SourceRef>,
    selected: &Option<SourceRef>,
    displayed: &Option<SourceRef>,
) -> Option<SourceRef> {
    current_file
        .clone()
        .or_else(|| selected.clone())
        .or_else(|| displayed.clone())
}

fn source_index(selected: Option<&SourceRef>, siblings: &[SourceRef]) -> Option<usize> {
    let selected = selected?;
    siblings.iter().position(|source| source == selected)
}

/// Progress stores archive member indices; layouts use sorted catalogue
/// positions. Saving an override must translate back the same way.
fn translate_overrides(
    overrides: BTreeMap<usize, PageOverride>,
    siblings: &[SourceRef],
) -> BTreeMap<usize, PageOverride> {
    overrides
        .into_iter()
        .filter_map(|(member, mode)| {
            let position = siblings.iter().position(
                |source| matches!(source, SourceRef::Archive { index, .. } if *index == member),
            )?;
            Some((position, mode))
        })
        .collect()
}

fn source_label(source: &SourceRef) -> String {
    if let Some(archive) = source.archive_root() {
        format!(
            "{} / {}",
            archive
                .file_name()
                .unwrap_or(archive.as_os_str())
                .to_string_lossy(),
            source.display_name()
        )
    } else {
        source.display_name()
    }
}

/// A commit or navigation step must cross a later completed output frame.
fn frame_advanced(painted_frame: u64, current_frame: u64) -> bool {
    current_frame > painted_frame
}

fn fit_scale(image: (u32, u32), viewport: Vec2, dpi: f32) -> f32 {
    ((viewport.x - 48.).max(1.) / image.0.max(1) as f32)
        .min((viewport.y - 48.).max(1.) / image.1.max(1) as f32)
        .min(actual_scale(dpi))
}

fn actual_scale(dpi: f32) -> f32 {
    1. / dpi
}

fn zoom_scale(current: f32, factor: f32, dpi: f32) -> f32 {
    (current * factor).clamp(0.005 / dpi, 32. / dpi)
}

fn anchored_pan(pan: [f32; 2], anchor: [f32; 2], ratio: f32) -> [f32; 2] {
    [
        anchor[0] - (anchor[0] - pan[0]) * ratio,
        anchor[1] - (anchor[1] - pan[1]) * ratio,
    ]
}

fn arrow_pan(pan: f32, image_extent: f32, viewport_extent: f32, forward: bool) -> Option<f32> {
    let overflow = (image_extent - viewport_extent) / 2.;
    (overflow > 0.5).then(|| (pan + if forward { -80. } else { 80. }).clamp(-overflow, overflow))
}

fn adjacent_index(current: usize, length: usize, next: bool) -> Option<usize> {
    let index = if next {
        current.checked_add(1)?
    } else {
        current.checked_sub(1)?
    };
    (index < length).then_some(index)
}

/// Explicit opens and drops discard the FIFO rather than reusing old targets.
fn reset_navigation(flow: &mut NavigationFlow, paths: &mut Vec<SourceRef>) {
    flow.clear();
    paths.clear();
}

#[derive(Clone, Copy)]
enum Zoom {
    Fit,
    Actual,
    Manual(f32),
}

enum Work {
    Load(
        SourceRef,
        Prefetcher,
        ForegroundToken,
        u32,
        ReadingMode,
        Vec<usize>,
        Language,
        u8,
    ),
    Spread {
        media: Vec<Arc<model::LoadedMedia>>,
        frames: Vec<usize>,
        turns: u8,
        cap: u32,
    },
    AnimateSpread {
        generation: u64,
        pages: Vec<AnimatePage>,
        turns: u8,
        cap: u32,
    },
    Frame {
        media: Arc<model::LoadedMedia>,
        frame: usize,
        turns: u8,
        cap: u32,
    },
}

struct Completion {
    revision: u64,
    result: WorkResult,
}

struct WorkResult {
    selection: Option<(SourceRef, Result<Vec<SourceRef>, String>)>,
    image: Result<Prepared, String>,
    animation: Option<Result<SpreadUpdate, String>>,
}

struct AnimatePage {
    page: usize,
    media: Arc<model::LoadedMedia>,
    frame: usize,
}

struct SpreadUpdate {
    generation: u64,
    turns: u8,
    pages: Vec<SpreadPageUpdate>,
}

struct SpreadPageUpdate {
    page: usize,
    frame: usize,
    tiled: Arc<TiledFrame>,
}

#[derive(Clone, Copy, Default)]
struct Rotation {
    requested: u8,
    displayed: u8,
}

impl Rotation {
    fn request(&mut self, clockwise: bool) -> u8 {
        self.requested = (self.requested + if clockwise { 1 } else { 3 }) % 4;
        self.requested
    }

    fn cancel(&mut self) {
        self.requested = self.displayed;
    }

    fn commit(&mut self, turns: u8) {
        self.displayed = turns;
        self.requested = turns;
    }
}

/// Confirmation before a destructive write; Enter never confirms, only the button does.
struct SaveConfirm {
    path: PathBuf,
    format: export::ExportFormat,
    /// True = replace the frozen edit source; false = Save As copy.
    overwrite: bool,
}

/// One running `atomic_write`; the worker owns the pixels, not the UI.
struct SaveJob {
    receiver: Receiver<Result<(), String>>,
    overwrite: bool,
    path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CropAspect {
    Free,
    Square,
    FourThree,
    ThreeTwo,
    SixteenNine,
    Original,
}

impl CropAspect {
    fn ratio(self, dimensions: (u32, u32)) -> Option<(u32, u32)> {
        match self {
            Self::Free => None,
            Self::Square => Some((1, 1)),
            Self::FourThree => Some((4, 3)),
            Self::ThreeTwo => Some((3, 2)),
            Self::SixteenNine => Some((16, 9)),
            Self::Original => Some(dimensions),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CropHandle {
    NorthWest,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
}

#[derive(Clone, Copy)]
enum CropDrag {
    New {
        anchor: [f32; 2],
    },
    /// Origin rect + press point: every frame recomputes from the total delta,
    /// so sub-pixel drags cannot round away.
    Move {
        origin: edit::PixelRect,
        anchor: [f32; 2],
    },
    Resize {
        handle: CropHandle,
        /// Press-time rect: the outward-drawn handle never becomes a boundary.
        origin: edit::PixelRect,
        anchor: [f32; 2],
    },
}

struct CropState {
    rect: edit::PixelRect,
    aspect: CropAspect,
    drag: Option<CropDrag>,
}

/// Editing is a temporary mode; the on-disk frame stays in `original_media`.
struct EditUi {
    session: edit::EditSession,
    /// Frozen at entry: never follow `selected`, which may point at a failed load.
    source: SourceRef,
    original_media: Arc<model::LoadedMedia>,
    original_tiled: Option<Arc<TiledFrame>>,
    original_rotation: Rotation,
    restore_mode: Option<ReadingMode>,
    crop: Option<CropState>,
    confirm: Option<SaveConfirm>,
    saving: Option<SaveJob>,
    quality: u8,
}

/// Book -> Single transition waiting for its load to commit.
struct PendingEdit {
    source: SourceRef,
    rotation: Rotation,
    restore: ReadingMode,
    selected: Option<SourceRef>,
    displayed: Option<SourceRef>,
    current_file: Option<SourceRef>,
    crop: bool,
}

fn editable_format(source: &SourceRef) -> Option<export::ExportFormat> {
    match source {
        SourceRef::File(path) => export::format_for_path(path),
        SourceRef::Archive { name, .. } => export::format_for_path(Path::new(name)),
    }
}

fn format_label(format: export::ExportFormat) -> &'static str {
    match format {
        export::ExportFormat::Png => "PNG",
        export::ExportFormat::Jpeg => "JPEG",
        export::ExportFormat::Webp => "WebP",
        export::ExportFormat::Bmp => "BMP",
    }
}

fn edit_error_text(text: &ui_text::Catalog, error: edit::EditError) -> String {
    match error {
        edit::EditError::EmptyImage => text.edit_empty.to_owned(),
        edit::EditError::ZeroArea => text.crop_empty.to_owned(),
        edit::EditError::Oversize { width, height } => text.edit_oversize(width, height),
        edit::EditError::TooManyOps => text.edit_too_many_ops.to_owned(),
    }
}

/// Absolute identity for a path, resolving the deepest existing ancestor.
///
/// Nonexistent targets are compared by their future parent (symlinks resolved
/// where they exist) plus the remaining components, normalized for `.`/`..`.
fn identity_path(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut existing = absolute;
    let mut remainder = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            break;
        };
        remainder.push(name.to_os_string());
        match existing.parent() {
            Some(parent) if parent != existing => existing = parent.to_path_buf(),
            _ => break,
        }
    }
    let mut resolved = existing.canonicalize().unwrap_or(existing);
    for part in remainder.into_iter().rev() {
        resolved.push(part);
    }
    let mut normalized = PathBuf::new();
    for component in resolved.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Treats Save As onto the frozen source as the overwrite transaction.
fn same_save_target(a: &Path, b: &Path) -> bool {
    let a = identity_path(a);
    let b = identity_path(b);
    if a == b {
        return true;
    }
    // Only the platform's default case-insensitive filesystems fold names;
    // a false positive can only add one extra overwrite confirmation.
    //
    // Known limit: macOS can report the same file with different Unicode
    // normalization (NFC vs NFD); that is outside this comparison.
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        return a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase();
    }
    false
}

/// Save As risks shown before the worker starts, so the user can still cancel.
fn save_risk_reason(
    text: &ui_text::Catalog,
    format: export::ExportFormat,
    transparent: bool,
) -> Option<&'static str> {
    match format {
        export::ExportFormat::Webp => Some(text.save_confirm_webp),
        export::ExportFormat::Jpeg if transparent => Some(text.save_confirm_transparency),
        _ => None,
    }
}

/// egui activates a focused button from an Enter key event regardless of
/// modifiers; the save dialog strips every pressed Enter before rendering any
/// button, while Space and assistive activation still work normally.
fn save_confirm_strips_event(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key {
            key: Key::Enter,
            pressed: true,
            ..
        }
    )
}

/// Handles are tested in screen points so they stay grabbable at any zoom. The
/// hit box grows with the drawn handle and shares its centres, so what the user
/// sees is what they grab. Overlapping hit boxes (a rect smaller than one handle)
/// resolve to the nearest centre, never to the first one in the array.
fn crop_handle_at(screen: [f32; 4], pos: Pos2, handle_size: f32) -> Option<CropHandle> {
    let radius = handle_size * 0.8 + 2.;
    crop_handle_points(screen, handle_size)
        .into_iter()
        .filter(|(_, center)| {
            (pos.x - center.x).abs() <= radius && (pos.y - center.y).abs() <= radius
        })
        .min_by(|(_, a), (_, b)| a.distance_sq(pos).total_cmp(&b.distance_sq(pos)))
        .map(|(handle, _)| handle)
}

/// Handles grow a little with the display scale: one point is a fraction of a
/// millimetre on a 4K panel, so a fixed 7pt square becomes hard to grab.
fn crop_handle_size(dpi: f32) -> f32 {
    (8. + (dpi - 1.).max(0.) * 1.5).clamp(8., 12.)
}

/// Handle centres shared by drawing and hit-testing.
///
/// A rect too small for three handle widths (a 1×1 crop, or a tiny image at
/// Fit) pushes its handles outward per axis, so the eight squares never pile up
/// into one unclickable blob.
fn crop_handle_points(screen: [f32; 4], size: f32) -> [(CropHandle, Pos2); 8] {
    let [x, y, w, h] = screen;
    let dx = if w < size * 3. { size } else { 0. };
    let dy = if h < size * 3. { size } else { 0. };
    let (right, bottom) = (x + w, y + h);
    [
        (CropHandle::NorthWest, Pos2::new(x - dx, y - dy)),
        (CropHandle::North, Pos2::new(x + w / 2., y - dy)),
        (CropHandle::NorthEast, Pos2::new(right + dx, y - dy)),
        (CropHandle::East, Pos2::new(right + dx, y + h / 2.)),
        (CropHandle::SouthEast, Pos2::new(right + dx, bottom + dy)),
        (CropHandle::South, Pos2::new(x + w / 2., bottom + dy)),
        (CropHandle::SouthWest, Pos2::new(x - dx, bottom + dy)),
        (CropHandle::West, Pos2::new(x - dx, y + h / 2.)),
    ]
}

fn crop_cursor(handle: CropHandle) -> CursorIcon {
    match handle {
        CropHandle::NorthWest | CropHandle::SouthEast => CursorIcon::ResizeNwSe,
        CropHandle::NorthEast | CropHandle::SouthWest => CursorIcon::ResizeNeSw,
        CropHandle::North | CropHandle::South => CursorIcon::ResizeVertical,
        CropHandle::East | CropHandle::West => CursorIcon::ResizeHorizontal,
    }
}

/// Ergonomics hint, not a hard limit: below half a physical pixel per source
/// pixel a boundary is too coarse to place deliberately. This uses the same
/// unit as the footer percentage (`scale * dpi`), so the hint cannot contradict
/// the visible zoom label.
fn crop_needs_zoom(scale: f32, dpi: f32) -> bool {
    scale * dpi < 0.5
}

/// Moving window for the indeterminate progress bar, in points from the track's
/// left edge. Never wider than the thumb and never outside the track.
/// Footer navigation mirrors the `perform` interception list: while editing the
/// buttons are disabled instead of looking clickable but silently doing nothing.
fn footer_navigation_enabled(editing: bool, base: bool) -> bool {
    base && !editing
}

fn indeterminate_span(phase: f32, width: f32) -> (f32, f32) {
    let thumb = (width * 0.3).min(120.);
    let start = phase.clamp(0., 1.) * (width - thumb).max(0.);
    (start, start + thumb)
}

/// Warning rows of the save confirmation: metadata loss and irreversibility for
/// an overwrite, plus the format-specific risk. Order is stable for reading.
fn save_warning_rows(
    text: &ui_text::Catalog,
    format: export::ExportFormat,
    overwrite: bool,
    transparent: bool,
) -> Vec<&'static str> {
    let mut rows = Vec::new();
    if overwrite {
        rows.push(text.save_confirm_warning);
        rows.push(text.save_confirm_irreversible);
    }
    if let Some(reason) = save_risk_reason(text, format, transparent) {
        rows.push(reason);
    }
    rows
}

/// A dragged rectangle always covers at least one pixel inside the image.
fn crop_rect_from_points(a: [f32; 2], b: [f32; 2], dimensions: (u32, u32)) -> edit::PixelRect {
    let max_x = dimensions.0.saturating_sub(1) as f32;
    let max_y = dimensions.1.saturating_sub(1) as f32;
    let x0 = a[0].min(b[0]).clamp(0., max_x);
    let y0 = a[1].min(b[1]).clamp(0., max_y);
    let x1 = a[0].max(b[0]).clamp(x0 + 1., dimensions.0 as f32);
    let y1 = a[1].max(b[1]).clamp(y0 + 1., dimensions.1 as f32);
    edit::PixelRect {
        x: x0.round() as u32,
        y: y0.round() as u32,
        w: (x1 - x0).round().max(1.) as u32,
        h: (y1 - y0).round().max(1.) as u32,
    }
}

fn translate_crop(
    rect: edit::PixelRect,
    delta: [f32; 2],
    dimensions: (u32, u32),
) -> edit::PixelRect {
    let x = (rect.x as f32 + delta[0])
        .round()
        .clamp(0., dimensions.0.saturating_sub(rect.w) as f32) as u32;
    let y = (rect.y as f32 + delta[1])
        .round()
        .clamp(0., dimensions.1.saturating_sub(rect.h) as f32) as u32;
    edit::PixelRect { x, y, ..rect }
}

/// Move from the press-time rect and point: total delta, not per-frame deltas.
fn moved_crop(
    origin: edit::PixelRect,
    anchor: [f32; 2],
    point: [f32; 2],
    dimensions: (u32, u32),
) -> edit::PixelRect {
    translate_crop(
        origin,
        [point[0] - anchor[0], point[1] - anchor[1]],
        dimensions,
    )
}

/// Resize from the press-time rect by the pointer's total image-space delta.
///
/// Handles may be drawn outside a small rect, so the pointer position itself is
/// never taken as the new boundary: only its displacement moves the edges. That
/// keeps the first drag frame from jumping by the outward offset.
fn resize_crop(
    rect: edit::PixelRect,
    handle: CropHandle,
    delta: [f32; 2],
    dimensions: (u32, u32),
) -> edit::PixelRect {
    let mut left = rect.x as f32;
    let mut top = rect.y as f32;
    let mut right = (rect.x + rect.w) as f32;
    let mut bottom = (rect.y + rect.h) as f32;
    let dx = delta[0];
    let dy = delta[1];
    let clamp_x = |value: f32| value.clamp(0., dimensions.0 as f32);
    let clamp_y = |value: f32| value.clamp(0., dimensions.1 as f32);
    match handle {
        CropHandle::NorthWest => {
            left = clamp_x(left + dx);
            top = clamp_y(top + dy);
        }
        CropHandle::North => top = clamp_y(top + dy),
        CropHandle::NorthEast => {
            right = clamp_x(right + dx);
            top = clamp_y(top + dy);
        }
        CropHandle::East => right = clamp_x(right + dx),
        CropHandle::SouthEast => {
            right = clamp_x(right + dx);
            bottom = clamp_y(bottom + dy);
        }
        CropHandle::South => bottom = clamp_y(bottom + dy),
        CropHandle::SouthWest => {
            left = clamp_x(left + dx);
            bottom = clamp_y(bottom + dy);
        }
        CropHandle::West => left = clamp_x(left + dx),
    }
    if right - left < 1. {
        if matches!(
            handle,
            CropHandle::West | CropHandle::NorthWest | CropHandle::SouthWest
        ) {
            right = (left + 1.).min(dimensions.0 as f32);
            left = right - 1.;
        } else {
            left = (right - 1.).max(0.);
            right = left + 1.;
        }
    }
    if bottom - top < 1. {
        if matches!(
            handle,
            CropHandle::North | CropHandle::NorthWest | CropHandle::NorthEast
        ) {
            bottom = (top + 1.).min(dimensions.1 as f32);
            top = bottom - 1.;
        } else {
            top = (bottom - 1.).max(0.);
            bottom = top + 1.;
        }
    }
    let resized = edit::PixelRect {
        x: left.round() as u32,
        y: top.round() as u32,
        w: (right - left).round().max(1.) as u32,
        h: (bottom - top).round().max(1.) as u32,
    };
    edit::clamp_crop(resized, dimensions.0, dimensions.1).unwrap_or(rect)
}

// One worker and one replaceable pending request bound decoding and rotation memory.
#[derive(Default)]
struct Loader {
    revision: u64,
    running: bool,
    pending: Option<(u64, Work)>,
    cancellation: Option<Arc<AtomicBool>>,
}

impl Loader {
    /// Drops queued work and invalidates an in-flight result without starting
    /// anything else. The running thread finishes and is ignored.
    fn cancel(&mut self) {
        if let Some(cancelled) = &self.cancellation {
            cancelled.store(true, Ordering::Relaxed);
        }
        self.revision += 1;
        self.pending = None;
    }

    fn request(&mut self, work: Work) {
        if let Some(cancelled) = &self.cancellation {
            cancelled.store(true, Ordering::Relaxed);
        }
        self.revision += 1;
        self.pending = Some((self.revision, work));
    }

    fn take(&mut self) -> Option<(u64, Work, Arc<AtomicBool>)> {
        if self.running {
            return None;
        }
        let request = self.pending.take()?;
        self.running = true;
        let cancellation = Arc::new(AtomicBool::new(false));
        self.cancellation = Some(cancellation.clone());
        Some((request.0, request.1, cancellation))
    }

    fn finish(&mut self, revision: u64) -> bool {
        self.running = false;
        self.cancellation = None;
        revision == self.revision
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        if let Some(cancelled) = &self.cancellation {
            cancelled.store(true, Ordering::Relaxed);
        }
    }
}

struct Prepared {
    media: Arc<model::LoadedMedia>,
    tiled: Arc<TiledFrame>,
    others: Vec<PreparedPage>,
    dimensions: (u32, u32),
    loaded: bool,
    frame: usize,
    turns: u8,
    mode: ReadingMode,
}

struct PreparedPage {
    media: Arc<model::LoadedMedia>,
    tiled: Arc<TiledFrame>,
    frame: usize,
}

struct Candidate {
    revision: u64,
    image: Prepared,
    painted_frame: Option<u64>,
}

#[derive(Clone, Copy)]
struct NavigationPaint {
    ticket: NavigationTicket,
    shown_frame: Option<u64>,
}

fn prepare_frame(
    media: Arc<model::LoadedMedia>,
    frame: usize,
    turns: u8,
    loaded: bool,
    cap: u32,
    cancelled: &AtomicBool,
) -> Result<Prepared, String> {
    if cancelled.load(Ordering::Relaxed) {
        return Err("Image preparation cancelled.".into());
    }
    let source = &media
        .frames
        .get(frame)
        .ok_or("The image has no displayable frame.")?
        .pixels;
    let rotated;
    let pixels = match turns % 4 {
        0 => source.as_ref(),
        1 => {
            rotated = model::rotate_image(source, true);
            &rotated
        }
        2 => {
            let intermediate = model::rotate_image(source, true);
            if cancelled.load(Ordering::Relaxed) {
                return Err("Image preparation cancelled.".into());
            }
            rotated = model::rotate_image(&intermediate, true);
            &rotated
        }
        _ => {
            rotated = model::rotate_image(source, false);
            &rotated
        }
    };
    let tiled = Arc::new(prepare_tiles(pixels, cap, cancelled)?);
    Ok(Prepared {
        dimensions: tiled.dimensions,
        media,
        tiled,
        loaded,
        frame,
        turns,
        others: Vec::new(),
        mode: ReadingMode::Single,
    })
}

fn prepare_spread(
    media: Vec<Arc<model::LoadedMedia>>,
    frames: &[usize],
    turns: u8,
    loaded: bool,
    cap: u32,
    cancelled: &AtomicBool,
) -> Result<Prepared, String> {
    let mut media = media.into_iter();
    let first = media.next().ok_or("The spread has no displayable pages.")?;
    let first_frame = frames.first().copied().unwrap_or(0);
    let mut prepared = prepare_frame(first, first_frame, turns, loaded, cap, cancelled)?;
    prepared.mode = ReadingMode::Book;
    for (index, media) in media.enumerate() {
        let frame = frames.get(index + 1).copied().unwrap_or(0);
        let page = prepare_frame(media, frame, turns, loaded, cap, cancelled)?;
        prepared.others.push(PreparedPage {
            media: page.media,
            tiled: page.tiled,
            frame: page.frame,
        });
    }
    let dimensions: Vec<_> = std::iter::once(prepared.tiled.dimensions)
        .chain(prepared.others.iter().map(|page| page.tiled.dimensions))
        .collect();
    prepared.dimensions = spread_layout(&dimensions).0;
    Ok(prepared)
}

fn prepare(work: Work, cancelled: &AtomicBool) -> WorkResult {
    if cancelled.load(Ordering::Relaxed) {
        return WorkResult {
            selection: None,
            image: Err("Image preparation cancelled.".into()),
            animation: None,
        };
    }
    match work {
        Work::Load(path, prefetch, foreground, cap, mode, spread, language, turns) => {
            // Scan even when decoding fails, so a corrupt first file does not trap navigation.
            let siblings = match &path {
                SourceRef::File(file) => model::sibling_images(file)
                    .map(|paths| paths.into_iter().map(SourceRef::File).collect()),
                SourceRef::Archive { .. } => catalog_for(path.path()).map(|catalog| catalog.items),
            }
            .map_err(|error| ui_text::catalog(language).open_failed(&path.to_string(), &error));
            // The anchor must still be the catalogue item at its recorded index;
            // otherwise the archive changed and the read below rejects it.
            let targets = siblings
                .as_ref()
                .ok()
                .and_then(|siblings| {
                    let anchor = spread.first().and_then(|index| siblings.get(*index))?;
                    (anchor == &path)
                        .then(|| {
                            spread
                                .iter()
                                .map(|index| siblings.get(*index).cloned())
                                .collect::<Option<Vec<_>>>()
                        })
                        .flatten()
                })
                .unwrap_or_else(|| vec![path.clone()]);
            let selected = targets.first().cloned().unwrap_or_else(|| path.clone());
            let image = (|| {
                if path.is_archive()
                    && let Err(error) = &siblings
                {
                    return Err(error.clone());
                }
                let clicked = prefetch.load_source(&path, cancelled)?;
                let mut media = Vec::new();
                for target in &targets {
                    if target == &path {
                        media.push(clicked.clone());
                    } else {
                        if !prefetch.promote_source_if_current(target, foreground) {
                            return Err("Image preparation cancelled.".into());
                        }
                        media.push(prefetch.load_source(target, cancelled)?);
                    }
                }
                if mode == ReadingMode::Book {
                    prepare_spread(media, &[0], turns, true, cap, cancelled)
                } else {
                    prepare_frame(clicked, 0, turns, true, cap, cancelled)
                }
            })();
            WorkResult {
                selection: Some((selected, siblings)),
                image,
                animation: None,
            }
        }
        Work::Spread {
            media,
            frames,
            turns,
            cap,
        } => WorkResult {
            selection: None,
            image: prepare_spread(media, &frames, turns, false, cap, cancelled),
            animation: None,
        },
        Work::AnimateSpread {
            generation,
            pages,
            turns,
            cap,
        } => {
            let update = (|| {
                let mut updates = Vec::new();
                for page in pages {
                    let prepared =
                        prepare_frame(page.media, page.frame, turns, false, cap, cancelled)?;
                    updates.push(SpreadPageUpdate {
                        page: page.page,
                        frame: prepared.frame,
                        tiled: prepared.tiled,
                    });
                }
                Ok(SpreadUpdate {
                    generation,
                    turns,
                    pages: updates,
                })
            })();
            WorkResult {
                selection: None,
                image: Err("Animation job has no whole image.".into()),
                animation: Some(update),
            }
        }
        Work::Frame {
            media,
            frame,
            turns,
            cap,
        } => WorkResult {
            selection: None,
            image: prepare_frame(media, frame, turns, false, cap, cancelled),
            animation: None,
        },
    }
}

#[derive(Clone, Copy)]
struct Palette {
    canvas: Color32,
    surface: Color32,
    text: Color32,
    muted: Color32,
    border: Color32,
    accent: Color32,
    accent_bg: Color32,
    warning: Color32,
    warning_bg: Color32,
    error: Color32,
    error_bg: Color32,
}

impl Palette {
    fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Dark => Self {
                canvas: Color32::from_rgb(0x14, 0x19, 0x1c),
                surface: Color32::from_rgb(0x1e, 0x25, 0x29),
                text: Color32::from_rgb(0xe3, 0xe9, 0xed),
                muted: Color32::from_rgb(0xa1, 0xaf, 0xb8),
                border: Color32::from_rgb(0x32, 0x3d, 0x44),
                accent: Color32::from_rgb(0xb8, 0xd3, 0xff),
                accent_bg: Color32::from_rgb(0x2b, 0x43, 0x64),
                warning: Color32::from_rgb(0xef, 0xcf, 0x9a),
                warning_bg: Color32::from_rgb(0x3b, 0x32, 0x23),
                error: Color32::from_rgb(0xff, 0xc9, 0xbf),
                error_bg: Color32::from_rgb(0x46, 0x2f, 0x2e),
            },
            Theme::Light => Self {
                canvas: Color32::from_rgb(0xf1, 0xf3, 0xf5),
                surface: Color32::WHITE,
                text: Color32::from_rgb(0x20, 0x2b, 0x33),
                muted: Color32::from_rgb(0x61, 0x71, 0x7e),
                border: Color32::from_rgb(0xdd, 0xe3, 0xe8),
                accent: Color32::from_rgb(0x24, 0x4f, 0x87),
                accent_bg: Color32::from_rgb(0xe4, 0xed, 0xfc),
                warning: Color32::from_rgb(0x7a, 0x52, 0x00),
                warning_bg: Color32::from_rgb(0xff, 0xf4, 0xdc),
                error: Color32::from_rgb(0x8b, 0x30, 0x29),
                error_bg: Color32::from_rgb(0xff, 0xea, 0xe6),
            },
        }
    }
}

fn install_style(ctx: &egui::Context) {
    ctx.set_theme(ThemePreference::System);
    for theme in [Theme::Dark, Theme::Light] {
        let palette = Palette::for_theme(theme);
        ctx.style_mut_of(theme, |style| {
            style.spacing.item_spacing = egui::vec2(8., 6.);
            style.spacing.button_padding = egui::vec2(10., 5.);
        });
        let mut visuals = theme.default_visuals();
        visuals.panel_fill = palette.canvas;
        visuals.window_fill = palette.surface;
        visuals.override_text_color = Some(palette.text);
        visuals.selection.bg_fill = palette.accent_bg;
        visuals.selection.stroke = Stroke::new(1., palette.accent);
        visuals.widgets.inactive.bg_fill = palette.surface;
        visuals.widgets.inactive.weak_bg_fill = palette.surface;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1., palette.border);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1., palette.text);
        visuals.widgets.hovered.bg_fill = palette.accent_bg;
        visuals.widgets.hovered.weak_bg_fill = palette.accent_bg;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1., palette.accent);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1., palette.accent);
        visuals.widgets.active.bg_fill = palette.accent_bg;
        visuals.widgets.active.bg_stroke = Stroke::new(1., palette.accent);
        visuals.widgets.active.fg_stroke = Stroke::new(1., palette.accent);
        ctx.set_visuals_of(theme, visuals);
    }
}

fn panel_frame(palette: Palette) -> EguiFrame {
    EguiFrame::new()
        .fill(palette.surface)
        .inner_margin(Margin::symmetric(14, 9))
        .stroke(Stroke::new(1., palette.border))
}

fn action_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    active: bool,
    palette: Palette,
    hint: &str,
) -> bool {
    tone_button(
        ui,
        label,
        enabled,
        active,
        palette,
        hint,
        ButtonTone::Normal,
    )
}

/// Destructive actions (overwrite save) keep their own colour so they never
/// read as the ordinary forward path.
fn danger_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    palette: Palette,
    hint: &str,
) -> bool {
    tone_button(ui, label, enabled, false, palette, hint, ButtonTone::Danger)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ButtonTone {
    Normal,
    Danger,
}

fn tone_button(
    ui: &mut egui::Ui,
    label: &str,
    enabled: bool,
    active: bool,
    palette: Palette,
    hint: &str,
    tone: ButtonTone,
) -> bool {
    // egui fades non-interactive widgets to `disabled_alpha` (0.5); starting
    // from full-strength text keeps a disabled button readable instead of faint.
    let (text_color, fill, stroke) = match tone {
        ButtonTone::Danger if enabled => (palette.error, palette.error_bg, palette.error),
        ButtonTone::Danger => (palette.text, palette.canvas, palette.border),
        ButtonTone::Normal if !enabled => (palette.text, palette.canvas, palette.border),
        ButtonTone::Normal if active => (palette.accent, palette.accent_bg, palette.accent),
        ButtonTone::Normal => (palette.text, palette.surface, palette.border),
    };
    let button = egui::Button::new(RichText::new(label).size(12.5).color(text_color))
        .fill(fill)
        .stroke(Stroke::new(1., stroke))
        .corner_radius(6.)
        .min_size(egui::vec2(34., 28.));
    // `on_hover_text` also sets the accessible description and covers the
    // enabled case; `on_disabled_hover_text` supplies the visible tooltip that
    // egui otherwise suppresses on a disabled widget.
    let response = ui
        .add_enabled(enabled, button)
        .on_hover_text(hint)
        .on_disabled_hover_text(hint);
    if enabled && !active && response.hovered() {
        ui.painter().rect_stroke(
            response.rect,
            6.,
            Stroke::new(
                1.,
                match tone {
                    ButtonTone::Danger => palette.error,
                    ButtonTone::Normal => palette.accent,
                },
            ),
            egui::StrokeKind::Inside,
        );
    }
    response.clicked()
}

/// One rounded shell holding the crop ratio presets, so six choices cost one
/// compact block instead of six separate buttons.
fn aspect_segments(
    ui: &mut egui::Ui,
    options: &[(CropAspect, &str)],
    selected: CropAspect,
    enabled: bool,
    palette: Palette,
    hint: &str,
) -> Option<CropAspect> {
    let mut picked = None;
    EguiFrame::new()
        .fill(palette.canvas)
        .stroke(Stroke::new(1., palette.border))
        .corner_radius(7.)
        .inner_margin(Margin::symmetric(2, 2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.;
            ui.spacing_mut().button_padding = egui::vec2(7., 2.);
            ui.horizontal(|ui| {
                for &(value, label) in options {
                    let active = value == selected;
                    let (color, fill) = if !enabled {
                        (palette.text, palette.canvas)
                    } else if active {
                        (palette.accent, palette.accent_bg)
                    } else {
                        (palette.muted, palette.canvas)
                    };
                    let button = egui::Button::new(RichText::new(label).size(11.5).color(color))
                        .fill(fill)
                        .stroke(Stroke::new(
                            1.,
                            if active {
                                palette.accent
                            } else {
                                Color32::TRANSPARENT
                            },
                        ))
                        .corner_radius(5.)
                        .min_size(egui::vec2(0., 22.));
                    let response = ui.add_enabled(enabled, button);
                    if enabled && response.hovered() && !active {
                        ui.painter().rect_stroke(
                            response.rect,
                            5.,
                            Stroke::new(1., palette.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if response.clicked() {
                        picked = Some(value);
                    }
                }
            });
        })
        .response
        .on_hover_text(hint);
    picked
}

fn rect_from(rect: [f64; 4]) -> Rect {
    Rect::from_min_max(
        Pos2::new(rect[0] as f32, rect[1] as f32),
        Pos2::new(rect[2] as f32, rect[3] as f32),
    )
}

fn fit_rect(source: Vec2, bounds: Rect) -> Rect {
    if source.x <= 0. || source.y <= 0. {
        return bounds;
    }
    let scale = (bounds.width() / source.x).min(bounds.height() / source.y);
    let size = source * scale;
    Rect::from_center_size(bounds.center(), size)
}

/// Same colour with an explicit alpha, for overlays that must not hide pixels.
fn translucent(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsAction {
    Restore,
    Cancel,
    Apply,
}

fn truncated_label(
    ui: &mut egui::Ui,
    text: impl Into<String>,
    size: f32,
    color: Color32,
    reserve: f32,
) {
    let width = (ui.available_width() - reserve).max(40.);
    ui.allocate_ui_with_layout(
        egui::vec2(width, 20.),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add(egui::Label::new(RichText::new(text).size(size).color(color)).truncate());
        },
    );
}

#[derive(Default)]
struct Textures {
    tiles: HashMap<(usize, usize), TextureHandle>,
}

impl Textures {
    fn texture(
        &mut self,
        ctx: &egui::Context,
        frame: &Arc<TiledFrame>,
        index: usize,
        frame_ptr: usize,
    ) -> egui::TextureId {
        let key = (frame_ptr, index);
        if let Some(handle) = self.tiles.get(&key) {
            return handle.id();
        }
        let tile = &frame.tiles[index];
        let width = tile.geometry.sample.width as usize;
        let height = tile.geometry.sample.height as usize;
        let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &tile.pixels);
        let handle = ctx.load_texture(
            format!("oxeye-tile-{frame_ptr}-{index}"),
            image,
            TextureOptions::LINEAR,
        );
        let id = handle.id();
        self.tiles.insert(key, handle);
        id
    }

    /// Keeps only textures painted this frame, releasing off-screen tiles.
    fn retain(&mut self, keys: &[(usize, usize)]) {
        self.tiles
            .retain(|key, _| keys.iter().any(|painted| painted == key));
    }
}

#[derive(Default)]
struct SaveQueue {
    running: bool,
    pending: Option<(u64, AppSettings)>,
}

impl SaveQueue {
    fn request(&mut self, revision: u64, settings: AppSettings) -> bool {
        self.pending = Some((revision, settings));
        let start = !self.running;
        self.running = true;
        start
    }

    fn next(&mut self) -> Option<(u64, AppSettings)> {
        let next = self.pending.take();
        if next.is_none() {
            self.running = false;
        }
        next
    }
}

struct SaveDone {
    revision: u64,
    result: Result<(), String>,
}

fn drain_saves(
    saves: &Mutex<SaveQueue>,
    mut write: impl FnMut(&AppSettings) -> Result<(), String>,
) -> Option<(u64, Result<(), String>)> {
    let mut result = None;
    loop {
        let next = saves.lock().unwrap().next();
        let Some((revision, settings)) = next else {
            return result;
        };
        result = Some((revision, write(&settings)));
    }
}

fn save_settings_in_background(
    ctx: egui::Context,
    saves: Arc<Mutex<SaveQueue>>,
    sender: Sender<SaveDone>,
) {
    std::thread::spawn(move || {
        if let Some((revision, result)) = drain_saves(&saves, settings::save_settings) {
            let _ = sender.send(SaveDone { revision, result });
            ctx.request_repaint();
        }
    });
}

#[derive(Default)]
struct CloseState {
    completed_save: u64,
    requested: bool,
    failed: Option<u64>,
}

impl CloseState {
    /// A request after a failed latest save is the explicit acknowledgement
    /// that allows exiting without saving.
    fn request(&mut self, latest_save: u64) -> bool {
        if self.failed == Some(latest_save) {
            self.failed = None;
            self.completed_save = latest_save;
            self.requested = false;
            return true;
        }
        self.requested = self.completed_save != latest_save;
        !self.requested
    }

    fn saved(&mut self, revision: u64, latest_save: u64, success: bool) -> bool {
        if revision != latest_save {
            return false;
        }
        let close = self.requested && success;
        self.requested = false;
        if success {
            self.completed_save = revision;
            self.failed = None;
        } else {
            self.failed = Some(revision);
        }
        close
    }
}

struct ThumbTexture {
    source: Arc<RgbaImage>,
    image: TextureHandle,
}

#[derive(Default)]
struct ThumbTextures {
    map: HashMap<SourceRef, ThumbTexture>,
}

impl ThumbTextures {
    fn retain(&mut self, keep: &[SourceRef]) {
        self.map.retain(|path, _| keep.contains(path));
    }

    /// The service returns a thumbnail-sized Arc; a missing current-epoch value
    /// removes the old texture so a stale picture is never shown.
    fn update(&mut self, ctx: &egui::Context, path: &SourceRef, source: Option<Arc<RgbaImage>>) {
        let Some(source) = source else {
            self.map.remove(path);
            return;
        };
        if self
            .map
            .get(path)
            .is_some_and(|texture| Arc::ptr_eq(&texture.source, &source))
        {
            return;
        }
        let size = [source.width() as usize, source.height() as usize];
        let image = egui::ColorImage::from_rgba_unmultiplied(size, source.as_raw());
        let texture = ctx.load_texture(
            format!("oxeye-thumbnail-{}", path.display()),
            image,
            TextureOptions::LINEAR,
        );
        self.map.insert(
            path.clone(),
            ThumbTexture {
                source,
                image: texture,
            },
        );
    }

    fn texture(&self, path: &SourceRef) -> Option<&TextureHandle> {
        self.map.get(path).map(|texture| &texture.image)
    }

    fn clear(&mut self) {
        self.map.clear();
    }
}

struct Oxeye {
    initial: Option<PathBuf>,
    settings: AppSettings,
    progress: ProgressStore,
    progress_dirty: bool,
    progress_due: Option<Instant>,
    page_dimensions: HashMap<usize, (u32, u32)>,
    page_overrides: BTreeMap<usize, PageOverride>,
    layout_retry: Option<(SourceRef, Vec<usize>)>,
    prefetch: Prefetcher,
    loader: Loader,
    worker_tx: Sender<Completion>,
    worker_rx: Receiver<Completion>,
    navigation: NavigationFlow,
    navigation_paths: Vec<SourceRef>,
    nav: Option<NavigationPaint>,
    selected: Option<SourceRef>,
    displayed: Option<SourceRef>,
    current_file: Option<SourceRef>,
    siblings: Vec<SourceRef>,
    media: Option<Arc<model::LoadedMedia>>,
    tiled: Option<Arc<TiledFrame>>,
    other_pages: Vec<PreparedPage>,
    display_mode: ReadingMode,
    candidate: Option<Candidate>,
    dimensions: (u32, u32),
    frame: usize,
    playback: Option<Playback>,
    spread_playback: Option<SpreadPlayback>,
    spread_generation: u64,
    book_rotation_resume: bool,
    animation_job: bool,
    epoch: Instant,
    rotation: Rotation,
    edit_ui: Option<EditUi>,
    edit_pending: Option<PendingEdit>,
    close_after_save: bool,
    loading_file: bool,
    error: Option<String>,
    zoom: Zoom,
    pan: [f32; 2],
    textures: Textures,
    close_state: CloseState,
    save_revision: u64,
    saves: Arc<Mutex<SaveQueue>>,
    save_tx: Sender<SaveDone>,
    save_rx: Receiver<SaveDone>,
    close_pending_notice: bool,
    feedback: Feedback,
    feedback_deadline: Option<(u64, Instant)>,
    settings_draft: Option<AppSettings>,
    settings_control: usize,
    language: Language,
    thumbnails: Thumbnails,
    thumb_generation: u64,
    thumb_requests: Vec<SourceRef>,
    thumb_visible: Vec<SourceRef>,
    thumb_textures: ThumbTextures,
    thumb_offset: f32,
    thumb_width: f32,
    thumb_revision: u64,
    thumb_stable: u8,
    thumb_poll_due: Option<Instant>,
    thumb_allowed: bool,
    fullscreen: bool,
    viewport: Rect,
    cache_stats: CacheStats,
}

impl Oxeye {
    fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        install_style(&cc.egui_ctx);
        let loaded = settings::load_settings();
        Self::from_settings(initial, loaded.settings, loaded.warning)
    }

    fn from_settings(
        initial: Option<PathBuf>,
        settings: AppSettings,
        warning: Option<String>,
    ) -> Self {
        Self::from_parts(initial, settings, warning, ProgressStore::load())
    }

    fn from_parts(
        initial: Option<PathBuf>,
        settings: AppSettings,
        warning: Option<String>,
        progress: ProgressStore,
    ) -> Self {
        let display_mode = settings.reading_mode;
        let language = resolve_language(settings.language);
        let prefetch = Prefetcher::new(settings.prefetch.clone());
        prefetch.set_purpose(DecodePurpose::Animated);
        let thumbnails = Thumbnails::new(prefetch.clone());
        let mut feedback = Feedback::default();
        let text = ui_text::catalog(language);
        let mut warnings = Vec::new();
        if let Some(warning) = warning {
            warnings.push(text.settings_warning(&warning));
        }
        if let Some(warning) = progress.warning.as_deref() {
            warnings.push(text.progress_warning(warning));
        }
        if !warnings.is_empty() {
            feedback.replace(FeedbackKind::Error, warnings.join("\n"));
        }
        let (worker_tx, worker_rx) = mpsc::channel();
        let (save_tx, save_rx) = mpsc::channel();
        Self {
            initial,
            settings,
            progress,
            progress_dirty: false,
            progress_due: None,
            page_dimensions: HashMap::new(),
            page_overrides: BTreeMap::new(),
            layout_retry: None,
            prefetch,
            loader: Loader::default(),
            worker_tx,
            worker_rx,
            navigation: NavigationFlow::default(),
            navigation_paths: Vec::new(),
            nav: None,
            selected: None,
            displayed: None,
            current_file: None,
            siblings: Vec::new(),
            media: None,
            tiled: None,
            other_pages: Vec::new(),
            display_mode,
            candidate: None,
            dimensions: (0, 0),
            frame: 0,
            playback: None,
            spread_playback: None,
            spread_generation: 0,
            book_rotation_resume: false,
            animation_job: false,
            epoch: Instant::now(),
            rotation: Rotation::default(),
            edit_ui: None,
            edit_pending: None,
            close_after_save: false,
            loading_file: false,
            error: None,
            zoom: Zoom::Fit,
            pan: [0., 0.],
            textures: Textures::default(),
            close_state: CloseState::default(),
            save_revision: 0,
            saves: Arc::new(Mutex::new(SaveQueue::default())),
            save_tx,
            save_rx,
            close_pending_notice: false,
            feedback,
            feedback_deadline: None,
            settings_draft: None,
            settings_control: 0,
            language,
            thumbnails,
            thumb_generation: 0,
            thumb_requests: Vec::new(),
            thumb_visible: Vec::new(),
            thumb_textures: ThumbTextures::default(),
            thumb_offset: 0.,
            thumb_width: 0.,
            thumb_revision: 0,
            thumb_stable: 0,
            thumb_poll_due: None,
            thumb_allowed: false,
            fullscreen: false,
            viewport: Rect::NOTHING,
            cache_stats: CacheStats::default(),
        }
    }

    fn palette(&self, ctx: &egui::Context) -> Palette {
        Palette::for_theme(ctx.theme())
    }

    fn text(&self) -> &'static ui_text::Catalog {
        ui_text::catalog(self.language)
    }

    fn tile_cap(&self, ctx: &egui::Context) -> u32 {
        let max = ctx.input(|input| input.max_texture_side as u32);
        viewer::tile_cap(max)
    }

    fn busy(&self) -> bool {
        self.loader.running || self.loader.pending.is_some() || self.candidate.is_some()
    }

    fn set_feedback(&mut self, kind: FeedbackKind, text: String, ctx: &egui::Context) {
        self.feedback_deadline = None;
        let generation = self.feedback.replace(kind, text);
        if kind == FeedbackKind::Success {
            let deadline = Instant::now() + Duration::from_secs(2);
            self.feedback_deadline = Some((generation, deadline));
            ctx.request_repaint_after(Duration::from_secs(2));
        }
        ctx.request_repaint();
    }

    fn dismiss_feedback(&mut self, ctx: &egui::Context) {
        self.feedback.dismiss();
        self.feedback_deadline = None;
        ctx.request_repaint();
    }

    fn expire_feedback(&mut self, ctx: &egui::Context) {
        let Some((generation, deadline)) = self.feedback_deadline else {
            return;
        };
        let now = Instant::now();
        if now >= deadline {
            self.feedback.expire(generation);
            self.feedback_deadline = None;
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(deadline - now);
        }
    }

    /// Committed page positions are debounced; closing forces the write.
    /// A failed save stays dirty for the next commit or close attempt.
    fn flush_progress(&mut self, ctx: &egui::Context, force: bool) -> Result<(), String> {
        if !self.progress_dirty {
            return Ok(());
        }
        if !force {
            let Some(due) = self.progress_due else {
                return Ok(());
            };
            let now = Instant::now();
            if now < due {
                ctx.request_repaint_after(due - now);
                return Ok(());
            }
        }
        self.progress_due = None;
        self.progress.save()?;
        self.progress_dirty = false;
        Ok(())
    }

    fn record_progress(&mut self, ctx: &egui::Context) {
        // An unusable store was already reported at startup; keep retrying
        // would replace that warning with one error per page.
        if self.progress.warning.is_some() {
            return;
        }
        let Some(source) = self.selected.clone() else {
            return;
        };
        if self.progress.record(&source).is_err() {
            return;
        }
        self.schedule_progress_save(ctx);
    }

    fn schedule_progress_save(&mut self, ctx: &egui::Context) {
        self.progress_dirty = true;
        self.progress_due = Some(Instant::now() + PROGRESS_SAVE_DELAY);
        ctx.request_repaint_after(PROGRESS_SAVE_DELAY);
    }

    /// A close-time progress failure is reported but never blocks the exit.
    fn flush_progress_for_close(&mut self, ctx: &egui::Context) -> bool {
        match self.flush_progress(ctx, true) {
            Ok(()) => false,
            Err(error) => {
                self.set_feedback(
                    FeedbackKind::Error,
                    self.text().progress_save_failed(&error),
                    ctx,
                );
                true
            }
        }
    }

    fn persist_settings(&mut self, ctx: &egui::Context, message: &str) {
        self.save_revision += 1;
        let start = self
            .saves
            .lock()
            .unwrap()
            .request(self.save_revision, self.settings.clone());
        if start {
            save_settings_in_background(ctx.clone(), self.saves.clone(), self.save_tx.clone());
        }
        self.set_feedback(FeedbackKind::Progress, message.to_owned(), ctx);
    }

    fn poll_saves(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.save_rx.try_recv() {
            if done.revision != self.save_revision {
                continue;
            }
            let closing = self.close_state.requested;
            let close =
                self.close_state
                    .saved(done.revision, self.save_revision, done.result.is_ok());
            self.close_pending_notice = false;
            let text = self.text();
            let (kind, message) = match done.result {
                Ok(()) => (FeedbackKind::Success, text.settings_saved.to_owned()),
                Err(error) => (FeedbackKind::Error, text.save_failed(&error, closing)),
            };
            self.set_feedback(kind, message, ctx);
            self.sync_prefetch(ctx);
            if close {
                // A page may have committed while the settings save ran.
                self.flush_progress_for_close(ctx);
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn open_settings(&mut self, ctx: &egui::Context) {
        if self.settings_draft.is_some()
            || self.close_state.requested
            || self.editing()
            || self.edit_pending.is_some()
        {
            return;
        }
        self.cancel_navigation();
        self.settings_draft = Some(self.settings.clone());
        self.settings_control = SETTING_BEFORE;
        ctx.request_repaint();
    }

    fn close_settings(&mut self, ctx: &egui::Context) {
        self.settings_draft = None;
        ctx.request_repaint();
    }

    fn restore_settings(&mut self) {
        if let Some(draft) = &mut self.settings_draft {
            restore_defaults(draft);
            self.settings_control = SETTING_RESTORE;
        }
    }

    fn step_setting(&mut self, control: usize, increase: bool) {
        let Some(draft) = &mut self.settings_draft else {
            return;
        };
        self.settings_control = control;
        step_draft_setting(control, increase, draft);
    }

    fn activate_setting(&mut self, ctx: &egui::Context) {
        match self.settings_control {
            SETTING_PREVIEWS | SETTING_READING | SETTING_AUTO_WIDE | SETTING_ARROW
            | SETTING_LANGUAGE => self.step_setting(self.settings_control, true),
            SETTING_RESTORE => self.restore_settings(),
            SETTING_CANCEL => self.close_settings(ctx),
            SETTING_APPLY => self.apply_settings(ctx),
            _ => {}
        }
    }

    fn transition_to_mode(&mut self, mode: ReadingMode, ctx: &egui::Context) -> bool {
        if self.close_state.requested {
            return false;
        }
        let path = mode_target(&self.current_file, &self.selected, &self.displayed);
        self.settings.reading_mode = mode;
        self.layout_retry = None;
        self.cancel_navigation();
        // Controllers stay paused until a successful commit replaces them.
        self.pause_animation();
        self.spread_generation = self.spread_generation.wrapping_add(1);
        self.book_rotation_resume = false;
        self.animation_job = false;
        self.candidate = None;
        self.nav = None;
        self.rotation.cancel();
        self.prefetch.set_allowed(false);
        self.prefetch.set_purpose(DecodePurpose::Animated);
        self.loader.cancel();
        if let Some(path) = path {
            self.load(ctx, path, false);
        }
        self.sync_prefetch(ctx);
        ctx.request_repaint();
        true
    }

    fn toggle_reading_mode(&mut self, ctx: &egui::Context) {
        if self.edit_pending.is_some() {
            self.cancel_pending_edit(ctx);
        }
        if self.settings_draft.is_some() || self.close_state.requested || self.editing() {
            return;
        }
        let mode = toggled_reading_mode(self.settings.reading_mode);
        if mode == self.settings.reading_mode {
            return;
        }
        let target = mode_target(&self.current_file, &self.selected, &self.displayed);
        if let Some(path) = &target
            && !self.siblings.is_empty()
            && source_index(Some(path), &self.siblings).is_none()
        {
            return;
        }
        if self.transition_to_mode(mode, ctx) {
            self.persist_settings(ctx, self.text().reading_mode_saving);
        }
    }

    fn apply_settings(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.settings_draft.take() else {
            return;
        };
        if let Err(error) = draft.validate() {
            self.settings_draft = Some(draft);
            self.set_feedback(FeedbackKind::Error, error, ctx);
            return;
        }
        let mode_changed = reading_mode_changed(&self.settings, &draft);
        let auto_wide_changed = self.settings.auto_wide != draft.auto_wide;
        let was_visible = self.settings.thumbnails_visible;
        let language = resolve_language(draft.language);
        self.settings = draft;
        self.language = language;
        self.prefetch.configure(self.settings.prefetch.clone());
        if self.settings.thumbnails_visible && !was_visible {
            self.reveal_thumbnails();
            self.wake_thumbnails(ctx);
        }
        if mode_changed || (auto_wide_changed && self.settings.reading_mode == ReadingMode::Book) {
            let _ = self.transition_to_mode(self.settings.reading_mode, ctx);
        } else {
            self.sync_prefetch(ctx);
        }
        self.persist_settings(ctx, self.text().applying_saving);
    }

    fn toggle_wide_page(&mut self, ctx: &egui::Context) {
        if self.edit_pending.is_some() {
            self.cancel_pending_edit(ctx);
        }
        if self.settings_draft.is_some() || self.close_state.requested || self.editing() {
            return;
        }
        let Some(position) = self.position() else {
            return;
        };
        let source = self.siblings[position].clone();
        if self.reject_changed_archive(&source, ctx) {
            return;
        }
        let mode = match self
            .page_overrides
            .get(&position)
            .copied()
            .unwrap_or_default()
        {
            PageOverride::Auto => PageOverride::Single,
            PageOverride::Single => PageOverride::Spread,
            PageOverride::Spread => PageOverride::Auto,
        };
        if mode == PageOverride::Auto {
            self.page_overrides.remove(&position);
        } else {
            self.page_overrides.insert(position, mode);
        }
        let member = match &source {
            SourceRef::Archive { index, .. } => *index,
            SourceRef::File(_) => position,
        };
        if self.progress.warning.is_none()
            && self.progress.set_override(&source, member, mode).is_ok()
        {
            self.schedule_progress_save(ctx);
        }
        self.layout_retry = None;
        self.cancel_navigation();
        if self.settings.reading_mode == ReadingMode::Book {
            let _ = self.transition_to_mode(ReadingMode::Book, ctx);
        } else {
            self.sync_prefetch(ctx);
            ctx.request_repaint();
        }
    }

    fn toggle_previews(&mut self, ctx: &egui::Context) {
        if self.editing() || self.edit_pending.is_some() {
            return;
        }
        self.settings.thumbnails_visible = !self.settings.thumbnails_visible;
        if self.settings.thumbnails_visible {
            self.reveal_thumbnails();
            self.wake_thumbnails(ctx);
        }
        self.sync_prefetch(ctx);
        self.persist_settings(
            ctx,
            if self.settings.thumbnails_visible {
                self.text().previews_showing
            } else {
                self.text().previews_hiding
            },
        );
    }

    fn handle_settings_keys(&mut self, ctx: &egui::Context) {
        if self.settings_draft.is_none() {
            return;
        }
        let events = ctx.input(|input| input.events.clone());
        for event in events {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
                && let Some(action) = settings_key_action(key, modifiers)
            {
                match action {
                    SettingsKey::Next => {
                        self.settings_control = next_settings_control(self.settings_control, false)
                    }
                    SettingsKey::Previous => {
                        self.settings_control = next_settings_control(self.settings_control, true)
                    }
                    SettingsKey::Decrease => self.step_setting(self.settings_control, false),
                    SettingsKey::Increase => self.step_setting(self.settings_control, true),
                    SettingsKey::Activate => self.activate_setting(ctx),
                    SettingsKey::Cancel => self.close_settings(ctx),
                    SettingsKey::CloseWindow => self.request_close(ctx),
                }
            }
        }
        // The modal owns the keyboard; viewer shortcuts must not see these events.
        ctx.input_mut(|input| {
            input.events.retain(|event| {
                !matches!(
                    event,
                    egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::Copy
                )
            })
        });
        ctx.request_repaint();
    }

    fn wake_thumbnails(&mut self, ctx: &egui::Context) {
        if !self.settings.thumbnails_visible || !self.background_allowed() {
            return;
        }
        self.thumb_stable = 0;
        self.thumb_poll_due = Some(Instant::now());
        ctx.request_repaint();
    }

    fn reveal_thumbnails(&mut self) {
        if !self.settings.thumbnails_visible || self.thumb_width <= 0. {
            return;
        }
        let mut indices: Vec<_> = self
            .displayed_paths()
            .iter()
            .filter_map(|path| self.siblings.iter().position(|item| item == path))
            .collect();
        if indices.is_empty()
            && let Some(index) = self
                .selected
                .as_ref()
                .and_then(|path| self.siblings.iter().position(|item| item == path))
        {
            indices.push(index);
        }
        let (Some(first), Some(last)) = (indices.iter().min(), indices.iter().max()) else {
            return;
        };
        self.thumb_offset = reveal_offset(
            *first,
            *last,
            self.siblings.len(),
            self.thumb_width,
            self.thumb_offset,
        );
    }

    fn set_thumb_offset(&mut self, offset: f32, ctx: &egui::Context) {
        let clamped = clamp_offset(offset, self.siblings.len(), self.thumb_width);
        if (clamped - self.thumb_offset).abs() > 0.01 {
            self.thumb_offset = clamped;
            self.wake_thumbnails(ctx);
        }
    }

    fn sync_thumbnails(&mut self, ctx: &egui::Context) {
        if !self.settings.thumbnails_visible {
            self.thumb_allowed = false;
            self.thumbnails.set_allowed(false);
            self.thumb_poll_due = None;
            self.thumb_visible.clear();
            self.thumb_textures.clear();
            if !self.thumb_requests.is_empty() {
                self.thumb_generation += 1;
                self.thumb_requests.clear();
                self.thumbnails
                    .set_visible_sources(Vec::new(), self.thumb_generation);
            }
            return;
        }
        let max_offset = (self.siblings.len() as f32 * CELL_WIDTH - self.thumb_width).max(0.);
        self.thumb_offset = self.thumb_offset.clamp(0., max_offset);
        let range = visible_range(self.siblings.len(), self.thumb_offset, self.thumb_width);
        let requests = request_range(range.clone(), self.siblings.len());
        let paths = self.siblings[requests].to_vec();
        if paths != self.thumb_requests {
            self.thumb_generation += 1;
            self.thumb_requests = paths;
            self.thumb_revision = self.thumbnails.revision();
            self.thumb_stable = 0;
            self.thumbnails
                .set_visible_sources(self.thumb_requests.clone(), self.thumb_generation);
            self.wake_thumbnails(ctx);
        }
        let allowed = self.background_allowed();
        self.thumbnails.set_allowed(allowed);
        if allowed != self.thumb_allowed {
            self.thumb_allowed = allowed;
            if allowed {
                self.wake_thumbnails(ctx);
            } else {
                self.thumb_poll_due = None;
            }
        }
        self.thumb_visible = self.siblings[range].to_vec();
        self.thumb_textures.retain(&self.thumb_requests);
        for path in self.thumb_requests.clone() {
            let source = self.thumbnails.get_source(&path);
            self.thumb_textures.update(ctx, &path, source);
        }
    }

    fn poll_thumbnails(&mut self, ctx: &egui::Context) {
        if !self.settings.thumbnails_visible || !self.background_allowed() {
            self.thumb_poll_due = None;
            return;
        }
        let now = Instant::now();
        if let Some(due) = self.thumb_poll_due {
            if now < due {
                ctx.request_repaint_after(due - now);
                return;
            }
        } else {
            return;
        }
        let revision = self.thumbnails.revision();
        if revision != self.thumb_revision {
            self.thumb_revision = revision;
            self.thumb_stable = 0;
            self.sync_thumbnails(ctx);
        } else {
            self.thumb_stable = self.thumb_stable.saturating_add(1);
        }
        let missing = self
            .thumb_visible
            .iter()
            .any(|path| self.thumb_textures.texture(path).is_none());
        if let Some(delay) = thumbnail_poll_delay(self.thumb_stable, missing) {
            self.thumb_poll_due = Some(now + delay);
            ctx.request_repaint_after(delay);
        } else {
            self.thumb_poll_due = None;
        }
    }

    fn cancel_navigation(&mut self) {
        reset_navigation(&mut self.navigation, &mut self.navigation_paths);
        self.nav = None;
    }

    fn enqueue_navigation(&mut self, action: Navigation, ctx: &egui::Context) {
        if self.edit_pending.is_some() {
            self.cancel_pending_edit(ctx);
        }
        if self.close_state.requested || self.settings_draft.is_some() || self.editing() {
            return;
        }
        if self.navigation.is_idle() {
            self.navigation_paths = self.siblings.clone();
        }
        let Some(base) = source_index(self.selected.as_ref(), &self.navigation_paths) else {
            return;
        };
        let length = self.navigation_paths.len();
        let accepted = if self.settings.reading_mode == ReadingMode::Single {
            self.navigation
                .enqueue(action, base, length, ReadingMode::Single)
        } else {
            let layout = self.layout_for(length);
            self.navigation.enqueue_with(action, base, layout.as_ref())
        };
        if accepted {
            self.pause_animation();
            self.start_navigation(ctx);
            self.sync_prefetch(ctx);
            ctx.request_repaint();
        }
    }

    fn start_navigation(&mut self, ctx: &egui::Context) {
        if (self.busy() && !self.animation_job)
            || self.close_state.requested
            || self.settings_draft.is_some()
        {
            return;
        }
        while let Some(ticket) = self.navigation.start_next(self.loader.revision + 1) {
            let path = {
                let layout = self.layout_for(self.navigation_paths.len());
                layout.anchor(ticket.index)
            }
            .and_then(|index| self.navigation_paths.get(index))
            .cloned();
            let Some(path) = path else {
                self.navigation.failed(self.loader.revision + 1);
                continue;
            };
            let revision = self.loader.revision + 1;
            self.load(ctx, path, true);
            if self.loader.revision == revision {
                return;
            }
            self.navigation.failed(revision);
        }
    }

    fn navigation_painted(&mut self, revision: u64, ctx: &egui::Context) {
        if let Some(ticket) = self.navigation.painted(revision) {
            self.nav = Some(NavigationPaint {
                ticket,
                shown_frame: None,
            });
        } else {
            self.start_navigation(ctx);
        }
    }

    fn update_prefetch_context(&self) {
        self.prefetch.update_context_many_sources(
            self.siblings.clone(),
            self.selected.clone(),
            self.displayed_media(),
        );
    }

    fn displayed_media(&self) -> Vec<Arc<model::LoadedMedia>> {
        self.media
            .iter()
            .cloned()
            .chain(self.other_pages.iter().map(|page| page.media.clone()))
            .collect()
    }

    fn displayed_paths(&self) -> Vec<SourceRef> {
        self.displayed_media()
            .iter()
            .map(|media| media.source.clone())
            .collect()
    }

    fn layout_for(&self, page_count: usize) -> Box<dyn PageLayout> {
        if self.settings.reading_mode == ReadingMode::Book {
            Box::new(comic_layout(
                page_count,
                &self.page_dimensions,
                &self.page_overrides,
                self.settings.auto_wide,
            ))
        } else {
            Box::new(FixedLayout {
                length: page_count,
                mode: ReadingMode::Single,
            })
        }
    }

    fn layout(&self) -> Box<dyn PageLayout> {
        self.layout_for(self.siblings.len())
    }

    /// Dimensions of the pages that were just committed, in display order.
    fn record_page_dimensions(&mut self) {
        // Preview pixels are not the disk truth; a page write would poison Book layout.
        if self.edit_ui.is_some() {
            return;
        }
        for media in self.displayed_media() {
            let Some(frame) = media.frames.first() else {
                continue;
            };
            if let Some(index) = source_index(Some(&media.source), &self.siblings) {
                self.page_dimensions
                    .insert(index, frame.pixels.dimensions());
            }
        }
    }

    /// Cache-only hints around the current page; never probes or decodes.
    fn fill_cached_dimensions(&mut self) {
        let Some(current) = self.position() else {
            return;
        };
        let start = current.saturating_sub(2);
        let end = (current + 3).min(self.siblings.len());
        for index in start..end {
            if self.page_dimensions.contains_key(&index) {
                continue;
            }
            if let Some(dimensions) = self.prefetch.cached_dimensions(&self.siblings[index]) {
                self.page_dimensions.insert(index, dimensions);
            }
        }
    }

    /// The requested page's spread under the freshly learned dimensions, when
    /// it differs from the spread that was just committed.
    fn requested_layout_change(&self, layout: &ComicLayout) -> Option<(SourceRef, Vec<usize>)> {
        let requested = self.current_file.clone()?;
        let index = source_index(Some(&requested), &self.siblings)?;
        debug_assert!(
            layout.spread_count() >= 1,
            "an existing catalogue item has at least one spread"
        );
        let pages = layout.pages(index);
        let desired: Vec<_> = pages
            .iter()
            .filter_map(|index| self.siblings.get(*index).cloned())
            .collect();
        (!desired.is_empty() && desired != self.displayed_paths()).then_some((requested, pages))
    }

    fn displayed_tiles(&self) -> Vec<Arc<TiledFrame>> {
        self.tiled
            .iter()
            .cloned()
            .chain(self.other_pages.iter().map(|page| page.tiled.clone()))
            .collect()
    }

    fn book_frames(&self) -> Vec<usize> {
        std::iter::once(self.frame)
            .chain(self.other_pages.iter().map(|page| page.frame))
            .collect()
    }

    fn has_animation(&self) -> bool {
        let media = self.displayed_media();
        animation_available(
            self.display_mode,
            self.spread_playback.is_some(),
            media.iter().map(|media| media.frames.len()),
        )
    }

    fn all_finished(&self) -> bool {
        if self.display_mode == ReadingMode::Book {
            self.spread_playback
                .as_ref()
                .is_some_and(|spread| spread.all_finished(self.epoch.elapsed()))
        } else {
            self.playback
                .as_ref()
                .is_some_and(|clock| clock.position(self.epoch.elapsed()).finished)
        }
    }

    fn playing(&self) -> bool {
        let now = self.epoch.elapsed();
        if self.display_mode == ReadingMode::Book {
            spread_any_playing(&self.spread_playback, now)
        } else {
            self.playback
                .as_ref()
                .is_some_and(|clock| clock.playing(now))
        }
    }

    fn background_allowed(&self) -> bool {
        !self.close_state.requested
            && self.navigation.is_idle()
            && !self.busy()
            && !self.loading_file
            && !self.playing()
    }

    fn sync_prefetch(&mut self, ctx: &egui::Context) {
        let allowed = self.background_allowed();
        self.prefetch.set_allowed(allowed);
        self.update_prefetch_context();
        self.sync_thumbnails(ctx);
        let stats = self.prefetch.stats();
        let changed = stats != self.cache_stats;
        self.cache_stats = stats;
        let prefetch = &self.settings.prefetch;
        let active = allowed
            && self.selected.is_some()
            && prefetch.memory_mib > 0
            && (prefetch.before > 0 || prefetch.after > 0);
        if active && (stats.loading || changed) {
            ctx.request_repaint_after(Duration::from_millis(400));
        }
    }

    fn open_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.close_state.requested {
            return;
        }
        if self.edit_pending.is_some() {
            self.cancel_pending_edit(ctx);
        }
        self.cancel_navigation();
        self.loader.cancel();
        self.candidate = None;
        self.loading_file = false;
        self.pause_animation();
        self.prefetch.set_allowed(false);
        self.thumb_offset = 0.;
        self.page_dimensions.clear();
        self.page_overrides.clear();
        self.layout_retry = None;
        let result = catalog_for(&path)
            .and_then(|catalog| {
                let file = SourceRef::file(&path);
                let items = catalog.items;
                // Only archives carry a fingerprint; a folder keeps following
                // the file that was actually opened.
                let restored = items
                    .first()
                    .filter(|source| source.is_archive())
                    .and_then(|source| self.progress.restore(source))
                    .and_then(|progress| {
                        items
                            .iter()
                            .find(|source| match source {
                                SourceRef::Archive { index, name, .. } => {
                                    *index == progress.entry_index && *name == progress.entry_name
                                }
                                SourceRef::File(_) => false,
                            })
                            .cloned()
                    });
                let selected = restored
                    .or_else(|| items.iter().find(|source| **source == file).cloned())
                    .or_else(|| items.first().cloned())
                    .ok_or_else(|| self.text().no_images.to_owned())?;
                Ok((selected, items))
            })
            .or_else(|error| {
                if model::supported_path(&path) {
                    let selected = SourceRef::file(&path);
                    Ok((selected.clone(), vec![selected]))
                } else {
                    Err(error)
                }
            });
        match result {
            Ok((selected, siblings)) => {
                self.siblings = siblings;
                if selected.is_archive() {
                    let overrides = self.progress.overrides_for(&selected);
                    self.page_overrides = translate_overrides(overrides, &self.siblings);
                } else {
                    self.page_overrides = self.progress.overrides_for(&selected);
                }
                self.load(ctx, selected, false);
            }
            Err(error) => {
                self.siblings.clear();
                self.selected = None;
                self.current_file = None;
                self.error = Some(self.text().open_failed(&path.display().to_string(), &error));
                self.sync_prefetch(ctx);
                ctx.request_repaint();
            }
        }
    }

    fn reject_changed_archive(&mut self, source: &SourceRef, ctx: &egui::Context) -> bool {
        if !source.is_archive() || source_identity(source).is_some() {
            return false;
        }
        self.cancel_navigation();
        self.loader.cancel();
        self.candidate = None;
        self.loading_file = false;
        self.page_dimensions.clear();
        self.page_overrides.clear();
        self.layout_retry = None;
        self.pause_animation();
        self.error = Some(
            self.text()
                .open_failed(&source_label(source), self.text().archive_changed),
        );
        self.sync_prefetch(ctx);
        ctx.request_repaint();
        true
    }

    fn load(&mut self, ctx: &egui::Context, path: SourceRef, queued: bool) {
        self.load_with_turns(ctx, path, queued, 0);
    }

    /// `turns` restores an edit-exit rotation without changing normal browsing.
    fn load_with_turns(&mut self, ctx: &egui::Context, path: SourceRef, queued: bool, turns: u8) {
        if self.close_state.requested || self.reject_changed_archive(&path, ctx) {
            return;
        }
        if !queued {
            self.cancel_navigation();
        }
        let (anchor_source, spread) = match source_index(Some(&path), &self.siblings) {
            Some(index) => {
                let layout = self.layout();
                let anchor = layout.anchor(index).unwrap_or(index);
                let mut spread = layout.pages(anchor);
                if spread.is_empty() {
                    spread.push(0);
                }
                let anchor_source = self
                    .siblings
                    .get(anchor)
                    .cloned()
                    .unwrap_or_else(|| path.clone());
                (anchor_source, spread)
            }
            None => (path.clone(), vec![0]),
        };
        let foreground = self.prefetch.foreground_requested_source(&anchor_source);
        self.prefetch.set_allowed(false);
        self.rotation.cancel();
        // Keep the old clocks paused, not dropped: a failed load must remain recoverable.
        self.pause_animation();
        self.spread_generation = self.spread_generation.wrapping_add(1);
        self.book_rotation_resume = false;
        self.animation_job = false;
        self.candidate = None;
        self.nav = None;
        self.selected = Some(anchor_source.clone());
        // Keep the explicitly requested page for mode switches; only the
        // selection and foreground token normalize to the spread anchor.
        self.current_file = Some(path);
        self.update_prefetch_context();
        self.loading_file = true;
        if !queued {
            self.error = None;
        }
        self.loader.request(Work::Load(
            anchor_source,
            self.prefetch.clone(),
            foreground,
            self.tile_cap(ctx),
            self.settings.reading_mode,
            spread,
            self.language,
            turns,
        ));
        self.start_work(ctx);
        self.sync_prefetch(ctx);
        ctx.request_repaint();
    }

    fn start_work(&mut self, ctx: &egui::Context) {
        let Some((revision, work, cancelled)) = self.loader.take() else {
            return;
        };
        let sender = self.worker_tx.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = prepare(work, &cancelled);
            let _ = sender.send(Completion { revision, result });
            repaint.request_repaint();
        });
    }

    fn poll_workers(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.worker_rx.try_recv() {
            if self.loader.finish(done.revision) {
                if let Some(update) = done.result.animation {
                    self.animation_job = false;
                    match update {
                        Ok(update) => self.apply_animation_update(update, ctx),
                        Err(error) => {
                            self.error = Some(error);
                            if self.display_mode == ReadingMode::Book {
                                if let Some(spread) = &mut self.spread_playback {
                                    spread.pause(self.epoch.elapsed());
                                }
                                self.book_rotation_resume = false;
                            }
                        }
                    }
                    self.start_work(ctx);
                    self.start_navigation(ctx);
                    self.sync_prefetch(ctx);
                    ctx.request_repaint();
                    continue;
                }
                if let Some((path, siblings)) = done.result.selection {
                    self.selected = Some(path.clone());
                    match siblings {
                        Ok(paths) => {
                            if paths != self.siblings {
                                self.page_dimensions.clear();
                                self.page_overrides.clear();
                                self.layout_retry = None;
                            }
                            self.siblings = paths;
                        }
                        Err(error) => {
                            self.page_dimensions.clear();
                            self.page_overrides.clear();
                            self.layout_retry = None;
                            self.siblings = vec![path];
                            self.error = Some(error);
                        }
                    }
                    self.update_prefetch_context();
                }
                match done.result.image {
                    Ok(prepared) => {
                        self.candidate = Some(Candidate {
                            revision: done.revision,
                            image: prepared,
                            painted_frame: None,
                        });
                    }
                    Err(error) => {
                        self.loading_file = false;
                        self.current_file = self.displayed.clone();
                        self.rotation.cancel();
                        self.pause_animation();
                        if self.display_mode == ReadingMode::Book
                            && self.book_rotation_resume
                            && let Some(spread) = &mut self.spread_playback
                        {
                            spread.resume(self.epoch.elapsed());
                        }
                        self.book_rotation_resume = false;
                        self.error = Some(error);
                        self.navigation.failed(done.revision);
                        self.nav = None;
                        if let Some(selected) = self.selected.clone() {
                            self.reject_changed_archive(&selected, ctx);
                        }
                    }
                }
                self.start_work(ctx);
                self.start_navigation(ctx);
                self.sync_prefetch(ctx);
                ctx.request_repaint();
            } else {
                self.start_work(ctx);
            }
        }
    }

    fn apply_animation_update(&mut self, update: SpreadUpdate, ctx: &egui::Context) {
        if self.display_mode != ReadingMode::Book
            || update.generation != self.spread_generation
            || update.turns != self.rotation.displayed
        {
            return;
        }
        let Some(spread) = &self.spread_playback else {
            return;
        };
        let now = self.epoch.elapsed();
        let mut changed = false;
        for page in update.pages {
            let target = spread.frame(page.page, now);
            let committed = if page.page == 0 {
                Some(self.frame)
            } else {
                self.other_pages.get(page.page - 1).map(|page| page.frame)
            };
            if !animation_frame_is_current(page.frame, target, committed.unwrap_or(usize::MAX)) {
                continue;
            }
            if page.page == 0 {
                self.frame = page.frame;
                self.tiled = Some(page.tiled);
            } else if let Some(existing) = self.other_pages.get_mut(page.page - 1) {
                existing.frame = page.frame;
                existing.tiled = page.tiled;
            } else {
                continue;
            }
            changed = true;
        }
        if changed {
            ctx.request_repaint();
        }
    }

    fn commit_candidate(&mut self, ctx: &egui::Context) {
        let Some(candidate) = self.candidate.take() else {
            return;
        };
        let prepared = candidate.image;
        let loaded = prepared.loaded;
        let timings: Vec<_> = std::iter::once(&prepared.media)
            .chain(prepared.others.iter().map(|page| &page.media))
            .map(|media| page_timing(media))
            .collect();
        self.loading_file = false;
        if prepared.loaded {
            let path = prepared.media.source.clone();
            self.selected = Some(path.clone());
            self.displayed = Some(path);
            self.error = None;
            self.zoom = Zoom::Fit;
            self.pan = [0., 0.];
            self.playback = (prepared.mode == ReadingMode::Single
                && prepared.media.frames.len() > 1)
                .then(|| {
                    Playback::new(
                        prepared.media.frames.iter().map(|frame| frame.delay),
                        prepared.media.loop_mode,
                        self.epoch.elapsed(),
                    )
                });
            self.spread_playback = if prepared.mode == ReadingMode::Book {
                Some(SpreadPlayback::new(timings, self.epoch.elapsed()))
            } else {
                None
            };
            self.book_rotation_resume = false;
        } else if prepared.mode == ReadingMode::Book && self.book_rotation_resume {
            if let Some(spread) = &mut self.spread_playback {
                spread.resume(self.epoch.elapsed());
            }
            self.book_rotation_resume = false;
        }
        self.rotation.commit(prepared.turns);
        self.frame = prepared.frame;
        self.dimensions = prepared.dimensions;
        self.display_mode = prepared.mode;
        self.other_pages = prepared.others;
        self.media = Some(prepared.media);
        self.tiled = Some(prepared.tiled);
        let previous_retry = self.layout_retry.take();
        self.record_page_dimensions();
        self.fill_cached_dimensions();
        // Record the committed page before navigation_painted starts a queued
        // load, which replaces `selected` with the next, not-yet-shown page.
        if loaded {
            self.record_progress(ctx);
        }
        let correction = if loaded
            && self.display_mode == ReadingMode::Book
            && self.settings.reading_mode == ReadingMode::Book
            && !self.close_state.requested
        {
            let layout = comic_layout(
                self.siblings.len(),
                &self.page_dimensions,
                &self.page_overrides,
                self.settings.auto_wide,
            );
            self.requested_layout_change(&layout)
                .filter(|retry| Some(retry) != previous_retry.as_ref())
                .map(|(source, pages)| (source, pages, layout))
        } else {
            None
        };
        match correction {
            Some((source, pages, layout)) => {
                // The wrong spread did paint; settle its navigation barrier so
                // a queued step can resume after the corrected reload.
                if let Some(ticket) = self.navigation.painted(candidate.revision) {
                    self.navigation.crossed_frame(ticket);
                }
                self.navigation.retarget(pages[0], &layout);
                let revision = self.loader.revision;
                self.load_with_turns(ctx, source.clone(), true, prepared.turns);
                if self.loader.revision != revision {
                    self.layout_retry = Some((source, pages));
                } else {
                    self.navigation_painted(candidate.revision, ctx);
                }
            }
            None => self.navigation_painted(candidate.revision, ctx),
        }
        if loaded {
            self.reveal_thumbnails();
        }
        self.tick(ctx);
        self.sync_prefetch(ctx);
        ctx.request_repaint();
    }

    fn advance_frames(&mut self, ctx: &egui::Context) {
        let current = ctx.cumulative_frame_nr();
        if self
            .candidate
            .as_ref()
            .and_then(|candidate| candidate.painted_frame)
            .is_some_and(|painted| frame_advanced(painted, current))
        {
            self.commit_candidate(ctx);
        }
        let crossed = self.nav.filter(|nav| {
            nav.shown_frame
                .is_some_and(|shown| frame_advanced(shown, current))
        });
        if let Some(nav) = crossed {
            self.nav = None;
            if self.navigation.crossed_frame(nav.ticket) {
                self.start_navigation(ctx);
                self.sync_prefetch(ctx);
                ctx.request_repaint();
            }
        }
    }

    fn pause_animation(&mut self) {
        pause_controllers(
            &mut self.playback,
            &mut self.spread_playback,
            self.display_mode,
            self.epoch.elapsed(),
        );
    }

    fn request_frame(&mut self, ctx: &egui::Context, frame: usize) {
        let Some(media) = self.media.clone() else {
            return;
        };
        self.prefetch.set_allowed(false);
        self.candidate = None;
        self.animation_job = false;
        let work = if self.display_mode == ReadingMode::Book {
            self.spread_generation = self.spread_generation.wrapping_add(1);
            let now = self.epoch.elapsed();
            let frames = self
                .spread_playback
                .as_ref()
                .map(|spread| spread.targets(now))
                .unwrap_or_else(|| self.book_frames());
            Work::Spread {
                media: self.displayed_media(),
                frames,
                turns: self.rotation.requested,
                cap: self.tile_cap(ctx),
            }
        } else {
            Work::Frame {
                media,
                frame,
                turns: self.rotation.requested,
                cap: self.tile_cap(ctx),
            }
        };
        self.loader.request(work);
        self.start_work(ctx);
        self.sync_prefetch(ctx);
        ctx.request_repaint();
    }

    fn tick(&mut self, ctx: &egui::Context) {
        if self.loading_file || self.busy() || !self.navigation.is_idle() {
            return;
        }
        let now = self.epoch.elapsed();
        if self.display_mode == ReadingMode::Book {
            self.tick_book(ctx, now);
            return;
        }
        let Some(position) = self.playback.as_ref().map(|clock| clock.position(now)) else {
            return;
        };
        if position.frame != self.frame {
            self.request_frame(ctx, position.frame);
            return;
        }
        if let Some(next) = position.next {
            ctx.request_repaint_after(next.saturating_sub(now));
        }
    }

    fn tick_book(&mut self, ctx: &egui::Context, now: Duration) {
        let Some(spread) = &self.spread_playback else {
            return;
        };
        let playing = spread.any_playing(now);
        let targets = spread.targets(now);
        let deadline = spread.next_deadline(now);
        let committed = self.book_frames();
        let changed = animation_changes(&targets, &committed);
        if changed.is_empty() {
            if playing && let Some(deadline) = deadline {
                ctx.request_repaint_after(deadline.saturating_sub(now));
            }
            return;
        }
        let media = self.displayed_media();
        let pages: Vec<_> = changed
            .into_iter()
            .filter_map(|(page, frame)| {
                media.get(page).map(|media| AnimatePage {
                    page,
                    media: media.clone(),
                    frame,
                })
            })
            .collect();
        if pages.is_empty() {
            return;
        }
        self.prefetch.set_allowed(false);
        self.animation_job = true;
        self.loader.request(Work::AnimateSpread {
            generation: self.spread_generation,
            pages,
            turns: self.rotation.displayed,
            cap: self.tile_cap(ctx),
        });
        self.start_work(ctx);
        self.sync_prefetch(ctx);
        ctx.request_repaint();
    }

    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.fullscreen = !self.fullscreen;
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(self.fullscreen));
        ctx.request_repaint();
    }

    fn toggle_playback(&mut self, ctx: &egui::Context) {
        if self.loading_file || !self.has_animation() || self.editing() {
            return;
        }
        let now = self.epoch.elapsed();
        if self.display_mode == ReadingMode::Book {
            let Some(spread) = &mut self.spread_playback else {
                return;
            };
            match group_playback_action(spread.any_playing(now), spread.all_finished(now)) {
                GroupPlaybackAction::Pause => spread.pause(now),
                GroupPlaybackAction::Resume => spread.resume(now),
                GroupPlaybackAction::Restart => spread.restart_all(now),
            }
            self.tick(ctx);
            ctx.request_repaint();
            return;
        }
        if self.playing() {
            self.pause_animation();
            if self.busy() && self.navigation.is_idle() {
                let frame = self
                    .playback
                    .as_ref()
                    .map(|clock| clock.position(now).frame)
                    .unwrap_or_default();
                self.request_frame(ctx, frame);
            }
        } else {
            if let Some(clock) = &mut self.playback {
                clock.play(now);
            }
            self.tick(ctx);
        }
        ctx.request_repaint();
    }

    fn open(&mut self, ctx: &egui::Context) {
        if self.editing() || self.edit_pending.is_some() {
            return;
        }
        self.cancel_navigation();
        if let Some(path) = rfd::FileDialog::new()
            .set_title(self.text().open_picker_title)
            .add_filter(
                self.text().open_picker_title,
                &[
                    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff", "cbz", "zip",
                    "cbr", "rar", "cb7", "7z",
                ],
            )
            .pick_file()
        {
            self.open_path(path, ctx);
        }
        ctx.request_repaint();
    }

    fn edge(&mut self, last: bool, ctx: &egui::Context) {
        let index = if self.settings.reading_mode == ReadingMode::Book {
            let layout = comic_layout(
                self.siblings.len(),
                &self.page_dimensions,
                &self.page_overrides,
                self.settings.auto_wide,
            );
            if last { layout.last() } else { layout.first() }
        } else {
            let layout = self.layout();
            let count = layout.length();
            (count > 0).then(|| if last { count - 1 } else { 0 })
        };
        if let Some(path) = index.and_then(|index| self.siblings.get(index)).cloned() {
            self.load(ctx, path, false);
        }
    }

    fn scale_for(&self, zoom: Zoom, dimensions: (u32, u32), viewport: Vec2, dpi: f32) -> f32 {
        match zoom {
            Zoom::Fit => fit_scale(dimensions, viewport, dpi),
            Zoom::Actual => actual_scale(dpi),
            Zoom::Manual(scale) => scale,
        }
    }

    fn zoom_by(
        &mut self,
        factor: f32,
        anchor: Option<[f32; 2]>,
        viewport: Rect,
        ctx: &egui::Context,
    ) {
        if self.tiled.is_none() {
            return;
        }
        let dpi = ctx.pixels_per_point();
        let old = self
            .scale_for(self.zoom, self.dimensions, viewport.size(), dpi)
            .max(f32::EPSILON);
        let scale = zoom_scale(old, factor, dpi);
        self.pan = anchored_pan(self.pan, anchor.unwrap_or([0., 0.]), scale / old);
        self.zoom = Zoom::Manual(scale);
        ctx.request_repaint();
    }

    fn arrow(&mut self, vertical: bool, forward: bool, viewport: Rect, ctx: &egui::Context) {
        if !vertical && self.settings.reading_mode == ReadingMode::Book {
            self.enqueue_navigation(
                horizontal_navigation(forward, self.settings.comic_arrow_direction),
                ctx,
            );
            return;
        }
        let axis = usize::from(vertical);
        let extent = if vertical {
            self.dimensions.1
        } else {
            self.dimensions.0
        } as f32
            * self.scale_for(
                self.zoom,
                self.dimensions,
                viewport.size(),
                ctx.pixels_per_point(),
            );
        let available = if vertical {
            viewport.height()
        } else {
            viewport.width()
        };
        if self.tiled.is_some()
            && let Some(pan) = arrow_pan(self.pan[axis], extent, available, forward)
        {
            self.pan[axis] = pan;
            ctx.request_repaint();
        } else {
            self.enqueue_navigation(
                if forward {
                    Navigation::Next
                } else {
                    Navigation::Previous
                },
                ctx,
            );
        }
    }

    fn copy_current_view(&mut self, ctx: &egui::Context) {
        let Some(frame) = self
            .media
            .as_ref()
            .and_then(|media| media.frames.get(self.frame))
        else {
            return;
        };
        let mut pages = vec![frame.pixels.as_ref()];
        if self.display_mode == ReadingMode::Book {
            pages.extend(self.other_pages.iter().filter_map(|page| {
                page.media
                    .frames
                    .get(page.frame)
                    .map(|frame| frame.pixels.as_ref())
            }));
        }
        let Some((pixels, fallback)) =
            viewer::copy_pixels(&pages, self.rotation.displayed, viewer::COPY_SPREAD_LIMIT)
        else {
            return;
        };
        let size = [pixels.width() as usize, pixels.height() as usize];
        let image = egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_raw());
        ctx.copy_image(image);
        let message = if fallback {
            self.text().copied_selected_page
        } else {
            self.text().copied_image
        };
        self.set_feedback(FeedbackKind::Success, message.to_owned(), ctx);
    }

    fn editing(&self) -> bool {
        self.edit_ui.is_some()
    }

    fn request_edit(&mut self, crop: bool, ctx: &egui::Context) {
        if self.editing()
            || self.edit_pending.is_some()
            || self.settings_draft.is_some()
            || self.close_state.requested
            || self.busy()
            || self.loading_file
            || self.tiled.is_none()
        {
            return;
        }
        let (Some(selected), Some(displayed), Some(media)) = (
            self.selected.clone(),
            self.displayed.clone(),
            self.media.clone(),
        ) else {
            return;
        };
        // A failed load moves `selected` but keeps the old `media`; never mix them.
        if selected != displayed || selected != media.source {
            self.set_feedback(
                FeedbackKind::Error,
                self.text().edit_source_mismatch.to_owned(),
                ctx,
            );
            return;
        }
        let book_transition = self.display_mode == ReadingMode::Book
            || self.settings.reading_mode == ReadingMode::Book;
        // In Book the transition lands on the explicitly requested page, which
        // can be the other half of the current spread; edit that page, not the anchor.
        let source = if book_transition {
            mode_target(&self.current_file, &self.selected, &self.displayed)
                .unwrap_or_else(|| media.source.clone())
        } else {
            media.source.clone()
        };
        if editable_format(&source).is_none() {
            self.set_feedback(
                FeedbackKind::Error,
                self.text().edit_unsupported_format.to_owned(),
                ctx,
            );
            return;
        }
        let target_media = if source == media.source {
            Some(media.clone())
        } else {
            self.other_pages
                .iter()
                .find(|page| page.media.source == source)
                .map(|page| page.media.clone())
        };
        let Some(target_media) = target_media else {
            self.set_feedback(
                FeedbackKind::Error,
                self.text().edit_source_mismatch.to_owned(),
                ctx,
            );
            return;
        };
        if target_media.frames.len() != 1 || target_media.loop_mode != model::LoopMode::Once {
            self.set_feedback(
                FeedbackKind::Error,
                self.text().edit_single_frame.to_owned(),
                ctx,
            );
            return;
        }
        if book_transition {
            let pending = PendingEdit {
                source,
                rotation: self.rotation,
                restore: ReadingMode::Book,
                selected: self.selected.clone(),
                displayed: self.displayed.clone(),
                current_file: self.current_file.clone(),
                crop,
            };
            if self.transition_to_mode(ReadingMode::Single, ctx) {
                self.edit_pending = Some(pending);
            }
            return;
        }
        self.begin_edit(ctx, self.rotation, None, source, crop);
    }

    /// Snapshots the single displayed frame, bakes the shown rotation, and
    /// replaces the view with the preview through the normal commit path.
    fn begin_edit(
        &mut self,
        ctx: &egui::Context,
        rotation: Rotation,
        restore: Option<ReadingMode>,
        source: SourceRef,
        crop: bool,
    ) {
        let Some(media) = self.media.clone() else {
            return;
        };
        if media.source != source {
            return;
        }
        let Some(frame) = media.frames.first().map(|frame| Arc::clone(&frame.pixels)) else {
            return;
        };
        let session = match edit::EditSession::start(frame, rotation.displayed) {
            Ok(session) => session,
            Err(error) => {
                let message = edit_error_text(self.text(), error);
                self.set_feedback(FeedbackKind::Error, message, ctx);
                return;
            }
        };
        self.cancel_navigation();
        self.pause_animation();
        self.candidate = None;
        self.rotation.commit(0);
        self.edit_ui = Some(EditUi {
            session,
            source,
            original_media: media,
            original_tiled: self.tiled.clone(),
            original_rotation: rotation,
            restore_mode: restore,
            crop: None,
            confirm: None,
            saving: None,
            quality: export::JPEG_QUALITY_DEFAULT,
        });
        self.commit_edit_preview(ctx);
        if crop {
            self.enter_crop();
        }
    }

    /// Abandons a Book -> Single transition and restores the pre-request view.
    fn cancel_pending_edit(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.edit_pending.take() else {
            return;
        };
        let committed = self
            .media
            .as_ref()
            .is_some_and(|media| media.source == pending.source)
            && self.display_mode == ReadingMode::Single;
        self.candidate = None;
        self.loader.cancel();
        self.loading_file = false;
        self.nav = None;
        self.settings.reading_mode = pending.restore;
        if committed {
            // The Single frame already committed; rebuild the Book view from
            // the same page instead of only relabeling a single-page state.
            self.display_mode = pending.restore;
            self.rotation = pending.rotation;
            self.selected = Some(pending.source.clone());
            self.displayed = Some(pending.source.clone());
            self.current_file = Some(pending.source.clone());
            self.sync_prefetch(ctx);
            let turns = pending.rotation.displayed;
            self.load_with_turns(ctx, pending.source, false, turns);
        } else {
            self.display_mode = pending.restore;
            self.selected = pending.selected;
            self.displayed = pending.displayed;
            self.current_file = pending.current_file;
            self.rotation.cancel();
            self.sync_prefetch(ctx);
        }
        ctx.request_repaint();
    }

    /// Puts the edited pixels on screen without claiming they are the file on disk.
    fn commit_edit_preview(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.edit_ui.as_ref() else {
            return;
        };
        let source = edit.source.clone();
        let pixels = Arc::clone(edit.session.preview());
        let media = Arc::new(model::LoadedMedia {
            source,
            frames: vec![model::MediaFrame {
                pixels,
                delay: Duration::ZERO,
            }],
            loop_mode: model::LoopMode::Once,
        });
        let cancelled = AtomicBool::new(false);
        let prepared = prepare_frame(media, 0, 0, false, self.tile_cap(ctx), &cancelled);
        match prepared {
            Ok(prepared) => {
                self.candidate = Some(Candidate {
                    revision: self.loader.revision,
                    image: prepared,
                    painted_frame: None,
                });
                self.commit_candidate(ctx);
            }
            Err(error) => {
                let message = self.text().edit_failed(&error);
                self.set_feedback(FeedbackKind::Error, message, ctx);
            }
        }
        self.sync_prefetch(ctx);
        ctx.request_repaint();
    }

    fn apply_edit_op(&mut self, op: edit::EditOp, ctx: &egui::Context) {
        let result = self.edit_ui.as_mut().and_then(|edit| {
            if edit.confirm.is_some() || edit.saving.is_some() {
                None
            } else {
                Some(edit.session.apply(op))
            }
        });
        match result {
            Some(Ok(())) => {
                // A successful op can change dimensions; a stale crop box would lie.
                if let Some(edit) = self.edit_ui.as_mut() {
                    edit.crop = None;
                }
                self.commit_edit_preview(ctx);
            }
            Some(Err(error)) => {
                let message = edit_error_text(self.text(), error);
                self.set_feedback(FeedbackKind::Error, message, ctx);
            }
            None => {}
        }
    }

    fn undo_edit(&mut self, ctx: &egui::Context) {
        let undone = self.edit_ui.as_mut().is_some_and(|edit| {
            edit.confirm.is_none() && edit.saving.is_none() && edit.session.undo()
        });
        if undone {
            if let Some(edit) = self.edit_ui.as_mut() {
                edit.crop = None;
            }
            self.commit_edit_preview(ctx);
        }
    }

    fn crop_active(&self) -> bool {
        self.edit_ui
            .as_ref()
            .is_some_and(|edit| edit.crop.is_some())
    }

    fn crop_layer(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        painter: &egui::Painter,
        viewport: Rect,
        palette: Palette,
    ) {
        let dpi = ctx.pixels_per_point();
        let handle_size = crop_handle_size(dpi);
        let dimensions = self.dimensions;
        let scale = self.scale_for(self.zoom, dimensions, viewport.size(), dpi);
        let transform = viewer::CanvasTransform::new(
            [viewport.center().x, viewport.center().y],
            dimensions,
            scale,
            self.pan,
            dpi,
        );
        let to_image = |pos: Pos2| viewer::screen_to_image(transform, [pos.x, pos.y]);
        let screen_rect = |rect: edit::PixelRect| {
            viewer::image_rect_to_screen(
                transform,
                [rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32],
            )
        };
        let Some(crop) = self.edit_ui.as_mut().and_then(|edit| edit.crop.as_mut()) else {
            return;
        };
        let hold = crop.aspect.ratio(dimensions);
        if response.drag_started_by(PointerButton::Primary)
            && let Some(pos) = response.interact_pointer_pos()
        {
            let screen = screen_rect(crop.rect);
            let drawn = Rect::from_min_size(
                Pos2::new(screen[0], screen[1]),
                egui::vec2(screen[2], screen[3]),
            );
            if let Some(handle) = crop_handle_at(screen, pos, handle_size) {
                crop.drag = Some(CropDrag::Resize {
                    handle,
                    origin: crop.rect,
                    anchor: to_image(pos),
                });
            } else if drawn.contains(pos) {
                crop.drag = Some(CropDrag::Move {
                    origin: crop.rect,
                    anchor: to_image(pos),
                });
            } else {
                let anchor = to_image(pos);
                crop.drag = Some(CropDrag::New { anchor });
                crop.rect = edit::aspect_fit(
                    crop_rect_from_points(anchor, anchor, dimensions),
                    hold,
                    dimensions,
                );
            }
        }
        if response.dragged_by(PointerButton::Primary)
            && let (Some(drag), Some(pos)) = (crop.drag, response.interact_pointer_pos())
        {
            let point = to_image(pos);
            crop.rect = match drag {
                CropDrag::New { anchor } => edit::aspect_fit(
                    crop_rect_from_points(anchor, point, dimensions),
                    hold,
                    dimensions,
                ),
                CropDrag::Move { origin, anchor } => moved_crop(origin, anchor, point, dimensions),
                CropDrag::Resize {
                    handle,
                    origin,
                    anchor,
                } => edit::aspect_fit(
                    resize_crop(
                        origin,
                        handle,
                        [point[0] - anchor[0], point[1] - anchor[1]],
                        dimensions,
                    ),
                    hold,
                    dimensions,
                ),
            };
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            crop.drag = None;
        }
        if response.double_clicked_by(PointerButton::Primary) {
            crop.rect = edit::aspect_fit(
                edit::PixelRect {
                    x: 0,
                    y: 0,
                    w: dimensions.0,
                    h: dimensions.1,
                },
                hold,
                dimensions,
            );
            crop.drag = None;
        }

        let screen = screen_rect(crop.rect);
        let drawn = Rect::from_min_size(
            Pos2::new(screen[0], screen[1]),
            egui::vec2(screen[2], screen[3]),
        );
        let dragging = crop.drag.is_some();
        let active_handle = match crop.drag {
            Some(CropDrag::Resize { handle, .. }) => Some(handle),
            _ => None,
        };
        // Dim what is discarded; the selection itself is never filled over.
        let shade = Color32::from_black_alpha(if dragging { 104 } else { 120 });
        let clamp_x = |x: f32| x.clamp(viewport.left(), viewport.right());
        let clamp_y = |y: f32| y.clamp(viewport.top(), viewport.bottom());
        let bands = [
            Rect::from_min_max(
                viewport.min,
                Pos2::new(viewport.max.x, clamp_y(drawn.top())),
            ),
            Rect::from_min_max(
                Pos2::new(viewport.min.x, clamp_y(drawn.bottom())),
                viewport.max,
            ),
            Rect::from_min_max(
                Pos2::new(viewport.min.x, drawn.top().max(viewport.top())),
                Pos2::new(clamp_x(drawn.left()), drawn.bottom().min(viewport.bottom())),
            ),
            Rect::from_min_max(
                Pos2::new(clamp_x(drawn.right()), drawn.top().max(viewport.top())),
                Pos2::new(viewport.max.x, drawn.bottom().min(viewport.bottom())),
            ),
        ];
        for band in bands {
            if band.width() > 0. && band.height() > 0. {
                painter.rect_filled(band, 0., shade);
            }
        }
        // Dark halo plus an accent hairline: one of the two always separates the
        // frame from a white or a black image.
        painter.rect_stroke(
            drawn,
            0.,
            Stroke::new(3., Color32::from_black_alpha(150)),
            egui::StrokeKind::Outside,
        );
        painter.rect_stroke(
            drawn,
            0.,
            Stroke::new(1.5, palette.accent),
            egui::StrokeKind::Inside,
        );
        if drawn.width() > 48. && drawn.height() > 48. {
            // Thirds help composition at rest and get out of the way while the
            // pointer is already dragging a handle.
            let alpha = if dragging { 0.25 } else { 0.5 };
            for third in 1..3 {
                let x = drawn.left() + drawn.width() * third as f32 / 3.;
                let y = drawn.top() + drawn.height() * third as f32 / 3.;
                painter.line_segment(
                    [Pos2::new(x, drawn.top()), Pos2::new(x, drawn.bottom())],
                    Stroke::new(1., palette.accent.gamma_multiply(alpha)),
                );
                painter.line_segment(
                    [Pos2::new(drawn.left(), y), Pos2::new(drawn.right(), y)],
                    Stroke::new(1., palette.accent.gamma_multiply(alpha)),
                );
            }
        }
        for (handle, center) in crop_handle_points(screen, handle_size) {
            let focused = active_handle == Some(handle);
            let size = if focused {
                handle_size + 2.
            } else {
                handle_size
            };
            let square = Rect::from_center_size(center, egui::vec2(size, size));
            painter.rect_filled(
                square,
                2.,
                if focused {
                    palette.text
                } else {
                    palette.accent
                },
            );
            painter.rect_stroke(
                square,
                2.,
                Stroke::new(1., Color32::from_black_alpha(130)),
                egui::StrokeKind::Inside,
            );
        }
        if response.hovered() || response.dragged() {
            let icon = if crop.drag.is_some() || response.dragged() {
                CursorIcon::Grabbing
            } else if let Some(pos) = response.hover_pos() {
                if let Some(handle) = crop_handle_at(screen, pos, handle_size) {
                    crop_cursor(handle)
                } else if drawn.contains(pos) {
                    CursorIcon::Grab
                } else {
                    CursorIcon::Crosshair
                }
            } else {
                CursorIcon::Crosshair
            };
            ctx.set_cursor_icon(icon);
        }
    }

    /// Restores the pre-edit frame and layout; edited pixels are never written here.
    fn exit_edit(&mut self, ctx: &egui::Context) {
        if self
            .edit_ui
            .as_ref()
            .is_some_and(|edit| edit.saving.is_some())
        {
            return;
        }
        let Some(edit) = self.edit_ui.take() else {
            return;
        };
        let dirty = edit.session.dirty();
        let source = edit.source.clone();
        let turns = edit.original_rotation.displayed;
        self.candidate = None;
        self.cancel_navigation();
        self.rotation = edit.original_rotation;
        self.media = Some(edit.original_media);
        self.tiled = edit.original_tiled;
        self.other_pages.clear();
        self.frame = 0;
        self.selected = Some(source.clone());
        self.displayed = Some(source.clone());
        self.current_file = Some(source.clone());
        self.dimensions = self
            .tiled
            .as_ref()
            .map(|tiled| tiled.dimensions)
            .unwrap_or((0, 0));
        self.display_mode = self.settings.reading_mode;
        if let Some(mode) = edit.restore_mode {
            self.settings.reading_mode = mode;
            self.sync_prefetch(ctx);
            self.load_with_turns(ctx, source, false, turns);
        } else {
            self.sync_prefetch(ctx);
        }
        if dirty {
            self.set_feedback(
                FeedbackKind::Success,
                self.text().edit_discarded.to_owned(),
                ctx,
            );
        }
        ctx.request_repaint();
    }

    fn enter_crop(&mut self) {
        let dimensions = self.dimensions;
        let Some(edit) = self.edit_ui.as_mut() else {
            return;
        };
        if edit.session.preview().width() == 0 || edit.session.preview().height() == 0 {
            return;
        }
        let rect = edit::PixelRect {
            x: 0,
            y: 0,
            w: dimensions.0,
            h: dimensions.1,
        };
        edit.crop = Some(CropState {
            rect,
            aspect: CropAspect::Free,
            drag: None,
        });
    }

    fn set_crop_aspect(&mut self, aspect: CropAspect) {
        let dimensions = self.dimensions;
        let Some(crop) = self.edit_ui.as_mut().and_then(|edit| edit.crop.as_mut()) else {
            return;
        };
        crop.aspect = aspect;
        crop.rect = edit::aspect_fit(crop.rect, aspect.ratio(dimensions), dimensions);
    }

    fn apply_crop(&mut self, ctx: &egui::Context) {
        if self
            .edit_ui
            .as_ref()
            .is_some_and(|edit| edit.confirm.is_some() || edit.saving.is_some())
        {
            return;
        }
        let rect = self
            .edit_ui
            .as_mut()
            .and_then(|edit| edit.crop.take())
            .map(|crop| crop.rect);
        if let Some(rect) = rect {
            self.apply_edit_op(edit::EditOp::Crop(rect), ctx);
        }
    }

    fn request_save_overwrite(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.edit_ui.as_ref() else {
            return;
        };
        if edit.confirm.is_some() {
            return;
        }
        if edit.saving.is_some() {
            self.set_feedback(
                FeedbackKind::Progress,
                self.text().saving_image.to_owned(),
                ctx,
            );
            return;
        }
        let Some(source) = self.edit_ui.as_ref().map(|edit| edit.source.clone()) else {
            return;
        };
        if source.is_archive() {
            self.set_feedback(
                FeedbackKind::Error,
                self.text().archive_cannot_overwrite.to_owned(),
                ctx,
            );
            return;
        }
        let path = source.path().to_path_buf();
        let Some(format) = export::format_for_path(&path) else {
            self.set_feedback(
                FeedbackKind::Error,
                self.text().edit_unsupported_format.to_owned(),
                ctx,
            );
            return;
        };
        if let Some(edit) = self.edit_ui.as_mut() {
            edit.confirm = Some(SaveConfirm {
                path,
                format,
                overwrite: true,
            });
        }
    }

    fn save_as(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.edit_ui.as_ref() else {
            return;
        };
        if edit.confirm.is_some() {
            return;
        }
        if edit.saving.is_some() {
            self.set_feedback(
                FeedbackKind::Progress,
                self.text().saving_image.to_owned(),
                ctx,
            );
            return;
        }
        let source = edit.source.clone();
        let name = export::default_file_name(&source.display_name());
        let picked = rfd::FileDialog::new()
            .set_title(self.text().save_as)
            .set_file_name(name)
            .add_filter("PNG", &["png"])
            .add_filter("JPEG", &["jpg", "jpeg"])
            .add_filter("WebP", &["webp"])
            .add_filter("BMP", &["bmp"])
            .save_file();
        let Some(path) = picked else {
            return;
        };
        let Some(format) = export::format_for_path(&path) else {
            let message = self.text().unknown_save_format(&path.display().to_string());
            self.set_feedback(FeedbackKind::Error, message, ctx);
            return;
        };
        // Save As onto the frozen source is the overwrite transaction, never a
        // plain copy that skips cache invalidation.
        if same_save_target(&path, source.path()) {
            if source.is_archive() {
                self.set_feedback(
                    FeedbackKind::Error,
                    self.text().archive_cannot_overwrite.to_owned(),
                    ctx,
                );
                return;
            }
            if let Some(edit) = self.edit_ui.as_mut() {
                edit.confirm = Some(SaveConfirm {
                    path,
                    format,
                    overwrite: true,
                });
            }
            return;
        }
        let transparent = self
            .edit_ui
            .as_ref()
            .is_some_and(|edit| export::has_transparency(edit.session.preview()));
        if save_risk_reason(self.text(), format, transparent).is_some() {
            if let Some(edit) = self.edit_ui.as_mut() {
                edit.confirm = Some(SaveConfirm {
                    path,
                    format,
                    overwrite: false,
                });
            }
            return;
        }
        self.start_save(path, format, false, ctx);
    }

    fn start_save(
        &mut self,
        path: PathBuf,
        format: export::ExportFormat,
        overwrite: bool,
        ctx: &egui::Context,
    ) {
        if self.close_state.requested {
            self.set_feedback(
                FeedbackKind::Error,
                self.text().save_blocked_closing.to_owned(),
                ctx,
            );
            return;
        }
        let Some(edit) = self.edit_ui.as_mut() else {
            return;
        };
        if edit.saving.is_some() {
            return;
        }
        let pixels = Arc::clone(edit.session.preview());
        let quality = edit.quality;
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        let target = path.clone();
        std::thread::spawn(move || {
            let result = export::atomic_write(&target, &pixels, format, quality)
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
            repaint.request_repaint();
        });
        edit.saving = Some(SaveJob {
            receiver,
            overwrite,
            path,
        });
        self.set_feedback(
            FeedbackKind::Progress,
            self.text().saving_image.to_owned(),
            ctx,
        );
    }

    fn poll_save(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.edit_ui.as_mut() else {
            return;
        };
        let Some(job) = edit.saving.as_ref() else {
            return;
        };
        let outcome = match job.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("save worker stopped".to_owned()),
        };
        let overwrite = job.overwrite;
        let path = job.path.clone();
        let source = edit.source.clone();
        edit.saving = None;
        let success = outcome.is_ok();
        match outcome {
            Ok(()) if overwrite => self.finish_overwrite(ctx),
            Ok(()) => {
                let mut message = self.text().saved_as(&path.display().to_string());
                let same_directory =
                    !source.is_archive() && source.path().parent() == path.parent();
                if same_directory {
                    message.push_str(self.text().reopen_folder);
                }
                self.set_feedback(FeedbackKind::Success, message, ctx);
            }
            Err(error) => {
                let message = self.text().export_failed(&error);
                self.set_feedback(FeedbackKind::Error, message, ctx);
            }
        }
        // A close requested during the write waits for the result; failure keeps
        // the window open so the user can retry.
        if self.close_after_save {
            self.close_after_save = false;
            if success {
                self.request_close(ctx);
            }
        }
    }

    /// After a successful overwrite the disk is the only truth: drop caches and re-decode.
    fn finish_overwrite(&mut self, ctx: &egui::Context) {
        let Some(edit) = self.edit_ui.take() else {
            return;
        };
        let source = edit.source.clone();
        self.prefetch.forget_paths(std::slice::from_ref(&source));
        if let Some(index) = source_index(Some(&source), &self.siblings) {
            self.page_dimensions.remove(&index);
        }
        self.thumb_generation = self.thumb_generation.wrapping_add(1);
        self.thumb_textures.clear();
        self.thumb_requests.clear();
        self.thumb_visible.clear();
        self.thumb_stable = 0;
        self.settings.reading_mode = edit.restore_mode.unwrap_or(self.settings.reading_mode);
        self.display_mode = self.settings.reading_mode;
        // The edited page already contains its baked rotation on disk; reload
        // it at turns=0. A Book spread is rebuilt as a whole, so applying the
        // old turns there would rotate every page a second time.
        self.load(ctx, source, false);
        self.reveal_thumbnails();
        self.set_feedback(FeedbackKind::Success, self.text().saved.to_owned(), ctx);
    }

    fn perform(&mut self, action: Action, viewport: Rect, ctx: &egui::Context) {
        if self.editing()
            && matches!(
                action,
                Action::Previous
                    | Action::Next
                    | Action::JumpFiles(_)
                    | Action::Pan { .. }
                    | Action::Playback
                    | Action::ToggleMode
                    | Action::WideToggle
                    | Action::First
                    | Action::Last
                    | Action::Open
                    | Action::Settings
            )
        {
            return;
        }
        match action {
            Action::Previous => self.enqueue_navigation(Navigation::Previous, ctx),
            Action::Next => self.enqueue_navigation(Navigation::Next, ctx),
            Action::JumpFiles(delta) => self.enqueue_navigation(Navigation::JumpFiles(delta), ctx),
            Action::Pan { vertical, forward } => self.arrow(vertical, forward, viewport, ctx),
            Action::ZoomIn => self.zoom_by(1.25, None, viewport, ctx),
            Action::ZoomOut => self.zoom_by(0.8, None, viewport, ctx),
            Action::Fit => {
                self.zoom = Zoom::Fit;
                self.pan = [0., 0.];
                ctx.request_repaint();
            }
            Action::Actual => {
                self.zoom = Zoom::Actual;
                self.pan = [0., 0.];
                ctx.request_repaint();
            }
            Action::RotateLeft | Action::RotateRight => {
                let clockwise = matches!(action, Action::RotateRight);
                if self.editing() {
                    let op = if clockwise {
                        edit::EditOp::RotateCw
                    } else {
                        edit::EditOp::RotateCcw
                    };
                    self.apply_edit_op(op, ctx);
                } else if !self.loading_file && self.media.is_some() {
                    self.rotation.request(clockwise);
                    self.error = None;
                    let now = self.epoch.elapsed();
                    if self.display_mode == ReadingMode::Book {
                        self.book_rotation_resume = self.book_rotation_resume
                            || self
                                .spread_playback
                                .as_ref()
                                .is_some_and(|spread| spread.any_playing(now));
                        if let Some(spread) = &mut self.spread_playback {
                            spread.pause(now);
                        }
                    } else {
                        self.book_rotation_resume = false;
                    }
                    let frame = self
                        .playback
                        .as_ref()
                        .map(|clock| clock.position(now).frame)
                        .unwrap_or(self.frame);
                    self.request_frame(ctx, frame);
                }
            }
            Action::Playback => self.toggle_playback(ctx),
            Action::Fullscreen => self.toggle_fullscreen(ctx),
            Action::Escape => {
                if self.edit_pending.is_some() {
                    self.cancel_pending_edit(ctx);
                    return;
                }
                if let Some(edit) = self.edit_ui.as_mut() {
                    if edit.confirm.is_some() {
                        edit.confirm = None;
                        ctx.request_repaint();
                        return;
                    }
                    if edit.crop.is_some() {
                        edit.crop = None;
                        ctx.request_repaint();
                        return;
                    }
                }
                if self.editing() {
                    self.exit_edit(ctx);
                    return;
                }
                if !self.navigation.is_idle() {
                    self.cancel_navigation();
                    self.sync_prefetch(ctx);
                    ctx.request_repaint();
                } else if self.fullscreen {
                    self.toggle_fullscreen(ctx);
                }
            }
            Action::Open => self.open(ctx),
            Action::Copy => self.copy_current_view(ctx),
            Action::First => self.edge(false, ctx),
            Action::Last => self.edge(true, ctx),
            Action::Close => self.request_close(ctx),
            Action::Settings => self.open_settings(ctx),
            Action::ToggleMode => self.toggle_reading_mode(ctx),
            Action::WideToggle => self.toggle_wide_page(ctx),
            Action::Edit => {
                if self.editing() {
                    let blocked = self
                        .edit_ui
                        .as_ref()
                        .is_some_and(|edit| edit.confirm.is_some() || edit.saving.is_some());
                    if !blocked {
                        self.enter_crop();
                    }
                } else {
                    self.request_edit(true, ctx);
                }
            }
            Action::UndoEdit => self.undo_edit(ctx),
            Action::FlipHorizontal => {
                self.apply_edit_op(edit::EditOp::FlipHorizontal, ctx);
            }
            Action::FlipVertical => {
                self.apply_edit_op(edit::EditOp::FlipVertical, ctx);
            }
            Action::SaveOverwrite => self.request_save_overwrite(ctx),
            Action::SaveAs => self.save_as(ctx),
            Action::CropApply => self.apply_crop(ctx),
            Action::CropCancel => {
                if let Some(edit) = self.edit_ui.as_mut() {
                    edit.crop = None;
                }
            }
            Action::EditExit => self.exit_edit(ctx),
        }
    }

    fn handle_keys(&mut self, viewport: Rect, ctx: &egui::Context) {
        if self.settings_draft.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        // A save confirmation owns the keyboard; only Esc (cancel) passes.
        let confirming = self
            .edit_ui
            .as_ref()
            .is_some_and(|edit| edit.confirm.is_some());
        let events = ctx.input(|input| input.events.clone());
        let mut acted = false;
        for event in events {
            if confirming
                && !matches!(
                    &event,
                    egui::Event::Key {
                        key: Key::Escape,
                        pressed: true,
                        ..
                    }
                )
            {
                continue;
            }
            // egui-winit consumes the native copy shortcut before emitting Key.
            if matches!(event, egui::Event::Copy) {
                self.perform(Action::Copy, viewport, ctx);
                acted = true;
                continue;
            }
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                repeat,
                ..
            } = event
                && let Some(action) = key_action(key, modifiers)
                && (action_allows_repeat(action) || !repeat)
            {
                self.perform(action, viewport, ctx);
                acted = true;
            }
        }
        if acted {
            ctx.request_repaint();
        }
    }

    fn handle_wheel(&mut self, response: &egui::Response, viewport: Rect, ctx: &egui::Context) {
        if !response.hovered() || self.tiled.is_none() {
            return;
        }
        let Some(pointer) = response.hover_pos() else {
            return;
        };
        let events = ctx.input(|input| input.events.clone());
        let center = viewport.center();
        let anchor = [pointer.x - center.x, pointer.y - center.y];
        for event in events {
            if let egui::Event::MouseWheel { unit, delta, .. } = event {
                let points = match unit {
                    egui::MouseWheelUnit::Point => delta.y,
                    egui::MouseWheelUnit::Line => delta.y * 24.,
                    egui::MouseWheelUnit::Page => delta.y * viewport.height().max(1.),
                };
                let factor = (points * 0.005).clamp(-1., 1.).exp();
                self.zoom_by(factor, Some(anchor), viewport, ctx);
            }
        }
    }

    /// Defers closing until the running image save reports a result.
    fn save_delays_close(&mut self, ctx: &egui::Context, native: bool) -> bool {
        if !self
            .edit_ui
            .as_ref()
            .is_some_and(|edit| edit.saving.is_some())
        {
            return false;
        }
        self.close_after_save = true;
        if native {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        }
        ctx.request_repaint();
        true
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        if self.save_delays_close(ctx, true) {
            return;
        }
        self.cancel_navigation();
        let close = self.close_state.request(self.save_revision);
        let progress_error = if close {
            false
        } else {
            self.flush_progress_for_close(ctx)
        };
        if close {
            self.flush_progress_for_close(ctx);
            self.close_pending_notice = false;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        } else {
            if self.settings_draft.is_some() {
                self.close_settings(ctx);
            }
            if !self.close_pending_notice {
                self.close_pending_notice = true;
                if !progress_error {
                    self.set_feedback(
                        FeedbackKind::Progress,
                        self.text().saving_before_close.to_owned(),
                        ctx,
                    );
                }
            }
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        }
    }

    fn request_close(&mut self, ctx: &egui::Context) {
        if self.save_delays_close(ctx, false) {
            return;
        }
        self.cancel_navigation();
        let close = self.close_state.request(self.save_revision);
        let progress_error = if close {
            false
        } else {
            self.flush_progress_for_close(ctx)
        };
        if close {
            self.flush_progress_for_close(ctx);
            self.close_pending_notice = false;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        } else {
            if self.settings_draft.is_some() {
                self.close_settings(ctx);
            }
            if !self.close_pending_notice {
                self.close_pending_notice = true;
                if !progress_error {
                    self.set_feedback(
                        FeedbackKind::Progress,
                        self.text().saving_before_close.to_owned(),
                        ctx,
                    );
                }
            }
        }
        ctx.request_repaint();
    }

    fn position(&self) -> Option<usize> {
        source_index(self.selected.as_ref(), &self.siblings)
    }

    fn neighbor(&self, next: bool) -> Option<SourceRef> {
        let index = self.position()?;
        let index = if self.settings.reading_mode == ReadingMode::Single {
            adjacent_index(index, self.siblings.len(), next)?
        } else {
            let action = if next {
                Navigation::Next
            } else {
                Navigation::Previous
            };
            self.layout().destination(index, action)?
        };
        self.siblings.get(index).cloned()
    }

    fn display_name(&self) -> String {
        let names: Vec<_> = self.displayed_paths().iter().map(source_label).collect();
        let text = self.text();
        if names.is_empty() {
            self.selected
                .as_ref()
                .map(source_label)
                .unwrap_or_else(|| text.image_viewer.into())
        } else {
            text.image_names(&names)
        }
    }

    fn position_label(&self) -> String {
        self.position()
            .map(|index| {
                let pages = self.layout().pages(index);
                if let Some(last) = pages.last().filter(|last| **last != index) {
                    format!("{}–{} / {}", index + 1, last + 1, self.siblings.len())
                } else {
                    format!("{} / {}", index + 1, self.siblings.len())
                }
            })
            .unwrap_or_else(|| "—".into())
    }

    fn zoom_label(&self, ctx: &egui::Context) -> String {
        if self.tiled.is_none() {
            return "—".into();
        }
        let dpi = ctx.pixels_per_point();
        format!(
            "{:.0}%",
            self.scale_for(self.zoom, self.dimensions, self.viewport.size(), dpi) * dpi * 100.
        )
    }

    fn status_text(&self) -> String {
        let name = self.selected.as_ref().map(source_label).unwrap_or_default();
        let text = self.text();
        if self.selected.is_some() && self.selected != self.displayed && !self.busy() {
            return text.could_not_open(&name, self.tiled.is_some());
        }
        if self.tiled.is_none() {
            return text.idle_read_only.into();
        }
        let mut status = text.dimensions(self.dimensions.0, self.dimensions.1);
        if self.display_mode == ReadingMode::Book {
            let pages = self
                .spread_playback
                .as_ref()
                .map_or(self.other_pages.len() + 1, |spread| spread.page_count());
            status.push_str(&text.book_pages(pages));
            let frames = self.book_frames();
            let counters: Vec<_> = self
                .displayed_media()
                .iter()
                .enumerate()
                .filter(|(_, media)| media.frames.len() > 1)
                .map(|(page, media)| {
                    format!(
                        "{}/{}",
                        frames.get(page).copied().unwrap_or(0) + 1,
                        media.frames.len()
                    )
                })
                .collect();
            if !counters.is_empty() {
                let state = if self.playing() {
                    text.playing
                } else if self.all_finished() {
                    text.finished
                } else {
                    text.paused
                };
                status.push_str(&text.book_gif(&counters.join(text.name_separator), state));
            }
        } else if let Some(media) = self.media.as_ref().filter(|media| media.frames.len() > 1) {
            let state = if self.playing() {
                text.playing
            } else if self
                .playback
                .as_ref()
                .is_some_and(|clock| clock.position(self.epoch.elapsed()).finished)
            {
                text.finished
            } else {
                text.paused
            };
            status.push_str(&text.single_gif(self.frame + 1, media.frames.len(), state));
        }
        status
    }

    fn notice_banner(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let Some(notice) = self.feedback.notice.clone() else {
            return;
        };
        let (tone, background) = match notice.kind {
            FeedbackKind::Error => (palette.error, palette.error_bg),
            FeedbackKind::Success | FeedbackKind::Progress => (palette.accent, palette.accent_bg),
        };
        let progress = notice.kind == FeedbackKind::Progress;
        egui::Panel::top("notice-banner")
            .frame(
                EguiFrame::new()
                    .fill(background)
                    .inner_margin(Margin::symmetric(14, 7)),
            )
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(egui::vec2(6., 6.), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 3., tone);
                        ui.add(
                            egui::Label::new(RichText::new(notice.text).size(12.).color(tone))
                                .wrap(),
                        );
                    });
                    if progress {
                        // A running save cannot be cancelled: the bar only
                        // reports that work is still going and offers no button.
                        let (track, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width().max(80.), 3.),
                            Sense::hover(),
                        );
                        ui.painter()
                            .rect_filled(track, 1.5, tone.gamma_multiply(0.25));
                        let phase = (ui.input(|input| input.time) * 0.8).rem_euclid(1.) as f32;
                        let (start, end) = indeterminate_span(phase, track.width());
                        ui.painter().rect_filled(
                            Rect::from_min_max(
                                Pos2::new(track.left() + start, track.top()),
                                Pos2::new(track.left() + end, track.bottom()),
                            ),
                            1.5,
                            tone,
                        );
                        ui.ctx().request_repaint();
                    }
                    if notice.kind == FeedbackKind::Error {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let text = self.text();
                            if action_button(
                                ui,
                                text.dismiss,
                                true,
                                false,
                                palette,
                                text.dismiss_tooltip,
                            ) {
                                self.dismiss_feedback(ui.ctx());
                            }
                        });
                    }
                });
            });
    }

    fn preview_strip(&mut self, ui: &mut egui::Ui, palette: Palette) {
        if !self.settings.thumbnails_visible || self.editing() {
            return;
        }
        egui::Panel::bottom("previews")
            .exact_size(STRIP_HEIGHT)
            .frame(
                EguiFrame::new()
                    .fill(palette.surface)
                    .stroke(Stroke::new(1., palette.border))
                    .inner_margin(Margin::ZERO),
            )
            .show(ui, |ui| {
                let (response, painter) =
                    ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
                let rect = response.rect;
                painter.rect_filled(rect, 0., palette.surface);
                if rect.width() < 1. || rect.height() < 1. {
                    return;
                }
                let old_width = self.thumb_width;
                self.thumb_width = rect.width();
                self.thumb_offset =
                    clamp_offset(self.thumb_offset, self.siblings.len(), self.thumb_width);
                if (old_width - self.thumb_width).abs() > 0.5 {
                    self.reveal_thumbnails();
                    self.thumb_stable = 0;
                }
                self.sync_thumbnails(ui.ctx());
                let mut selected = self.displayed_paths();
                if selected.is_empty() {
                    selected.extend(self.selected.clone());
                }
                let range = visible_range(self.siblings.len(), self.thumb_offset, self.thumb_width);
                if self.siblings.is_empty() {
                    painter.text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        self.text().no_previews,
                        FontId::proportional(12.),
                        palette.muted,
                    );
                }
                for index in range {
                    let path = &self.siblings[index];
                    let x = rect.min.x + index as f32 * CELL_WIDTH - self.thumb_offset;
                    let cell = Rect::from_min_size(
                        Pos2::new(x, rect.min.y + 3.),
                        egui::vec2(CELL_WIDTH, rect.height() - 6.),
                    );
                    if !cell.intersects(rect) {
                        continue;
                    }
                    let active = selected.contains(path);
                    painter.rect_filled(
                        cell.shrink(2.),
                        6.,
                        if active {
                            palette.accent_bg
                        } else {
                            palette.canvas
                        },
                    );
                    let image_bounds = Rect::from_min_size(
                        Pos2::new(cell.left() + 6., cell.top() + 4.),
                        egui::vec2(CELL_WIDTH - 12., cell.height() - 26.),
                    );
                    if let Some(texture) = self.thumb_textures.texture(path) {
                        let destination = fit_rect(texture.size_vec2(), image_bounds);
                        painter.image(
                            texture.id(),
                            destination,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)),
                            Color32::WHITE,
                        );
                    } else {
                        painter.rect_filled(image_bounds, 4., palette.surface);
                        painter.text(
                            image_bounds.center(),
                            Align2::CENTER_CENTER,
                            "…",
                            FontId::proportional(16.),
                            palette.muted,
                        );
                    }
                    painter.with_clip_rect(cell.intersect(rect)).text(
                        Pos2::new(cell.center().x, cell.bottom() - 10.),
                        Align2::CENTER_CENTER,
                        path.display_name(),
                        FontId::proportional(10.),
                        if active {
                            palette.accent
                        } else {
                            palette.muted
                        },
                    );
                    painter.rect_stroke(
                        cell.shrink(1.),
                        6.,
                        Stroke::new(
                            if active { 2. } else { 1. },
                            if active {
                                palette.accent
                            } else {
                                palette.border
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );
                }
                if self.settings_draft.is_none() && self.edit_pending.is_none() {
                    if response.hovered() {
                        let events = ui.ctx().input(|input| input.events.clone());
                        for event in events {
                            if let egui::Event::MouseWheel { unit, delta, .. } = event {
                                let points = match unit {
                                    egui::MouseWheelUnit::Point => delta.y + delta.x,
                                    egui::MouseWheelUnit::Line => (delta.y + delta.x) * 24.,
                                    egui::MouseWheelUnit::Page => {
                                        (delta.y + delta.x) * rect.width().max(1.)
                                    }
                                };
                                self.set_thumb_offset(self.thumb_offset + points, ui.ctx());
                            }
                        }
                    }
                    if response.dragged_by(PointerButton::Primary) {
                        let delta = response.drag_delta().x;
                        self.set_thumb_offset(self.thumb_offset - delta, ui.ctx());
                    }
                    if response.clicked()
                        && let Some(position) = response.interact_pointer_pos()
                        && let Some(index) = cell_at(
                            self.thumb_offset,
                            position.x - rect.min.x,
                            self.siblings.len(),
                        )
                    {
                        let path = self.siblings[index].clone();
                        self.load(ui.ctx(), path, false);
                    }
                    if response.hovered() || response.dragged() {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                }
            });
    }

    fn settings_modal(&mut self, ctx: &egui::Context, palette: Palette) {
        let Some(mut draft) = self.settings_draft.take() else {
            return;
        };
        let mut control = self.settings_control;
        let mut action = None;
        let text = self.text();
        let modal = egui::Modal::new(egui::Id::unique("settings-modal"))
            .frame(
                EguiFrame::new()
                    .fill(palette.surface)
                    .stroke(Stroke::new(1., palette.border))
                    .corner_radius(10.)
                    .inner_margin(Margin::symmetric(18, 16)),
            )
            .show(ctx, |ui| {
                let max_height = (ctx.content_rect().height() - 96.).max(180.);
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.set_width(430.);
                        ui.label(
                            RichText::new(text.settings_title)
                                .size(19.)
                                .strong()
                                .color(palette.text),
                        );
                        ui.label(
                            RichText::new(text.settings_desc)
                                .size(12.)
                                .color(palette.muted),
                        );
                        ui.add_space(4.);
                        for (index, label, value) in [
                            (SETTING_BEFORE, text.before, draft.prefetch.before as u64),
                            (SETTING_AFTER, text.after, draft.prefetch.after as u64),
                            (SETTING_MEMORY, text.memory, draft.prefetch.memory_mib),
                        ] {
                            let step = if index == SETTING_MEMORY { 64 } else { 1 };
                            let mut decrease = false;
                            let mut increase = false;
                            EguiFrame::new()
                                .fill(if control == index {
                                    palette.accent_bg
                                } else {
                                    palette.canvas
                                })
                                .stroke(Stroke::new(
                                    1.,
                                    if control == index {
                                        palette.accent
                                    } else {
                                        palette.border
                                    },
                                ))
                                .corner_radius(6.)
                                .inner_margin(Margin::symmetric(9, 6))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add(
                                                egui::Label::new(
                                                    RichText::new(label)
                                                        .size(13.)
                                                        .color(palette.text),
                                                )
                                                .sense(Sense::click()),
                                            )
                                            .clicked()
                                        {
                                            control = index;
                                        }
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                if action_button(
                                                    ui,
                                                    "+",
                                                    true,
                                                    false,
                                                    palette,
                                                    &text.change_step(step, true),
                                                ) {
                                                    increase = true;
                                                }
                                                ui.add_sized(
                                                    [48., 24.],
                                                    egui::Label::new(
                                                        RichText::new(value.to_string())
                                                            .size(13.)
                                                            .color(palette.text),
                                                    ),
                                                );
                                                if action_button(
                                                    ui,
                                                    "-",
                                                    true,
                                                    false,
                                                    palette,
                                                    &text.change_step(step, false),
                                                ) {
                                                    decrease = true;
                                                }
                                            },
                                        );
                                    });
                                });
                            if decrease {
                                control = index;
                                match index {
                                    SETTING_BEFORE => {
                                        draft.prefetch.before = step_value(
                                            draft.prefetch.before as u64,
                                            false,
                                            step,
                                            50,
                                        )
                                            as usize
                                    }
                                    SETTING_AFTER => {
                                        draft.prefetch.after =
                                            step_value(draft.prefetch.after as u64, false, step, 50)
                                                as usize
                                    }
                                    _ => {
                                        draft.prefetch.memory_mib =
                                            step_value(draft.prefetch.memory_mib, false, step, 2048)
                                    }
                                }
                            }
                            if increase {
                                control = index;
                                if index == SETTING_BEFORE {
                                    draft.prefetch.before =
                                        step_value(draft.prefetch.before as u64, true, step, 50)
                                            as usize;
                                } else if index == SETTING_AFTER {
                                    draft.prefetch.after =
                                        step_value(draft.prefetch.after as u64, true, step, 50)
                                            as usize;
                                } else {
                                    draft.prefetch.memory_mib =
                                        step_value(draft.prefetch.memory_mib, true, step, 2048);
                                }
                            }
                        }
                        let mut toggle = false;
                        EguiFrame::new()
                            .fill(if control == SETTING_PREVIEWS {
                                palette.accent_bg
                            } else {
                                palette.canvas
                            })
                            .stroke(Stroke::new(
                                1.,
                                if control == SETTING_PREVIEWS {
                                    palette.accent
                                } else {
                                    palette.border
                                },
                            ))
                            .corner_radius(6.)
                            .inner_margin(Margin::symmetric(9, 6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(text.preview_strip)
                                            .size(13.)
                                            .color(palette.text),
                                    );
                                    if action_button(
                                        ui,
                                        if draft.thumbnails_visible {
                                            text.shown
                                        } else {
                                            text.hidden
                                        },
                                        true,
                                        draft.thumbnails_visible,
                                        palette,
                                        text.preview_tooltip,
                                    ) {
                                        toggle = true;
                                    }
                                });
                            });
                        if toggle {
                            control = SETTING_PREVIEWS;
                            draft.thumbnails_visible = !draft.thumbnails_visible;
                        }
                        let mut reading_toggle = false;
                        EguiFrame::new()
                            .fill(if control == SETTING_READING {
                                palette.accent_bg
                            } else {
                                palette.canvas
                            })
                            .stroke(Stroke::new(
                                1.,
                                if control == SETTING_READING {
                                    palette.accent
                                } else {
                                    palette.border
                                },
                            ))
                            .corner_radius(6.)
                            .inner_margin(Margin::symmetric(9, 6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(text.reading_mode)
                                                    .size(13.)
                                                    .color(palette.text),
                                            )
                                            .sense(Sense::click()),
                                        )
                                        .clicked()
                                    {
                                        control = SETTING_READING;
                                        reading_toggle = true;
                                    }
                                    if action_button(
                                        ui,
                                        if draft.reading_mode == ReadingMode::Book {
                                            text.book_rtl
                                        } else {
                                            text.single_image
                                        },
                                        true,
                                        draft.reading_mode == ReadingMode::Book,
                                        palette,
                                        text.reading_tooltip,
                                    ) {
                                        control = SETTING_READING;
                                        reading_toggle = true;
                                    }
                                });
                            });
                        if reading_toggle {
                            draft.reading_mode = toggled_reading_mode(draft.reading_mode);
                        }
                        let mut wide_toggle = false;
                        EguiFrame::new()
                            .fill(if control == SETTING_AUTO_WIDE {
                                palette.accent_bg
                            } else {
                                palette.canvas
                            })
                            .stroke(Stroke::new(
                                1.,
                                if control == SETTING_AUTO_WIDE {
                                    palette.accent
                                } else {
                                    palette.border
                                },
                            ))
                            .corner_radius(6.)
                            .inner_margin(Margin::symmetric(9, 6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(text.wide_pages)
                                                    .size(13.)
                                                    .color(palette.text),
                                            )
                                            .sense(Sense::click()),
                                        )
                                        .on_hover_text(text.auto_wide_tooltip)
                                        .clicked()
                                    {
                                        wide_toggle = true;
                                    }
                                    if action_button(
                                        ui,
                                        if draft.auto_wide {
                                            text.auto_wide_auto
                                        } else {
                                            text.auto_wide_off
                                        },
                                        true,
                                        draft.auto_wide,
                                        palette,
                                        text.auto_wide_tooltip,
                                    ) {
                                        wide_toggle = true;
                                    }
                                });
                            });
                        if wide_toggle {
                            control = SETTING_AUTO_WIDE;
                            draft.auto_wide = !draft.auto_wide;
                        }
                        let mut arrow_toggle = false;
                        EguiFrame::new()
                            .fill(if control == SETTING_ARROW {
                                palette.accent_bg
                            } else {
                                palette.canvas
                            })
                            .stroke(Stroke::new(
                                1.,
                                if control == SETTING_ARROW {
                                    palette.accent
                                } else {
                                    palette.border
                                },
                            ))
                            .corner_radius(6.)
                            .inner_margin(Margin::symmetric(9, 6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(text.book_next_spread)
                                                    .size(13.)
                                                    .color(palette.text),
                                            )
                                            .sense(Sense::click()),
                                        )
                                        .clicked()
                                    {
                                        control = SETTING_ARROW;
                                        arrow_toggle = true;
                                    }
                                    if action_button(
                                        ui,
                                        if draft.comic_arrow_direction
                                            == ComicArrowDirection::RightNext
                                        {
                                            text.right_arrow
                                        } else {
                                            text.left_arrow
                                        },
                                        true,
                                        false,
                                        palette,
                                        text.arrow_tooltip,
                                    ) {
                                        control = SETTING_ARROW;
                                        arrow_toggle = true;
                                    }
                                });
                            });
                        let mut language_toggle = false;
                        EguiFrame::new()
                            .fill(if control == SETTING_LANGUAGE {
                                palette.accent_bg
                            } else {
                                palette.canvas
                            })
                            .stroke(Stroke::new(
                                1.,
                                if control == SETTING_LANGUAGE {
                                    palette.accent
                                } else {
                                    palette.border
                                },
                            ))
                            .corner_radius(6.)
                            .inner_margin(Margin::symmetric(9, 6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(text.language)
                                                    .size(13.)
                                                    .color(palette.text),
                                            )
                                            .sense(Sense::click()),
                                        )
                                        .clicked()
                                    {
                                        control = SETTING_LANGUAGE;
                                        language_toggle = true;
                                    }
                                    if action_button(
                                        ui,
                                        text.language_name(draft.language),
                                        true,
                                        false,
                                        palette,
                                        text.language,
                                    ) {
                                        control = SETTING_LANGUAGE;
                                        language_toggle = true;
                                    }
                                });
                            });
                        if language_toggle {
                            control = SETTING_LANGUAGE;
                            draft.language = cycled_language(draft.language);
                        }
                        if let Err(error) = draft.validate() {
                            ui.label(RichText::new(error).size(11.).color(palette.error));
                        }
                        ui.add_space(4.);
                        ui.label(
                            RichText::new(text.settings_help)
                                .size(11.)
                                .color(palette.muted),
                        );
                        ui.add_space(2.);
                        ui.horizontal(|ui| {
                            if action_button(
                                ui,
                                text.restore_defaults,
                                true,
                                control == SETTING_RESTORE,
                                palette,
                                text.restore_tooltip,
                            ) {
                                control = SETTING_RESTORE;
                                action = Some(SettingsAction::Restore);
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if action_button(
                                    ui,
                                    text.apply,
                                    true,
                                    control == SETTING_APPLY,
                                    palette,
                                    text.apply_tooltip,
                                ) {
                                    control = SETTING_APPLY;
                                    action = Some(SettingsAction::Apply);
                                }
                                if action_button(
                                    ui,
                                    text.cancel,
                                    true,
                                    control == SETTING_CANCEL,
                                    palette,
                                    text.cancel_tooltip,
                                ) {
                                    control = SETTING_CANCEL;
                                    action = Some(SettingsAction::Cancel);
                                }
                            });
                        });
                    });
            });
        if modal.should_close() {
            action = Some(SettingsAction::Cancel);
        }
        match action {
            Some(SettingsAction::Restore) => {
                restore_defaults(&mut draft);
                self.settings_draft = Some(draft);
                self.settings_control = SETTING_RESTORE;
            }
            Some(SettingsAction::Cancel) => {
                self.settings_control = control;
                self.close_settings(ctx);
            }
            Some(SettingsAction::Apply) => {
                self.settings_control = control;
                self.settings_draft = Some(draft);
                self.apply_settings(ctx);
            }
            None => {
                self.settings_draft = Some(draft);
                self.settings_control = control;
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let name = self.display_name();
        let text = self.text();
        egui::Panel::top("header")
            .frame(panel_frame(palette))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Oxeye")
                            .size(17.)
                            .strong()
                            .color(palette.text),
                    );
                    ui.add_space(6.);
                    truncated_label(ui, name, 12., palette.muted, 330.);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if action_button(
                            ui,
                            text.fullscreen,
                            true,
                            self.fullscreen,
                            palette,
                            text.fullscreen_tooltip,
                        ) {
                            self.toggle_fullscreen(ui.ctx());
                        }
                        if action_button(
                            ui,
                            text.settings,
                            true,
                            self.settings_draft.is_some(),
                            palette,
                            text.settings_tooltip,
                        ) {
                            self.open_settings(ui.ctx());
                        }
                        if action_button(
                            ui,
                            if self.settings.thumbnails_visible {
                                text.previews_hide
                            } else {
                                text.previews_show
                            },
                            true,
                            self.settings.thumbnails_visible,
                            palette,
                            text.previews_tooltip,
                        ) {
                            self.toggle_previews(ui.ctx());
                        }
                        if action_button(ui, text.open, true, false, palette, text.open_tooltip) {
                            self.open(ui.ctx());
                        }
                    });
                });
            });
    }

    fn error_banner(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let detail = self.error.clone().unwrap_or_default();
        let text = self.text();
        let title = text.error_title;
        egui::Panel::top("error-banner")
            .frame(
                EguiFrame::new()
                    .fill(palette.error_bg)
                    .inner_margin(Margin::symmetric(14, 7)),
            )
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).size(12.).strong().color(palette.error));
                    ui.add(
                        egui::Label::new(RichText::new(detail).size(11.).color(palette.error))
                            .wrap(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if action_button(
                            ui,
                            text.dismiss,
                            true,
                            false,
                            palette,
                            text.dismiss_tooltip,
                        ) {
                            self.error = None;
                        }
                    });
                });
            });
    }

    fn footer(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let text = self.text();
        let position = self.position_label();
        let zoom = self.zoom_label(ui.ctx());
        let status = self.status_text();
        let has_image = self.tiled.is_some();
        let editing = self.editing();
        let previous = footer_navigation_enabled(
            editing,
            !self.navigation.is_idle() || self.neighbor(false).is_some(),
        );
        let next = footer_navigation_enabled(
            editing,
            !self.navigation.is_idle() || self.neighbor(true).is_some(),
        );
        let playing = self.playing();
        let has_playback = self.has_animation();
        let finished = self.all_finished();
        let can_rotate = has_image && !self.loading_file;
        let stats = self.cache_stats;
        let queued = self.navigation.pending_len();
        let viewport = self.viewport;
        let wide = self.position().map(|position| {
            self.page_overrides
                .get(&position)
                .copied()
                .unwrap_or_default()
        });
        egui::Panel::bottom("footer")
            .frame(panel_frame(palette))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if action_button(ui, text.prev, previous, false, palette, text.prev_tooltip) {
                        self.enqueue_navigation(Navigation::Previous, ui.ctx());
                    }
                    if action_button(ui, text.next, next, false, palette, text.next_tooltip) {
                        self.enqueue_navigation(Navigation::Next, ui.ctx());
                    }
                    ui.label(RichText::new(position).size(11.).color(palette.muted));
                    if let Some(mode) = wide
                        && action_button(
                            ui,
                            text.wide_label(mode),
                            footer_navigation_enabled(editing, true),
                            mode != PageOverride::Auto,
                            palette,
                            text.wide_tooltip,
                        )
                    {
                        self.toggle_wide_page(ui.ctx());
                    }
                    ui.separator();
                    if action_button(ui, "-", has_image, false, palette, text.zoom_out_tooltip) {
                        self.zoom_by(0.8, None, viewport, ui.ctx());
                    }
                    ui.label(RichText::new(zoom).size(12.).color(palette.text));
                    if action_button(ui, "+", has_image, false, palette, text.zoom_in_tooltip) {
                        self.zoom_by(1.25, None, viewport, ui.ctx());
                    }
                    if action_button(
                        ui,
                        text.fit,
                        has_image,
                        matches!(self.zoom, Zoom::Fit),
                        palette,
                        text.fit_tooltip,
                    ) {
                        self.zoom = Zoom::Fit;
                        self.pan = [0., 0.];
                    }
                    if action_button(
                        ui,
                        text.actual,
                        has_image,
                        matches!(self.zoom, Zoom::Actual),
                        palette,
                        text.actual_tooltip,
                    ) {
                        self.zoom = Zoom::Actual;
                        self.pan = [0., 0.];
                    }
                    if has_playback
                        && action_button(
                            ui,
                            if playing {
                                text.pause
                            } else if finished && self.display_mode == ReadingMode::Book {
                                text.replay
                            } else {
                                text.play
                            },
                            !self.loading_file,
                            playing,
                            palette,
                            text.playback_tooltip,
                        )
                    {
                        self.toggle_playback(ui.ctx());
                    }
                    if action_button(
                        ui,
                        text.rotate_right,
                        can_rotate,
                        false,
                        palette,
                        text.rotate_right_tooltip,
                    ) {
                        self.perform(Action::RotateRight, viewport, ui.ctx());
                    }
                    if action_button(
                        ui,
                        text.rotate_left,
                        can_rotate,
                        false,
                        palette,
                        text.rotate_left_tooltip,
                    ) {
                        self.perform(Action::RotateLeft, viewport, ui.ctx());
                    }
                });
                ui.add_space(2.);
                ui.horizontal(|ui| {
                    truncated_label(ui, status, 11., palette.muted, 280.);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(text.prefetch_stats(
                                stats.entries,
                                stats.bytes as f64 / 1_048_576.,
                                stats.loading,
                                queued,
                            ))
                            .size(11.)
                            .color(palette.muted),
                        );
                    });
                });
            });
    }

    fn edit_toolbar(&mut self, ui: &mut egui::Ui, palette: Palette) {
        if !self.editing() {
            return;
        }
        let text = self.text();
        let Some(edit) = self.edit_ui.as_ref() else {
            return;
        };
        let crop = edit.crop.as_ref().map(|crop| (crop.aspect, crop.rect));
        let dirty = edit.session.dirty();
        let archive = edit.source.is_archive();
        let blocked = edit.saving.is_some() || edit.confirm.is_some();
        let viewport = self.viewport;
        let zoom_scale = self.scale_for(
            self.zoom,
            self.dimensions,
            viewport.size(),
            ui.ctx().pixels_per_point(),
        );
        // Groups stay in one wrapped row: transform, history, then output. The
        // row never nests a right-to-left layout, so a narrow window wraps whole
        // buttons in reading order instead of scattering them.
        egui::Panel::top("edit-toolbar")
            .frame(panel_frame(palette))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    // The mode stays visible in a clean crop too: the canvas
                    // badge is hidden while cropping, so this is the anchor.
                    let (dot, _) = ui.allocate_exact_size(egui::vec2(6., 6.), Sense::hover());
                    ui.painter().circle_filled(
                        dot.center(),
                        3.,
                        if dirty {
                            palette.warning
                        } else {
                            palette.accent
                        },
                    );
                    ui.label(
                        RichText::new(if dirty {
                            text.editing_unsaved
                        } else {
                            text.editing
                        })
                        .size(11.)
                        .color(if dirty {
                            palette.warning
                        } else {
                            palette.accent
                        }),
                    );
                    ui.separator();
                    if let Some((aspect, _)) = crop {
                        if let Some(value) = aspect_segments(
                            ui,
                            &[
                                (CropAspect::Free, text.aspect_free),
                                (CropAspect::Square, text.aspect_square),
                                (CropAspect::FourThree, text.aspect_four_three),
                                (CropAspect::ThreeTwo, text.aspect_three_two),
                                (CropAspect::SixteenNine, text.aspect_sixteen_nine),
                                (CropAspect::Original, text.aspect_original),
                            ],
                            aspect,
                            !blocked,
                            palette,
                            text.crop_aspect_tooltip,
                        ) {
                            self.set_crop_aspect(value);
                        }
                        if action_button(
                            ui,
                            text.crop_apply,
                            !blocked,
                            true,
                            palette,
                            text.crop_apply_tooltip,
                        ) {
                            self.perform(Action::CropApply, viewport, ui.ctx());
                        }
                        if action_button(
                            ui,
                            text.crop_cancel,
                            !blocked,
                            false,
                            palette,
                            text.crop_cancel_tooltip,
                        ) {
                            self.perform(Action::CropCancel, viewport, ui.ctx());
                        }
                    } else {
                        if action_button(ui, text.crop, !blocked, false, palette, text.crop_tooltip)
                        {
                            self.enter_crop();
                        }
                        if action_button(
                            ui,
                            text.rotate_left,
                            !blocked,
                            false,
                            palette,
                            text.edit_rotate_tooltip,
                        ) {
                            self.perform(Action::RotateLeft, viewport, ui.ctx());
                        }
                        if action_button(
                            ui,
                            text.rotate_right,
                            !blocked,
                            false,
                            palette,
                            text.edit_rotate_tooltip,
                        ) {
                            self.perform(Action::RotateRight, viewport, ui.ctx());
                        }
                        if action_button(
                            ui,
                            text.flip_h,
                            !blocked,
                            false,
                            palette,
                            text.flip_h_tooltip,
                        ) {
                            self.perform(Action::FlipHorizontal, viewport, ui.ctx());
                        }
                        if action_button(
                            ui,
                            text.flip_v,
                            !blocked,
                            false,
                            palette,
                            text.flip_v_tooltip,
                        ) {
                            self.perform(Action::FlipVertical, viewport, ui.ctx());
                        }
                        ui.separator();
                        if action_button(
                            ui,
                            text.undo,
                            !blocked && dirty,
                            false,
                            palette,
                            text.undo_tooltip,
                        ) {
                            self.perform(Action::UndoEdit, viewport, ui.ctx());
                        }
                        ui.separator();
                        if action_button(
                            ui,
                            text.save_as,
                            !blocked,
                            false,
                            palette,
                            text.save_as_tooltip,
                        ) {
                            self.perform(Action::SaveAs, viewport, ui.ctx());
                        }
                        // A member of an archive has no file to replace; saying so
                        // here beats failing after the confirmation.
                        let (save_enabled, save_hint) = if archive {
                            (false, text.archive_cannot_overwrite)
                        } else {
                            (!blocked, text.save_tooltip)
                        };
                        if danger_button(ui, text.save, save_enabled, palette, save_hint) {
                            self.perform(Action::SaveOverwrite, viewport, ui.ctx());
                        }
                        if action_button(
                            ui,
                            text.edit_exit,
                            !blocked,
                            false,
                            palette,
                            text.edit_exit_tooltip,
                        ) {
                            self.perform(Action::EditExit, viewport, ui.ctx());
                        }
                    }
                });
                if let Some((_, rect)) = crop {
                    // Wrap instead of overflowing: on a 560 pt window the zoom
                    // hint moves to the next row. Cap it at 240 pt using the
                    // helper's reserve argument, so it keeps the usual ellipsis.
                    ui.horizontal_wrapped(|ui| {
                        truncated_label(ui, text.crop_hint, 11., palette.muted, 210.);
                        ui.label(
                            RichText::new(text.dimensions(rect.w, rect.h))
                                .size(11.)
                                .monospace()
                                .color(palette.text),
                        );
                        if viewport.width() > 0.
                            && crop_needs_zoom(zoom_scale, ui.ctx().pixels_per_point())
                        {
                            let remaining = ui.available_width();
                            let reserve = (remaining - 240.).max(0.);
                            truncated_label(ui, text.crop_zoom_hint, 11., palette.warning, reserve);
                        }
                    });
                }
            });
    }

    fn save_confirm_modal(&mut self, ctx: &egui::Context, palette: Palette) {
        let Some(confirm) = self.edit_ui.as_ref().and_then(|edit| edit.confirm.as_ref()) else {
            return;
        };
        let path = confirm.path.clone();
        let format = confirm.format;
        let overwrite = confirm.overwrite;
        let Some(edit) = self.edit_ui.as_ref() else {
            return;
        };
        let preview = Arc::clone(edit.session.preview());
        let mut quality = edit.quality;
        let transparent =
            format == export::ExportFormat::Jpeg && export::has_transparency(&preview);
        let text = self.text();
        // Closing blocks new writes; show that here instead of failing after the
        // fact.
        let blocked = self.close_state.requested;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let warnings = save_warning_rows(text, format, overwrite, transparent);
        let mut confirm_clicked = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::unique("save-confirm"))
            .frame(
                EguiFrame::new()
                    .fill(palette.surface)
                    .stroke(Stroke::new(1., palette.border))
                    .corner_radius(10.)
                    .inner_margin(Margin::symmetric(18, 16)),
            )
            .show(ctx, |ui| {
                // egui can turn Enter into a click on the focused button; strip
                // every pressed Enter before any widget is built. The buttons
                // below are the only way to confirm.
                ui.input_mut(|input| {
                    input
                        .events
                        .retain(|event| !save_confirm_strips_event(event));
                });
                ui.set_width(460.);
                ui.add(
                    egui::Label::new(
                        RichText::new(text.save_confirm_heading(&name, overwrite))
                            .size(17.)
                            .strong()
                            .color(palette.text),
                    )
                    .wrap(),
                );
                ui.add_space(2.);
                let parent = path
                    .parent()
                    .map(|parent| parent.display().to_string())
                    .unwrap_or_default();
                if !parent.is_empty() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(parent)
                                .size(11.)
                                .monospace()
                                .color(palette.muted),
                        )
                        .wrap(),
                    );
                }
                // Keep the file name on one line: truncation beats splitting the
                // extension away from the stem in the confirmation dialog.
                ui.add(
                    egui::Label::new(
                        RichText::new(&name)
                            .size(11.)
                            .monospace()
                            .color(palette.text),
                    )
                    .truncate(),
                );
                ui.label(
                    RichText::new(text.save_confirm_format(format_label(format)))
                        .size(11.)
                        .color(palette.muted),
                );
                if !warnings.is_empty() {
                    ui.add_space(8.);
                    EguiFrame::new()
                        .fill(palette.warning_bg)
                        .stroke(Stroke::new(1., palette.warning))
                        .corner_radius(6.)
                        .inner_margin(Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            for row in warnings {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("•  {row}"))
                                            .size(11.5)
                                            .color(palette.warning),
                                    )
                                    .wrap(),
                                );
                            }
                        });
                }
                if format == export::ExportFormat::Jpeg {
                    ui.add_space(8.);
                    ui.add(
                        egui::Slider::new(
                            &mut quality,
                            export::JPEG_QUALITY_MIN..=export::JPEG_QUALITY_MAX,
                        )
                        .text(text.save_confirm_quality)
                        .max_decimals(0),
                    );
                }
                ui.add_space(6.);
                ui.label(
                    RichText::new(text.save_confirm_atomic)
                        .size(11.)
                        .color(palette.muted),
                );
                ui.add_space(10.);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (label, hint) = if overwrite {
                            (text.save_confirm_replace, text.save_confirm_replace_tooltip)
                        } else {
                            (text.save, text.save_as_tooltip)
                        };
                        if danger_button(ui, label, !blocked, palette, hint) {
                            confirm_clicked = true;
                        }
                        if action_button(ui, text.cancel, true, false, palette, text.cancel_tooltip)
                        {
                            cancel = true;
                        }
                    });
                });
            });
        if let Some(edit) = self.edit_ui.as_mut() {
            edit.quality = quality;
        }
        if confirm_clicked {
            if let Some(edit) = self.edit_ui.as_mut() {
                edit.confirm = None;
            }
            self.start_save(path, format, overwrite, ctx);
        } else if (cancel || modal.should_close())
            && let Some(edit) = self.edit_ui.as_mut()
        {
            edit.confirm = None;
        }
    }

    fn resolve_pending_edit(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.edit_pending.as_ref() else {
            return;
        };
        if self.loading_file || self.busy() {
            return;
        }
        let media_matches = self
            .media
            .as_ref()
            .is_some_and(|media| media.source == pending.source);
        let source_ready = media_matches
            && self.selected.as_ref() == Some(&pending.source)
            && self.displayed.as_ref() == Some(&pending.source)
            && self.display_mode == ReadingMode::Single
            && self.other_pages.is_empty()
            && self.media.as_ref().is_some_and(|media| {
                media.frames.len() == 1 && media.loop_mode == model::LoopMode::Once
            })
            && editable_format(&pending.source).is_some();
        if !source_ready {
            self.cancel_pending_edit(ctx);
            return;
        }
        let pending = self.edit_pending.take().expect("checked above");
        self.begin_edit(
            ctx,
            pending.rotation,
            Some(pending.restore),
            pending.source,
            pending.crop,
        );
    }

    fn canvas(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let viewport = response.rect;
        painter.rect_filled(viewport, 0., palette.canvas);
        if viewport.width() < 1. || viewport.height() < 1. {
            return;
        }
        self.viewport = viewport;
        if self.settings_draft.is_none() {
            self.handle_keys(viewport, ui.ctx());
            self.handle_wheel(&response, viewport, ui.ctx());
            if response.dragged_by(PointerButton::Primary)
                && self.tiled.is_some()
                && !self.crop_active()
            {
                let delta = response.drag_delta();
                self.pan[0] += delta.x;
                self.pan[1] += delta.y;
                ui.ctx().request_repaint();
            }
            if self.tiled.is_some() && response.hovered() {
                ui.ctx().set_cursor_icon(if response.dragged() {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Grab
                });
            }
        }
        let ctx = ui.ctx();
        let dpi = ctx.pixels_per_point();
        let candidate = self.candidate.as_ref().map(|candidate| {
            (
                std::iter::once(candidate.image.tiled.clone())
                    .chain(candidate.image.others.iter().map(|page| page.tiled.clone()))
                    .collect::<Vec<_>>(),
                candidate.image.dimensions,
                candidate.image.loaded,
            )
        });
        let mut painted = Vec::new();
        if let Some((pages, dimensions, loaded)) = candidate {
            let zoom = if loaded { Zoom::Fit } else { self.zoom };
            let pan = if loaded { [0., 0.] } else { self.pan };
            let scale = self.scale_for(zoom, dimensions, viewport.size(), dpi);
            painted = self.paint_pages(ctx, &painter, viewport, &pages, scale, pan);
            if !ctx.will_discard()
                && let Some(candidate) = &mut self.candidate
            {
                candidate.painted_frame = Some(ctx.cumulative_frame_nr());
            }
        } else {
            let pages = self.displayed_tiles();
            if pages.is_empty() {
                self.draw_empty(&painter, viewport, palette);
            } else {
                let scale = self.scale_for(self.zoom, self.dimensions, viewport.size(), dpi);
                painted = self.paint_pages(ctx, &painter, viewport, &pages, scale, self.pan);
                if !ctx.will_discard()
                    && let Some(nav) = &mut self.nav
                {
                    nav.shown_frame.get_or_insert(ctx.cumulative_frame_nr());
                }
            }
        }
        self.textures.retain(&painted);
        if self.crop_active() {
            self.crop_layer(ui.ctx(), &response, &painter, viewport, palette);
        }
        self.draw_badge(&painter, viewport, palette);
    }

    fn paint_pages(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        viewport: Rect,
        pages: &[Arc<TiledFrame>],
        scale: f32,
        pan: [f32; 2],
    ) -> Vec<(usize, usize)> {
        if pages.len() <= 1 {
            return pages
                .first()
                .map(|page| self.paint_tiles(ctx, painter, viewport, page, scale, pan))
                .unwrap_or_default();
        }
        let page_dimensions: Vec<_> = pages.iter().map(|page| page.dimensions).collect();
        let (_, centers) = spread_layout(&page_dimensions);
        let mut painted = Vec::new();
        for (page, center) in pages.iter().zip(centers) {
            painted.extend(self.paint_tiles(
                ctx,
                painter,
                viewport,
                page,
                scale,
                [pan[0] + center[0] * scale, pan[1] + center[1] * scale],
            ));
        }
        painted
    }

    fn paint_tiles(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        viewport: Rect,
        frame: &Arc<TiledFrame>,
        scale: f32,
        pan: [f32; 2],
    ) -> Vec<(usize, usize)> {
        let dpi = ctx.pixels_per_point() as f64;
        let frame_ptr = Arc::as_ptr(frame) as usize;
        let center = viewport.center();
        let origin = [
            center.x as f64 - frame.dimensions.0 as f64 * scale as f64 / 2. + pan[0] as f64,
            center.y as f64 - frame.dimensions.1 as f64 * scale as f64 / 2. + pan[1] as f64,
        ];
        let visible = [
            viewport.min.x as f64,
            viewport.min.y as f64,
            viewport.max.x as f64,
            viewport.max.y as f64,
        ];
        let mut painted = Vec::new();
        for (index, tile) in frame.tiles.iter().enumerate() {
            let projected = tile.geometry.project(origin, scale as f64, dpi);
            if !intersects(projected.core, visible) {
                continue;
            }
            let core = rect_from(projected.core).intersect(viewport);
            if core.width() <= 0. || core.height() <= 0. {
                continue;
            }
            let texture = self.textures.texture(ctx, frame, index, frame_ptr);
            let sample = tile.geometry.sample;
            let destination = Rect::from_min_size(
                Pos2::new(projected.sample[0] as f32, projected.sample[1] as f32),
                egui::vec2(sample.width as f32 * scale, sample.height as f32 * scale),
            );
            painter.with_clip_rect(core).image(
                texture,
                destination,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)),
                Color32::WHITE,
            );
            painted.push((frame_ptr, index));
        }
        painted
    }

    fn draw_empty(&self, painter: &egui::Painter, viewport: Rect, palette: Palette) {
        let center = viewport.center();
        let text = self.text();
        let (title, subtitle) = if self.loading_file {
            (text.opening_image, text.reading_file)
        } else {
            (text.open_image, text.drop_hint)
        };
        painter.text(
            Pos2::new(center.x, center.y - 34.),
            Align2::CENTER_CENTER,
            title,
            FontId::proportional(25.),
            palette.text,
        );
        painter.text(
            Pos2::new(center.x, center.y),
            Align2::CENTER_CENTER,
            subtitle,
            FontId::proportional(13.),
            palette.muted,
        );
        if !self.loading_file {
            painter.text(
                Pos2::new(center.x, center.y + 28.),
                Align2::CENTER_CENTER,
                text.formats,
                FontId::proportional(11.),
                palette.muted,
            );
        }
    }

    /// Temporary state over the canvas: a small translucent pill in the top-left
    /// corner, away from the image centre and light enough to read the pixels
    /// underneath.
    fn draw_badge(&self, painter: &egui::Painter, viewport: Rect, palette: Palette) {
        // While cropping the pill would sit on top of the corner handles, and the
        // crop frame already says the app is in edit mode.
        if self.tiled.is_none() || self.crop_active() {
            return;
        }
        let rotating = self.rotation.requested != self.rotation.displayed;
        let text = self.text();
        let editing = self.edit_ui.as_ref().map(|edit| {
            if edit.session.dirty() {
                (text.editing_unsaved, palette.warning)
            } else {
                (text.editing, palette.accent)
            }
        });
        let (message, dot) = if self.loading_file {
            let message = self
                .selected
                .as_ref()
                .map(|source| text.opening_file(&source_label(source)))
                .unwrap_or_else(|| text.opening_image.to_owned());
            (message, palette.muted)
        } else if let Some((editing, dot)) = editing {
            (editing.to_owned(), dot)
        } else if rotating {
            (text.rotating.to_owned(), palette.muted)
        } else {
            return;
        };
        let galley = painter.layout_no_wrap(message, FontId::proportional(11.5), palette.text);
        let padding = egui::vec2(9., 5.);
        let dot_space = 12.;
        let background = Rect::from_min_size(
            viewport.min + egui::vec2(12., 12.),
            galley.size() + egui::vec2(padding.x * 2. + dot_space, padding.y * 2.),
        );
        painter.rect_filled(background, 5., translucent(palette.surface, 228));
        painter.rect_stroke(
            background,
            5.,
            Stroke::new(1., translucent(palette.border, 200)),
            egui::StrokeKind::Inside,
        );
        painter.circle_filled(
            Pos2::new(background.left() + padding.x + 3., background.center().y),
            3.,
            dot,
        );
        painter.galley(
            Pos2::new(
                background.left() + padding.x + dot_space,
                background.top() + padding.y,
            ),
            galley,
            palette.text,
        );
    }
}

impl Drop for Oxeye {
    fn drop(&mut self) {
        self.cancel_navigation();
        self.thumbnails.shutdown();
        self.prefetch.shutdown();
    }
}

impl eframe::App for Oxeye {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.fullscreen = ctx
            .input(|input| input.viewport().fullscreen)
            .unwrap_or(self.fullscreen);
        self.handle_close_request(ctx);
        if let Err(error) = self.flush_progress(ctx, false) {
            self.set_feedback(
                FeedbackKind::Error,
                self.text().progress_save_failed(&error),
                ctx,
            );
        }
        self.expire_feedback(ctx);
        self.poll_saves(ctx);
        if !self.close_state.requested && self.settings_draft.is_some() {
            self.handle_settings_keys(ctx);
        }
        if let Some(path) = self.initial.take() {
            self.open_path(path, ctx);
        }
        if self.settings_draft.is_none() && !self.editing() {
            let dropped = ctx.input(|input| {
                input
                    .raw
                    .dropped_files
                    .first()
                    .map(|dropped| dropped.path().to_path_buf())
            });
            if let Some(path) = dropped {
                self.open_path(path, ctx);
            }
        }
        self.poll_workers(ctx);
        self.advance_frames(ctx);
        self.resolve_pending_edit(ctx);
        self.poll_save(ctx);
        self.fill_cached_dimensions();
        self.tick(ctx);
        self.sync_prefetch(ctx);
        self.poll_thumbnails(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let palette = self.palette(ui.ctx());
        self.header(ui, palette);
        self.notice_banner(ui, palette);
        if self.error.is_some() {
            self.error_banner(ui, palette);
        }
        self.edit_toolbar(ui, palette);
        self.footer(ui, palette);
        self.preview_strip(ui, palette);
        egui::CentralPanel::no_frame()
            .frame(EguiFrame::new().fill(palette.canvas))
            .show(ui, |ui| self.canvas(ui, palette));
        self.settings_modal(ui.ctx(), palette);
        self.save_confirm_modal(ui.ctx(), palette);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        Color32::from_rgb(0x14, 0x19, 0x1c).to_normalized_gamma_f32()
    }
}

fn main() -> eframe::Result {
    let initial = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1060., 760.])
            .with_min_inner_size([560., 360.])
            .with_title("Oxeye"),
        renderer: eframe::Renderer::Glow,
        system_font_fallback: true,
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "Oxeye",
        options,
        Box::new(move |cc| Ok(Box::new(Oxeye::new(cc, initial)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use viewer::MAX_TILE;

    fn load_work(path: PathBuf) -> Work {
        let prefetch = Prefetcher::new(PrefetchSettings {
            memory_mib: 0,
            ..Default::default()
        });
        requested_work(path, &prefetch, ReadingMode::Single)
    }

    fn requested_work(path: PathBuf, prefetch: &Prefetcher, mode: ReadingMode) -> Work {
        requested_source_work(SourceRef::file(&path), prefetch, mode)
    }

    fn requested_source_work(path: SourceRef, prefetch: &Prefetcher, mode: ReadingMode) -> Work {
        requested_source_work_spread(path, prefetch, mode, vec![0])
    }

    fn requested_source_work_spread(
        path: SourceRef,
        prefetch: &Prefetcher,
        mode: ReadingMode,
        spread: Vec<usize>,
    ) -> Work {
        let foreground = prefetch.foreground_requested_source(&path);
        Work::Load(
            path,
            prefetch.clone(),
            foreground,
            MAX_TILE,
            mode,
            spread,
            Language::English,
            0,
        )
    }

    fn write_test_gif(path: &Path) {
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = gif::Encoder::new(file, 1, 1, &[255, 0, 0, 0, 255, 0]).unwrap();
        encoder.set_repeat(gif::Repeat::Infinite).unwrap();
        for index in [0, 1] {
            let frame = gif::Frame {
                width: 1,
                height: 1,
                delay: 2,
                buffer: std::borrow::Cow::Owned(vec![index]),
                ..Default::default()
            };
            encoder.write_frame(&frame).unwrap();
        }
    }

    fn test_settings() -> AppSettings {
        AppSettings {
            language: Language::English,
            prefetch: PrefetchSettings {
                before: 0,
                after: 2,
                memory_mib: 8,
            },
            ..Default::default()
        }
    }

    fn test_app() -> Oxeye {
        // A path that must not exist: these tests never persist progress.
        test_app_at(Path::new("oxeye-test-progress.json"))
    }

    fn test_app_at(progress: &Path) -> Oxeye {
        Oxeye::from_parts(
            None,
            test_settings(),
            None,
            ProgressStore::load_from(progress),
        )
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "background UI work timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn finish_load(app: &mut Oxeye, ctx: &egui::Context) {
        wait_until(|| {
            app.poll_workers(ctx);
            !app.loader.running && app.loader.pending.is_none()
        });
        if app.candidate.is_some() {
            app.commit_candidate(ctx);
        }
        if let Some(nav) = app.nav.take() {
            assert!(app.navigation.crossed_frame(nav.ticket));
        }
        app.sync_prefetch(ctx);
    }

    fn png_bytes(color: u8) -> Vec<u8> {
        png_of(2, 3, color)
    }

    fn png_of(width: u32, height: u32, color: u8) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        RgbaImage::from_pixel(width, height, image::Rgba([color, 20, 30, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn write_comic(path: &Path) {
        let first = png_bytes(1);
        let second = png_bytes(2);
        let tenth = png_bytes(10);
        model::source::test_support::write_zip(
            path,
            &[
                ("chapter/page10.png", &tenth),
                ("chapter/page2.png", &second),
                ("chapter/page1.png", &first),
                ("readme.txt", b"not a page"),
            ],
        );
    }

    #[test]
    fn archives_open_navigate_pair_preview_preload_and_copy_entry_pixels() {
        let directory = tempfile::tempdir().unwrap();
        for extension in ["cbz", "zip"] {
            let archive = directory.path().join(format!("comic.{extension}"));
            write_comic(&archive);
            let ctx = egui::Context::default();
            let mut app = test_app();
            app.thumb_width = 3. * CELL_WIDTH;
            app.open_path(archive.clone(), &ctx);
            let sources = app.siblings.clone();
            assert_eq!(
                sources
                    .iter()
                    .map(SourceRef::display_name)
                    .collect::<Vec<_>>(),
                ["page1.png", "page2.png", "page10.png"]
            );
            assert!(
                sources
                    .iter()
                    .all(|source| source.archive_root() == Some(archive.as_path()))
            );
            finish_load(&mut app, &ctx);
            assert_eq!(app.selected, Some(sources[0].clone()));
            assert_eq!(app.displayed_paths(), vec![sources[0].clone()]);
            assert_eq!(app.position_label(), "1 / 3");
            assert_eq!(app.display_name(), format!("comic.{extension} / page1.png"));
            assert!(app.error.is_none());

            wait_until(|| {
                sources[1..].iter().all(|source| {
                    app.prefetch
                        .peek_source(source, &AtomicBool::new(false))
                        .is_some()
                })
            });
            let cached = app
                .prefetch
                .peek_source(&sources[1], &AtomicBool::new(false))
                .unwrap();
            assert_eq!(cached.source, sources[1]);
            assert_eq!(cached.frames[0].pixels.get_pixel(0, 0).0[0], 2);
            assert!(
                app.prefetch
                    .peek_source(&SourceRef::file(&archive), &AtomicBool::new(false))
                    .is_none()
            );
            wait_until(|| {
                sources
                    .iter()
                    .all(|source| app.thumbnails.get_source(source).is_some())
            });
            app.sync_thumbnails(&ctx);
            assert_eq!(app.thumb_requests, sources);
            assert_eq!(app.thumb_textures.map.len(), 3);
            for (source, color) in sources.iter().zip([1, 2, 10]) {
                assert_eq!(
                    app.thumb_textures.map[source].source.get_pixel(0, 0).0[0],
                    color
                );
            }

            let action = key_action(Key::PageDown, Modifiers::NONE).unwrap();
            app.perform(action, Rect::NOTHING, &ctx);
            finish_load(&mut app, &ctx);
            assert_eq!(app.selected, Some(sources[1].clone()));
            assert_eq!(app.position_label(), "2 / 3");
            assert_eq!(app.neighbor(false), Some(sources[0].clone()));
            assert_eq!(app.neighbor(true), Some(sources[2].clone()));
            assert!(
                Arc::ptr_eq(app.media.as_ref().unwrap(), &cached),
                "foreground uses the entry cache key"
            );

            assert!(app.transition_to_mode(ReadingMode::Book, &ctx));
            finish_load(&mut app, &ctx);
            assert_eq!(app.displayed_paths(), sources[1..]);
            assert_eq!(app.position_label(), "2–3 / 3");
            app.copy_current_view(&ctx);
            let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
            let copied = output
                .platform_output
                .commands
                .iter()
                .find_map(|command| {
                    if let egui::OutputCommand::CopyImage(image) = command {
                        Some(image)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(copied.size, [4 + viewer::BOOK_GAP as usize, 3]);
            assert_eq!(copied.pixels[0], Color32::from_rgb(10, 20, 30));
            assert_eq!(
                copied.pixels[2 + viewer::BOOK_GAP as usize],
                Color32::from_rgb(2, 20, 30)
            );
            output.textures_delta.clear();

            app.edge(false, &ctx);
            finish_load(&mut app, &ctx);
            assert_eq!(
                app.displayed_paths(),
                vec![sources[0].clone()],
                "Book cover is single"
            );
            app.edge(true, &ctx);
            finish_load(&mut app, &ctx);
            assert_eq!(app.displayed_paths(), sources[1..]);
        }
    }

    #[test]
    fn reopening_an_archive_restores_the_last_committed_page() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        write_comic(&archive);
        let progress = directory.path().join("progress.json");
        let ctx = egui::Context::default();

        let mut app = test_app_at(&progress);
        app.open_path(archive.clone(), &ctx);
        let sources = app.siblings.clone();
        finish_load(&mut app, &ctx);
        app.load(&ctx, sources[2].clone(), false);
        finish_load(&mut app, &ctx);
        assert_eq!(app.selected, Some(sources[2].clone()));
        app.flush_progress(&ctx, true).unwrap();
        assert!(progress.exists());

        let mut reopened = test_app_at(&progress);
        reopened.open_path(archive, &ctx);
        assert_eq!(reopened.selected, Some(sources[2].clone()));
        finish_load(&mut reopened, &ctx);
        assert_eq!(reopened.displayed, Some(sources[2].clone()));
        assert_eq!(reopened.position_label(), "3 / 3");
    }

    #[test]
    fn progress_records_on_commit_and_waits_for_the_debounce() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        write_comic(&archive);
        let progress = directory.path().join("progress.json");
        let ctx = egui::Context::default();

        let mut app = test_app_at(&progress);
        app.open_path(archive, &ctx);
        let sources = app.siblings.clone();
        finish_load(&mut app, &ctx);
        assert!(app.progress_dirty);
        assert!(app.progress_due.is_some());

        app.flush_progress(&ctx, false).unwrap();
        assert!(app.progress_dirty, "a pending debounce must not write yet");
        assert!(!progress.exists());

        app.flush_progress(&ctx, true).unwrap();
        assert!(!app.progress_dirty);
        assert!(app.progress_due.is_none());
        let store = ProgressStore::load_from(&progress);
        let restored = store.restore(&sources[0]).unwrap();
        assert_eq!(restored.entry_index, 2);
        assert_eq!(restored.entry_name, "chapter/page1.png");
    }

    #[test]
    fn changed_archive_fingerprint_ignores_saved_progress() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        write_comic(&archive);
        let progress = directory.path().join("progress.json");
        let ctx = egui::Context::default();

        let mut app = test_app_at(&progress);
        app.open_path(archive.clone(), &ctx);
        let sources = app.siblings.clone();
        finish_load(&mut app, &ctx);
        app.load(&ctx, sources[2].clone(), false);
        finish_load(&mut app, &ctx);
        app.flush_progress(&ctx, true).unwrap();

        let first = png_bytes(1);
        let second = png_bytes(2);
        let third = png_bytes(3);
        let tenth = png_bytes(10);
        model::source::test_support::write_zip(
            &archive,
            &[
                ("chapter/page10.png", &tenth),
                ("chapter/page2.png", &second),
                ("chapter/page1.png", &first),
                ("readme.txt", b"not a page"),
                ("chapter/page3.png", &third),
            ],
        );

        let mut reopened = test_app_at(&progress);
        reopened.open_path(archive, &ctx);
        let pages = reopened.siblings.clone();
        assert!(
            reopened.progress.restore(&pages[0]).is_none(),
            "the rewritten archive has a new fingerprint"
        );
        assert_eq!(
            reopened.selected,
            Some(pages[0].clone()),
            "stale progress must not select the old page"
        );
        finish_load(&mut reopened, &ctx);
        assert_eq!(reopened.display_name(), "comic.cbz / page1.png");
    }

    #[test]
    fn closing_saves_a_pending_reading_position() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        write_comic(&archive);
        let progress = directory.path().join("progress.json");
        let ctx = egui::Context::default();

        let mut app = test_app_at(&progress);
        app.open_path(archive, &ctx);
        let sources = app.siblings.clone();
        finish_load(&mut app, &ctx);
        app.load(&ctx, sources[2].clone(), false);
        finish_load(&mut app, &ctx);
        assert!(app.progress_dirty);

        app.request_close(&ctx);
        assert!(!app.progress_dirty);
        let restored = ProgressStore::load_from(&progress)
            .restore(&sources[2])
            .unwrap();
        assert_eq!(
            (restored.entry_index, restored.entry_name.as_str()),
            (0, "chapter/page10.png")
        );
    }

    #[test]
    fn folder_open_ignores_saved_progress() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("1.png");
        let second = directory.path().join("2.png");
        std::fs::write(&first, png_bytes(1)).unwrap();
        std::fs::write(&second, png_bytes(2)).unwrap();
        let progress = directory.path().join("progress.json");
        let ctx = egui::Context::default();

        let mut app = test_app_at(&progress);
        app.open_path(first.clone(), &ctx);
        finish_load(&mut app, &ctx);
        app.flush_progress(&ctx, true).unwrap();

        let mut reopened = test_app_at(&progress);
        reopened.open_path(second.clone(), &ctx);
        assert_eq!(reopened.selected, Some(SourceRef::file(&second)));
        finish_load(&mut reopened, &ctx);
        assert_eq!(reopened.display_name(), "2.png");
    }

    #[test]
    fn queued_next_page_does_not_replace_progress_when_its_load_fails() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let cover = png_bytes(1);
        model::source::test_support::write_zip(
            &archive,
            &[("cover.png", &cover), ("next.png", b"not an image")],
        );
        let progress = directory.path().join("progress.json");
        let ctx = egui::Context::default();

        let mut app = test_app_at(&progress);
        app.open_path(archive, &ctx);
        // Queue the next page while the cover is still loading.
        app.perform(Action::Next, Rect::NOTHING, &ctx);
        finish_load(&mut app, &ctx);
        finish_load(&mut app, &ctx);
        assert!(
            app.error.is_some(),
            "the queued next page must fail to decode"
        );
        app.flush_progress(&ctx, true).unwrap();

        let cover_source = app.siblings[0].clone();
        let restored = ProgressStore::load_from(&progress)
            .restore(&cover_source)
            .unwrap();
        assert_eq!(restored.entry_name, "cover.png");
    }

    #[test]
    fn final_close_flushes_progress_committed_while_settings_save_was_pending() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        write_comic(&archive);
        let progress = directory.path().join("progress.json");
        let ctx = egui::Context::default();

        let mut app = test_app_at(&progress);
        app.open_path(archive, &ctx);
        let sources = app.siblings.clone();
        finish_load(&mut app, &ctx);
        assert_eq!(app.selected, Some(sources[0].clone()));

        // A page is in flight when closing starts; a settings save is pending.
        app.load(&ctx, sources[1].clone(), false);
        app.save_revision = 1;
        app.request_close(&ctx);
        assert!(
            app.close_state.requested,
            "the settings save should gate the close"
        );

        // The in-flight page commits while the close waits for the save.
        finish_load(&mut app, &ctx);
        assert_eq!(app.selected, Some(sources[1].clone()));
        assert!(app.progress_dirty);

        app.save_tx
            .send(SaveDone {
                revision: 1,
                result: Ok(()),
            })
            .unwrap();
        wait_until(|| {
            app.poll_saves(&ctx);
            !app.close_state.requested
        });
        assert!(!app.progress_dirty);
        let restored = ProgressStore::load_from(&progress)
            .restore(&sources[1])
            .unwrap();
        assert_eq!(restored.entry_name, "chapter/page2.png");
    }

    #[test]
    fn book_layout_uses_known_dimensions_for_spreads_and_anchors() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app();
        app.siblings = (0..6)
            .map(|index| SourceRef::file(&directory.path().join(format!("{index}.png"))))
            .collect();
        app.settings.reading_mode = ReadingMode::Book;
        app.page_dimensions = HashMap::from([(0, (400, 100)), (2, (400, 100))]);
        let layout = app.layout();
        assert_eq!(layout.pages(0), [0], "cover stays single even when wide");
        assert_eq!(
            layout.pages(1),
            [1],
            "a wide next page cannot be a left page"
        );
        assert_eq!(layout.pages(2), [2]);
        assert_eq!(layout.pages(3), [3, 4]);
        assert_eq!(layout.anchor(4), Some(3));
        assert_eq!(layout.destination(0, Navigation::Next), Some(1));
        assert_eq!(layout.destination(1, Navigation::Next), Some(2));
        assert_eq!(layout.destination(2, Navigation::Next), Some(3));
        assert_eq!(layout.destination(3, Navigation::Next), Some(5));
    }

    #[test]
    fn queued_navigation_targets_dynamic_spread_anchors() {
        let directory = tempfile::tempdir().unwrap();
        let sources: Vec<_> = (0..6)
            .map(|index| {
                let path = directory.path().join(format!("{index}.png"));
                std::fs::write(&path, png_bytes(index as u8 + 1)).unwrap();
                SourceRef::file(&path)
            })
            .collect();
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.siblings = sources.clone();
        app.settings.reading_mode = ReadingMode::Book;
        app.page_dimensions = HashMap::from([(2, (400, 100))]);
        app.selected = Some(sources[0].clone());
        // Keep the loader busy so enqueueing does not start a real decode.
        app.loader
            .request(load_work(directory.path().join("blocked.png")));
        app.enqueue_navigation(Navigation::Next, &ctx);
        app.enqueue_navigation(Navigation::Next, &ctx);
        assert_eq!(app.navigation_paths, sources);
        let first = app.navigation.start_next(10).unwrap();
        assert_eq!(first.index, 1);
        app.navigation.failed(10);
        let second = app.navigation.start_next(11).unwrap();
        assert_eq!(second.index, 2, "a wide page is its own anchor");
    }

    #[test]
    fn book_load_uses_dynamic_spread_pages_in_reading_order() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let pages: Vec<_> = (0..6)
            .map(|index| (format!("p{index}.png"), png_bytes(index as u8 + 1)))
            .collect();
        let entries: Vec<_> = pages
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect();
        model::source::test_support::write_zip(&archive, &entries);
        let ctx = egui::Context::default();
        let mut app = test_app_at(&directory.path().join("progress.json"));
        app.settings.reading_mode = ReadingMode::Book;
        app.open_path(archive, &ctx);
        let sources = app.siblings.clone();
        finish_load(&mut app, &ctx);
        app.page_dimensions.insert(2, (400, 100));
        app.load(&ctx, sources[4].clone(), false);
        finish_load(&mut app, &ctx);
        assert_eq!(app.selected, Some(sources[3].clone()));
        assert_eq!(
            app.displayed_paths(),
            vec![sources[3].clone(), sources[4].clone()]
        );
        assert_eq!(app.position_label(), "4–5 / 6");
    }

    #[test]
    fn cached_prefetch_dimensions_fill_neighbors_without_io() {
        let directory = tempfile::tempdir().unwrap();
        let sources: Vec<_> = (0..5)
            .map(|index| {
                let path = directory.path().join(format!("{index}.png"));
                std::fs::write(&path, png_bytes(index as u8 + 1)).unwrap();
                SourceRef::file(&path)
            })
            .collect();
        let mut app = test_app();
        app.siblings = sources.clone();
        app.settings.reading_mode = ReadingMode::Single;
        app.selected = Some(sources[2].clone());
        app.page_dimensions.clear();
        app.prefetch.update_context_many_sources(
            app.siblings.clone(),
            app.selected.clone(),
            Vec::new(),
        );
        app.prefetch.set_allowed(true);
        wait_until(|| app.prefetch.cached_dimensions(&sources[3]).is_some());
        app.prefetch.set_allowed(false);
        app.fill_cached_dimensions();
        assert_eq!(app.page_dimensions.get(&3), Some(&(2, 3)));
        assert!(
            !app.page_dimensions.contains_key(&0),
            "pages outside the cached neighborhood are not probed"
        );
    }

    #[test]
    fn auto_wide_correction_reloads_once_and_does_not_loop() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let normal = png_bytes(1);
        let wide = png_of(4, 1, 2);
        model::source::test_support::write_zip(
            &archive,
            &[
                ("p0.png", &normal),
                ("p1.png", &wide),
                ("p2.png", &normal),
                ("p3.png", &normal),
            ],
        );
        let ctx = egui::Context::default();
        let mut app = test_app_at(&directory.path().join("progress.json"));
        app.settings.reading_mode = ReadingMode::Book;
        app.open_path(archive, &ctx);
        let sources = app.siblings.clone();
        finish_load(&mut app, &ctx);
        assert_eq!(app.displayed_paths(), vec![sources[0].clone()]);

        app.load(&ctx, sources[1].clone(), false);
        finish_load(&mut app, &ctx);
        finish_load(&mut app, &ctx);
        assert_eq!(app.displayed_paths(), vec![sources[1].clone()]);
        assert_eq!(app.selected, Some(sources[1].clone()));
        assert!(app.other_pages.is_empty());
        assert!(app.layout_retry.is_none());
        assert!(
            app.page_dimensions
                .get(&1)
                .is_some_and(|&(width, height)| is_wide(width, height))
        );
        assert!(!app.loader.running && app.loader.pending.is_none());
    }

    #[test]
    fn wide_left_page_correction_keeps_the_requested_page_before_advancing() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let normal = png_bytes(1);
        let wide = png_of(4, 1, 2);
        model::source::test_support::write_zip(
            &archive,
            &[("p0.png", &normal), ("p1.png", &normal), ("p2.png", &wide)],
        );
        let ctx = egui::Context::default();
        let mut app = test_app_at(&directory.path().join("progress.json"));
        app.settings.reading_mode = ReadingMode::Book;
        app.open_path(archive, &ctx);
        let sources = app.siblings.clone();
        finish_load(&mut app, &ctx);

        // Next requests page 1, which unknowingly forms [1, 2] with the wide page.
        app.perform(Action::Next, Rect::NOTHING, &ctx);
        finish_load(&mut app, &ctx);
        finish_load(&mut app, &ctx);
        assert_eq!(
            app.displayed_paths(),
            vec![sources[1].clone()],
            "the requested portrait page must display, not the wide page"
        );
        assert_eq!(app.selected, Some(sources[1].clone()));
        assert_eq!(app.position_label(), "2 / 3");
        assert!(app.layout_retry.is_none());

        // Only now does the wide page become its own spread.
        app.perform(Action::Next, Rect::NOTHING, &ctx);
        finish_load(&mut app, &ctx);
        assert_eq!(app.displayed_paths(), vec![sources[2].clone()]);
        assert!(app.other_pages.is_empty());
    }

    #[test]
    fn auto_wide_correction_retargets_queued_next_to_the_new_single_page() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let normal = png_bytes(1);
        let wide = png_of(4, 1, 2);
        model::source::test_support::write_zip(
            &archive,
            &[
                ("p0.png", &normal),
                ("p1.png", &normal),
                ("p2.png", &wide),
                ("p3.png", &normal),
            ],
        );
        let ctx = egui::Context::default();
        let mut app = test_app_at(&directory.path().join("progress.json"));
        app.settings.reading_mode = ReadingMode::Book;
        app.settings.prefetch.memory_mib = 0;
        app.prefetch.configure(app.settings.prefetch.clone());
        app.open_path(archive, &ctx);
        finish_load(&mut app, &ctx);
        let sources = app.siblings.clone();
        assert!(!app.page_dimensions.contains_key(&2));

        app.perform(Action::Next, Rect::NOTHING, &ctx);
        app.perform(Action::Next, Rect::NOTHING, &ctx);
        assert_eq!(app.navigation.pending_len(), 1);
        finish_load(&mut app, &ctx);
        assert_eq!(app.displayed_paths(), sources[1..3]);
        assert_eq!(app.layout_retry, Some((sources[1].clone(), vec![1])));
        finish_load(&mut app, &ctx);
        assert_eq!(
            app.displayed_paths(),
            vec![sources[1].clone()],
            "the corrected portrait page must commit before the queued Next"
        );
        assert_eq!(
            app.current_file,
            Some(sources[2].clone()),
            "queued Next must target the newly single wide page, not page 3"
        );
        finish_load(&mut app, &ctx);
        assert_eq!(app.displayed_paths(), vec![sources[2].clone()]);
        assert!(app.navigation.is_idle());
        assert!(app.layout_retry.is_none());
        assert!(!app.loader.running && app.loader.pending.is_none());
    }

    #[test]
    fn cascading_wide_discovery_retries_changed_spreads_for_the_same_source_then_stops() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let normal = png_bytes(1);
        let wide = png_of(4, 1, 2);
        model::source::test_support::write_zip(
            &archive,
            &[
                ("p0.png", &normal),
                ("p1.png", &normal),
                ("p2.png", &normal),
                ("p3.png", &wide),
                ("p4.png", &normal),
                ("p5.png", &wide),
                ("p6.png", &normal),
            ],
        );
        let ctx = egui::Context::default();
        let mut app = test_app_at(&directory.path().join("progress.json"));
        app.settings.reading_mode = ReadingMode::Book;
        app.settings.prefetch.memory_mib = 0;
        app.prefetch.configure(app.settings.prefetch.clone());
        app.open_path(archive, &ctx);
        finish_load(&mut app, &ctx);
        let sources = app.siblings.clone();
        let revision = app.loader.revision;
        app.load(&ctx, sources[4].clone(), false);
        finish_load(&mut app, &ctx);
        assert_eq!(app.displayed_paths(), sources[3..5]);
        assert_eq!(app.layout_retry, Some((sources[4].clone(), vec![4, 5])));
        assert!(!app.page_dimensions.contains_key(&5));

        finish_load(&mut app, &ctx);
        assert_eq!(app.displayed_paths(), sources[4..6]);
        assert_eq!(
            app.layout_retry,
            Some((sources[4].clone(), vec![4])),
            "learning page 5 is wide must allow a second correction of page 4"
        );
        finish_load(&mut app, &ctx);
        assert_eq!(app.displayed_paths(), vec![sources[4].clone()]);
        assert_eq!(app.current_file, Some(sources[4].clone()));
        assert_eq!(
            app.loader.revision,
            revision + 3,
            "exactly two corrective reloads"
        );
        assert!(app.layout_retry.is_none());
        assert!(app.navigation.is_idle());
        for _ in 0..3 {
            app.poll_workers(&ctx);
            app.advance_frames(&ctx);
        }
        assert_eq!(app.loader.revision, revision + 3);
        assert!(!app.loader.running && app.loader.pending.is_none());
    }

    #[test]
    fn archive_overrides_translate_member_indices_to_sorted_positions() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let first = png_bytes(1);
        let second = png_bytes(2);
        let tenth = png_bytes(10);
        let twentieth = png_bytes(20);
        model::source::test_support::write_zip(
            &archive,
            &[
                ("page10.png", &tenth),     // member 0
                ("page1.png", &first),      // member 1
                ("readme.txt", b"note"),    // member 2
                ("page2.png", &second),     // member 3
                ("page20.png", &twentieth), // member 4
            ],
        );
        let sources = catalog_for(&archive).unwrap().items;
        assert_eq!(
            sources
                .iter()
                .map(SourceRef::display_name)
                .collect::<Vec<_>>(),
            ["page1.png", "page2.png", "page10.png", "page20.png"]
        );
        let progress = directory.path().join("progress.json");
        let mut store = ProgressStore::load_from(&progress);
        store
            .set_override(&sources[0], 3, PageOverride::Single)
            .unwrap();
        store.save().unwrap();

        let ctx = egui::Context::default();
        let mut app = test_app_at(&progress);
        app.settings.reading_mode = ReadingMode::Book;
        app.open_path(archive, &ctx);
        finish_load(&mut app, &ctx);
        assert_eq!(
            app.page_overrides,
            BTreeMap::from([(1, PageOverride::Single)]),
            "member index 3 is page2 at sorted position 1"
        );
        let layout = app.layout();
        assert_eq!(layout.pages(1), [1], "page2 is single");
        assert_eq!(layout.pages(2), [2, 3], "page10 and page20 stay paired");
    }

    #[test]
    fn wide_shortcut_requires_plain_w_and_does_not_repeat() {
        assert_eq!(
            key_action(Key::W, Modifiers::NONE),
            Some(Action::WideToggle)
        );
        for modifiers in [Modifiers::SHIFT, Modifiers::ALT, Modifiers::CTRL] {
            assert_eq!(key_action(Key::W, modifiers), None);
        }
        assert_eq!(key_action(Key::W, Modifiers::COMMAND), Some(Action::Close));
        assert!(!action_allows_repeat(Action::WideToggle));
        assert_eq!(settings_key_action(Key::W, Modifiers::NONE), None);
    }

    #[test]
    fn wide_toggle_cycles_persists_member_indices_and_restores_the_layout() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let page = png_bytes(1);
        model::source::test_support::write_zip(
            &archive,
            &[
                ("page10.png", &page),
                ("page1.png", &page),
                ("readme.txt", b"note"),
                ("page2.png", &page),
                ("page20.png", &page),
            ],
        );
        let progress = directory.path().join("progress.json");
        let ctx = egui::Context::default();
        let mut app = test_app_at(&progress);
        app.open_path(archive.clone(), &ctx);
        finish_load(&mut app, &ctx);
        let sources = app.siblings.clone();
        app.load(&ctx, sources[1].clone(), false);
        finish_load(&mut app, &ctx);
        app.flush_progress(&ctx, true).unwrap();
        let bookmark = app.progress.restore(&sources[1]);
        let revision = app.loader.revision;
        for mode in [
            PageOverride::Single,
            PageOverride::Spread,
            PageOverride::Auto,
            PageOverride::Single,
        ] {
            app.perform(Action::WideToggle, Rect::NOTHING, &ctx);
            assert_eq!(
                app.page_overrides.get(&1).copied().unwrap_or_default(),
                mode
            );
            let expected = if mode == PageOverride::Auto {
                BTreeMap::new()
            } else {
                BTreeMap::from([(3, mode)])
            };
            assert_eq!(app.progress.overrides_for(&sources[1]), expected);
            assert_eq!(
                app.progress.restore(&sources[1]),
                bookmark,
                "W must not move the bookmark"
            );
            assert!(app.progress_dirty && app.progress_due.is_some());
            app.flush_progress(&ctx, false).unwrap();
            assert!(
                app.progress_dirty,
                "override saves use the progress debounce"
            );
            app.flush_progress(&ctx, true).unwrap();
            assert_eq!(
                ProgressStore::load_from(&progress).overrides_for(&sources[1]),
                expected
            );
        }
        assert_eq!(app.settings.reading_mode, ReadingMode::Single);
        assert_eq!(app.loader.revision, revision, "Single W never reloads");

        let mut reopened = test_app_at(&progress);
        reopened.settings.reading_mode = ReadingMode::Book;
        reopened.open_path(archive, &ctx);
        finish_load(&mut reopened, &ctx);
        assert_eq!(
            reopened.page_overrides,
            BTreeMap::from([(1, PageOverride::Single)])
        );
        assert_eq!(reopened.displayed_paths(), vec![sources[1].clone()]);
        assert_eq!(reopened.layout().pages(2), [2, 3]);
    }

    #[test]
    fn wide_toggle_reloads_requested_spread_clears_queue_and_resets_retry() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let page = png_bytes(1);
        model::source::test_support::write_zip(
            &archive,
            &[
                ("p0.png", &page),
                ("p1.png", &page),
                ("p2.png", &page),
                ("p3.png", &page),
            ],
        );
        let ctx = egui::Context::default();
        let mut app = test_app_at(&directory.path().join("progress.json"));
        app.settings.reading_mode = ReadingMode::Book;
        app.open_path(archive, &ctx);
        finish_load(&mut app, &ctx);
        let sources = app.siblings.clone();
        app.load(&ctx, sources[2].clone(), false);
        finish_load(&mut app, &ctx);
        assert_eq!(app.selected, Some(sources[1].clone()));
        app.navigation_paths = sources.clone();
        assert!(
            app.navigation
                .enqueue(Navigation::Next, 1, 4, ReadingMode::Book)
        );
        app.layout_retry = Some((sources[2].clone(), vec![1, 2]));
        let revision = app.loader.revision;
        app.perform(Action::WideToggle, Rect::NOTHING, &ctx);
        assert!(app.loader.revision > revision);
        assert!(app.navigation.is_idle() && app.navigation_paths.is_empty());
        assert!(app.layout_retry.is_none());
        assert_eq!(
            app.page_overrides,
            BTreeMap::from([(1, PageOverride::Single)])
        );
        assert_eq!(app.current_file, Some(sources[2].clone()));
        finish_load(&mut app, &ctx);
        assert_eq!(app.displayed_paths(), sources[2..]);
        assert!(app.navigation.is_idle());
        assert!(app.layout_retry.is_none());
    }

    #[test]
    fn book_wide_toggle_cycles_single_spread_auto_through_real_reloads() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let normal = png_bytes(1);
        let wide = png_of(4, 1, 2);
        model::source::test_support::write_zip(
            &archive,
            &[("p0.png", &normal), ("p1.png", &wide), ("p2.png", &normal)],
        );
        let ctx = egui::Context::default();
        let mut app = test_app_at(&directory.path().join("progress.json"));
        app.settings.reading_mode = ReadingMode::Book;
        app.open_path(archive, &ctx);
        finish_load(&mut app, &ctx);
        let sources = app.siblings.clone();
        app.page_dimensions.insert(1, (4, 1));
        app.load(&ctx, sources[1].clone(), false);
        finish_load(&mut app, &ctx);
        for (mode, pages) in [
            (PageOverride::Single, vec![sources[1].clone()]),
            (PageOverride::Spread, sources[1..].to_vec()),
            (PageOverride::Auto, vec![sources[1].clone()]),
        ] {
            app.perform(Action::WideToggle, Rect::NOTHING, &ctx);
            finish_load(&mut app, &ctx);
            assert_eq!(app.displayed_paths(), pages);
            assert_eq!(
                app.page_overrides.get(&1).copied().unwrap_or_default(),
                mode
            );
            assert_eq!(app.current_file, Some(sources[1].clone()));
            assert!(app.layout_retry.is_none());
            assert!(!app.loader.running && app.loader.pending.is_none());
        }
    }

    #[test]
    fn single_wide_toggle_persists_file_position_without_reloading_or_changing_mode() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("1.png");
        let second = directory.path().join("2.png");
        std::fs::write(&first, png_bytes(1)).unwrap();
        std::fs::write(&second, png_of(4, 1, 2)).unwrap();
        let progress = directory.path().join("progress.json");
        let ctx = egui::Context::default();
        let mut app = test_app_at(&progress);
        app.open_path(second.clone(), &ctx);
        finish_load(&mut app, &ctx);
        let revision = app.loader.revision;
        let media = app.media.clone().unwrap();
        app.navigation_paths = app.siblings.clone();
        assert!(
            app.navigation
                .enqueue(Navigation::Previous, 1, 2, ReadingMode::Single)
        );
        app.perform(Action::WideToggle, Rect::NOTHING, &ctx);
        assert_eq!(app.settings.reading_mode, ReadingMode::Single);
        assert_eq!(app.loader.revision, revision);
        assert!(Arc::ptr_eq(app.media.as_ref().unwrap(), &media));
        assert_eq!(app.layout().pages(1), [1]);
        assert!(app.navigation.is_idle() && app.navigation_paths.is_empty());
        app.request_close(&ctx);
        assert!(!app.progress_dirty, "close flushes the override");
        assert_eq!(
            ProgressStore::load_from(&progress).overrides_for(&SourceRef::file(&second)),
            BTreeMap::from([(1, PageOverride::Single)]),
        );
        let mut reopened = test_app_at(&progress);
        reopened.open_path(second, &ctx);
        finish_load(&mut reopened, &ctx);
        assert_eq!(
            reopened.page_overrides,
            BTreeMap::from([(1, PageOverride::Single)])
        );
    }

    #[test]
    fn apply_auto_wide_reloads_book_only_and_unrelated_settings_do_not_reload() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let normal = png_bytes(1);
        let wide = png_of(4, 1, 2);
        model::source::test_support::write_zip(
            &archive,
            &[("p0.png", &normal), ("p1.png", &wide), ("p2.png", &wide)],
        );
        let ctx = egui::Context::default();
        let mut app = test_app_at(&directory.path().join("progress.json"));
        app.settings.reading_mode = ReadingMode::Book;
        // Keep saves queued so the test never writes the user's settings.
        assert!(app.saves.lock().unwrap().request(0, app.settings.clone()));
        app.open_path(archive, &ctx);
        finish_load(&mut app, &ctx);
        let sources = app.siblings.clone();
        app.page_dimensions.extend([(1, (4, 1)), (2, (4, 1))]);
        app.load(&ctx, sources[1].clone(), false);
        finish_load(&mut app, &ctx);
        assert_eq!(app.displayed_paths(), vec![sources[1].clone()]);

        let revision = app.loader.revision;
        app.open_settings(&ctx);
        let draft = app.settings_draft.as_mut().unwrap();
        draft.language = Language::Japanese;
        draft.prefetch.before = 0;
        draft.thumbnails_visible = false;
        draft.comic_arrow_direction = ComicArrowDirection::LeftNext;
        app.apply_settings(&ctx);
        assert_eq!(app.loader.revision, revision);
        assert_eq!(app.displayed_paths(), vec![sources[1].clone()]);

        app.open_settings(&ctx);
        app.settings_control = SETTING_AUTO_WIDE;
        app.activate_setting(&ctx);
        assert!(!app.settings_draft.as_ref().unwrap().auto_wide);
        assert!(app.settings.auto_wide, "the row edits only the draft");
        app.layout_retry = Some((sources[1].clone(), vec![1]));
        app.apply_settings(&ctx);
        assert!(app.loader.revision > revision);
        assert!(app.layout_retry.is_none());
        finish_load(&mut app, &ctx);
        assert_eq!(app.displayed_paths(), sources[1..], "Off pairs wide pages");

        app.open_settings(&ctx);
        app.step_setting(SETTING_AUTO_WIDE, false);
        app.apply_settings(&ctx);
        finish_load(&mut app, &ctx);
        assert_eq!(
            app.displayed_paths(),
            vec![sources[1].clone()],
            "Auto singles wide pages"
        );

        assert!(app.transition_to_mode(ReadingMode::Single, &ctx));
        finish_load(&mut app, &ctx);
        let revision = app.loader.revision;
        app.open_settings(&ctx);
        app.step_setting(SETTING_AUTO_WIDE, true);
        app.apply_settings(&ctx);
        assert_eq!(app.loader.revision, revision, "auto-wide is Book-only");
        assert!(!app.settings.auto_wide);
    }

    #[test]
    fn mode_switch_keeps_the_explicitly_requested_spread_page() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        let page = png_bytes(1);
        model::source::test_support::write_zip(
            &archive,
            &[
                ("p0.png", &page),
                ("p1.png", &page),
                ("p2.png", &page),
                ("p3.png", &page),
            ],
        );
        let ctx = egui::Context::default();
        let mut app = test_app_at(&directory.path().join("progress.json"));
        app.settings.reading_mode = ReadingMode::Book;
        app.open_path(archive, &ctx);
        let sources = app.siblings.clone();
        finish_load(&mut app, &ctx);

        // Request the left page of the first pair; the anchor is page 1.
        app.load(&ctx, sources[2].clone(), false);
        finish_load(&mut app, &ctx);
        assert_eq!(
            app.displayed_paths(),
            vec![sources[1].clone(), sources[2].clone()]
        );
        assert_eq!(app.selected, Some(sources[1].clone()));
        assert_eq!(app.current_file, Some(sources[2].clone()));

        assert!(app.transition_to_mode(ReadingMode::Single, &ctx));
        finish_load(&mut app, &ctx);
        assert_eq!(app.selected, Some(sources[2].clone()));
        assert_eq!(app.displayed_paths(), vec![sources[2].clone()]);
        assert!(!app.loader.running && app.loader.pending.is_none());
        assert!(app.layout_retry.is_none());
    }

    #[test]
    fn folder_open_keeps_selected_file_siblings_previews_and_copy() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("1.png");
        let second = directory.path().join("2.png");
        std::fs::write(&first, png_bytes(1)).unwrap();
        std::fs::write(&second, png_bytes(2)).unwrap();
        write_comic(&directory.path().join("comic.cbz"));
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.thumb_width = 2. * CELL_WIDTH;
        app.open_path(second.clone(), &ctx);
        finish_load(&mut app, &ctx);
        assert_eq!(
            app.siblings,
            vec![SourceRef::file(&first), SourceRef::file(&second)]
        );
        assert_eq!(app.selected, Some(SourceRef::file(&second)));
        assert_eq!(app.display_name(), "2.png");
        assert_eq!(app.position_label(), "2 / 2");
        wait_until(|| {
            app.thumbnails
                .get_source(&SourceRef::file(&second))
                .is_some()
        });
        app.copy_current_view(&ctx);
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyImage(image) if image.size == [2, 3] && image.pixels[0] == Color32::from_rgb(2, 20, 30))
        }));
        output.textures_delta.clear();
        app.perform(Action::Previous, Rect::NOTHING, &ctx);
        finish_load(&mut app, &ctx);
        assert_eq!(app.selected, Some(SourceRef::file(&first)));
    }

    #[test]
    fn explicit_open_reserves_prefetched_file_and_archive_page_before_context_update() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("1.png");
        let second = directory.path().join("2.png");
        std::fs::write(&first, png_bytes(1)).unwrap();
        std::fs::write(&second, png_bytes(2)).unwrap();
        let archive = directory.path().join("comic.cbz");
        write_comic(&archive);
        for archive_mode in [false, true] {
            let ctx = egui::Context::default();
            let mut app = test_app();
            app.settings.prefetch.before = 2;
            app.prefetch.configure(app.settings.prefetch.clone());
            app.open_path(
                if archive_mode {
                    archive.clone()
                } else {
                    first.clone()
                },
                &ctx,
            );
            finish_load(&mut app, &ctx);
            let target = if archive_mode {
                let first_page = app.siblings[0].clone();
                app.load(&ctx, app.siblings[1].clone(), false);
                finish_load(&mut app, &ctx);
                // Reopening now restores the committed page; this test checks
                // prefetch reuse, so start it without a saved position.
                app.progress = ProgressStore::load_from(Path::new("oxeye-test-progress.json"));
                app.progress_dirty = false;
                app.progress_due = None;
                first_page
            } else {
                SourceRef::file(&second)
            };
            let mut cached = None;
            wait_until(|| {
                cached = app.prefetch.peek_source(&target, &AtomicBool::new(false));
                cached.is_some()
            });
            app.open_path(
                if archive_mode {
                    archive.clone()
                } else {
                    second.clone()
                },
                &ctx,
            );
            finish_load(&mut app, &ctx);
            assert_eq!(app.displayed, Some(target));
            assert!(Arc::ptr_eq(
                app.media.as_ref().unwrap(),
                cached.as_ref().unwrap()
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn readable_file_opens_when_its_directory_cannot_be_listed() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("image.png");
        std::fs::write(&file, png_bytes(42)).unwrap();
        let permissions = std::fs::metadata(directory.path()).unwrap().permissions();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o111)).unwrap();
        if model::sibling_images(&file).is_ok() {
            std::fs::set_permissions(directory.path(), permissions).unwrap();
            eprintln!("skipping unlistable-directory test: process bypasses directory permissions");
            return;
        }
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.open_path(file.clone(), &ctx);
        finish_load(&mut app, &ctx);
        std::fs::set_permissions(directory.path(), permissions).unwrap();
        assert_eq!(app.selected, Some(SourceRef::file(&file)));
        assert_eq!(app.siblings, vec![SourceRef::file(&file)]);
        assert_eq!(
            app.media.as_ref().unwrap().frames[0]
                .pixels
                .get_pixel(0, 0)
                .0[0],
            42
        );
        assert!(app.error.is_none());
    }

    #[test]
    fn replaced_archive_cancels_queued_navigation_before_load_and_after_worker_error() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("comic.cbz");
        for worker_started in [false, true] {
            write_comic(&archive);
            let ctx = egui::Context::default();
            let mut app = test_app();
            app.open_path(archive.clone(), &ctx);
            finish_load(&mut app, &ctx);
            let previous = app.media.clone().unwrap();
            let sources = app.siblings.clone();
            app.navigation_paths = sources.clone();
            assert!(
                app.navigation
                    .enqueue(Navigation::Next, 0, 3, ReadingMode::Single)
            );
            assert!(
                app.navigation
                    .enqueue(Navigation::Next, 0, 3, ReadingMode::Single)
            );
            let work = if worker_started {
                let ticket = app.navigation.start_next(app.loader.revision + 1).unwrap();
                assert_eq!(ticket.index, 1);
                app.selected = Some(sources[1].clone());
                app.loading_file = true;
                app.loader.request(requested_source_work(
                    sources[1].clone(),
                    &app.prefetch,
                    ReadingMode::Single,
                ));
                Some(app.loader.take().unwrap())
            } else {
                None
            };
            app.page_dimensions = HashMap::from([(0, (400, 100)), (2, (400, 100))]);
            app.page_overrides = BTreeMap::from([(2, PageOverride::Single)]);
            let changed = png_bytes(88);
            model::source::test_support::write_zip(
                &archive,
                &[
                    ("chapter/page1.png", &changed),
                    ("extra.txt", b"replacement archive"),
                ],
            );
            assert!(source_identity(&sources[1]).is_none());
            if let Some((revision, work, cancelled)) = work {
                let result = prepare(work, &cancelled);
                assert!(result.image.is_err());
                app.worker_tx.send(Completion { revision, result }).unwrap();
                app.poll_workers(&ctx);
            } else {
                app.start_navigation(&ctx);
            }
            assert!(app.navigation.is_idle());
            assert!(app.navigation_paths.is_empty());
            assert!(app.loader.pending.is_none());
            assert!(!app.loading_file);
            assert!(
                app.error
                    .as_ref()
                    .unwrap()
                    .contains(app.text().archive_changed)
            );
            assert!(Arc::ptr_eq(app.media.as_ref().unwrap(), &previous));
            assert!(
                app.page_dimensions.is_empty(),
                "old archive dimensions must not survive a catalogue change"
            );
            assert!(app.page_overrides.is_empty());
            if worker_started {
                // The replacement catalogue was installed despite the old
                // source failing, so fresh indices can be navigated directly.
                let fresh = app.siblings.clone();
                assert_eq!(fresh.len(), 1);
                app.load(&ctx, fresh[0].clone(), false);
                finish_load(&mut app, &ctx);
                assert_eq!(app.displayed_paths(), vec![fresh[0].clone()]);
            } else {
                app.open_path(archive.clone(), &ctx);
                finish_load(&mut app, &ctx);
                assert_eq!(app.siblings.len(), 1);
                assert_eq!(app.displayed_paths(), vec![app.siblings[0].clone()]);
            }
        }
    }

    #[test]
    fn archive_errors_are_localized_and_keep_last_image_without_old_siblings() {
        let directory = tempfile::tempdir().unwrap();
        let good = directory.path().join("good.cbz");
        write_comic(&good);
        let corrupt = directory.path().join("corrupt.zip");
        std::fs::write(&corrupt, b"not a ZIP").unwrap();
        let empty = directory.path().join("empty.cbz");
        model::source::test_support::write_zip(&empty, &[]);
        let text_only = directory.path().join("text.zip");
        model::source::test_support::write_zip(&text_only, &[("nested/readme.txt", b"text")]);
        for language in [
            Language::English,
            Language::SimplifiedChinese,
            Language::Japanese,
        ] {
            let ctx = egui::Context::default();
            let mut app = test_app();
            app.language = language;
            app.open_path(good.clone(), &ctx);
            finish_load(&mut app, &ctx);
            let previous = app.media.clone().unwrap();
            for path in [&corrupt, &empty, &text_only] {
                app.open_path(path.clone(), &ctx);
                assert!(
                    app.error.as_ref().unwrap().contains(
                        &app.text()
                            .could_not_open(&path.display().to_string(), false)
                    )
                );
                if path != &corrupt {
                    assert!(app.error.as_ref().unwrap().contains(app.text().no_images));
                }
                assert!(Arc::ptr_eq(app.media.as_ref().unwrap(), &previous));
                assert!(app.siblings.is_empty());
                assert!(app.navigation_paths.is_empty());
                assert!(app.selected.is_none());
                assert!(app.neighbor(true).is_none());
            }
        }
    }

    #[test]
    fn explicit_archive_and_file_opens_supersede_queued_old_contexts() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.cbz");
        let second = directory.path().join("second.zip");
        write_comic(&first);
        write_comic(&second);
        let folder = directory.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("only.png");
        std::fs::write(&file, png_bytes(99)).unwrap();
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.thumb_width = 3. * CELL_WIDTH;
        app.open_path(first.clone(), &ctx);
        finish_load(&mut app, &ctx);
        wait_until(|| {
            app.siblings
                .iter()
                .all(|source| app.thumbnails.get_source(source).is_some())
        });
        app.sync_thumbnails(&ctx);
        let old_sources = app.siblings.clone();
        for target in [&second, &file] {
            app.enqueue_navigation(Navigation::Next, &ctx);
            app.enqueue_navigation(Navigation::Next, &ctx);
            assert!(!app.navigation.is_idle());
            app.open_path(target.clone(), &ctx);
            assert!(app.navigation.is_idle());
            assert!(app.navigation_paths.is_empty());
            let expected = catalog_for(target).unwrap().items;
            assert_eq!(app.siblings, expected);
            finish_load(&mut app, &ctx);
            assert_eq!(app.siblings, expected);
            assert_eq!(app.selected, expected.first().cloned());
            assert!(
                app.thumb_textures
                    .map
                    .keys()
                    .all(|source| expected.contains(source))
            );
            assert!(
                old_sources
                    .iter()
                    .all(|source| !app.thumb_textures.map.contains_key(source))
            );
            assert!(
                app.displayed_paths()
                    .iter()
                    .all(|source| expected.contains(source))
            );
        }
        assert_eq!(
            app.media.as_ref().unwrap().frames[0]
                .pixels
                .get_pixel(0, 0)
                .0[0],
            99
        );
    }

    #[test]
    fn fit_respects_both_axes_and_small_viewports() {
        assert_eq!(fit_scale((1000, 500), egui::vec2(1048., 548.), 1.), 1.);
        assert_eq!(fit_scale((1000, 500), egui::vec2(548., 548.), 1.), 0.5);
        assert_eq!(fit_scale((100, 50), egui::vec2(1048., 548.), 2.), 0.5);
        assert!(fit_scale((1000, 500), egui::vec2(0., 0.), 1.).is_finite());
        assert!(fit_scale((1000, 500), egui::vec2(0., 0.), 1.) > 0.);
    }

    #[test]
    fn zoom_keeps_the_image_point_under_the_pointer() {
        let pan = [20., -10.];
        let anchor = [100., 60.];
        let result = anchored_pan(pan, anchor, 2.5);
        for axis in 0..2 {
            assert!(
                ((anchor[axis] - result[axis]) / 2.5 - (anchor[axis] - pan[axis])).abs() < 0.001
            );
        }
        assert_eq!(anchored_pan(pan, anchor, 1.), pan);
    }

    #[test]
    fn actual_size_matches_physical_pixels_on_high_dpi() {
        for dpi in [1., 1.25, 1.5, 2., 3.] {
            assert!((400. * actual_scale(dpi) * dpi - 400.).abs() < 0.001);
        }
    }

    #[test]
    fn manual_zoom_is_bounded_at_every_dpi() {
        for dpi in [1., 1.5, 2.] {
            assert_eq!(zoom_scale(1., 1_000_000., dpi), 32. / dpi);
            assert_eq!(zoom_scale(1., 0.000_001, dpi), 0.005 / dpi);
        }
    }

    #[test]
    fn arrows_pan_only_on_overflowing_axis_and_stop_at_its_edges() {
        assert_eq!(arrow_pan(0., 100., 200., true), None);
        assert_eq!(arrow_pan(0., 600., 200., true), Some(-80.));
        assert_eq!(arrow_pan(0., 600., 200., false), Some(80.));
        assert_eq!(arrow_pan(-190., 600., 200., true), Some(-200.));
        assert_eq!(arrow_pan(190., 600., 200., false), Some(200.));
    }

    #[test]
    fn navigation_does_not_wrap_at_either_end() {
        assert_eq!(adjacent_index(0, 3, false), None);
        assert_eq!(adjacent_index(2, 3, true), None);
        assert_eq!(adjacent_index(0, 1, true), None);
        assert_eq!(adjacent_index(0, 0, false), None);
        assert_eq!(adjacent_index(1, 3, false), Some(0));
        assert_eq!(adjacent_index(1, 3, true), Some(2));
    }

    #[test]
    fn ordered_key_events_map_without_reordering() {
        let keys = [
            Key::PageDown,
            Key::PageDown,
            Key::ArrowUp,
            Key::ArrowDown,
            Key::Space,
            Key::PageUp,
            Key::Num1,
            Key::Num0,
        ];
        let actions: Vec<_> = keys
            .iter()
            .map(|key| key_action(*key, Modifiers::NONE).unwrap())
            .collect();
        assert_eq!(
            actions,
            vec![
                Action::Next,
                Action::Next,
                Action::JumpFiles(-10),
                Action::JumpFiles(10),
                Action::Pan {
                    vertical: true,
                    forward: true
                },
                Action::Previous,
                Action::Fit,
                Action::Actual,
            ]
        );
    }

    #[test]
    fn command_shortcuts_do_not_fire_for_plain_letters() {
        assert_eq!(key_action(Key::C, Modifiers::COMMAND), Some(Action::Copy));
        // Plain C is the edit entry point since 0.3.0.
        assert_eq!(key_action(Key::C, Modifiers::NONE), Some(Action::Edit));
        assert_eq!(
            key_action(Key::C, Modifiers::COMMAND | Modifiers::ALT),
            None
        );
        assert_eq!(settings_key_action(Key::C, Modifiers::COMMAND), None);
        assert_eq!(key_action(Key::O, Modifiers::COMMAND), Some(Action::Open));
        assert_eq!(key_action(Key::O, Modifiers::NONE), None);
        assert_eq!(key_action(Key::W, Modifiers::COMMAND), Some(Action::Close));
        assert_eq!(
            key_action(Key::R, Modifiers::COMMAND),
            Some(Action::RotateRight)
        );
        assert_eq!(
            key_action(Key::L, Modifiers::COMMAND),
            Some(Action::RotateLeft)
        );
        assert_eq!(
            key_action(Key::Equals, Modifiers::COMMAND),
            Some(Action::ZoomIn)
        );
        assert_eq!(
            key_action(Key::Minus, Modifiers::COMMAND),
            Some(Action::ZoomOut)
        );
        assert_eq!(key_action(Key::R, Modifiers::NONE), None);
        assert_eq!(
            key_action(Key::ArrowLeft, Modifiers::COMMAND),
            None,
            "modified arrows must not pan"
        );
        assert_eq!(
            key_action(Key::Enter, Modifiers::ALT),
            Some(Action::Fullscreen)
        );
        assert_eq!(
            key_action(Key::Comma, Modifiers::COMMAND),
            Some(Action::Settings)
        );
        assert_eq!(key_action(Key::Comma, Modifiers::NONE), None);
    }

    #[test]
    fn commits_and_navigation_wait_for_a_later_output_frame() {
        assert!(!frame_advanced(4, 4));
        assert!(!frame_advanced(4, 3));
        assert!(frame_advanced(4, 5));
        let mut painted: Option<u64> = None;
        assert!(!painted.is_some_and(|frame| frame_advanced(frame, 7)));
        painted = Some(7);
        assert!(!painted.is_some_and(|frame| frame_advanced(frame, 7)));
        assert!(painted.is_some_and(|frame| frame_advanced(frame, 8)));
    }

    #[test]
    fn corrupt_initial_image_does_not_prevent_sibling_navigation() {
        let directory = std::env::temp_dir().join(format!("oxeye-ui-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let corrupt = directory.join("01-corrupt.png");
        let other = directory.join("02-other.png");
        std::fs::write(&corrupt, b"not a PNG").unwrap();
        std::fs::write(&other, b"another listed image").unwrap();
        let result = prepare(load_work(corrupt.clone()), &AtomicBool::new(false));
        assert!(result.image.is_err());
        let (selected, siblings) = result.selection.unwrap();
        assert_eq!(selected, SourceRef::file(&corrupt));
        assert_eq!(
            siblings.unwrap(),
            vec![SourceRef::file(&corrupt), SourceRef::file(&other)]
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn worker_is_bounded_and_only_latest_result_can_commit() {
        let mut loader = Loader::default();
        loader.request(load_work("first.png".into()));
        let (first, _, cancelled) = loader.take().unwrap();
        loader.request(load_work("second.png".into()));
        loader.request(load_work("third.png".into()));
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(loader.take().is_none());
        assert!(!loader.finish(first));
        let (latest, work, _) = loader.take().unwrap();
        assert!(
            matches!(work, Work::Load(path, _, _, _, _, _, _, _) if path == SourceRef::file(Path::new("third.png")))
        );
        assert!(loader.finish(latest));
        assert!(loader.take().is_none());
    }

    #[test]
    fn queued_real_preparation_preserves_targets_and_continues_past_failed_files() {
        let directory = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..5)
            .map(|index| directory.path().join(format!("{index}.png")))
            .collect();
        for path in &paths {
            RgbaImage::new(2, 2).save(path).unwrap();
        }
        std::fs::write(&paths[1], b"corrupt PNG").unwrap();
        let mut navigation = NavigationFlow::default();
        for _ in 0..4 {
            assert!(navigation.enqueue(Navigation::Next, 0, paths.len(), ReadingMode::Single));
        }
        std::fs::remove_file(&paths[2]).unwrap();
        let prefetch = Prefetcher::new(PrefetchSettings {
            memory_mib: 0,
            ..Default::default()
        });
        let mut loader = Loader::default();
        let sources: Vec<_> = paths.iter().map(|path| SourceRef::file(path)).collect();
        let mut displayed = sources[0].clone();
        let mut committed = Vec::new();
        let mut failed = Vec::new();
        for (index, path) in paths.iter().enumerate().skip(1) {
            let ticket = navigation.start_next(loader.revision + 1).unwrap();
            assert_eq!(ticket.index, index);
            let foreground = prefetch.foreground_requested_source(&sources[index]);
            prefetch.set_allowed(false);
            prefetch.update_context_many_sources(
                sources.clone(),
                Some(sources[index].clone()),
                Vec::new(),
            );
            loader.request(Work::Load(
                sources[index].clone(),
                prefetch.clone(),
                foreground,
                MAX_TILE,
                ReadingMode::Single,
                vec![index],
                Language::English,
                0,
            ));
            let (revision, work, cancelled) = loader.take().unwrap();
            let result = prepare(work, &cancelled);
            assert!(loader.finish(revision));
            let (selected, siblings) = result.selection.unwrap();
            assert_eq!(selected, SourceRef::file(path));
            assert!(!siblings.unwrap().contains(&sources[2]));
            assert!(navigation.start_next(revision + 1).is_none());
            match result.image {
                Ok(prepared) => {
                    assert_eq!(prepared.media.source, sources[index]);
                    assert!(!prepared.tiled.tiles.is_empty());
                    assert!(!navigation.crossed_frame(ticket));
                    // Supply the two UI events explicitly; this test does not run a window.
                    assert_eq!(navigation.painted(revision), Some(ticket));
                    displayed = prepared.media.source.clone();
                    committed.push(index);
                    assert!(navigation.start_next(revision + 1).is_none());
                    assert!(navigation.crossed_frame(ticket));
                }
                Err(_) => {
                    assert_eq!(displayed, sources[0]);
                    failed.push(index);
                    assert!(navigation.failed(revision));
                }
            }
        }
        assert_eq!(failed, vec![1, 2]);
        assert_eq!(committed, vec![3, 4]);
        assert_eq!(displayed, sources[4]);
        assert!(navigation.is_idle());
        assert!(loader.take().is_none());
        prefetch.shutdown();
    }

    #[test]
    fn close_gate_waits_for_latest_save_and_failure_needs_one_more_close() {
        let mut close = CloseState::default();
        assert!(close.request(0), "no saves pending closes immediately");
        assert!(!close.request(2), "a save revision blocks closing");
        assert!(close.requested);
        assert!(
            !close.saved(1, 2, true),
            "a stale save result cannot complete the latest revision"
        );
        assert!(close.requested, "still waiting after the stale result");
        assert!(close.saved(2, 2, true), "latest success closes");

        let mut close = CloseState::default();
        assert!(!close.request(1));
        assert!(!close.saved(1, 1, false), "failure keeps the window open");
        assert!(!close.requested);
        assert!(
            close.request(1),
            "closing again acknowledges the failed save"
        );
        assert_eq!(close.completed_save, 1);
    }

    #[test]
    fn saving_reports_only_the_latest_pending_settings() {
        let mut queue = SaveQueue::default();
        let first = AppSettings {
            prefetch: PrefetchSettings {
                before: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(queue.request(1, first));
        assert_eq!(queue.next().unwrap().0, 1);
        assert!(!queue.request(2, AppSettings::default()));
        let latest = AppSettings {
            thumbnails_visible: false,
            ..Default::default()
        };
        assert!(!queue.request(3, latest.clone()));
        let (revision, saved) = queue.next().unwrap();
        assert_eq!(revision, 3);
        assert_eq!(saved, latest);
        assert!(!saved.thumbnails_visible, "preview toggle is persisted");
        assert!(queue.next().is_none());
        assert!(queue.request(4, AppSettings::default()));
    }

    #[test]
    fn drain_saves_reports_the_last_write_and_reopens_the_queue() {
        let saves = Arc::new(Mutex::new(SaveQueue::default()));
        let settings = AppSettings {
            prefetch: PrefetchSettings {
                before: 4,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(saves.lock().unwrap().request(7, settings));
        let mut written = Vec::new();
        let result = drain_saves(&saves, |settings| {
            written.push(settings.prefetch.before);
            Ok(())
        });
        assert_eq!(result, Some((7, Ok(()))));
        assert_eq!(written, vec![4]);
        assert!(saves.lock().unwrap().request(8, AppSettings::default()));
    }

    #[test]
    fn success_feedback_expires_only_for_the_current_generation() {
        let mut feedback = Feedback::default();
        let old = feedback.replace(FeedbackKind::Success, "Saved".into());
        let progress = feedback.replace(FeedbackKind::Progress, "Saving".into());
        assert!(!feedback.expire(old));
        assert!(!feedback.expire(progress));
        let current = feedback.replace(FeedbackKind::Success, "Saved again".into());
        assert!(feedback.expire(current));
        assert!(feedback.notice.is_none());
    }

    #[test]
    fn settings_controls_cycle_and_restore_resets_all_fields() {
        let mut control = 0;
        let mut seen = Vec::new();
        for _ in 0..SETTING_COUNT {
            seen.push(control);
            control = next_settings_control(control, false);
        }
        assert_eq!(seen, (0..SETTING_COUNT).collect::<Vec<_>>());
        assert_eq!(control, 0);
        assert_eq!(next_settings_control(0, true), SETTING_APPLY);
        assert_eq!(step_value(0, false, 1, 50), 0);
        assert_eq!(step_value(50, true, 1, 50), 50);
        assert_eq!(step_value(2040, true, 64, 2048), 2048);

        let mut draft = AppSettings {
            prefetch: PrefetchSettings {
                before: 0,
                after: 0,
                memory_mib: 0,
            },
            thumbnails_visible: false,
            reading_mode: settings::ReadingMode::Book,
            auto_wide: false,
            comic_arrow_direction: settings::ComicArrowDirection::LeftNext,
            language: Language::Japanese,
        };
        restore_defaults(&mut draft);
        assert_eq!(draft.prefetch, PrefetchSettings::default());
        assert!(draft.thumbnails_visible);
        assert_eq!(draft.reading_mode, ReadingMode::Single);
        assert!(draft.auto_wide);
        assert_eq!(draft.comic_arrow_direction, ComicArrowDirection::RightNext);
        assert_eq!(draft.language, Language::Automatic);
    }

    #[test]
    fn reading_and_arrow_setting_toggles_stay_symmetric() {
        assert_eq!(toggled_reading_mode(ReadingMode::Single), ReadingMode::Book);
        assert_eq!(toggled_reading_mode(ReadingMode::Book), ReadingMode::Single);
        assert_eq!(
            toggled_arrow_direction(ComicArrowDirection::RightNext),
            ComicArrowDirection::LeftNext
        );
        assert_eq!(
            toggled_arrow_direction(ComicArrowDirection::LeftNext),
            ComicArrowDirection::RightNext
        );
    }

    #[test]
    fn reading_mode_change_detection_ignores_other_settings() {
        let active = AppSettings::default();
        let mut draft = active.clone();
        draft.prefetch.before = 9;
        draft.thumbnails_visible = false;
        draft.comic_arrow_direction = ComicArrowDirection::LeftNext;
        assert!(!reading_mode_changed(&active, &draft));
        draft.reading_mode = ReadingMode::Book;
        assert!(reading_mode_changed(&active, &draft));
        let mut back = draft.clone();
        back.reading_mode = ReadingMode::Single;
        assert!(!reading_mode_changed(&active, &back));
    }

    #[test]
    fn language_cycles_and_apply_does_not_require_a_media_reload() {
        assert_eq!(cycled_language(Language::Automatic), Language::English);
        assert_eq!(
            cycled_language(Language::English),
            Language::SimplifiedChinese
        );
        assert_eq!(
            cycled_language(Language::SimplifiedChinese),
            Language::Japanese
        );
        assert_eq!(cycled_language(Language::Japanese), Language::Automatic);

        let active = AppSettings::default();
        let mut draft = active.clone();
        draft.language = Language::Japanese;
        assert!(
            !reading_mode_changed(&active, &draft),
            "language-only Apply must not reload media or clocks"
        );
        let resolved = resolve_language(draft.language);
        assert_eq!(resolved, Language::Japanese);
        assert_eq!(
            ui_text::catalog(resolved).applying_saving,
            "適用しました・設定を保存中…",
            "feedback starts in the newly applied language"
        );
        draft.language = Language::English;
        assert_eq!(resolve_language(draft.language), Language::English);
    }

    #[test]
    fn failed_load_retains_paused_controllers_for_recovery() {
        let mut spread = Some(SpreadPlayback::new(
            [
                Some(timing(&[20, 30, 50], model::LoopMode::Infinite)),
                Some(timing(&[100, 100], model::LoopMode::Infinite)),
            ],
            Duration::ZERO,
        ));
        let prior = spread.as_ref().unwrap().frame(0, ms(35));
        assert_eq!(prior, 1);
        let mut playback = None;

        pause_controllers(&mut playback, &mut spread, ReadingMode::Book, ms(35));
        assert!(
            spread.is_some(),
            "Book controller must not be dropped on load"
        );
        assert!(!spread.as_ref().unwrap().any_playing(ms(35)));
        assert!(animation_available(ReadingMode::Book, true, [1, 3]));
        assert!(!animation_available(ReadingMode::Book, false, [1, 3]));

        let spread = spread.as_mut().unwrap();
        assert_eq!(
            group_playback_action(spread.any_playing(ms(35)), spread.all_finished(ms(35))),
            GroupPlaybackAction::Resume
        );
        spread.resume(ms(35));
        assert!(spread.any_playing(ms(35)));
        assert_eq!(
            spread.frame(0, ms(35)),
            prior,
            "recovery resumes the prior frame, not frame 0"
        );

        let mut playback = Some(Playback::new(
            [ms(20), ms(30), ms(50)],
            model::LoopMode::Infinite,
            Duration::ZERO,
        ));
        let mut no_spread = None;
        let prior = playback.as_ref().unwrap().position(ms(35)).frame;
        pause_controllers(&mut playback, &mut no_spread, ReadingMode::Single, ms(35));
        assert!(playback.is_some(), "Single controller must not be dropped");
        assert!(!playback.as_ref().unwrap().playing(ms(35)));
        assert!(animation_available(ReadingMode::Single, false, [3]));
        playback.as_mut().unwrap().play(ms(35));
        assert_eq!(playback.as_ref().unwrap().position(ms(35)).frame, prior);
    }

    #[test]
    fn keyboard_step_cycles_the_language_and_other_choices() {
        let mut draft = AppSettings::default();
        for expected in [
            Language::English,
            Language::SimplifiedChinese,
            Language::Japanese,
            Language::Automatic,
        ] {
            step_draft_setting(SETTING_LANGUAGE, true, &mut draft);
            assert_eq!(draft.language, expected);
        }
        step_draft_setting(SETTING_LANGUAGE, false, &mut draft);
        assert_eq!(draft.language, Language::English, "both arrows cycle");
        step_draft_setting(SETTING_PREVIEWS, false, &mut draft);
        assert!(!draft.thumbnails_visible);
        step_draft_setting(SETTING_AUTO_WIDE, false, &mut draft);
        assert!(!draft.auto_wide);
        step_draft_setting(SETTING_AUTO_WIDE, true, &mut draft);
        assert!(draft.auto_wide);
    }

    #[test]
    fn settings_save_payload_carries_the_language_choice() {
        let mut queue = SaveQueue::default();
        let settings = AppSettings {
            language: Language::SimplifiedChinese,
            ..Default::default()
        };
        assert!(queue.request(11, settings.clone()));
        let (revision, saved) = queue.next().unwrap();
        assert_eq!(revision, 11);
        assert_eq!(saved, settings);
        assert_eq!(saved.language, Language::SimplifiedChinese);
    }

    #[test]
    fn settings_draft_apply_cancel_and_restore_leave_active_until_committed() {
        let active = AppSettings::default();
        let mut draft = active.clone();
        draft.prefetch.before = 9;
        draft.thumbnails_visible = false;
        assert_ne!(active, draft, "editing only the draft");
        // Cancel: drop the draft.
        let cancelled = active.clone();
        assert_eq!(active, cancelled);
        // Apply: validate, then commit the draft.
        assert!(draft.validate().is_ok());
        let applied = draft.clone();
        assert_eq!(applied.prefetch.before, 9);
        assert!(!applied.thumbnails_visible);
        // Restore: all fields return to defaults in the draft only.
        restore_defaults(&mut draft);
        assert_eq!(draft, active);
        assert!(AppSettings::default().thumbnails_visible);
    }

    #[test]
    fn settings_keys_tab_adjust_activate_and_escape() {
        assert_eq!(
            settings_key_action(Key::Tab, Modifiers::NONE),
            Some(SettingsKey::Next)
        );
        assert_eq!(
            settings_key_action(Key::Tab, Modifiers::SHIFT),
            Some(SettingsKey::Previous)
        );
        assert_eq!(
            settings_key_action(Key::ArrowLeft, Modifiers::NONE),
            Some(SettingsKey::Decrease)
        );
        assert_eq!(
            settings_key_action(Key::ArrowUp, Modifiers::NONE),
            Some(SettingsKey::Increase)
        );
        assert_eq!(
            settings_key_action(Key::Enter, Modifiers::NONE),
            Some(SettingsKey::Activate)
        );
        assert_eq!(
            settings_key_action(Key::Escape, Modifiers::NONE),
            Some(SettingsKey::Cancel)
        );
        assert_eq!(
            settings_key_action(Key::W, Modifiers::COMMAND),
            Some(SettingsKey::CloseWindow)
        );
        assert_eq!(settings_key_action(Key::R, Modifiers::NONE), None);
    }

    #[test]
    fn explicit_jump_discards_queued_navigation_targets() {
        let mut flow = NavigationFlow::default();
        let mut paths: Vec<SourceRef> = ["0.png", "1.png", "2.png"]
            .map(|path| SourceRef::file(Path::new(path)))
            .into();
        assert!(flow.enqueue(Navigation::Next, 0, paths.len(), ReadingMode::Single));
        assert!(flow.enqueue(Navigation::Next, 0, paths.len(), ReadingMode::Single));
        assert!(!flow.is_idle());
        reset_navigation(&mut flow, &mut paths);
        assert!(flow.is_idle());
        assert_eq!(flow.pending_len(), 0);
        assert!(paths.is_empty());
    }

    #[test]
    fn thumbnail_poll_stays_low_frequency_until_visible_placeholders_settle() {
        assert_eq!(
            thumbnail_poll_delay(0, true),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            thumbnail_poll_delay(1, false),
            Some(Duration::from_millis(250))
        );
        assert_eq!(thumbnail_poll_delay(2, true), Some(Duration::from_secs(1)));
        assert_eq!(thumbnail_poll_delay(2, false), None);
    }

    #[test]
    fn thumbnail_textures_retire_on_missing_source_and_hidden_strip() {
        let ctx = egui::Context::default();
        let path = SourceRef::file(Path::new("a.png"));
        let source = Arc::new(RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255])));
        let mut textures = ThumbTextures::default();
        textures.update(&ctx, &path, Some(source.clone()));
        let id = textures.texture(&path).unwrap().id();
        assert_eq!(textures.map.len(), 1);

        textures.update(&ctx, &path, Some(source));
        assert_eq!(textures.texture(&path).unwrap().id(), id);
        assert_eq!(textures.map.len(), 1);

        textures.update(&ctx, &path, None);
        assert!(
            textures.texture(&path).is_none(),
            "placeholder replaces stale texture"
        );

        let replacement = Arc::new(RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255])));
        textures.update(&ctx, &path, Some(replacement));
        assert!(textures.map.contains_key(&path));
        textures.retain(&[]);
        assert!(
            textures.map.is_empty(),
            "hidden strip or directory change retires"
        );
    }

    fn media(pixels: Arc<RgbaImage>) -> Arc<model::LoadedMedia> {
        Arc::new(model::LoadedMedia {
            source: model::source::SourceRef::file(Path::new("fixture.png")),
            frames: vec![model::MediaFrame {
                pixels,
                delay: Duration::ZERO,
            }],
            loop_mode: model::LoopMode::Once,
        })
    }

    fn install_fixture(app: &mut Oxeye, turns: u8) -> Arc<RgbaImage> {
        let pixels = Arc::new(RgbaImage::from_fn(4, 3, |x, y| {
            image::Rgba([(y * 4 + x) as u8, 0, 0, 255])
        }));
        let media = media(Arc::clone(&pixels));
        let source = media.source.clone();
        let tiled = Arc::new(prepare_tiles(&pixels, MAX_TILE, &AtomicBool::new(false)).unwrap());
        app.media = Some(media);
        app.tiled = Some(tiled);
        app.selected = Some(source.clone());
        app.displayed = Some(source.clone());
        app.current_file = Some(source.clone());
        app.siblings = vec![source];
        app.dimensions = (4, 3);
        app.rotation.commit(turns);
        pixels
    }

    fn editing_fixture() -> Oxeye {
        let mut app = test_app();
        install_fixture(&mut app, 0);
        let rotation = app.rotation;
        let source = app.media.as_ref().unwrap().source.clone();
        app.begin_edit(&egui::Context::default(), rotation, None, source, false);
        app
    }

    fn pending_book_fixture(crop: bool) -> Oxeye {
        let mut app = test_app();
        install_fixture(&mut app, 1);
        app.settings.reading_mode = ReadingMode::Book;
        app.display_mode = ReadingMode::Book;
        app.rotation.commit(1);
        app.request_edit(crop, &egui::Context::default());
        app
    }

    fn animated_media(
        frames: Vec<Arc<RgbaImage>>,
        delays: &[u64],
        loop_mode: model::LoopMode,
    ) -> Arc<model::LoadedMedia> {
        Arc::new(model::LoadedMedia {
            source: model::source::SourceRef::file(Path::new("animation.gif")),
            frames: frames
                .into_iter()
                .zip(delays)
                .map(|(pixels, delay)| model::MediaFrame {
                    pixels,
                    delay: Duration::from_millis(*delay),
                })
                .collect(),
            loop_mode,
        })
    }

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn timing(delays: &[u64], loop_mode: model::LoopMode) -> PageTiming {
        PageTiming {
            frame_count: delays.len(),
            delays: delays.iter().copied().map(ms).collect(),
            loop_mode,
        }
    }

    #[test]
    fn superseded_rotation_then_failed_load_keeps_the_displayed_rotation_baseline() {
        let mut rotation = Rotation::default();
        let mut loader = Loader::default();
        let source = Arc::new(RgbaImage::new(1, 1));
        let turns = rotation.request(true);
        loader.request(Work::Frame {
            media: media(source),
            frame: 0,
            turns,
            cap: MAX_TILE,
        });
        let (rotating, _, _) = loader.take().unwrap();
        // A0 is displayed; opening corrupt B supersedes the in-flight A90 rotation.
        rotation.cancel();
        loader.request(load_work("corrupt-B.png".into()));
        assert!(!loader.finish(rotating));
        let (loading, _, _) = loader.take().unwrap();
        assert!(loader.finish(loading));
        rotation.cancel();
        assert_eq!(rotation.displayed, 0);
        assert_eq!(rotation.request(true), 1);
        rotation.commit(1);
        assert_eq!(rotation.displayed, 1);
    }

    #[test]
    fn stale_playback_frame_is_replaced_with_requested_rotation() {
        let media = Arc::new(model::LoadedMedia {
            source: model::source::SourceRef::file(Path::new("clock.gif")),
            loop_mode: model::LoopMode::Infinite,
            frames: (0..3)
                .map(|_| model::MediaFrame {
                    pixels: Arc::new(RgbaImage::new(1, 1)),
                    delay: Duration::from_millis(10),
                })
                .collect(),
        });
        let mut rotation = Rotation::default();
        let turns = rotation.request(true);
        let mut loader = Loader::default();
        loader.request(Work::Frame {
            media: media.clone(),
            frame: 1,
            turns,
            cap: MAX_TILE,
        });
        let (old_revision, _, _) = loader.take().unwrap();
        assert!(loader.finish(old_revision));
        loader.request(Work::Frame {
            media,
            frame: 2,
            turns: rotation.requested,
            cap: MAX_TILE,
        });
        assert_eq!(loader.revision, old_revision + 1);
        let (_, work, _) = loader.take().unwrap();
        assert!(matches!(
            work,
            Work::Frame {
                frame: 2,
                turns: 1,
                ..
            }
        ));
        assert!(loader.pending.is_none());
        assert!(loader.take().is_none());
        assert_eq!(rotation.displayed, 0);
    }

    #[test]
    fn rotating_animation_prepares_only_the_requested_frame_without_resetting_progress() {
        let frames = (0..3)
            .map(|index| model::MediaFrame {
                pixels: Arc::new(RgbaImage::from_pixel(2, 1, image::Rgba([index, 0, 0, 255]))),
                delay: Duration::from_millis(20),
            })
            .collect();
        let media = Arc::new(model::LoadedMedia {
            source: model::source::SourceRef::file(Path::new("animation.gif")),
            frames,
            loop_mode: model::LoopMode::Finite(2),
        });
        let clock = Playback::new(
            media.frames.iter().map(|frame| frame.delay),
            media.loop_mode,
            Duration::ZERO,
        );
        let position = clock.position(Duration::from_millis(85));
        assert_eq!(position.frame, 1);
        let prepared = prepare(
            Work::Frame {
                media: media.clone(),
                frame: position.frame,
                turns: 1,
                cap: MAX_TILE,
            },
            &AtomicBool::new(false),
        )
        .image
        .unwrap();
        assert!(Arc::ptr_eq(&prepared.media, &media));
        assert_eq!(prepared.frame, 1);
        assert_eq!(prepared.dimensions, (1, 2));
        assert_eq!(prepared.tiled.tiles.len(), 1);
        assert!(
            media
                .frames
                .iter()
                .all(|frame| frame.pixels.dimensions() == (2, 1))
        );
        assert_eq!(clock.position(Duration::from_millis(105)).frame, 2);
        assert!(clock.position(Duration::from_millis(180)).finished);
    }

    #[test]
    fn closing_the_loader_cancels_its_in_flight_work() {
        let mut loader = Loader::default();
        loader.request(load_work("image.png".into()));
        let (_, _, cancelled) = loader.take().unwrap();
        assert!(!cancelled.load(Ordering::Relaxed));
        drop(loader);
        assert!(cancelled.load(Ordering::Relaxed));
    }

    #[test]
    fn large_source_is_prepared_at_original_resolution_in_safe_tiles() {
        let directory =
            std::env::temp_dir().join(format!("oxeye-ui-limit-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("too-wide.png");
        RgbaImage::new(4097, 1).save(&path).unwrap();
        let result = prepare(load_work(path), &AtomicBool::new(false))
            .image
            .unwrap();
        assert_eq!(result.dimensions, (4097, 1));
        assert_eq!(result.media.frames[0].pixels.dimensions(), (4097, 1));
        assert!(result.tiled.tiles.len() > 1);
        assert!(
            result
                .tiled
                .tiles
                .iter()
                .all(|tile| tile.geometry.sample.width <= MAX_TILE)
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn animation_uses_every_frame_in_single_mode() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cover.gif");
        write_test_gif(&path);
        let prefetch = Prefetcher::new(PrefetchSettings {
            memory_mib: 0,
            ..Default::default()
        });
        prefetch.set_purpose(DecodePurpose::Animated);
        let prepared = prepare(
            requested_work(path, &prefetch, ReadingMode::Single),
            &AtomicBool::new(false),
        )
        .image
        .unwrap();
        assert_eq!(prepared.media.frames.len(), 2);
        assert!(prepared.loaded);
        prefetch.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn selected_symlink_keeps_navigation_in_the_selected_folder() {
        let directory =
            std::env::temp_dir().join(format!("oxeye-ui-symlink-test-{}", std::process::id()));
        let selected_folder = directory.join("selected");
        std::fs::create_dir_all(&selected_folder).unwrap();
        let target = directory.join("target.png");
        std::fs::write(&target, b"corrupt image").unwrap();
        let selected = selected_folder.join("alias.png");
        std::os::unix::fs::symlink(&target, &selected).unwrap();
        let result = prepare(load_work(selected.clone()), &AtomicBool::new(false));
        let (path, siblings) = result.selection.unwrap();
        assert_eq!(path, SourceRef::file(&selected));
        assert_eq!(siblings.unwrap(), vec![SourceRef::file(&selected)]);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn book_pairing_covers_cover_spreads_tail_and_anchor_normalization() {
        let expected = [
            (0usize, Vec::<usize>::new()),
            (1, vec![0]),
            (2, vec![0, 1]),
            (3, vec![0, 1, 2]),
            (4, vec![0, 1, 2, 3]),
            (5, vec![0, 1, 2, 3, 4]),
            (6, vec![0, 1, 2, 3, 4, 5]),
            (7, vec![0, 1, 2, 3, 4, 5, 6]),
            (8, vec![0, 1, 2, 3, 4, 5, 6, 7]),
        ];
        for (length, files) in expected {
            if files.is_empty() {
                assert!(navigation::pages(0, 0, ReadingMode::Book).is_empty());
                continue;
            }
            let mut seen = Vec::new();
            for index in &files {
                let pages = navigation::pages(*index, length, ReadingMode::Book);
                assert_eq!(
                    navigation::anchor(*index, length, ReadingMode::Book),
                    pages.first().copied()
                );
                seen.extend(pages.iter().copied());
            }
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen, files, "length {length}");
            assert_eq!(navigation::pages(0, length, ReadingMode::Book), vec![0]);
            let tail = navigation::pages(length - 1, length, ReadingMode::Book);
            assert_eq!(tail.last().copied(), Some(length - 1));
        }
    }

    #[test]
    fn book_arrows_pgup_dn_and_file_jumps_keep_their_own_meaning() {
        let length = 12;
        assert_eq!(
            navigation::destination(1, length, ReadingMode::Book, Navigation::Next),
            Some(3)
        );
        assert_eq!(
            navigation::destination(3, length, ReadingMode::Book, Navigation::Previous),
            Some(1)
        );
        assert_eq!(
            navigation::destination(0, length, ReadingMode::Book, Navigation::Next),
            Some(1)
        );
        assert_eq!(
            navigation::destination(0, length, ReadingMode::Book, Navigation::JumpFiles(10)),
            Some(9)
        );
        assert_eq!(
            navigation::destination(11, length, ReadingMode::Book, Navigation::JumpFiles(-10)),
            Some(1)
        );
        assert_eq!(
            key_action(Key::PageDown, Modifiers::NONE),
            Some(Action::Next)
        );
        assert_eq!(
            key_action(Key::PageUp, Modifiers::NONE),
            Some(Action::Previous)
        );
        assert_eq!(
            key_action(Key::ArrowRight, Modifiers::NONE),
            Some(Action::Pan {
                vertical: false,
                forward: true
            })
        );
        assert_eq!(
            horizontal_navigation(true, ComicArrowDirection::RightNext),
            Navigation::Next
        );
        assert_eq!(
            horizontal_navigation(true, ComicArrowDirection::LeftNext),
            Navigation::Previous
        );
        assert_eq!(
            horizontal_navigation(false, ComicArrowDirection::RightNext),
            Navigation::Previous
        );
        assert_eq!(
            horizontal_navigation(false, ComicArrowDirection::LeftNext),
            Navigation::Next
        );
    }

    #[test]
    fn joint_spread_fit_actual_top_alignment_and_gap() {
        let dimensions = [(200, 400), (300, 250)];
        let (joint, centers) = spread_layout(&dimensions);
        assert_eq!(joint, (524, 400));
        let top = |index: usize| centers[index][1] - dimensions[index].1 as f32 / 2.;
        assert_eq!(top(0), top(1));
        let right_left = centers[0][0] - 200. / 2.;
        let left_right = centers[1][0] + 300. / 2.;
        assert_eq!(right_left - left_right, viewer::BOOK_GAP as f32);
        assert_eq!(fit_scale(joint, egui::vec2(1048., 448.), 1.), 1.);
        assert_eq!(fit_scale(joint, egui::vec2(524., 200.), 1.), 0.38);
        for dpi in [1., 1.25, 2.] {
            assert!((joint.0 as f32 * actual_scale(dpi) * dpi - joint.0 as f32).abs() < 0.001);
            assert!((joint.1 as f32 * actual_scale(dpi) * dpi - joint.1 as f32).abs() < 0.001);
        }
    }

    #[test]
    fn book_whole_spread_preparation_is_atomic_and_keeps_previous_pages() {
        let directory = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..3)
            .map(|index| directory.path().join(format!("{index}.png")))
            .collect();
        for path in &paths {
            RgbaImage::new(4, 3).save(path).unwrap();
        }
        let prefetch = Prefetcher::new(PrefetchSettings {
            memory_mib: 0,
            ..Default::default()
        });
        prefetch.set_purpose(DecodePurpose::Animated);
        let good = prepare(
            requested_source_work_spread(
                SourceRef::file(&paths[1]),
                &prefetch,
                ReadingMode::Book,
                vec![1, 2],
            ),
            &AtomicBool::new(false),
        )
        .image
        .unwrap();
        assert_eq!(good.others.len(), 1);
        std::fs::write(&paths[2], b"broken partner").unwrap();
        let failed = prepare(
            requested_source_work_spread(
                SourceRef::file(&paths[1]),
                &prefetch,
                ReadingMode::Book,
                vec![1, 2],
            ),
            &AtomicBool::new(false),
        );
        assert!(failed.image.is_err());
        assert_eq!(failed.selection.unwrap().0, SourceRef::file(&paths[1]));
        assert_eq!(good.others.len(), 1, "old spread remains intact");
        prefetch.shutdown();
    }

    #[test]
    fn book_rotation_prepares_both_pages_and_preserves_source_pixels() {
        let first = media(Arc::new(RgbaImage::from_pixel(
            2,
            3,
            image::Rgba([255, 1, 2, 255]),
        )));
        let second = media(Arc::new(RgbaImage::from_pixel(
            4,
            2,
            image::Rgba([3, 200, 4, 128]),
        )));
        let image = prepare(
            Work::Spread {
                media: vec![first.clone(), second.clone()],
                frames: vec![0, 0],
                turns: 1,
                cap: MAX_TILE,
            },
            &AtomicBool::new(false),
        )
        .image
        .unwrap();
        assert_eq!(image.mode, ReadingMode::Book);
        assert_eq!(image.tiled.dimensions, (3, 2));
        assert_eq!(image.others[0].tiled.dimensions, (2, 4));
        assert_eq!(image.dimensions, (29, 4));
        assert_eq!(image.tiled.tiles[0].pixels[..4], [255, 1, 2, 255]);
        assert_eq!(image.others[0].tiled.tiles[0].pixels[..4], [3, 200, 4, 128]);
        assert_eq!(first.frames[0].pixels.dimensions(), (2, 3));
        assert_eq!(second.frames[0].pixels.dimensions(), (4, 2));
    }

    #[test]
    fn book_keeps_animated_gif_frames_like_single() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cover.gif");
        write_test_gif(&path);
        let prefetch = Prefetcher::new(PrefetchSettings {
            memory_mib: 0,
            ..Default::default()
        });
        prefetch.set_purpose(DecodePurpose::Animated);
        let book = prepare(
            requested_work(path.clone(), &prefetch, ReadingMode::Book),
            &AtomicBool::new(false),
        )
        .image
        .unwrap();
        assert_eq!(book.mode, ReadingMode::Book);
        assert_eq!(book.media.frames.len(), 2);
        assert!(page_timing(&book.media).is_some());
        let single = prepare(
            requested_work(path, &prefetch, ReadingMode::Single),
            &AtomicBool::new(false),
        )
        .image
        .unwrap();
        assert_eq!(single.media.frames.len(), 2);
        prefetch.shutdown();
    }

    #[test]
    fn book_textures_retire_pages_together() {
        let ctx = egui::Context::default();
        let first = Arc::new(
            prepare_tiles(&RgbaImage::new(2, 2), MAX_TILE, &AtomicBool::new(false)).unwrap(),
        );
        let second = Arc::new(
            prepare_tiles(&RgbaImage::new(3, 2), MAX_TILE, &AtomicBool::new(false)).unwrap(),
        );
        let next_frame = Arc::new(
            prepare_tiles(&RgbaImage::new(2, 2), MAX_TILE, &AtomicBool::new(false)).unwrap(),
        );
        let mut textures = Textures::default();
        let first_key = (Arc::as_ptr(&first) as usize, 0);
        let second_key = (Arc::as_ptr(&second) as usize, 0);
        let next_key = (Arc::as_ptr(&next_frame) as usize, 0);
        textures.texture(&ctx, &first, 0, first_key.0);
        textures.texture(&ctx, &second, 0, second_key.0);
        textures.texture(&ctx, &next_frame, 0, next_key.0);
        let both = [first_key, second_key];
        textures.retain(&both);
        assert_eq!(textures.tiles.len(), 2, "future frame not retained");
        textures.texture(&ctx, &next_frame, 0, next_key.0);
        textures.retain(&[next_key, second_key]);
        assert_eq!(textures.tiles.len(), 2);
        assert!(textures.tiles.contains_key(&next_key));
        assert!(
            !textures.tiles.contains_key(&first_key),
            "replaced frame retired"
        );
        textures.retain(&[next_key]);
        assert_eq!(textures.tiles.len(), 1);
        textures.retain(&[]);
        assert!(textures.tiles.is_empty());
    }

    #[test]
    fn mode_shortcut_is_repeat_suppressed_and_persisted_in_the_queue() {
        assert_eq!(
            key_action(Key::M, Modifiers::NONE),
            Some(Action::ToggleMode)
        );
        assert_eq!(key_action(Key::M, Modifiers::COMMAND), None);
        assert_eq!(key_action(Key::M, Modifiers::SHIFT), None);
        assert!(!action_allows_repeat(Action::ToggleMode));
        assert!(action_allows_repeat(Action::Next));
        let mut flow = NavigationFlow::default();
        let mut paths: Vec<SourceRef> = ["0.png", "1.png", "2.png"]
            .map(|path| SourceRef::file(Path::new(path)))
            .into();
        assert!(flow.enqueue(Navigation::Next, 0, paths.len(), ReadingMode::Book));
        reset_navigation(&mut flow, &mut paths);
        assert!(flow.is_idle() && paths.is_empty());

        let mut queue = SaveQueue::default();
        let settings = AppSettings {
            reading_mode: ReadingMode::Book,
            ..Default::default()
        };
        assert!(queue.request(1, settings.clone()));
        assert_eq!(queue.next().unwrap(), (1, settings));

        let clicked = Some(SourceRef::file(Path::new("2.png")));
        let anchor = Some(SourceRef::file(Path::new("1.png")));
        assert_eq!(
            mode_target(&clicked, &anchor, &None),
            clicked,
            "switching layouts keeps the clicked page, not the spread anchor"
        );
        assert_eq!(mode_target(&None, &anchor, &clicked), anchor);
    }
    #[test]
    fn animation_changes_only_pages_whose_target_moved() {
        assert_eq!(animation_changes(&[0, 0], &[0, 0]), Vec::new());
        assert_eq!(animation_changes(&[1, 0], &[0, 0]), vec![(0, 1)]);
        assert_eq!(animation_changes(&[1, 1], &[0, 0]), vec![(0, 1), (1, 1)]);
        assert!(animation_frame_is_current(2, 2, 0));
        assert!(!animation_frame_is_current(1, 2, 0), "target advanced");
        assert!(!animation_frame_is_current(0, 0, 0), "already committed");
    }

    #[test]
    fn slow_preparation_rejects_stale_pages_and_resubmits_both_targets() {
        let requested = [1, 0];
        let now_targets = [2, 1];
        let committed = [0, 0];
        assert!(requested.iter().zip(now_targets).zip(committed).all(
            |((prepared, target), committed)| !animation_frame_is_current(
                *prepared, target, committed
            )
        ));
        assert_eq!(
            animation_changes(&now_targets, &committed),
            vec![(0, 2), (1, 1)],
            "both pages remain scheduled after stale preparation"
        );
    }

    #[test]
    fn finite_page_finishes_while_infinite_partner_keeps_playing() {
        let spread = SpreadPlayback::new(
            [
                Some(timing(&[20, 30, 50], model::LoopMode::Once)),
                Some(timing(&[100, 100], model::LoopMode::Infinite)),
            ],
            Duration::ZERO,
        );
        assert_eq!(spread.targets(ms(500)), [2, 1]);
        assert!(spread_any_playing(
            &Some(SpreadPlayback::new(
                [
                    Some(timing(&[20, 30, 50], model::LoopMode::Once)),
                    Some(timing(&[100, 100], model::LoopMode::Infinite)),
                ],
                Duration::ZERO,
            )),
            ms(500)
        ));
        assert!(!spread.all_finished(ms(500)));
    }

    #[test]
    fn group_playback_policy_pauses_resumes_and_restarts() {
        assert_eq!(
            group_playback_action(true, false),
            GroupPlaybackAction::Pause
        );
        assert_eq!(
            group_playback_action(false, true),
            GroupPlaybackAction::Restart
        );
        assert_eq!(
            group_playback_action(false, false),
            GroupPlaybackAction::Resume
        );
    }

    #[test]
    fn rotation_keeps_current_page_frames_and_clock_progress() {
        let first = animated_media(
            (0..3)
                .map(|index| Arc::new(RgbaImage::from_pixel(2, 1, image::Rgba([index, 0, 0, 255]))))
                .collect(),
            &[20, 30, 50],
            model::LoopMode::Infinite,
        );
        let second = animated_media(
            (0..2)
                .map(|index| Arc::new(RgbaImage::from_pixel(3, 1, image::Rgba([0, index, 0, 255]))))
                .collect(),
            &[100, 100],
            model::LoopMode::Infinite,
        );
        let mut clocks = SpreadPlayback::new(
            [
                Some(timing(&[20, 30, 50], model::LoopMode::Infinite)),
                Some(timing(&[100, 100], model::LoopMode::Infinite)),
            ],
            Duration::ZERO,
        );
        clocks.pause(ms(35));
        let frames = clocks.targets(ms(35));
        assert_eq!(frames, [1, 0]);
        let prepared = prepare(
            Work::Spread {
                media: vec![first, second],
                frames: frames.clone(),
                turns: 1,
                cap: MAX_TILE,
            },
            &AtomicBool::new(false),
        )
        .image
        .unwrap();
        assert_eq!(prepared.frame, frames[0]);
        assert_eq!(prepared.others[0].frame, frames[1]);
        assert_eq!(prepared.tiled.dimensions, (1, 2));
        assert_eq!(prepared.others[0].tiled.dimensions, (1, 3));
        clocks.resume(ms(35));
        assert!(clocks.any_playing(ms(35)));
        assert_eq!(clocks.frame(0, ms(35)), 1, "rotation did not reset A");
        assert_eq!(clocks.frame(1, ms(35)), 0, "rotation did not reset B");
    }

    #[test]
    fn animation_job_prepares_only_changed_pages_and_frame_indices() {
        let media = animated_media(
            (0..3)
                .map(|index| Arc::new(RgbaImage::from_pixel(2, 1, image::Rgba([index, 0, 0, 255]))))
                .collect(),
            &[20, 30, 50],
            model::LoopMode::Infinite,
        );
        let result = prepare(
            Work::AnimateSpread {
                generation: 7,
                pages: vec![AnimatePage {
                    page: 0,
                    media,
                    frame: 2,
                }],
                turns: 0,
                cap: MAX_TILE,
            },
            &AtomicBool::new(false),
        );
        assert!(result.image.is_err(), "animation carries no whole image");
        let update = result.animation.unwrap().unwrap();
        assert_eq!(update.generation, 7);
        assert_eq!(update.pages.len(), 1);
        assert_eq!(update.pages[0].page, 0);
        assert_eq!(update.pages[0].frame, 2);
        assert_eq!(update.pages[0].tiled.dimensions, (2, 1));
        assert_eq!(update.pages[0].tiled.tiles[0].pixels[..4], [2, 0, 0, 255]);
    }

    #[test]
    fn gif_plus_static_builds_one_clock_and_suppresses_background_while_playing() {
        let still = media(Arc::new(RgbaImage::new(2, 2)));
        let gif = animated_media(
            (0..2)
                .map(|index| Arc::new(RgbaImage::from_pixel(2, 2, image::Rgba([index, 0, 0, 255]))))
                .collect(),
            &[100, 100],
            model::LoopMode::Once,
        );
        assert!(page_timing(&still).is_none());
        assert!(page_timing(&gif).is_some());
        let spread = SpreadPlayback::new([page_timing(&still), page_timing(&gif)], Duration::ZERO);
        assert_eq!(spread.targets(ms(150)), [0, 1]);
        assert!(spread_any_playing(&Some(spread), ms(150)));
        assert!(!SpreadPlayback::new([None, None], Duration::ZERO).any_playing(Duration::ZERO));
    }

    #[test]
    fn edit_shortcuts_distinguish_command_shift_and_keep_legacy_keys() {
        assert_eq!(key_action(Key::C, Modifiers::NONE), Some(Action::Edit));
        assert_eq!(
            key_action(Key::X, Modifiers::NONE),
            Some(Action::FlipHorizontal)
        );
        assert_eq!(
            key_action(Key::Y, Modifiers::NONE),
            Some(Action::FlipVertical)
        );
        assert_eq!(
            key_action(Key::Enter, Modifiers::NONE),
            Some(Action::CropApply)
        );
        assert_eq!(
            key_action(Key::Escape, Modifiers::NONE),
            Some(Action::Escape)
        );
        assert_eq!(
            key_action(Key::S, Modifiers::COMMAND),
            Some(Action::SaveOverwrite)
        );
        let command_shift = Modifiers {
            command: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(key_action(Key::S, command_shift), Some(Action::SaveAs));
        assert_eq!(
            key_action(Key::Z, Modifiers::COMMAND),
            Some(Action::UndoEdit)
        );
        assert_eq!(key_action(Key::S, Modifiers::NONE), None);
        // Existing shortcuts keep their meaning.
        assert_eq!(key_action(Key::C, Modifiers::COMMAND), Some(Action::Copy));
        assert_eq!(
            key_action(Key::W, Modifiers::NONE),
            Some(Action::WideToggle)
        );
        assert_eq!(
            key_action(Key::M, Modifiers::NONE),
            Some(Action::ToggleMode)
        );
        assert_eq!(key_action(Key::P, Modifiers::NONE), Some(Action::Playback));
        assert_eq!(
            key_action(Key::Enter, Modifiers::ALT),
            Some(Action::Fullscreen)
        );
        for action in [
            Action::Edit,
            Action::UndoEdit,
            Action::FlipHorizontal,
            Action::FlipVertical,
            Action::SaveOverwrite,
            Action::SaveAs,
            Action::CropApply,
            Action::CropCancel,
            Action::EditExit,
        ] {
            assert!(!action_allows_repeat(action), "{action:?} must not repeat");
        }
    }

    #[test]
    fn editing_bakes_display_rotation_and_exit_restores_the_original_view() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        install_fixture(&mut app, 1);
        let original = app.media.clone().unwrap();
        let rotation = app.rotation;
        let source = app.media.as_ref().unwrap().source.clone();
        app.begin_edit(&ctx, rotation, None, source, false);
        assert!(app.editing());
        assert_eq!(app.rotation.displayed, 0);
        assert_eq!(app.rotation.requested, 0);
        assert_eq!(app.dimensions, (3, 4));
        assert_eq!(app.edit_ui.as_ref().unwrap().original_rotation.displayed, 1);

        app.perform(Action::RotateLeft, Rect::NOTHING, &ctx);
        assert!(app.edit_ui.as_ref().unwrap().session.dirty());
        assert_eq!(app.dimensions, (4, 3));
        app.perform(Action::FlipHorizontal, Rect::NOTHING, &ctx);
        assert_eq!(app.dimensions, (4, 3));

        app.perform(Action::UndoEdit, Rect::NOTHING, &ctx);
        assert_eq!(app.dimensions, (4, 3));
        assert!(app.edit_ui.as_ref().unwrap().session.dirty());
        app.perform(Action::UndoEdit, Rect::NOTHING, &ctx);
        assert_eq!(app.dimensions, (3, 4));
        assert!(!app.edit_ui.as_ref().unwrap().session.dirty());
        assert_eq!(app.rotation.displayed, 0);

        app.perform(Action::EditExit, Rect::NOTHING, &ctx);
        assert!(!app.editing());
        assert_eq!(app.rotation.displayed, 1);
        assert_eq!(app.rotation.requested, 1);
        assert_eq!(app.dimensions, (4, 3));
        assert!(Arc::ptr_eq(app.media.as_ref().unwrap(), &original));
    }

    #[test]
    fn book_edit_remembers_and_restores_book_mode() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        install_fixture(&mut app, 0);
        app.settings.reading_mode = ReadingMode::Book;
        app.display_mode = ReadingMode::Book;
        let rotation = app.rotation;
        let source = app.media.as_ref().unwrap().source.clone();
        app.begin_edit(&ctx, rotation, Some(ReadingMode::Book), source, false);
        assert_eq!(
            app.edit_ui.as_ref().unwrap().restore_mode,
            Some(ReadingMode::Book)
        );
        app.perform(Action::EditExit, Rect::NOTHING, &ctx);
        assert!(!app.editing());
        assert_eq!(app.settings.reading_mode, ReadingMode::Book);
        app.loader.cancel();
    }

    #[test]
    fn edit_rejects_read_only_formats_animated_media_and_archive_overwrite() {
        use crate::model::source::ArchiveKind;
        let ctx = egui::Context::default();
        for name in ["photo.gif", "scan.tiff", "icon.ico"] {
            let mut app = test_app();
            install_fixture(&mut app, 0);
            let source = SourceRef::file(Path::new(name));
            app.media = Some(Arc::new(model::LoadedMedia {
                source: source.clone(),
                frames: vec![model::MediaFrame {
                    pixels: Arc::new(RgbaImage::new(4, 3)),
                    delay: Duration::ZERO,
                }],
                loop_mode: model::LoopMode::Once,
            }));
            app.selected = Some(source.clone());
            app.displayed = Some(source.clone());
            app.current_file = Some(source);
            app.request_edit(false, &ctx);
            assert!(!app.editing(), "{name} must stay read-only");
            assert!(
                app.feedback
                    .notice
                    .as_ref()
                    .is_some_and(|notice| notice.text.contains("PNG")),
                "{name} should explain the writable formats"
            );
        }

        let mut app = test_app();
        install_fixture(&mut app, 0);
        let pixels = Arc::new(RgbaImage::new(2, 2));
        let animated = Arc::new(model::LoadedMedia {
            source: SourceRef::file(Path::new("still.png")),
            frames: (0..2)
                .map(|_| model::MediaFrame {
                    pixels: Arc::clone(&pixels),
                    delay: Duration::ZERO,
                })
                .collect(),
            loop_mode: model::LoopMode::Once,
        });
        app.selected = Some(animated.source.clone());
        app.displayed = Some(animated.source.clone());
        app.current_file = Some(animated.source.clone());
        app.media = Some(animated);
        app.request_edit(false, &ctx);
        assert!(!app.editing());
        assert_eq!(
            app.feedback
                .notice
                .as_ref()
                .map(|notice| notice.text.as_str()),
            Some(app.text().edit_single_frame)
        );

        let mut app = editing_fixture();
        let archive = SourceRef::Archive {
            archive: PathBuf::from("book.cbz"),
            archive_len: 1,
            archive_mtime_secs: 0,
            archive_mtime_nanos: 0,
            kind: ArchiveKind::Zip,
            index: 0,
            name: "page.png".into(),
            size: 0,
            crc: 0,
        };
        app.selected = Some(archive.clone());
        app.edit_ui.as_mut().unwrap().source = archive.clone();
        app.request_save_overwrite(&ctx);
        assert!(app.edit_ui.as_ref().unwrap().confirm.is_none());
        assert_eq!(
            app.feedback
                .notice
                .as_ref()
                .map(|notice| notice.text.as_str()),
            Some(app.text().archive_cannot_overwrite)
        );
        assert_eq!(editable_format(&archive), Some(export::ExportFormat::Png));
        let mut gif = archive;
        match &mut gif {
            SourceRef::Archive { name, .. } => *name = "page.gif".into(),
            SourceRef::File(_) => unreachable!(),
        }
        assert_eq!(editable_format(&gif), None);
        assert_eq!(
            editable_format(&SourceRef::file(Path::new("scan.tiff"))),
            None
        );
    }

    #[test]
    fn crop_enter_applies_and_escape_cancels_without_touching_the_session() {
        let ctx = egui::Context::default();
        let mut app = editing_fixture();
        app.perform(Action::CropApply, Rect::NOTHING, &ctx);
        assert!(!app.crop_active());
        assert!(!app.edit_ui.as_ref().unwrap().session.dirty());

        app.enter_crop();
        assert!(app.crop_active());
        if let Some(crop) = app.edit_ui.as_mut().and_then(|edit| edit.crop.as_mut()) {
            crop.rect = edit::PixelRect {
                x: 1,
                y: 0,
                w: 2,
                h: 2,
            };
        }
        app.perform(Action::CropApply, Rect::NOTHING, &ctx);
        assert!(!app.crop_active());
        assert_eq!(app.dimensions, (2, 2));
        assert!(app.edit_ui.as_ref().unwrap().session.dirty());

        app.enter_crop();
        assert!(app.crop_active());
        // Esc leaves crop mode but keeps the applied edit.
        app.perform(Action::Escape, Rect::NOTHING, &ctx);
        assert!(!app.crop_active());
        assert!(app.editing());
        assert_eq!(app.dimensions, (2, 2));
        assert!(app.edit_ui.as_ref().unwrap().session.dirty());

        app.perform(Action::Escape, Rect::NOTHING, &ctx);
        assert!(!app.editing());
        assert_eq!(app.dimensions, (4, 3));
    }

    #[test]
    fn editing_swallows_navigation_mode_and_preview_shortcuts() {
        let ctx = egui::Context::default();
        let mut app = editing_fixture();
        app.siblings.push(SourceRef::file(Path::new("next.png")));
        let source = app.selected.clone().unwrap();
        for action in [
            Action::Next,
            Action::Previous,
            Action::First,
            Action::Last,
            Action::JumpFiles(1),
            Action::Pan {
                vertical: false,
                forward: true,
            },
            Action::ToggleMode,
            Action::WideToggle,
            Action::Playback,
        ] {
            app.perform(action, Rect::NOTHING, &ctx);
        }
        assert_eq!(app.selected.as_ref(), Some(&source));
        assert!(app.navigation.is_idle());
        assert_eq!(app.settings.reading_mode, ReadingMode::Single);
        let visible = app.settings.thumbnails_visible;
        app.toggle_previews(&ctx);
        assert_eq!(app.settings.thumbnails_visible, visible);
    }

    #[test]
    fn book_request_edit_defers_entry_until_the_single_frame_commits() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        install_fixture(&mut app, 0);
        app.settings.reading_mode = ReadingMode::Book;
        app.display_mode = ReadingMode::Book;
        app.request_edit(true, &ctx);
        assert!(!app.editing());
        assert!(app.edit_pending.is_some());
        let pending = app.edit_pending.as_ref().unwrap();
        assert_eq!(pending.restore, ReadingMode::Book);
        assert!(pending.crop);
        assert_eq!(pending.source, app.media.as_ref().unwrap().source);
        assert_eq!(app.settings.reading_mode, ReadingMode::Single);
        app.edit_pending = None;
        app.loader.cancel();
    }

    #[test]
    fn overwrite_confirmation_targets_the_original_and_escape_cancels_it() {
        let ctx = egui::Context::default();
        let mut app = editing_fixture();
        app.request_save_overwrite(&ctx);
        let confirm = app
            .edit_ui
            .as_ref()
            .unwrap()
            .confirm
            .as_ref()
            .expect("overwrite asks for confirmation");
        assert_eq!(confirm.format, export::ExportFormat::Png);
        assert!(confirm.path.ends_with("fixture.png"));
        assert!(confirm.overwrite);
        app.perform(Action::Escape, Rect::NOTHING, &ctx);
        assert!(app.edit_ui.as_ref().unwrap().confirm.is_none());
        assert!(app.editing());
    }

    #[test]
    fn edit_rejects_when_the_selection_no_longer_matches_the_media() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        install_fixture(&mut app, 0);
        let failed = SourceRef::file(Path::new("failed-target.png"));
        app.selected = Some(failed.clone());
        app.request_edit(false, &ctx);
        assert!(!app.editing());
        assert_eq!(
            app.feedback
                .notice
                .as_ref()
                .map(|notice| notice.text.as_str()),
            Some(app.text().edit_source_mismatch)
        );
        // The frozen edit source is used for saves, not `selected`.
        let mut app = editing_fixture();
        let frozen = app.edit_ui.as_ref().unwrap().source.clone();
        app.selected = Some(failed);
        app.request_save_overwrite(&ctx);
        let confirm = app
            .edit_ui
            .as_ref()
            .unwrap()
            .confirm
            .as_ref()
            .expect("overwrite still targets the frozen source");
        assert_eq!(confirm.path.as_path(), frozen.path());
        assert!(confirm.overwrite);
    }

    #[test]
    fn save_confirm_strips_enter_for_every_modifier_combination() {
        for modifiers in [
            Modifiers::NONE,
            Modifiers::SHIFT,
            Modifiers::COMMAND,
            Modifiers::ALT,
            Modifiers::COMMAND | Modifiers::SHIFT,
            Modifiers::COMMAND | Modifiers::ALT,
        ] {
            let enter = egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            };
            assert!(save_confirm_strips_event(&enter), "{modifiers:?}");
        }
        for event in [
            egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            egui::Event::Key {
                key: Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            },
            egui::Event::Text("\n".into()),
        ] {
            assert!(!save_confirm_strips_event(&event), "{event:?}");
        }
    }

    #[test]
    fn crop_handles_scale_with_dpi_and_stay_grabbable() {
        assert_eq!(crop_handle_size(1.), 8.);
        assert_eq!(crop_handle_size(0.5), 8.);
        assert!((crop_handle_size(2.) - 9.5).abs() < 1e-4);
        assert_eq!(crop_handle_size(4.), 12.);
        assert!(crop_handle_size(2.) > crop_handle_size(1.));

        // The same point that is outside a 1x hit radius is inside a 2x one.
        let screen = [100., 100., 200., 100.];
        let edge = Pos2::new(100. + 200. + 7., 100. + 50.);
        assert_eq!(
            crop_handle_at(screen, edge, crop_handle_size(1.)),
            Some(CropHandle::East)
        );
        assert_eq!(crop_handle_at(screen, edge, 1.), None);
    }

    #[test]
    fn crop_handle_points_push_outward_only_for_tiny_rects() {
        let screen = [100., 200., 300., 200.];
        let points = crop_handle_points(screen, 9.);
        let center_of = |handle: CropHandle| {
            points
                .iter()
                .find(|(candidate, _)| *candidate == handle)
                .map(|(_, pos)| *pos)
                .unwrap()
        };
        assert_eq!(center_of(CropHandle::NorthWest), Pos2::new(100., 200.));
        assert_eq!(center_of(CropHandle::SouthEast), Pos2::new(400., 400.));
        assert_eq!(center_of(CropHandle::North), Pos2::new(250., 200.));
        assert_eq!(center_of(CropHandle::West), Pos2::new(100., 300.));

        // A 1x1 crop at high zoom is a single point on screen; the corner and
        // edge handles must leave the rect, or all eight overlap.
        let tiny = crop_handle_points([100., 100., 0., 0.], 9.);
        for (handle, pos) in tiny {
            assert!(
                pos != Pos2::new(100., 100.),
                "{handle:?} is still stacked on the rect"
            );
            if matches!(handle, CropHandle::NorthWest | CropHandle::SouthEast) {
                assert!(
                    pos.x <= 100. - 9. + 1e-4 || pos.x >= 100. + 9. - 1e-4,
                    "{handle:?} {pos:?}"
                );
            }
        }
        let tiny_center_of = |handle: CropHandle| {
            tiny.iter()
                .find(|(candidate, _)| *candidate == handle)
                .map(|(_, pos)| *pos)
                .unwrap()
        };
        assert_eq!(tiny_center_of(CropHandle::NorthWest), Pos2::new(91., 91.));
        assert_eq!(tiny_center_of(CropHandle::SouthEast), Pos2::new(109., 109.));
    }

    #[test]
    fn tiny_rect_handles_hit_their_own_centres() {
        use CropHandle::*;
        // A sub-pixel rect on screen: every handle is pushed out by `size`, so
        // neighbouring hit boxes overlap heavily.
        let screen = [100., 100., 0.1, 0.1];
        let size = 8.;
        let points = crop_handle_points(screen, size);
        for (handle, center) in points {
            assert_eq!(
                crop_handle_at(screen, center, size),
                Some(handle),
                "{handle:?} centre must hit itself, not a neighbour"
            );
        }
        // Off-centre clicks inside an overlap still resolve to the nearest one.
        let north = points
            .iter()
            .find(|(handle, _)| *handle == North)
            .unwrap()
            .1;
        let closer_to_north = Pos2::new(north.x - 1., north.y);
        assert_eq!(crop_handle_at(screen, closer_to_north, size), Some(North));
    }

    #[test]
    fn resize_uses_the_pointer_delta_so_outward_handles_do_not_jump() {
        use CropHandle::*;
        let origin = edit::PixelRect {
            x: 10,
            y: 10,
            w: 2,
            h: 2,
        };
        for handle in [
            NorthWest, North, NorthEast, East, SouthEast, South, SouthWest, West,
        ] {
            assert_eq!(
                resize_crop(origin, handle, [0., 0.], (100, 100)),
                origin,
                "{handle:?} must not move before the pointer does"
            );
        }
        // The East handle of this 2px rect is drawn 8pt out, so the press point is
        // at ~20.5px, not at the 12px edge. One pixel of drag must add one pixel.
        let anchor = [20.5, 11.];
        let point = [21.5, 11.];
        let dragged = resize_crop(
            origin,
            East,
            [point[0] - anchor[0], point[1] - anchor[1]],
            (100, 100),
        );
        assert_eq!(dragged.w, 3, "the outward offset is not a boundary");
        assert_eq!(dragged, edit::PixelRect { w: 3, ..origin });

        // Edges stay in the image and the one-pixel minimum is preserved.
        assert_eq!(
            resize_crop(origin, NorthWest, [-50., -50.], (100, 100)),
            edit::PixelRect {
                x: 0,
                y: 0,
                w: 12,
                h: 12
            }
        );
        assert_eq!(
            resize_crop(origin, East, [-50., 0.], (100, 100)),
            edit::PixelRect {
                x: 0,
                y: 10,
                w: 1,
                h: 2
            }
        );
    }

    #[test]
    fn crop_hover_maps_every_handle_to_a_resize_cursor() {
        use CropHandle::*;
        for (handle, icon) in [
            (NorthWest, CursorIcon::ResizeNwSe),
            (SouthEast, CursorIcon::ResizeNwSe),
            (NorthEast, CursorIcon::ResizeNeSw),
            (SouthWest, CursorIcon::ResizeNeSw),
            (North, CursorIcon::ResizeVertical),
            (South, CursorIcon::ResizeVertical),
            (East, CursorIcon::ResizeHorizontal),
            (West, CursorIcon::ResizeHorizontal),
        ] {
            assert_eq!(crop_cursor(handle), icon, "{handle:?}");
        }
    }

    #[test]
    fn crop_zoom_hint_only_appears_below_half_a_pixel_per_pixel() {
        assert!(crop_needs_zoom(0.2, 1.));
        assert!(!crop_needs_zoom(0.5, 1.));
        assert!(!crop_needs_zoom(1., 1.));
        // The hint must agree with the footer's physical-pixel percentage.
        assert!(!crop_needs_zoom(0.45, 1.25));
        assert!(crop_needs_zoom(0.35, 1.25));
        assert!(crop_needs_zoom(0.2, 2.));
        assert!(!crop_needs_zoom(0.25, 2.));
    }

    #[test]
    fn footer_navigation_buttons_disable_during_editing() {
        assert!(!footer_navigation_enabled(true, true));
        assert!(footer_navigation_enabled(false, true));
        assert!(!footer_navigation_enabled(false, false));
        assert!(!footer_navigation_enabled(true, false));
    }

    #[test]
    fn indeterminate_progress_span_stays_inside_its_track() {
        for width in [80.0_f32, 460.] {
            let thumb = (width * 0.3).min(120.);
            for step in 0..10 {
                let (start, end) = indeterminate_span(step as f32 / 10., width);
                assert!(start >= 0. && end <= width + 1e-4, "{start}..{end}");
                assert!((end - start - thumb).abs() < 1e-4);
            }
            let (start, end) = indeterminate_span(0., width);
            assert!(
                start.abs() < 1e-4 && (end - thumb).abs() < 1e-4,
                "enters left"
            );
        }
        // Clamped phases never leave the track.
        let clamped = indeterminate_span(-1., 100.);
        assert!(clamped.0.abs() < 1e-4 && (clamped.1 - 30.).abs() < 1e-4);
        let clamped = indeterminate_span(2., 100.);
        assert!((clamped.0 - 70.).abs() < 1e-4 && (clamped.1 - 100.).abs() < 1e-4);
    }

    #[test]
    fn save_warnings_cover_metadata_undo_and_format_risks() {
        for catalog in [&ui_text::EN, &ui_text::ZH, &ui_text::JA] {
            let overwrite = save_warning_rows(catalog, export::ExportFormat::Jpeg, true, true);
            assert_eq!(overwrite.len(), 3);
            assert_eq!(overwrite[0], catalog.save_confirm_warning);
            assert_eq!(overwrite[1], catalog.save_confirm_irreversible);
            assert_eq!(overwrite[2], catalog.save_confirm_transparency);

            let copy = save_warning_rows(catalog, export::ExportFormat::Webp, false, true);
            assert_eq!(copy, vec![catalog.save_confirm_webp]);

            assert!(
                save_warning_rows(catalog, export::ExportFormat::Png, false, true).is_empty(),
                "a plain copy has no overwrite or format-specific warning to show"
            );
        }
        assert_ne!(
            save_warning_rows(&ui_text::EN, export::ExportFormat::Png, true, false),
            save_warning_rows(&ui_text::ZH, export::ExportFormat::Png, true, false)
        );
    }

    #[test]
    fn cancelling_a_pending_edit_restores_the_view_and_ignores_stale_work() {
        let ctx = egui::Context::default();
        let mut app = pending_book_fixture(false);
        let revision = app.loader.revision;
        let selected = app.selected.clone();
        let displayed = app.displayed.clone();
        app.cancel_pending_edit(&ctx);
        assert!(app.edit_pending.is_none());
        assert!(!app.loading_file);
        assert!(app.candidate.is_none());
        assert_eq!(app.settings.reading_mode, ReadingMode::Book);
        assert_eq!(app.display_mode, ReadingMode::Book);
        assert_eq!(app.selected, selected);
        assert_eq!(app.displayed, displayed);
        assert_eq!(app.rotation.displayed, 1);
        assert!(
            !app.loader.finish(revision),
            "a stale completion must not commit"
        );
        // The same page can request editing again.
        app.request_edit(false, &ctx);
        assert!(app.edit_pending.is_some());
        app.cancel_pending_edit(&ctx);
    }

    #[test]
    fn pending_edit_is_cancelled_when_the_source_changes_before_commit() {
        let ctx = egui::Context::default();
        let mut app = pending_book_fixture(false);
        app.loader.cancel();
        app.loader.running = false;
        app.candidate = None;
        app.loading_file = false;
        let pixels = Arc::new(RgbaImage::new(2, 2));
        app.media = Some(Arc::new(model::LoadedMedia {
            source: SourceRef::file(Path::new("other.png")),
            frames: vec![model::MediaFrame {
                pixels,
                delay: Duration::ZERO,
            }],
            loop_mode: model::LoopMode::Once,
        }));
        app.resolve_pending_edit(&ctx);
        assert!(app.edit_pending.is_none());
        assert!(!app.editing());
        assert_eq!(app.settings.reading_mode, ReadingMode::Book);
    }

    #[test]
    fn opening_another_file_cancels_the_pending_edit_and_restores_the_mode() {
        let ctx = egui::Context::default();
        let mut app = pending_book_fixture(false);
        app.open_path(PathBuf::from("notes.txt"), &ctx);
        assert!(app.edit_pending.is_none());
        assert!(!app.editing());
        assert_eq!(app.settings.reading_mode, ReadingMode::Book);
        assert!(!app.loading_file);
    }

    #[test]
    fn save_as_risks_are_decided_before_starting_a_worker() {
        let text = &ui_text::EN;
        assert_eq!(
            save_risk_reason(text, export::ExportFormat::Webp, false),
            Some(text.save_confirm_webp)
        );
        assert_eq!(
            save_risk_reason(text, export::ExportFormat::Jpeg, true),
            Some(text.save_confirm_transparency)
        );
        assert_eq!(
            save_risk_reason(text, export::ExportFormat::Jpeg, false),
            None
        );
        assert_eq!(
            save_risk_reason(text, export::ExportFormat::Png, true),
            None
        );
    }

    #[test]
    fn close_waits_for_a_running_image_save() {
        let ctx = egui::Context::default();
        let mut app = editing_fixture();
        let (sender, receiver) = mpsc::channel();
        app.edit_ui.as_mut().unwrap().saving = Some(SaveJob {
            receiver,
            overwrite: false,
            path: PathBuf::from("copy.png"),
        });
        app.request_close(&ctx);
        assert!(app.close_after_save);
        assert!(!app.close_state.requested);
        // A repeated close attempt must not bypass the pending save.
        app.request_close(&ctx);
        assert!(app.close_after_save);
        assert!(!app.close_state.requested);
        sender.send(Ok(())).unwrap();
        app.poll_save(&ctx);
        assert!(!app.close_after_save);
    }

    #[test]
    fn save_as_target_identity_handles_dot_segments_symlinks_and_platform_case() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("a.png");
        std::fs::write(&file, b"x").unwrap();
        let dotted = dir.path().join(".").join("a.png");
        assert!(same_save_target(&file, &dotted));
        assert!(same_save_target(&dotted, &file));
        assert!(same_save_target(&file, &file));
        let other = dir.path().join("b.png");
        std::fs::write(&other, b"x").unwrap();
        assert!(!same_save_target(&file, &other));
        assert!(!same_save_target(
            Path::new("/tmp/x/../y/a.png"),
            Path::new("/tmp/x/a.png")
        ));

        // Nonexistent target behind a symlinked parent keeps one identity.
        #[cfg(unix)]
        {
            let alias = dir.path().join("alias");
            std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
            assert!(same_save_target(
                &dir.path().join("future.png"),
                &alias.join("future.png")
            ));
            assert!(!same_save_target(
                &dir.path().join("future.png"),
                &alias.join("different.png")
            ));
        }

        // Relative and absolute spellings agree.
        let cwd = std::env::current_dir().unwrap();
        let relative = Path::new("oxeye-future-target.png");
        assert!(same_save_target(relative, &cwd.join(relative)));

        let case_insensitive = cfg!(any(target_os = "windows", target_os = "macos"));
        assert_eq!(
            same_save_target(Path::new("/tmp/x/A.PNG"), Path::new("/tmp/x/a.png")),
            case_insensitive,
            "case folding follows the platform's default filesystem"
        );
        assert_eq!(
            same_save_target(Path::new("/tmp/x/Ä.png"), Path::new("/tmp/x/ä.png")),
            case_insensitive,
            "non-ASCII case folding uses Unicode lowercasing"
        );
        assert!(
            !same_save_target(Path::new("/tmp/x/Ä.png"), Path::new("/tmp/x/ö.png")),
            "different names stay different"
        );
    }

    #[test]
    fn successful_ops_drop_the_crop_but_failed_ops_keep_it() {
        let ctx = egui::Context::default();
        let mut app = editing_fixture();
        app.enter_crop();
        assert!(app.crop_active());
        app.perform(Action::RotateRight, Rect::NOTHING, &ctx);
        assert!(!app.crop_active());
        assert!(app.edit_ui.as_ref().unwrap().session.dirty());

        app.enter_crop();
        let before = app.edit_ui.as_ref().unwrap().crop.as_ref().unwrap().rect;
        app.apply_edit_op(
            edit::EditOp::Crop(edit::PixelRect {
                x: 99,
                y: 99,
                w: 1,
                h: 1,
            }),
            &ctx,
        );
        let after = app.edit_ui.as_ref().unwrap().crop.as_ref().unwrap().rect;
        assert_eq!(before, after, "a rejected crop must not drop the selection");

        app.perform(Action::FlipHorizontal, Rect::NOTHING, &ctx);
        app.enter_crop();
        app.perform(Action::UndoEdit, Rect::NOTHING, &ctx);
        assert!(!app.crop_active(), "undo also changes dimensions");
    }

    #[test]
    fn c_starts_crop_and_reuses_the_same_session() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        install_fixture(&mut app, 0);
        app.perform(Action::Edit, Rect::NOTHING, &ctx);
        assert!(app.editing());
        assert!(app.crop_active());
        app.perform(Action::FlipHorizontal, Rect::NOTHING, &ctx);
        assert!(app.edit_ui.as_ref().unwrap().session.dirty());
        app.perform(Action::Edit, Rect::NOTHING, &ctx);
        assert!(app.crop_active());
        assert!(
            app.edit_ui.as_ref().unwrap().session.dirty(),
            "pressing C in edit mode must not start a new session"
        );
    }

    #[test]
    fn book_edit_restores_the_pre_entry_display_rotation() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        install_fixture(&mut app, 1);
        app.settings.reading_mode = ReadingMode::Book;
        app.display_mode = ReadingMode::Book;
        let rotation = app.rotation;
        let source = app.media.as_ref().unwrap().source.clone();
        app.begin_edit(&ctx, rotation, Some(ReadingMode::Book), source, false);
        assert_eq!(app.dimensions, (3, 4));
        app.perform(Action::EditExit, Rect::NOTHING, &ctx);
        assert_eq!(app.rotation.displayed, 1);
        assert_eq!(app.rotation.requested, 1);
        assert_eq!(app.settings.reading_mode, ReadingMode::Book);
        app.loader.cancel();
    }

    #[test]
    fn slow_crop_moves_accumulate_subpixel_motion() {
        let origin = edit::PixelRect {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
        };
        let anchor = [0., 0.];
        let first = moved_crop(origin, anchor, [0.2, 0.2], (100, 100));
        let middle = moved_crop(origin, anchor, [0.4, 0.4], (100, 100));
        let end = moved_crop(origin, anchor, [0.6, 0.6], (100, 100));
        assert_eq!(first.x, 0);
        assert_eq!(middle.x, 0);
        assert_eq!(end.x, 1, "the total delta must not be rounded per frame");
        assert_eq!(end.y, 1);
        // Clamping still applies to the composed delta.
        let clamped = moved_crop(origin, anchor, [500., 500.], (100, 100));
        assert_eq!(clamped.x, 98);
        assert_eq!(clamped.y, 98);
    }

    fn fixture_media(source: SourceRef, pixels: Arc<RgbaImage>) -> Arc<model::LoadedMedia> {
        Arc::new(model::LoadedMedia {
            source,
            frames: vec![model::MediaFrame {
                pixels,
                delay: Duration::ZERO,
            }],
            loop_mode: model::LoopMode::Once,
        })
    }

    #[test]
    fn overwrite_reload_applies_the_baked_rotation_only_once() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("rotated.png");
        let original =
            RgbaImage::from_fn(4, 3, |x, y| image::Rgba([(y * 4 + x + 1) as u8, 0, 0, 255]));
        original.save(&path).unwrap();
        let source = SourceRef::file(&path);
        let pixels = Arc::new(original);
        let mut app = test_app();
        app.settings.reading_mode = ReadingMode::Single;
        app.display_mode = ReadingMode::Single;
        app.media = Some(fixture_media(source.clone(), Arc::clone(&pixels)));
        app.tiled = Some(Arc::new(
            prepare_tiles(&pixels, MAX_TILE, &AtomicBool::new(false)).unwrap(),
        ));
        app.selected = Some(source.clone());
        app.displayed = Some(source.clone());
        app.current_file = Some(source.clone());
        app.siblings = vec![source.clone()];
        app.dimensions = (4, 3);
        app.rotation.commit(1);
        let ctx = egui::Context::default();
        let rotation = app.rotation;
        app.begin_edit(&ctx, rotation, None, source.clone(), false);
        let saved = (**app.edit_ui.as_ref().unwrap().session.preview()).clone();
        assert_eq!(saved.dimensions(), (3, 4));
        export::atomic_write(&path, &saved, export::ExportFormat::Png, 90).unwrap();

        // Hold the loader so the reload request is inspectable.
        app.loader.running = true;
        app.finish_overwrite(&ctx);
        assert_eq!(
            app.rotation.displayed, 0,
            "an overwrite must not keep the pre-edit display rotation"
        );
        assert_eq!(app.rotation.requested, 0);
        let turns = match app.loader.pending.as_ref().map(|(_, work)| work) {
            Some(Work::Load(_, _, _, _, _, _, _, turns)) => *turns,
            _ => panic!("overwrite did not queue a reload"),
        };
        assert_eq!(turns, 0, "the saved pixels already contain the rotation");
        let (revision, work) = app.loader.pending.take().unwrap();
        app.loader.running = false;
        let prepared = prepare(work, &AtomicBool::new(false)).image.unwrap();
        assert_eq!(prepared.turns, 0);
        assert_eq!(prepared.media.frames[0].pixels.as_ref(), &saved);
        app.candidate = Some(Candidate {
            revision,
            image: prepared,
            painted_frame: None,
        });
        app.commit_candidate(&ctx);
        assert_eq!(app.rotation.displayed, 0);
        assert_eq!(
            app.media.as_ref().unwrap().frames[0].pixels.as_ref(),
            &saved,
            "reload must show the once-rotated file, not rotate again"
        );
    }

    #[test]
    fn layout_correction_reload_keeps_the_prepared_turns() {
        let wide = SourceRef::file(Path::new("wide.png"));
        let tall = SourceRef::file(Path::new("tall.png"));
        let wide_pixels = Arc::new(RgbaImage::from_pixel(40, 10, image::Rgba([1, 0, 0, 255])));
        let tall_pixels = Arc::new(RgbaImage::from_pixel(10, 40, image::Rgba([2, 0, 0, 255])));
        let mut app = test_app();
        app.settings.reading_mode = ReadingMode::Book;
        app.settings.auto_wide = true;
        app.display_mode = ReadingMode::Book;
        app.siblings = vec![wide.clone(), tall.clone()];
        app.selected = Some(wide.clone());
        app.displayed = Some(wide.clone());
        app.current_file = Some(wide.clone());
        let ctx = egui::Context::default();
        let prepared = prepare_spread(
            vec![
                fixture_media(wide.clone(), wide_pixels),
                fixture_media(tall.clone(), tall_pixels),
            ],
            &[0, 0],
            1,
            true,
            MAX_TILE,
            &AtomicBool::new(false),
        )
        .unwrap();
        app.loader.running = true;
        app.candidate = Some(Candidate {
            revision: app.loader.revision,
            image: prepared,
            painted_frame: None,
        });
        app.commit_candidate(&ctx);
        let turns = match app.loader.pending.as_ref().map(|(_, work)| work) {
            Some(Work::Load(_, _, _, _, _, _, _, turns)) => *turns,
            _ => panic!("layout correction did not queue a reload"),
        };
        assert_eq!(
            turns, 1,
            "a correction reload must keep the displayed turns"
        );

        // Ordinary navigation still starts from an unrotated file.
        app.load(&ctx, tall, true);
        let turns = match app.loader.pending.as_ref().map(|(_, work)| work) {
            Some(Work::Load(_, _, _, _, _, _, _, turns)) => *turns,
            _ => panic!("navigation did not queue a load"),
        };
        assert_eq!(turns, 0);
    }

    #[test]
    fn cancelling_a_committed_pending_edit_rebuilds_the_book_view() {
        let ctx = egui::Context::default();
        let mut app = pending_book_fixture(false);
        app.loader.cancel();
        app.loader.running = false;
        app.candidate = None;
        app.loading_file = false;
        let pending = app.edit_pending.as_ref().unwrap();
        let source = pending.source.clone();
        let rotation = pending.rotation;
        let restore = pending.restore;
        let pixels = Arc::new(RgbaImage::new(2, 2));
        app.media = Some(Arc::new(model::LoadedMedia {
            source: source.clone(),
            frames: (0..2)
                .map(|_| model::MediaFrame {
                    pixels: Arc::clone(&pixels),
                    delay: Duration::ZERO,
                })
                .collect(),
            loop_mode: model::LoopMode::Once,
        }));
        app.selected = Some(source.clone());
        app.displayed = Some(source.clone());
        app.current_file = Some(source.clone());
        app.display_mode = ReadingMode::Single;
        app.rotation = rotation;
        app.resolve_pending_edit(&ctx);
        assert!(app.edit_pending.is_none());
        assert_eq!(app.settings.reading_mode, restore);
        assert_eq!(app.display_mode, ReadingMode::Book);
        assert_eq!(app.rotation.displayed, rotation.displayed);
        assert_eq!(app.media.as_ref().unwrap().source, source);
        assert!(
            app.loading_file,
            "the Book view is rebuilt from the edited page"
        );
        app.loader.cancel();
    }

    #[test]
    fn close_request_blocks_new_saves_and_failed_saves_stay_open() {
        let ctx = egui::Context::default();
        let mut app = editing_fixture();
        app.close_state.requested = true;
        app.start_save(
            PathBuf::from("copy.png"),
            export::ExportFormat::Png,
            false,
            &ctx,
        );
        assert!(app.edit_ui.as_ref().unwrap().saving.is_none());
        assert_eq!(
            app.feedback
                .notice
                .as_ref()
                .map(|notice| notice.text.as_str()),
            Some(app.text().save_blocked_closing)
        );

        app.close_state.requested = false;
        let (sender, receiver) = mpsc::channel();
        app.edit_ui.as_mut().unwrap().saving = Some(SaveJob {
            receiver,
            overwrite: false,
            path: PathBuf::from("copy.png"),
        });
        app.request_close(&ctx);
        assert!(app.close_after_save);
        sender.send(Err("disk full".into())).unwrap();
        app.poll_save(&ctx);
        assert!(
            !app.close_after_save,
            "a failed save must keep the window open"
        );
        assert!(!app.close_state.requested);
        assert!(app.editing());
        assert!(!app.save_delays_close(&ctx, false));
    }
}
