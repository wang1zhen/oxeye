//! Non-destructive crop/rotate/flip session, free of UI types.
//!
//! The session holds `base` (the frame with the viewer's display rotation
//! baked in) plus an op stack; `preview` is the materialized result, so undo
//! replays from `base` instead of keeping snapshots.
//!
//! Memory budget, with N = one full RGBA buffer (tile textures, the viewer's
//! original frame, and other callers' images excluded): `apply` holds `base`,
//! the session's old preview and the new output, i.e. at most 3N. `undo` holds
//! `base`, one replay input and one replay output, also 3N when nobody keeps
//! the old preview; if an external holder (the UI) still keeps a distinct old
//! preview, that adds one buffer and undo can reach 4N. These are session
//! buffers, not a process RSS cap: encoder buffers, textures and other images
//! add on top.

use std::sync::Arc;

use image::RgbaImage;

use crate::model::{MAX_DECODED_BYTES, MAX_SOURCE_SIDE, rotate_image};

/// Pixel-space crop rectangle; coordinates are in the edited image, not the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditOp {
    Crop(PixelRect),
    RotateCw,
    RotateCcw,
    FlipHorizontal,
    FlipVertical,
}

/// Undo replays from `base`; past this, storing snapshots would be required.
pub const MAX_EDIT_OPS: usize = 16;

/// Why a session or a single op was rejected, before any pixels are allocated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditError {
    EmptyImage,
    ZeroArea,
    Oversize { width: u32, height: u32 },
    TooManyOps,
}

/// Original baked frame, the op stack, and the materialized preview.
///
/// Internally one op peaks at about 3N (N = one full RGBA buffer): `base` +
/// old `preview` + new output. `undo` is also 3N (base + replay input/output),
/// or 4N when an external holder keeps a distinct old preview. Not a process
/// RSS cap: textures, the viewer's frame and encoders add on top.
pub struct EditSession {
    base: Arc<RgbaImage>,
    ops: Vec<EditOp>,
    preview: Arc<RgbaImage>,
}

impl EditSession {
    /// `displayed_rotation` is in quarter turns (the viewer's rotation count);
    /// baking it here keeps all later crop math in displayed orientation.
    pub fn start(frame: Arc<RgbaImage>, displayed_rotation: u8) -> Result<Self, EditError> {
        Self::start_with_limits(
            frame,
            displayed_rotation,
            MAX_SOURCE_SIDE,
            MAX_DECODED_BYTES,
        )
    }

    fn start_with_limits(
        frame: Arc<RgbaImage>,
        displayed_rotation: u8,
        max_side: u32,
        max_bytes: u64,
    ) -> Result<Self, EditError> {
        let (width, height) = frame.dimensions();
        if width == 0 || height == 0 {
            return Err(EditError::EmptyImage);
        }
        let oversize = EditError::Oversize { width, height };
        if width > max_side || height > max_side {
            return Err(oversize);
        }
        match pixel_bytes(width, height) {
            Some(bytes) if bytes <= max_bytes => {}
            _ => return Err(oversize),
        }
        let turns = displayed_rotation % 4;
        let base = match turns {
            0 => frame,
            1 => Arc::new(rotate_image(&frame, true)),
            2 => Arc::new(image::imageops::rotate180(&*frame)),
            _ => Arc::new(rotate_image(&frame, false)),
        };
        Ok(Self {
            preview: Arc::clone(&base),
            base,
            ops: Vec::new(),
        })
    }

    /// Materializes the op onto `preview`; failed ops leave the session untouched.
    pub fn apply(&mut self, op: EditOp) -> Result<(), EditError> {
        if self.ops.len() >= MAX_EDIT_OPS {
            return Err(EditError::TooManyOps);
        }
        let next = apply_op(&self.preview, op)?;
        self.ops.push(op);
        self.preview = Arc::new(next);
        Ok(())
    }

    /// Pops the last op and replays the rest from `base`; false on an empty stack.
    pub fn undo(&mut self) -> bool {
        if self.ops.pop().is_none() {
            return false;
        }
        // Release the old preview before replay allocates its intermediate.
        self.preview = Arc::clone(&self.base);
        self.preview = replay(&self.base, &self.ops);
        true
    }

    /// Pixels after every recorded op, ready to be wrapped as a display frame.
    pub fn preview(&self) -> &Arc<RgbaImage> {
        &self.preview
    }

    /// Whether anything since `start` is unsaved.
    pub fn dirty(&self) -> bool {
        !self.ops.is_empty()
    }
}

/// Applies one op to fresh pixels; crop rectangles are clamped to the image first.
pub fn apply_op(img: &RgbaImage, op: EditOp) -> Result<RgbaImage, EditError> {
    Ok(match op {
        EditOp::Crop(rect) => {
            let Some(rect) = clamp_crop(rect, img.width(), img.height()) else {
                return Err(EditError::ZeroArea);
            };
            image::imageops::crop_imm(img, rect.x, rect.y, rect.w, rect.h).to_image()
        }
        EditOp::RotateCw => rotate_image(img, true),
        EditOp::RotateCcw => rotate_image(img, false),
        EditOp::FlipHorizontal => image::imageops::flip_horizontal(img),
        EditOp::FlipVertical => image::imageops::flip_vertical(img),
    })
}

/// Clamps a selection to the image, returning `None` when it has no visible pixels.
pub fn clamp_crop(rect: PixelRect, w: u32, h: u32) -> Option<PixelRect> {
    if rect.w == 0 || rect.h == 0 {
        return None;
    }
    let x = rect.x.min(w);
    let y = rect.y.min(h);
    // u64 widening: `x + w` overflows u32 for selections dragged past the edge.
    let right = (u64::from(rect.x) + u64::from(rect.w)).min(u64::from(w)) as u32;
    let bottom = (u64::from(rect.y) + u64::from(rect.h)).min(u64::from(h)) as u32;
    (right > x && bottom > y).then_some(PixelRect {
        x,
        y,
        w: right - x,
        h: bottom - y,
    })
}

/// Reshapes `rect` to `aspect` around its center without leaving `bounds`.
pub fn aspect_fit(rect: PixelRect, aspect: Option<(u32, u32)>, bounds: (u32, u32)) -> PixelRect {
    let (bw, bh) = bounds;
    if bw == 0 || bh == 0 {
        return PixelRect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
    }
    let Some((aw, ah)) = aspect.filter(|(aw, ah)| *aw > 0 && *ah > 0) else {
        // A zero-area rect has nothing to preserve; fall back to one visible pixel.
        return clamp_crop(rect, bw, bh).unwrap_or(PixelRect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        });
    };
    let ratio = aw as f64 / ah as f64;
    let box_w = f64::from(rect.w.max(1));
    let box_h = f64::from(rect.h.max(1));
    let (mut fit_w, mut fit_h) = if ratio > box_w / box_h {
        (box_w, box_w / ratio)
    } else {
        (box_h * ratio, box_h)
    };
    if fit_w > f64::from(bw) {
        fit_w = f64::from(bw);
        fit_h = fit_w / ratio;
    }
    if fit_h > f64::from(bh) {
        fit_h = f64::from(bh);
        fit_w = fit_h * ratio;
    }
    let w = fit_w.round().clamp(1., f64::from(bw)) as u32;
    let h = fit_h.round().clamp(1., f64::from(bh)) as u32;
    let cx = f64::from(rect.x) + f64::from(rect.w) / 2.;
    let cy = f64::from(rect.y) + f64::from(rect.h) / 2.;
    let x = (cx - f64::from(w) / 2.)
        .round()
        .clamp(0., f64::from(bw - w));
    let y = (cy - f64::from(h) / 2.)
        .round()
        .clamp(0., f64::from(bh - h));
    PixelRect {
        x: x as u32,
        y: y as u32,
        w,
        h,
    }
}

fn pixel_bytes(width: u32, height: u32) -> Option<u64> {
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)
}

fn replay(base: &Arc<RgbaImage>, ops: &[EditOp]) -> Arc<RgbaImage> {
    let Some((first, rest)) = ops.split_first() else {
        // Reuse `base` instead of copying the whole image for an empty stack.
        return Arc::clone(base);
    };
    // `apply` only records ops that succeeded, so replay cannot fail.
    let mut img = apply_op(base, *first).expect("recorded edit op is valid");
    for &op in rest {
        img = apply_op(&img, op).expect("recorded edit op is valid");
    }
    Arc::new(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RgbaImage {
        RgbaImage::from_fn(4, 3, |x, y| image::Rgba([(y * 4 + x + 1) as u8, 0, 0, 255]))
    }

    fn ids(img: &RgbaImage) -> Vec<u8> {
        img.pixels().map(|pixel| pixel[0]).collect()
    }

    #[test]
    fn crop_clamps_to_bounds_rejects_zero_area_and_accepts_the_whole_image() {
        assert_eq!(
            clamp_crop(
                PixelRect {
                    x: 0,
                    y: 0,
                    w: 4,
                    h: 3
                },
                4,
                3
            ),
            Some(PixelRect {
                x: 0,
                y: 0,
                w: 4,
                h: 3
            })
        );
        assert_eq!(
            clamp_crop(
                PixelRect {
                    x: 1,
                    y: 1,
                    w: 1,
                    h: 1
                },
                4,
                3
            ),
            Some(PixelRect {
                x: 1,
                y: 1,
                w: 1,
                h: 1
            })
        );
        assert_eq!(
            clamp_crop(
                PixelRect {
                    x: 1,
                    y: 1,
                    w: 10,
                    h: 10
                },
                4,
                3
            ),
            Some(PixelRect {
                x: 1,
                y: 1,
                w: 3,
                h: 2
            })
        );
        assert_eq!(
            clamp_crop(
                PixelRect {
                    x: 3,
                    y: 2,
                    w: 10,
                    h: 10
                },
                4,
                3
            ),
            Some(PixelRect {
                x: 3,
                y: 2,
                w: 1,
                h: 1
            })
        );
        assert_eq!(
            clamp_crop(
                PixelRect {
                    x: 4,
                    y: 0,
                    w: 1,
                    h: 1
                },
                4,
                3
            ),
            None
        );
        assert_eq!(
            clamp_crop(
                PixelRect {
                    x: 0,
                    y: 3,
                    w: 1,
                    h: 1
                },
                4,
                3
            ),
            None
        );
        assert_eq!(
            clamp_crop(
                PixelRect {
                    x: 1,
                    y: 1,
                    w: 0,
                    h: 1
                },
                4,
                3
            ),
            None
        );
        assert_eq!(
            clamp_crop(
                PixelRect {
                    x: 1,
                    y: 1,
                    w: 1,
                    h: 0
                },
                4,
                3
            ),
            None
        );
        let huge = PixelRect {
            x: u32::MAX,
            y: u32::MAX,
            w: u32::MAX,
            h: u32::MAX,
        };
        assert_eq!(clamp_crop(huge, 4, 3), None);
    }

    #[test]
    fn crop_copies_only_the_selected_pixels() {
        let source = sample();
        let cropped = apply_op(
            &source,
            EditOp::Crop(PixelRect {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            }),
        )
        .unwrap();
        assert_eq!(cropped.dimensions(), (2, 2));
        assert_eq!(ids(&cropped), vec![6, 7, 10, 11]);
        let single = apply_op(
            &source,
            EditOp::Crop(PixelRect {
                x: 3,
                y: 2,
                w: 1,
                h: 1,
            }),
        )
        .unwrap();
        assert_eq!(ids(&single), vec![12]);
        let whole = apply_op(
            &source,
            EditOp::Crop(PixelRect {
                x: 0,
                y: 0,
                w: 4,
                h: 3,
            }),
        )
        .unwrap();
        assert_eq!(whole, source);
        assert_eq!(
            apply_op(
                &source,
                EditOp::Crop(PixelRect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 1
                })
            ),
            Err(EditError::ZeroArea)
        );
    }

    #[test]
    fn rotations_swap_axes_and_place_pixels_clockwise() {
        let source = sample();
        let cw = apply_op(&source, EditOp::RotateCw).unwrap();
        assert_eq!(cw.dimensions(), (3, 4));
        assert_eq!(ids(&cw), vec![9, 5, 1, 10, 6, 2, 11, 7, 3, 12, 8, 4]);
        let ccw = apply_op(&source, EditOp::RotateCcw).unwrap();
        assert_eq!(ccw.dimensions(), (3, 4));
        assert_eq!(ids(&ccw), vec![4, 8, 12, 3, 7, 11, 2, 6, 10, 1, 5, 9]);
        let half = apply_op(
            &apply_op(&source, EditOp::RotateCw).unwrap(),
            EditOp::RotateCw,
        )
        .unwrap();
        assert_eq!(half.dimensions(), (4, 3));
        assert_eq!(ids(&half), vec![12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]);
        let round_trip = apply_op(&cw, EditOp::RotateCcw).unwrap();
        assert_eq!(round_trip, source);
        let back = apply_op(&ccw, EditOp::RotateCw).unwrap();
        assert_eq!(back, source);
    }

    #[test]
    fn flips_mirror_in_place_and_twice_is_the_identity() {
        let source = sample();
        let horizontal = apply_op(&source, EditOp::FlipHorizontal).unwrap();
        assert_eq!(horizontal.dimensions(), (4, 3));
        assert_eq!(
            ids(&horizontal),
            vec![4, 3, 2, 1, 8, 7, 6, 5, 12, 11, 10, 9]
        );
        let vertical = apply_op(&source, EditOp::FlipVertical).unwrap();
        assert_eq!(ids(&vertical), vec![9, 10, 11, 12, 5, 6, 7, 8, 1, 2, 3, 4]);
        assert_eq!(
            apply_op(&horizontal, EditOp::FlipHorizontal).unwrap(),
            source
        );
        assert_eq!(apply_op(&vertical, EditOp::FlipVertical).unwrap(), source);
    }

    #[test]
    fn rotate_and_flip_do_not_commute() {
        let source = sample();
        let rotate_then_flip = apply_op(
            &apply_op(&source, EditOp::RotateCw).unwrap(),
            EditOp::FlipHorizontal,
        )
        .unwrap();
        let flip_then_rotate = apply_op(
            &apply_op(&source, EditOp::FlipHorizontal).unwrap(),
            EditOp::RotateCw,
        )
        .unwrap();
        assert_ne!(rotate_then_flip, flip_then_rotate);
        assert_eq!(
            ids(&rotate_then_flip),
            vec![1, 5, 9, 2, 6, 10, 3, 7, 11, 4, 8, 12]
        );
        assert_eq!(
            ids(&flip_then_rotate),
            vec![12, 8, 4, 11, 7, 3, 10, 6, 2, 9, 5, 1]
        );
    }

    #[test]
    fn undo_replays_the_prefix_of_the_op_stack() {
        let source = Arc::new(sample());
        let mut session = EditSession::start(Arc::clone(&source), 0).unwrap();
        assert!(!session.dirty());
        assert_eq!(**session.preview(), *source);

        let ops = [
            EditOp::Crop(PixelRect {
                x: 1,
                y: 0,
                w: 3,
                h: 3,
            }),
            EditOp::RotateCw,
            EditOp::FlipVertical,
        ];
        let mut snapshots = vec![(**session.preview()).clone()];
        for op in ops {
            session.apply(op).unwrap();
            snapshots.push((**session.preview()).clone());
        }
        assert!(session.dirty());

        assert!(session.undo());
        assert_eq!(**session.preview(), snapshots[2]);
        assert!(session.undo());
        assert_eq!(**session.preview(), snapshots[1]);
        assert!(session.undo());
        assert_eq!(**session.preview(), snapshots[0]);
        assert!(!session.dirty());
        assert!(!session.undo());
        assert_eq!(**session.preview(), *source);
    }

    #[test]
    fn start_bakes_each_displayed_rotation_into_base_and_preview() {
        let frame = Arc::new(sample());
        let cases = [
            (0, (4, 3), vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]),
            (1, (3, 4), vec![9, 5, 1, 10, 6, 2, 11, 7, 3, 12, 8, 4]),
            (2, (4, 3), vec![12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]),
            (3, (3, 4), vec![4, 8, 12, 3, 7, 11, 2, 6, 10, 1, 5, 9]),
            (4, (4, 3), vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]),
        ];
        for (turns, dimensions, expected) in cases {
            let session = EditSession::start(Arc::clone(&frame), turns).unwrap();
            assert!(!session.dirty());
            assert_eq!(session.preview().dimensions(), dimensions);
            assert_eq!(ids(session.preview()), expected);
        }
    }

    #[test]
    fn start_enforces_side_and_byte_budgets_and_rejects_empty_images() {
        let frame = Arc::new(sample());
        assert_eq!(
            EditSession::start_with_limits(Arc::clone(&frame), 0, 3, u64::MAX).err(),
            Some(EditError::Oversize {
                width: 4,
                height: 3
            })
        );
        assert_eq!(
            EditSession::start_with_limits(Arc::clone(&frame), 0, 65_535, 4 * 3 * 4 - 1).err(),
            Some(EditError::Oversize {
                width: 4,
                height: 3
            })
        );
        assert!(EditSession::start_with_limits(frame, 0, 65_535, 4 * 3 * 4).is_ok());
        let empty = Arc::new(RgbaImage::new(0, 0));
        assert_eq!(
            EditSession::start(empty, 0).err(),
            Some(EditError::EmptyImage)
        );
    }

    #[test]
    fn op_stack_stops_at_the_limit_and_keeps_the_preview_consistent() {
        let source = Arc::new(sample());
        let mut session = EditSession::start(Arc::clone(&source), 0).unwrap();
        for _ in 0..MAX_EDIT_OPS {
            session.apply(EditOp::FlipHorizontal).unwrap();
        }
        assert_eq!(session.apply(EditOp::RotateCw), Err(EditError::TooManyOps));
        // An even number of flips is the identity; the rejected op must not apply.
        assert_eq!(**session.preview(), *source);
        assert!(session.undo());
        assert!(session.apply(EditOp::RotateCw).is_ok());
    }

    #[test]
    fn aspect_fit_keeps_the_center_and_clamps_inside_bounds() {
        let bounds = (100, 100);
        assert_eq!(
            aspect_fit(
                PixelRect {
                    x: 10,
                    y: 10,
                    w: 80,
                    h: 40
                },
                None,
                bounds
            ),
            PixelRect {
                x: 10,
                y: 10,
                w: 80,
                h: 40
            }
        );
        assert_eq!(
            aspect_fit(
                PixelRect {
                    x: 10,
                    y: 10,
                    w: 80,
                    h: 40
                },
                Some((1, 1)),
                bounds
            ),
            PixelRect {
                x: 30,
                y: 10,
                w: 40,
                h: 40
            }
        );
        assert_eq!(
            aspect_fit(
                PixelRect {
                    x: 10,
                    y: 10,
                    w: 80,
                    h: 40
                },
                Some((2, 1)),
                bounds
            ),
            PixelRect {
                x: 10,
                y: 10,
                w: 80,
                h: 40
            }
        );
        // Center near the bottom-right corner is pulled back inside the image.
        assert_eq!(
            aspect_fit(
                PixelRect {
                    x: 90,
                    y: 90,
                    w: 20,
                    h: 20
                },
                Some((1, 1)),
                bounds
            ),
            PixelRect {
                x: 80,
                y: 80,
                w: 20,
                h: 20
            }
        );
        // Aspect wider than the bounds shrinks to fit the short bound.
        assert_eq!(
            aspect_fit(
                PixelRect {
                    x: 0,
                    y: 0,
                    w: 100,
                    h: 100
                },
                Some((4, 1)),
                (100, 20)
            ),
            PixelRect {
                x: 10,
                y: 0,
                w: 80,
                h: 20
            }
        );
        // No aspect and no area cannot produce a selection.
        assert_eq!(
            aspect_fit(
                PixelRect {
                    x: 5,
                    y: 5,
                    w: 0,
                    h: 0
                },
                None,
                bounds
            ),
            PixelRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1
            }
        );
        assert_eq!(
            aspect_fit(
                PixelRect {
                    x: 0,
                    y: 0,
                    w: 5,
                    h: 5
                },
                Some((1, 1)),
                (0, 0)
            ),
            PixelRect {
                x: 0,
                y: 0,
                w: 0,
                h: 0
            }
        );
        // A selection outside the image is clamped, not rejected.
        assert_eq!(
            aspect_fit(
                PixelRect {
                    x: 200,
                    y: 0,
                    w: 10,
                    h: 10
                },
                None,
                bounds
            ),
            PixelRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1
            }
        );
    }

    #[test]
    fn flips_then_crop_select_pixels_in_the_flipped_image() {
        let source = sample();
        let horizontal = apply_op(&source, EditOp::FlipHorizontal).unwrap();
        let cropped = apply_op(
            &horizontal,
            EditOp::Crop(PixelRect {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            }),
        )
        .unwrap();
        // Flipped rows are [4,3,2,1], [8,7,6,5], [12,11,10,9].
        assert_eq!(ids(&cropped), vec![7, 6, 11, 10]);

        let vertical = apply_op(&source, EditOp::FlipVertical).unwrap();
        let cropped = apply_op(
            &vertical,
            EditOp::Crop(PixelRect {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            }),
        )
        .unwrap();
        // Flipped rows are [9,10,11,12], [5,6,7,8], [1,2,3,4].
        assert_eq!(ids(&cropped), vec![6, 7, 2, 3]);
    }

    #[test]
    fn undo_after_two_rotations_and_a_crop_restores_just_the_rotations() {
        let mut session = EditSession::start(Arc::new(sample()), 0).unwrap();
        session.apply(EditOp::RotateCw).unwrap();
        session.apply(EditOp::RotateCw).unwrap();
        let rotated = (**session.preview()).clone();
        assert_eq!(rotated.dimensions(), (4, 3));

        session
            .apply(EditOp::Crop(PixelRect {
                x: 1,
                y: 0,
                w: 3,
                h: 2,
            }))
            .unwrap();
        assert_eq!(session.preview().dimensions(), (3, 2));
        assert_eq!(ids(session.preview()), vec![11, 10, 9, 7, 6, 5]);

        assert!(session.undo());
        assert_eq!(session.preview().dimensions(), (4, 3));
        assert_eq!(
            ids(session.preview()),
            vec![12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]
        );
        assert_eq!(**session.preview(), rotated);
    }

    #[test]
    fn rejected_crop_leaves_dirty_preview_and_stack_unchanged() {
        let source = Arc::new(sample());
        let mut session = EditSession::start(Arc::clone(&source), 0).unwrap();
        assert_eq!(
            session.apply(EditOp::Crop(PixelRect {
                x: 10,
                y: 10,
                w: 2,
                h: 2,
            })),
            Err(EditError::ZeroArea)
        );
        assert!(!session.dirty());
        assert_eq!(**session.preview(), *source);
        // A rejected op is never pushed, so undo has nothing to pop.
        assert!(!session.undo());

        session
            .apply(EditOp::Crop(PixelRect {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
            }))
            .unwrap();
        assert_eq!(session.preview().dimensions(), (2, 2));
        assert_eq!(
            session.apply(EditOp::Crop(PixelRect {
                x: 5,
                y: 0,
                w: 1,
                h: 1,
            })),
            Err(EditError::ZeroArea)
        );
        assert!(session.dirty());
        assert_eq!(session.preview().dimensions(), (2, 2));
        // Exactly one op was recorded, so one undo returns to the base.
        assert!(session.undo());
        assert_eq!(**session.preview(), *source);
        assert!(!session.undo());
    }

    #[test]
    fn aspect_fit_extremes_and_maximum_inputs_stay_inside_bounds() {
        let fitted = |rect, aspect, bounds: (u32, u32)| {
            let result = aspect_fit(rect, aspect, bounds);
            let in_bounds = u64::from(result.x) + u64::from(result.w) <= u64::from(bounds.0)
                && u64::from(result.y) + u64::from(result.h) <= u64::from(bounds.1);
            assert!(in_bounds, "{result:?} escapes {bounds:?}");
            if bounds.0 > 0 && bounds.1 > 0 {
                assert!(result.w >= 1 && result.h >= 1, "{result:?}");
            }
            result
        };

        let square = PixelRect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        assert_eq!(
            fitted(square, Some((1, 1000)), (100, 100)),
            PixelRect {
                x: 50,
                y: 0,
                w: 1,
                h: 100
            }
        );
        assert_eq!(
            fitted(square, Some((1000, 1)), (100, 100)),
            PixelRect {
                x: 0,
                y: 50,
                w: 100,
                h: 1
            }
        );

        let max = u32::MAX;
        let maximum = PixelRect {
            x: max,
            y: max,
            w: max,
            h: max,
        };
        assert_eq!(
            fitted(maximum, Some((1, 1)), (max, max)),
            PixelRect {
                x: 0,
                y: 0,
                w: max,
                h: max
            }
        );
        // No aspect still clamps; a rect entirely past the image has no pixels.
        assert_eq!(
            fitted(maximum, None, (max, max)),
            PixelRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1
            }
        );
        // A huge selection against small bounds is pulled fully inside.
        assert_eq!(
            fitted(maximum, Some((2, 1)), (100, 100)),
            PixelRect {
                x: 0,
                y: 50,
                w: 100,
                h: 50
            }
        );

        assert_eq!(
            fitted(
                PixelRect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1
                },
                Some((1, 1)),
                (1, 1)
            ),
            PixelRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1
            }
        );
        for bounds in [(0, 10), (10, 0), (0, 0)] {
            assert_eq!(
                fitted(square, Some((1, 1)), bounds),
                PixelRect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0
                }
            );
        }
    }
}
