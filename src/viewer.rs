//! Tile layout, projection, and playback math shared by the egui UI.
//!
//! No windowing or GPU types live here: tiles carry RGBA bytes and the UI turns
//! them into textures. Core edges are snapped together and gutters supply
//! filter neighbors, so zooming never shows seams.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use image::RgbaImage;

use crate::model::LoopMode;

/// Widest tile texture we ever request, before clamping to the GPU limit.
pub const MAX_TILE: u32 = 4096;
/// One source pixel on each side that linear filtering may sample.
const GUTTER: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileGeometry {
    pub core: Rect,
    pub sample: Rect,
}

/// Texture side limit the renderer can actually allocate.
pub fn tile_cap(max_texture_side: u32) -> u32 {
    max_texture_side.clamp(8, MAX_TILE)
}

#[cfg(test)]
pub fn tile_layout(width: u32, height: u32) -> Vec<TileGeometry> {
    tile_layout_capped(width, height, MAX_TILE)
}

/// Cores never overlap and every sample fits inside `cap`.
pub fn tile_layout_capped(width: u32, height: u32, cap: u32) -> Vec<TileGeometry> {
    let core = cap.saturating_sub(2 * GUTTER).max(1);
    let mut tiles = Vec::new();
    for y in (0..height).step_by(core as usize) {
        for x in (0..width).step_by(core as usize) {
            let right = (x + core).min(width);
            let bottom = (y + core).min(height);
            let left = x.saturating_sub(GUTTER);
            let top = y.saturating_sub(GUTTER);
            tiles.push(TileGeometry {
                core: Rect {
                    x,
                    y,
                    width: right - x,
                    height: bottom - y,
                },
                sample: Rect {
                    x: left,
                    y: top,
                    width: (right + GUTTER).min(width) - left,
                    height: (bottom + GUTTER).min(height) - top,
                },
            });
        }
    }
    tiles
}

/// One prepared tile: its source geometry and the RGBA sample crop.
pub struct Tile {
    pub geometry: TileGeometry,
    pub pixels: Vec<u8>,
}

pub struct TiledFrame {
    pub dimensions: (u32, u32),
    pub tiles: Vec<Tile>,
}

/// Cuts `pixels` into RGBA tiles no wider than `cap`.
pub fn prepare_tiles(
    pixels: &RgbaImage,
    cap: u32,
    cancelled: &AtomicBool,
) -> Result<TiledFrame, String> {
    let dimensions = pixels.dimensions();
    let mut tiles = Vec::new();
    for geometry in tile_layout_capped(pixels.width(), pixels.height(), cap) {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Image preparation cancelled.".into());
        }
        let sample = geometry.sample;
        let pixels =
            image::imageops::crop_imm(pixels, sample.x, sample.y, sample.width, sample.height)
                .to_image()
                .into_raw();
        tiles.push(Tile { geometry, pixels });
    }
    if cancelled.load(Ordering::Relaxed) {
        return Err("Image preparation cancelled.".into());
    }
    Ok(TiledFrame { dimensions, tiles })
}

pub const BOOK_GAP: u32 = 24;
pub const COPY_SPREAD_LIMIT: usize = 256 * 1024 * 1024;

/// Reject oversized or overflowing composites before allocating any pixels.
pub fn copy_spread_dimensions(dimensions: &[(u32, u32)], limit: usize) -> Option<(u32, u32)> {
    let mut pages = dimensions.iter();
    let &(mut width, mut height) = pages.next()?;
    for &(w, h) in pages {
        width = width.checked_add(BOOK_GAP)?.checked_add(w)?;
        height = height.max(h);
    }
    let bytes = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    (bytes <= limit).then_some((width, height))
}

fn copy_rotated(source: &RgbaImage, turns: u8) -> RgbaImage {
    match turns % 4 {
        0 => source.clone(),
        1 => crate::model::rotate_image(source, true),
        2 => image::imageops::rotate180(source),
        _ => crate::model::rotate_image(source, false),
    }
}

/// Source pages are in reading order (right first); output is visual left-to-right.
/// The flag reports a primary-page fallback, not a clipboard backend result.
pub fn copy_pixels(pages: &[&RgbaImage], turns: u8, limit: usize) -> Option<(RgbaImage, bool)> {
    let primary = *pages.first()?;
    if pages.len() == 1 {
        return Some((copy_rotated(primary, turns), false));
    }
    let dimensions: Vec<_> = pages
        .iter()
        .map(|page| {
            let (w, h) = page.dimensions();
            if turns.is_multiple_of(2) {
                (w, h)
            } else {
                (h, w)
            }
        })
        .collect();
    let Some((width, height)) = copy_spread_dimensions(&dimensions, limit) else {
        return Some((copy_rotated(primary, turns), true));
    };
    let mut combined = RgbaImage::new(width, height);
    let mut x = 0;
    for (index, page) in pages.iter().rev().enumerate() {
        let rotated = copy_rotated(page, turns);
        // Replace rather than blend to preserve unmultiplied RGB and alpha.
        image::imageops::replace(&mut combined, &rotated, i64::from(x), 0);
        x += rotated.width();
        if index + 1 < pages.len() {
            x += BOOK_GAP;
        }
    }
    Some((combined, false))
}

/// Joint spread bounds and page centers. The right page is first in reading
/// order; shorter pages share the top edge.
pub fn spread_layout(dimensions: &[(u32, u32)]) -> ((u32, u32), Vec<[f32; 2]>) {
    let height = dimensions.iter().map(|page| page.1).max().unwrap_or(0);
    let width = dimensions.iter().map(|page| page.0).sum::<u32>()
        + BOOK_GAP * dimensions.len().saturating_sub(1) as u32;
    let mut right = width as f32 / 2.;
    let centers = dimensions
        .iter()
        .map(|&(w, h)| {
            let center = [right - w as f32 / 2., (h as f32 - height as f32) / 2.];
            right -= w as f32 + BOOK_GAP as f32;
            center
        })
        .collect();
    ((width, height), centers)
}

/// Projected tile edges in UI points, snapped to physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct Projection {
    pub core: [f64; 4],
    pub sample: [f64; 4],
}

impl TileGeometry {
    pub fn project(self, origin: [f64; 2], scale: f64, dpi: f64) -> Projection {
        let origin = origin.map(|value| (value * dpi).round());
        let physical_scale = scale * dpi;
        let physical_scale = if (physical_scale - 1.).abs() < 0.000001 {
            1.
        } else {
            physical_scale
        };
        let project = |rect: Rect, core: bool| {
            let edge = |coordinate: u32, axis: usize, end: bool| {
                let physical = origin[axis] + coordinate as f64 * physical_scale;
                let outer_end = if axis == 0 {
                    self.sample.x + self.sample.width
                } else {
                    self.sample.y + self.sample.height
                };
                (if core && coordinate == 0 {
                    physical.floor()
                } else if core && end && coordinate == outer_end {
                    physical.ceil()
                } else if core {
                    physical.round()
                } else if end {
                    physical.ceil()
                } else {
                    physical.floor()
                }) / dpi
            };
            [
                edge(rect.x, 0, false),
                edge(rect.y, 1, false),
                edge(rect.x + rect.width, 0, true),
                edge(rect.y + rect.height, 1, true),
            ]
        };
        Projection {
            core: project(self.core, true),
            sample: project(self.sample, false),
        }
    }
}

pub fn intersects(rect: [f64; 4], viewport: [f64; 4]) -> bool {
    rect[0] < rect[2]
        && rect[1] < rect[3]
        && rect[0] < viewport[2]
        && rect[2] > viewport[0]
        && rect[1] < viewport[3]
        && rect[3] > viewport[1]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub frame: usize,
    pub next: Option<Duration>,
    pub finished: bool,
}

pub struct Playback {
    ends: Vec<Duration>,
    cycle: Duration,
    cycles: Option<u32>,
    elapsed: Duration,
    anchor: Option<Duration>,
}

impl Playback {
    pub fn new(
        delays: impl IntoIterator<Item = Duration>,
        loop_mode: LoopMode,
        now: Duration,
    ) -> Self {
        let mut cycle = Duration::ZERO;
        let ends = delays
            .into_iter()
            .map(|delay| {
                cycle += if delay.is_zero() {
                    Duration::from_millis(10)
                } else {
                    delay
                };
                cycle
            })
            .collect();
        Self {
            ends,
            cycle,
            cycles: match loop_mode {
                LoopMode::Once => Some(1),
                LoopMode::Finite(repeats) => Some(u32::from(repeats) + 1),
                LoopMode::Infinite => None,
            },
            elapsed: Duration::ZERO,
            anchor: Some(now),
        }
    }

    fn elapsed(&self, now: Duration) -> Duration {
        self.elapsed
            + self
                .anchor
                .map(|anchor| now.saturating_sub(anchor))
                .unwrap_or_default()
    }

    pub fn position(&self, now: Duration) -> Position {
        let elapsed = self.elapsed(now);
        if self.ends.is_empty() || self.cycle.is_zero() {
            return Position {
                frame: 0,
                next: None,
                finished: true,
            };
        }
        if self
            .cycles
            .is_some_and(|cycles| elapsed >= self.cycle * cycles)
        {
            return Position {
                frame: self.ends.len() - 1,
                next: None,
                finished: true,
            };
        }
        let remainder = Duration::from_nanos((elapsed.as_nanos() % self.cycle.as_nanos()) as u64);
        let frame = self.ends.partition_point(|end| *end <= remainder);
        Position {
            frame,
            next: self.anchor.map(|_| now + self.ends[frame] - remainder),
            finished: false,
        }
    }

    pub fn playing(&self, now: Duration) -> bool {
        self.anchor.is_some() && !self.position(now).finished
    }

    pub fn pause(&mut self, now: Duration) {
        self.elapsed = self.elapsed(now);
        self.anchor = None;
    }

    pub fn play(&mut self, now: Duration) {
        if self.position(now).finished {
            self.elapsed = Duration::ZERO;
            self.anchor = Some(now);
        }
        if self.anchor.is_none() {
            self.anchor = Some(now);
        }
    }
}

/// Image placement in UI points: the painted origin/scale, rounded to physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasTransform {
    pub origin: [f32; 2],
    pub scale: f32,
}

impl CanvasTransform {
    /// Maps image space to UI points for the crop overlay.
    ///
    /// `origin` uses the same f64 formula as `paint_tiles` (`main.rs:5226-5227`)
    /// and the same physical-pixel snapping as `TileGeometry::project`, so the
    /// image's top-left corner matches the painted tile (the test asserts
    /// equality there). Non-zero coordinates can still differ from painted
    /// edges by ≤1 physical pixel because of f32 evaluation order and per-tile
    /// sample rounding. That continuous mapping is fine for crop lines; do not
    /// require each crop edge to sit exactly on every tile's snapped edge.
    /// `dpi` must be positive.
    pub fn new(
        viewport_center: [f32; 2],
        dimensions: (u32, u32),
        scale: f32,
        pan: [f32; 2],
        dpi: f32,
    ) -> Self {
        let dpi = f64::from(dpi);
        let origin = [
            f64::from(viewport_center[0]) - f64::from(dimensions.0) * f64::from(scale) / 2.
                + f64::from(pan[0]),
            f64::from(viewport_center[1]) - f64::from(dimensions.1) * f64::from(scale) / 2.
                + f64::from(pan[1]),
        ];
        // `project` rounds the origin to physical pixels, then floors the
        // sample start; floor of an integer is that integer.
        let snap = |value: f64| ((value * dpi).round() / dpi) as f32;
        Self {
            origin: [snap(origin[0]), snap(origin[1])],
            scale,
        }
    }
}

/// UI point to image pixel; `scale` is the same fit/zoom factor used for drawing.
pub fn screen_to_image(transform: CanvasTransform, pos: [f32; 2]) -> [f32; 2] {
    [
        (pos[0] - transform.origin[0]) / transform.scale,
        (pos[1] - transform.origin[1]) / transform.scale,
    ]
}

/// Image rect `[x, y, w, h]` to a UI rect with the same layout.
pub fn image_rect_to_screen(transform: CanvasTransform, rect: [f32; 4]) -> [f32; 4] {
    [
        transform.origin[0] + rect[0] * transform.scale,
        transform.origin[1] + rect[1] * transform.scale,
        rect[2] * transform.scale,
        rect[3] * transform.scale,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_transform_round_trips_points_with_pan_and_zoom() {
        let transform = CanvasTransform::new([400., 300.], (800, 600), 0.5, [10., -20.], 1.);
        assert_eq!(transform.origin, [210., 130.]);
        assert_eq!(
            image_rect_to_screen(transform, [0., 0., 1., 1.]),
            [210., 130., 0.5, 0.5]
        );
        let screen = image_rect_to_screen(transform, [40., 20., 8., 6.]);
        assert_eq!(screen, [230., 140., 4., 3.]);
        assert_eq!(
            screen_to_image(transform, [screen[0], screen[1]]),
            [40., 20.]
        );
        assert_eq!(
            screen_to_image(transform, [screen[0] + screen[2], screen[1] + screen[3]]),
            [48., 26.]
        );
        // A centered, unscaled image maps screen center to image center.
        let centered = CanvasTransform::new([400., 300.], (800, 600), 1., [0., 0.], 1.);
        assert_eq!(screen_to_image(centered, [400., 300.]), [400., 300.]);
        assert_eq!(screen_to_image(centered, [0., 0.]), [0., 0.]);
    }

    #[test]
    fn canvas_transform_origin_matches_tile_projection_at_each_dpi() {
        // (center, dimensions, scale, pan, dpi)
        let cases = [
            ([400., 300.], (3, 2), 1., [0.25, 0.25], 1.),
            ([400., 300.], (800, 600), 0.25, [0.25, -7.], 1.),
            ([400., 300.], (800, 600), 0.5, [10., -20.], 1.25),
            ([640., 480.], (900, 700), 1.731, [-3., 7.], 1.5),
            ([400., 300.], (800, 600), 1., [0., 0.], 2.),
        ];
        for (center, dimensions, scale, pan, dpi) in cases {
            let transform = CanvasTransform::new(center, dimensions, scale, pan, dpi);
            let origin = [
                f64::from(center[0]) - f64::from(dimensions.0) * f64::from(scale) / 2.
                    + f64::from(pan[0]),
                f64::from(center[1]) - f64::from(dimensions.1) * f64::from(scale) / 2.
                    + f64::from(pan[1]),
            ];
            let geometry = TileGeometry {
                core: Rect {
                    x: 0,
                    y: 0,
                    width: dimensions.0,
                    height: dimensions.1,
                },
                sample: Rect {
                    x: 0,
                    y: 0,
                    width: dimensions.0,
                    height: dimensions.1,
                },
            };
            let projected = geometry.project(origin, f64::from(scale), f64::from(dpi));
            // Tolerance 0 covers only the origin: both sides compute the same
            // f64 value and snap it to a physical pixel. Non-zero coordinates
            // and per-tile sample edges may still differ by ≤1 physical pixel.
            assert_eq!(transform.origin[0], projected.sample[0] as f32);
            assert_eq!(transform.origin[1], projected.sample[1] as f32);
            assert_eq!(transform.origin[0], projected.core[0] as f32);
            assert_eq!(transform.origin[1], projected.core[1] as f32);
        }
        // Reproducer: a half-point offset snaps to 399, not 398.75.
        let transform = CanvasTransform::new([400., 300.], (3, 2), 1., [0.25, 0.25], 1.);
        assert_eq!(transform.origin, [399., 299.]);
    }

    #[test]
    fn copy_single_keeps_full_resolution_and_rotated_rgba_pixels() {
        let source = RgbaImage::from_fn(2, 3, |x, y| {
            let id = (y * 2 + x + 1) as u8;
            image::Rgba([id, 9, 21, id * 30])
        });
        for (turns, size, ids) in [
            (0, (2, 3), [1, 2, 3, 4, 5, 6]),
            (1, (3, 2), [5, 3, 1, 6, 4, 2]),
            (2, (2, 3), [6, 5, 4, 3, 2, 1]),
            (3, (3, 2), [2, 4, 6, 1, 3, 5]),
        ] {
            let (copied, fallback) = copy_pixels(&[&source], turns, 0).unwrap();
            assert!(!fallback, "single pages are not subject to the spread cap");
            assert_eq!(copied.dimensions(), size);
            for (pixel, id) in copied.pixels().zip(ids) {
                assert_eq!(*pixel, image::Rgba([id, 9, 21, id * 30]));
            }
        }
    }

    #[test]
    fn copy_book_uses_visual_order_transparent_gap_and_top_alignment() {
        let right = RgbaImage::from_pixel(2, 2, image::Rgba([200, 10, 20, 127]));
        let left = RgbaImage::from_pixel(1, 1, image::Rgba([10, 200, 20, 0]));
        let (copied, fallback) = copy_pixels(&[&right, &left], 0, COPY_SPREAD_LIMIT).unwrap();
        assert!(!fallback);
        assert_eq!(copied.dimensions(), (3 + BOOK_GAP, 2));
        for (x, y, pixel) in copied.enumerate_pixels() {
            let expected = if x == 0 && y == 0 {
                *left.get_pixel(0, 0)
            } else if x > BOOK_GAP {
                *right.get_pixel(0, 0)
            } else {
                image::Rgba([0, 0, 0, 0])
            };
            assert_eq!(*pixel, expected, "at {x},{y}");
        }
    }

    #[test]
    fn copy_book_rotates_each_page_before_composing() {
        let right = RgbaImage::from_fn(2, 1, |x, _| image::Rgba([x as u8 + 1, 0, 0, 255]));
        let left = RgbaImage::from_fn(1, 2, |_, y| image::Rgba([y as u8 + 3, 0, 0, 128]));
        let (copied, fallback) = copy_pixels(&[&right, &left], 1, COPY_SPREAD_LIMIT).unwrap();
        assert!(!fallback);
        assert_eq!(copied.dimensions(), (3 + BOOK_GAP, 2));
        assert_eq!(*copied.get_pixel(0, 0), image::Rgba([4, 0, 0, 128]));
        assert_eq!(*copied.get_pixel(1, 0), image::Rgba([3, 0, 0, 128]));
        assert_eq!(*copied.get_pixel(0, 1), image::Rgba([0, 0, 0, 0]));
        assert_eq!(*copied.get_pixel(2 + BOOK_GAP, 0), *right.get_pixel(0, 0));
        assert_eq!(*copied.get_pixel(2 + BOOK_GAP, 1), *right.get_pixel(1, 0));
    }

    #[test]
    fn copy_spread_cap_is_checked_without_allocating_large_images() {
        assert_eq!(
            copy_spread_dimensions(&[(4096, 8192), (4072, 8192)], COPY_SPREAD_LIMIT),
            Some((8192, 8192))
        );
        assert_eq!(
            copy_spread_dimensions(&[(4096, 8192), (4073, 8192)], COPY_SPREAD_LIMIT),
            None
        );
        assert_eq!(
            copy_spread_dimensions(&[(u32::MAX, 1), (1, 1)], usize::MAX),
            None
        );
        assert_eq!(
            copy_spread_dimensions(&[(u32::MAX, u32::MAX)], usize::MAX),
            None
        );
        let right = RgbaImage::from_pixel(2, 1, image::Rgba([1, 2, 3, 127]));
        let left = RgbaImage::new(1, 2);
        let bytes = (3 + BOOK_GAP) as usize * 2 * 4;
        assert!(!copy_pixels(&[&right, &left], 1, bytes).unwrap().1);
        let (copied, fallback) = copy_pixels(&[&right, &left], 1, bytes - 1).unwrap();
        assert!(fallback);
        assert_eq!(copied, crate::model::rotate_image(&right, true));
    }

    #[test]
    fn copy_without_an_image_is_a_noop() {
        assert_eq!(copy_pixels(&[], 0, COPY_SPREAD_LIMIT), None);
        assert_eq!(copy_spread_dimensions(&[], COPY_SPREAD_LIMIT), None);
    }

    #[test]
    fn rtl_spread_keeps_right_first_and_aligns_unequal_pages_at_top() {
        let (dimensions, centers) = spread_layout(&[(200, 400), (300, 250)]);
        assert_eq!(dimensions, (524, 400));
        assert_eq!(centers, vec![[162., 0.], [-112., -75.]]);
        assert_eq!(spread_layout(&[(200, 400)]), ((200, 400), vec![[0., 0.]]));
        assert_eq!(spread_layout(&[]), ((0, 0), Vec::new()));
        let scale = 0.5;
        let right_left = centers[0][0] * scale - 200. * scale / 2.;
        let left_right = centers[1][0] * scale + 300. * scale / 2.;
        assert_eq!(right_left - left_right, BOOK_GAP as f32 * scale);
        assert_eq!(centers[0][1] - 400. / 2., centers[1][1] - 250. / 2.);
    }

    #[test]
    fn cores_cover_source_once_and_gutters_stay_inside_the_cap() {
        let mut cases = Vec::new();
        for cap in [8, 64] {
            for dimensions in [(1, 1), (200, 50), (1, 257), (257, 1), (64, 64)] {
                cases.push((cap, dimensions));
            }
        }
        for dimensions in [
            (1, 1),
            (4096, 1),
            (1, 8193),
            (6720, 4480),
            (4480, 6720),
            (65535, 2),
        ] {
            cases.push((MAX_TILE, dimensions));
        }
        for (cap, dimensions) in cases {
            let tiles = tile_layout_capped(dimensions.0, dimensions.1, cap);
            let area: u64 = tiles
                .iter()
                .map(|tile| u64::from(tile.core.width) * u64::from(tile.core.height))
                .sum();
            assert_eq!(area, u64::from(dimensions.0) * u64::from(dimensions.1));
            for (index, tile) in tiles.iter().enumerate() {
                assert!(tile.sample.width <= cap && tile.sample.height <= cap);
                assert!(tile.sample.x + tile.sample.width <= dimensions.0);
                assert!(tile.sample.y + tile.sample.height <= dimensions.1);
                for other in &tiles[index + 1..] {
                    let rect = |r: Rect| {
                        [
                            r.x as f64,
                            r.y as f64,
                            (r.x + r.width) as f64,
                            (r.y + r.height) as f64,
                        ]
                    };
                    assert!(!intersects(rect(tile.core), rect(other.core)));
                }
            }
        }
        assert_eq!(tile_layout(4100, 3), tile_layout_capped(4100, 3, MAX_TILE));
        assert!(tile_layout(4100, 3).len() > 1);
    }

    #[test]
    fn tile_core_reconstruction_keeps_source_pixels_and_alpha() {
        let source = RgbaImage::from_fn(4100, 3, |x, y| {
            image::Rgba([(x % 251) as u8, y as u8, 19, 127])
        });
        let frame = prepare_tiles(&source, MAX_TILE, &AtomicBool::new(false)).unwrap();
        let mut reconstructed = RgbaImage::new(4100, 3);
        for tile in &frame.tiles {
            let geometry = tile.geometry;
            for y in geometry.core.y..geometry.core.y + geometry.core.height {
                for x in geometry.core.x..geometry.core.x + geometry.core.width {
                    let offset = (((y - geometry.sample.y) * geometry.sample.width + x
                        - geometry.sample.x)
                        * 4) as usize;
                    reconstructed.put_pixel(
                        x,
                        y,
                        image::Rgba([
                            tile.pixels[offset],
                            tile.pixels[offset + 1],
                            tile.pixels[offset + 2],
                            tile.pixels[offset + 3],
                        ]),
                    );
                }
            }
        }
        assert_eq!(source, reconstructed);
    }

    #[test]
    fn fractional_zoom_and_dpi_use_identical_neighbor_edges_and_cull() {
        let tiles = tile_layout(9000, 5000);
        for dpi in [1., 1.25, 1.5, 2.] {
            for scale in [0.071, 0.333, 1., 1.731] {
                let a = tiles[0].project([-111.37, 24.2], scale, dpi);
                let b = tiles[1].project([-111.37, 24.2], scale, dpi);
                assert_eq!(a.core[2], b.core[0]);
                assert_eq!(a.core[1], b.core[1]);
                assert!(a.sample[2] >= a.core[2]);
                assert!(b.sample[0] <= b.core[0]);
            }
        }
        assert!(!intersects([100., 0., 200., 100.], [0., 0., 100., 100.]));
        assert!(intersects([99., 0., 200., 100.], [0., 0., 100., 100.]));
        let visible = |viewport| {
            tiles
                .iter()
                .enumerate()
                .filter_map(|(index, tile)| {
                    intersects(tile.project([0., 0.], 1., 1.25).core, viewport).then_some(index)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(visible([100., 100., 200., 200.]), vec![0]);
        assert_eq!(visible([4200., 100., 4400., 200.]), vec![1]);
        assert!(visible([10000., 10000., 11000., 11000.]).is_empty());
    }

    #[test]
    fn physical_actual_size_snaps_sample_origins_and_keeps_cores_contiguous() {
        let tiles = tile_layout(9001, 5001);
        for dpi in [1.0_f32, 1.25, 1.5, 1.75, 2., 2.25, 2.75] {
            let scale = 1. / dpi;
            let origin = [
                (600. - 9001. / 2.) / dpi as f64,
                (400. - 5001. / 2.) / dpi as f64,
            ];
            let projected: Vec<_> = tiles
                .iter()
                .map(|tile| tile.project(origin, scale as f64, dpi as f64))
                .collect();
            for (tile, projected) in tiles.iter().zip(&projected) {
                assert_eq!(
                    (projected.sample[0] * dpi as f64).floor() as i64,
                    (origin[0] * dpi as f64).round() as i64 + i64::from(tile.sample.x)
                );
                assert_eq!(
                    (projected.sample[1] * dpi as f64).floor() as i64,
                    (origin[1] * dpi as f64).round() as i64 + i64::from(tile.sample.y)
                );
                assert!(projected.sample[0] <= projected.core[0]);
                assert!(projected.sample[1] <= projected.core[1]);
            }
            assert_eq!(projected[0].core[2], projected[1].core[0]);
            assert_eq!(projected[0].core[3], projected[3].core[1]);
        }
    }

    #[test]
    fn tile_preparation_honors_cancellation_and_cap() {
        assert!(prepare_tiles(&RgbaImage::new(10, 10), 64, &AtomicBool::new(true)).is_err());
        let frame = prepare_tiles(&RgbaImage::new(300, 10), 64, &AtomicBool::new(false)).unwrap();
        assert!(frame.tiles.iter().all(|tile| tile.pixels.len()
            == (tile.geometry.sample.width * tile.geometry.sample.height * 4) as usize));
        assert!(
            frame
                .tiles
                .iter()
                .all(|tile| tile.geometry.sample.width <= 64)
        );
    }

    #[test]
    fn subpixel_thin_sources_keep_a_nonempty_outer_mask() {
        let tile = tile_layout(1, 10000)[0];
        let projected = tile.project([10.49, -10.], 0.001, 1.25);
        assert!(projected.core[2] > projected.core[0]);
        assert!(projected.core[3] > projected.core[1]);
    }

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }
    fn playback(mode: LoopMode) -> Playback {
        Playback::new([ms(20), ms(30), ms(50)], mode, ms(0))
    }

    #[test]
    fn deadlines_skip_late_frames_without_drift() {
        let clock = playback(LoopMode::Infinite);
        assert_eq!(
            clock.position(ms(0)),
            Position {
                frame: 0,
                next: Some(ms(20)),
                finished: false
            }
        );
        assert_eq!(
            clock.position(ms(47)),
            Position {
                frame: 1,
                next: Some(ms(50)),
                finished: false
            }
        );
        assert_eq!(
            clock.position(ms(1073)),
            Position {
                frame: 2,
                next: Some(ms(1100)),
                finished: false
            }
        );
    }

    #[test]
    fn finite_means_additional_repetitions_and_once_stops_on_last_frame() {
        let clock = playback(LoopMode::Finite(2));
        assert!(!clock.position(ms(299)).finished);
        assert_eq!(
            clock.position(ms(300)),
            Position {
                frame: 2,
                next: None,
                finished: true
            }
        );
        assert!(playback(LoopMode::Once).position(ms(100)).finished);
        assert!(!playback(LoopMode::Infinite).position(ms(100000)).finished);
    }

    #[test]
    fn pause_resume_preserves_progress_and_finished_play_restarts() {
        let mut clock = playback(LoopMode::Once);
        clock.pause(ms(35));
        assert_eq!(clock.position(ms(1000)).frame, 1);
        assert_eq!(clock.position(ms(1000)).next, None);
        clock.play(ms(1000));
        assert_eq!(clock.position(ms(1000)).next, Some(ms(1015)));
        assert!(clock.position(ms(1065)).finished);
        clock.pause(ms(1065));
        clock.play(ms(2000));
        assert_eq!(clock.position(ms(2000)).frame, 0);
        assert!(clock.position(ms(2100)).finished);
        clock.play(ms(3000));
        assert_eq!(clock.position(ms(3000)).frame, 0);
        assert_eq!(clock.position(ms(3000)).next, Some(ms(3020)));
    }

    #[test]
    fn zero_delay_cannot_create_a_busy_loop() {
        let clock = Playback::new([Duration::ZERO, ms(10)], LoopMode::Infinite, ms(0));
        assert_eq!(clock.position(ms(0)).next, Some(ms(10)));
        assert_eq!(clock.position(ms(10)).frame, 1);
    }
}
