//! Preview-strip layout math.
//!
//! The strip UI arrives with Batch B; these helpers stay framework-free so the
//! visible window, request margin, and reveal scroll can be tested on their own.

use std::ops::Range;

pub const CELL_WIDTH: f32 = 112.;
pub const STRIP_HEIGHT: f32 = 108.;

pub fn visible_range(length: usize, offset: f32, width: f32) -> Range<usize> {
    if width <= 0. || length == 0 {
        return 0..0;
    }
    let start = (offset.max(0.) / CELL_WIDTH).floor() as usize;
    let end = ((offset.max(0.) + width) / CELL_WIDTH).ceil() as usize;
    start.min(length)..end.min(length)
}

pub fn request_range(visible: Range<usize>, length: usize) -> Range<usize> {
    if visible.is_empty() {
        return 0..0;
    }
    visible.start.saturating_sub(2)..visible.end.saturating_add(2).min(length)
}

/// Cell index under a local x offset, or `None` past either end.
pub fn cell_at(offset: f32, x: f32, length: usize) -> Option<usize> {
    if x < 0. {
        return None;
    }
    let index = ((offset.max(0.) + x) / CELL_WIDTH).floor() as usize;
    (index < length).then_some(index)
}

pub fn clamp_offset(offset: f32, length: usize, width: f32) -> f32 {
    offset
        .max(0.)
        .min((length as f32 * CELL_WIDTH - width).max(0.))
}

pub fn reveal_offset(first: usize, last: usize, length: usize, width: f32, current: f32) -> f32 {
    if length == 0 || width <= 0. {
        return 0.;
    }
    let left = first.min(length - 1) as f32 * CELL_WIDTH;
    let right = (last.min(length - 1) + 1) as f32 * CELL_WIDTH;
    let target = if left < current || right - left > width {
        left
    } else if right > current + width {
        right - width
    } else {
        current
    };
    clamp_offset(target, length, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_directory_only_requests_visible_cells_and_small_margin() {
        let visible = visible_range(100_000, 112_000., 560.);
        assert_eq!(visible, 1000..1005);
        assert_eq!(request_range(visible, 100_000), 998..1007);
        assert_eq!(visible_range(20, 0., 0.), 0..0);
        assert_eq!(request_range(0..0, 20), 0..0);
        assert_eq!(visible_range(2, 0., 560.), 0..2);
    }

    #[test]
    fn cell_hit_testing_matches_the_manual_scroll_offset() {
        assert_eq!(cell_at(0., 0., 10), Some(0));
        assert_eq!(cell_at(0., 111.9, 10), Some(0));
        assert_eq!(cell_at(0., 112., 10), Some(1));
        assert_eq!(cell_at(560., 10., 10), Some(5));
        assert_eq!(cell_at(0., -1., 10), None);
        assert_eq!(cell_at(1200., 10., 10), None);
        assert_eq!(cell_at(0., 10., 0), None);
    }

    #[test]
    fn reveal_handles_spreads_reshow_and_directory_end_without_overscroll() {
        assert_eq!(reveal_offset(10, 11, 100, 560., 0.), 784.);
        assert_eq!(reveal_offset(1, 2, 100, 560., 784.), 112.);
        assert_eq!(reveal_offset(99, 99, 100, 560., 0.), 10_640.);
        assert_eq!(reveal_offset(0, 1, 2, 80., 0.), 0.);
        assert_eq!(clamp_offset(1000., 2, 560.), 0.);
    }
}
