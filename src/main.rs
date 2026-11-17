//! Oxeye: a small read-only image viewer built on egui/eframe.

mod model;
mod navigation;
mod preload;
mod settings;
mod spread_playback;
mod thumb_ui;
mod thumbnails;
mod ui_state;
mod ui_text;
mod viewer;

#[cfg(test)]
use std::path::Path;
use std::{
    collections::HashMap,
    path::PathBuf,
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

use model::DecodePurpose;
use navigation::Navigation;
use preload::{CacheStats, ForegroundToken, Prefetcher};
use settings::{ComicArrowDirection, ReadingMode};
use thumb_ui::{
    CELL_WIDTH, STRIP_HEIGHT, cell_at, clamp_offset, request_range, reveal_offset, visible_range,
};
use thumbnails::Thumbnails;
use ui_state::{
    Feedback, FeedbackKind, NavigationFlow, NavigationTicket, horizontal_navigation,
    restore_defaults, selected_index,
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
}

/// Maps one key press to one viewer action. Keeping this pure makes key order
/// testable without a window.
fn key_action(key: Key, modifiers: Modifiers) -> Option<Action> {
    if modifiers.command && !modifiers.alt {
        return match key {
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
        _ => None,
    }
}

fn action_allows_repeat(action: Action) -> bool {
    !matches!(action, Action::ToggleMode)
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
const SETTING_ARROW: usize = 5;
const SETTING_LANGUAGE: usize = 6;
const SETTING_RESTORE: usize = 7;
const SETTING_CANCEL: usize = 8;
const SETTING_APPLY: usize = 9;
const SETTING_COUNT: usize = 10;

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
    current_file: &Option<PathBuf>,
    selected: &Option<PathBuf>,
    displayed: &Option<PathBuf>,
) -> Option<PathBuf> {
    current_file
        .clone()
        .or_else(|| selected.clone())
        .or_else(|| displayed.clone())
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
fn reset_navigation(flow: &mut NavigationFlow, paths: &mut Vec<PathBuf>) {
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
        PathBuf,
        Prefetcher,
        ForegroundToken,
        u32,
        ReadingMode,
        Language,
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
    selection: Option<(PathBuf, Result<Vec<PathBuf>, String>)>,
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

#[derive(Default)]
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
        Work::Load(path, prefetch, foreground, cap, mode, language) => {
            let path = match std::path::absolute(path) {
                Ok(path) => path,
                Err(error) => {
                    return WorkResult {
                        selection: None,
                        image: Err(
                            ui_text::catalog(language).path_failed_detail(&error.to_string())
                        ),
                        animation: None,
                    };
                }
            };
            // Scan even when decoding fails, so a corrupt first file does not trap navigation.
            let siblings = model::sibling_images(&path);
            let targets = siblings
                .as_ref()
                .ok()
                .and_then(|siblings| {
                    siblings.iter().position(|item| item == &path).map(|index| {
                        navigation::pages(index, siblings.len(), mode)
                            .into_iter()
                            .map(|index| siblings[index].clone())
                            .collect::<Vec<_>>()
                    })
                })
                .unwrap_or_else(|| vec![path.clone()]);
            let selected = targets.first().cloned().unwrap_or_else(|| path.clone());
            let image = (|| {
                let clicked = prefetch.load(&path, cancelled)?;
                let mut media = Vec::new();
                for target in &targets {
                    if target == &path {
                        media.push(clicked.clone());
                    } else {
                        if !prefetch.promote_if_current(target, foreground) {
                            return Err("Image preparation cancelled.".into());
                        }
                        media.push(prefetch.load(target, cancelled)?);
                    }
                }
                if mode == ReadingMode::Book {
                    prepare_spread(media, &[0], 0, true, cap, cancelled)
                } else {
                    prepare_frame(clicked, 0, 0, true, cap, cancelled)
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
    let text =
        RichText::new(label)
            .size(12.5)
            .color(if active { palette.accent } else { palette.text });
    let button = egui::Button::new(text)
        .fill(if active {
            palette.accent_bg
        } else {
            palette.surface
        })
        .stroke(Stroke::new(
            1.,
            if active {
                palette.accent
            } else {
                palette.border
            },
        ))
        .corner_radius(6.)
        .min_size(egui::vec2(34., 28.));
    ui.add_enabled(enabled, button)
        .on_hover_text(hint)
        .clicked()
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
    map: HashMap<PathBuf, ThumbTexture>,
}

impl ThumbTextures {
    fn retain(&mut self, keep: &[PathBuf]) {
        self.map.retain(|path, _| keep.contains(path));
    }

    /// The service returns a thumbnail-sized Arc; a missing current-epoch value
    /// removes the old texture so a stale picture is never shown.
    fn update(&mut self, ctx: &egui::Context, path: &PathBuf, source: Option<Arc<RgbaImage>>) {
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

    fn texture(&self, path: &PathBuf) -> Option<&TextureHandle> {
        self.map.get(path).map(|texture| &texture.image)
    }

    fn clear(&mut self) {
        self.map.clear();
    }
}

struct Oxeye {
    initial: Option<PathBuf>,
    settings: AppSettings,
    prefetch: Prefetcher,
    loader: Loader,
    worker_tx: Sender<Completion>,
    worker_rx: Receiver<Completion>,
    navigation: NavigationFlow,
    navigation_paths: Vec<PathBuf>,
    nav: Option<NavigationPaint>,
    selected: Option<PathBuf>,
    displayed: Option<PathBuf>,
    current_file: Option<PathBuf>,
    siblings: Vec<PathBuf>,
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
    thumb_requests: Vec<PathBuf>,
    thumb_visible: Vec<PathBuf>,
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
        let display_mode = loaded.settings.reading_mode;
        let language = resolve_language(loaded.settings.language);
        let prefetch = Prefetcher::new(loaded.settings.prefetch.clone());
        prefetch.set_purpose(DecodePurpose::Animated);
        let thumbnails = Thumbnails::new(prefetch.clone());
        let mut feedback = Feedback::default();
        if let Some(warning) = loaded.warning {
            feedback.replace(
                FeedbackKind::Error,
                ui_text::catalog(language).settings_warning(&warning),
            );
        }
        let (worker_tx, worker_rx) = mpsc::channel();
        let (save_tx, save_rx) = mpsc::channel();
        Self {
            initial,
            settings: loaded.settings,
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
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn open_settings(&mut self, ctx: &egui::Context) {
        if self.settings_draft.is_some() || self.close_state.requested {
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
            SETTING_PREVIEWS | SETTING_READING | SETTING_ARROW | SETTING_LANGUAGE => {
                self.step_setting(self.settings_control, true)
            }
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
        if self.settings_draft.is_some() || self.close_state.requested {
            return;
        }
        let mode = toggled_reading_mode(self.settings.reading_mode);
        if mode == self.settings.reading_mode {
            return;
        }
        let target = mode_target(&self.current_file, &self.selected, &self.displayed);
        if let Some(path) = &target
            && !self.siblings.is_empty()
            && selected_index(Some(path), &self.siblings).is_none()
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
        let was_visible = self.settings.thumbnails_visible;
        let language = resolve_language(draft.language);
        self.settings = draft;
        self.language = language;
        self.prefetch.configure(self.settings.prefetch.clone());
        if self.settings.thumbnails_visible && !was_visible {
            self.reveal_thumbnails();
            self.wake_thumbnails(ctx);
        }
        if mode_changed {
            let _ = self.transition_to_mode(self.settings.reading_mode, ctx);
        } else {
            self.sync_prefetch(ctx);
        }
        self.persist_settings(ctx, self.text().applying_saving);
    }

    fn toggle_previews(&mut self, ctx: &egui::Context) {
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
                    .set_visible(Vec::new(), self.thumb_generation);
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
                .set_visible(self.thumb_requests.clone(), self.thumb_generation);
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
            let source = self.thumbnails.get(&path);
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
        if self.close_state.requested || self.settings_draft.is_some() {
            return;
        }
        if self.navigation.is_idle() {
            self.navigation_paths = self.siblings.clone();
        }
        let Some(base) = selected_index(self.selected.as_deref(), &self.navigation_paths) else {
            return;
        };
        if self.navigation.enqueue(
            action,
            base,
            self.navigation_paths.len(),
            self.settings.reading_mode,
        ) {
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
            let path = navigation::pages(
                ticket.index,
                self.navigation_paths.len(),
                self.settings.reading_mode,
            )
            .first()
            .and_then(|index| self.navigation_paths.get(*index))
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
        self.prefetch.update_context_many(
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

    fn displayed_paths(&self) -> Vec<PathBuf> {
        self.displayed_media()
            .iter()
            .map(|media| media.path.clone())
            .collect()
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

    fn load(&mut self, ctx: &egui::Context, path: PathBuf, queued: bool) {
        if self.close_state.requested {
            return;
        }
        if !queued {
            self.cancel_navigation();
        }
        let path = match std::path::absolute(path) {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(self.text().path_failed_detail(&error.to_string()));
                ctx.request_repaint();
                return;
            }
        };
        let foreground = self.prefetch.foreground_requested(&path);
        self.prefetch.set_allowed(false);
        self.rotation.cancel();
        // Keep the old clocks paused, not dropped: a failed load must remain recoverable.
        self.pause_animation();
        self.spread_generation = self.spread_generation.wrapping_add(1);
        self.book_rotation_resume = false;
        self.animation_job = false;
        self.candidate = None;
        self.nav = None;
        self.selected = Some(path.clone());
        self.current_file = Some(path.clone());
        self.update_prefetch_context();
        self.loading_file = true;
        if !queued {
            self.error = None;
        }
        self.loader.request(Work::Load(
            path,
            self.prefetch.clone(),
            foreground,
            self.tile_cap(ctx),
            self.settings.reading_mode,
            self.language,
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
                        Ok(paths) => self.siblings = paths,
                        Err(error) => {
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
            let path = prepared.media.path.clone();
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
        self.navigation_painted(candidate.revision, ctx);
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
        if self.loading_file || !self.has_animation() {
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
        self.cancel_navigation();
        if let Some(path) = rfd::FileDialog::new()
            .set_title(self.text().open_picker_title)
            .pick_file()
        {
            self.load(ctx, path, false);
        }
        ctx.request_repaint();
    }

    fn edge(&mut self, last: bool, ctx: &egui::Context) {
        let path = if last {
            self.siblings.last()
        } else {
            self.siblings.first()
        };
        if let Some(path) = path.cloned() {
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

    fn perform(&mut self, action: Action, viewport: Rect, ctx: &egui::Context) {
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
                if !self.loading_file && self.media.is_some() {
                    self.rotation.request(matches!(action, Action::RotateRight));
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
        }
    }

    fn handle_keys(&mut self, viewport: Rect, ctx: &egui::Context) {
        if self.settings_draft.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let events = ctx.input(|input| input.events.clone());
        let mut acted = false;
        for event in events {
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

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        self.cancel_navigation();
        if self.close_state.request(self.save_revision) {
            self.close_pending_notice = false;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        } else {
            if self.settings_draft.is_some() {
                self.close_settings(ctx);
            }
            if !self.close_pending_notice {
                self.close_pending_notice = true;
                self.set_feedback(
                    FeedbackKind::Progress,
                    self.text().saving_before_close.to_owned(),
                    ctx,
                );
            }
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        }
    }

    fn request_close(&mut self, ctx: &egui::Context) {
        self.cancel_navigation();
        if self.close_state.request(self.save_revision) {
            self.close_pending_notice = false;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        } else {
            if self.settings_draft.is_some() {
                self.close_settings(ctx);
            }
            if !self.close_pending_notice {
                self.close_pending_notice = true;
                self.set_feedback(
                    FeedbackKind::Progress,
                    self.text().saving_before_close.to_owned(),
                    ctx,
                );
            }
        }
        ctx.request_repaint();
    }

    fn position(&self) -> Option<usize> {
        selected_index(self.selected.as_deref(), &self.siblings)
    }

    fn neighbor(&self, next: bool) -> Option<PathBuf> {
        let index = self.position()?;
        let index = if self.settings.reading_mode == ReadingMode::Single {
            adjacent_index(index, self.siblings.len(), next)?
        } else {
            navigation::destination(
                index,
                self.siblings.len(),
                self.settings.reading_mode,
                if next {
                    Navigation::Next
                } else {
                    Navigation::Previous
                },
            )?
        };
        self.siblings.get(index).cloned()
    }

    fn display_name(&self) -> String {
        let names: Vec<_> = self
            .displayed_paths()
            .iter()
            .filter_map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect();
        let text = self.text();
        if names.is_empty() {
            self.selected
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| text.image_viewer.into())
        } else {
            text.image_names(&names)
        }
    }

    fn position_label(&self) -> String {
        self.position()
            .map(|index| {
                let pages =
                    navigation::pages(index, self.siblings.len(), self.settings.reading_mode);
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
        let name = self
            .selected
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = self.text();
        if self.selected != self.displayed && !self.busy() {
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
        let is_error = notice.kind == FeedbackKind::Error;
        egui::Panel::top("notice-banner")
            .frame(
                EguiFrame::new()
                    .fill(if is_error {
                        palette.error_bg
                    } else {
                        palette.accent_bg
                    })
                    .inner_margin(Margin::symmetric(14, 7)),
            )
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new(notice.text).size(12.).color(if is_error {
                            palette.error
                        } else {
                            palette.accent
                        }))
                        .wrap(),
                    );
                    if is_error {
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
        if !self.settings.thumbnails_visible {
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
                    painter.text(
                        Pos2::new(cell.center().x, cell.bottom() - 10.),
                        Align2::CENTER_CENTER,
                        (index + 1).to_string(),
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
                if self.settings_draft.is_none() {
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
        let previous = !self.navigation.is_idle() || self.neighbor(false).is_some();
        let next = !self.navigation.is_idle() || self.neighbor(true).is_some();
        let playing = self.playing();
        let has_playback = self.has_animation();
        let finished = self.all_finished();
        let can_rotate = has_image && !self.loading_file;
        let stats = self.cache_stats;
        let queued = self.navigation.pending_len();
        let viewport = self.viewport;
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
            if response.dragged_by(PointerButton::Primary) && self.tiled.is_some() {
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

    fn draw_badge(&self, painter: &egui::Painter, viewport: Rect, palette: Palette) {
        if self.tiled.is_none() {
            return;
        }
        let rotating = self.rotation.requested != self.rotation.displayed;
        let text = self.text();
        let message = if self.loading_file {
            self.selected
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| text.opening_file(&name.to_string_lossy()))
                .unwrap_or_else(|| text.opening_image.to_owned())
        } else if rotating {
            text.rotating.to_owned()
        } else {
            return;
        };
        let galley = painter.layout_no_wrap(message, FontId::proportional(12.), palette.muted);
        let background = Rect::from_min_size(
            viewport.min + egui::vec2(12., 12.),
            galley.size() + egui::vec2(20., 12.),
        );
        painter.rect_filled(background, 6., palette.surface);
        painter.galley(background.min + egui::vec2(10., 6.), galley, palette.muted);
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
        self.expire_feedback(ctx);
        self.poll_saves(ctx);
        if !self.close_state.requested && self.settings_draft.is_some() {
            self.handle_settings_keys(ctx);
        }
        if let Some(path) = self.initial.take() {
            self.load(ctx, path, false);
        }
        if self.settings_draft.is_none() {
            let dropped = ctx.input(|input| {
                input
                    .raw
                    .dropped_files
                    .first()
                    .map(|dropped| dropped.path().to_path_buf())
            });
            if let Some(path) = dropped {
                self.load(ctx, path, false);
            }
        }
        self.poll_workers(ctx);
        self.advance_frames(ctx);
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
        self.footer(ui, palette);
        self.preview_strip(ui, palette);
        egui::CentralPanel::no_frame()
            .frame(EguiFrame::new().fill(palette.canvas))
            .show(ui, |ui| self.canvas(ui, palette));
        self.settings_modal(ui.ctx(), palette);
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
        let foreground = prefetch.foreground_requested(&path);
        Work::Load(
            path,
            prefetch.clone(),
            foreground,
            MAX_TILE,
            mode,
            Language::English,
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
        assert_eq!(key_action(Key::C, Modifiers::NONE), None);
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
        assert_eq!(selected, corrupt);
        assert_eq!(siblings.unwrap(), vec![corrupt, other]);
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
        assert!(matches!(work, Work::Load(path, _, _, _, _, _) if path == Path::new("third.png")));
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
        let mut displayed = paths[0].clone();
        let mut committed = Vec::new();
        let mut failed = Vec::new();
        for (index, path) in paths.iter().enumerate().skip(1) {
            let ticket = navigation.start_next(loader.revision + 1).unwrap();
            assert_eq!(ticket.index, index);
            let foreground = prefetch.foreground_requested(path);
            prefetch.set_allowed(false);
            prefetch.update_context(paths.clone(), Some(path.clone()), None);
            loader.request(Work::Load(
                path.clone(),
                prefetch.clone(),
                foreground,
                MAX_TILE,
                ReadingMode::Single,
                Language::English,
            ));
            let (revision, work, cancelled) = loader.take().unwrap();
            let result = prepare(work, &cancelled);
            assert!(loader.finish(revision));
            let (selected, siblings) = result.selection.unwrap();
            assert_eq!(&selected, path);
            assert!(!siblings.unwrap().contains(&paths[2]));
            assert!(navigation.start_next(revision + 1).is_none());
            match result.image {
                Ok(prepared) => {
                    assert_eq!(&prepared.media.path, path);
                    assert!(!prepared.tiled.tiles.is_empty());
                    assert!(!navigation.crossed_frame(ticket));
                    // Supply the two UI events explicitly; this test does not run a window.
                    assert_eq!(navigation.painted(revision), Some(ticket));
                    displayed = prepared.media.path.clone();
                    committed.push(index);
                    assert!(navigation.start_next(revision + 1).is_none());
                    assert!(navigation.crossed_frame(ticket));
                }
                Err(_) => {
                    assert_eq!(displayed, paths[0]);
                    failed.push(index);
                    assert!(navigation.failed(revision));
                }
            }
        }
        assert_eq!(failed, vec![1, 2]);
        assert_eq!(committed, vec![3, 4]);
        assert_eq!(displayed, paths[4]);
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
            comic_arrow_direction: settings::ComicArrowDirection::LeftNext,
            language: Language::Japanese,
        };
        restore_defaults(&mut draft);
        assert_eq!(draft.prefetch, PrefetchSettings::default());
        assert!(draft.thumbnails_visible);
        assert_eq!(draft.reading_mode, ReadingMode::Single);
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
    fn only_a_reading_mode_change_needs_a_reload() {
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
        let mut paths: Vec<PathBuf> = ["0.png", "1.png", "2.png"].map(PathBuf::from).into();
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
        let path = PathBuf::from("a.png");
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
            path: "fixture.png".into(),
            frames: vec![model::MediaFrame {
                pixels,
                delay: Duration::ZERO,
            }],
            loop_mode: model::LoopMode::Once,
        })
    }

    fn animated_media(
        frames: Vec<Arc<RgbaImage>>,
        delays: &[u64],
        loop_mode: model::LoopMode,
    ) -> Arc<model::LoadedMedia> {
        Arc::new(model::LoadedMedia {
            path: "animation.gif".into(),
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
            path: "clock.gif".into(),
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
            path: "animation.gif".into(),
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
        assert_eq!(path, selected);
        assert_eq!(siblings.unwrap(), vec![selected]);
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
            requested_work(paths[1].clone(), &prefetch, ReadingMode::Book),
            &AtomicBool::new(false),
        )
        .image
        .unwrap();
        assert_eq!(good.others.len(), 1);
        std::fs::write(&paths[2], b"broken partner").unwrap();
        let failed = prepare(
            requested_work(paths[1].clone(), &prefetch, ReadingMode::Book),
            &AtomicBool::new(false),
        );
        assert!(failed.image.is_err());
        assert_eq!(failed.selection.unwrap().0, paths[1]);
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
        let mut paths: Vec<PathBuf> = ["0.png", "1.png", "2.png"].map(PathBuf::from).into();
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

        let clicked = Some(PathBuf::from("2.png"));
        let anchor = Some(PathBuf::from("1.png"));
        assert_eq!(
            mode_target(&clicked, &anchor, &None),
            Some(PathBuf::from("2.png")),
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
}
