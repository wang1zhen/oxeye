//! Pure navigation math and the sequential-navigation queue.
//!
//! No UI or decoder types live here: the queue stores actions and destination
//! file indices. The UI decides when a rendered frame is committed and calls
//! [`NavigationQueue::complete`] at that boundary.

use std::collections::VecDeque;

use crate::settings::ReadingMode;

/// A requested navigation step, relative to the current view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Navigation {
    Previous,
    Next,
    /// Offset by this many files, clamped to the directory, then normalized.
    JumpFiles(isize),
}

/// Immutable page grouping used by navigation and reading-order rendering.
pub trait PageLayout {
    fn length(&self) -> usize;
    fn anchor(&self, index: usize) -> Option<usize>;
    fn pages(&self, index: usize) -> Vec<usize>;
    fn destination(&self, index: usize, action: Navigation) -> Option<usize>;
}

#[derive(Clone, Copy, Debug)]
pub struct FixedLayout {
    pub length: usize,
    pub mode: ReadingMode,
}

impl PageLayout for FixedLayout {
    fn length(&self) -> usize {
        self.length
    }

    fn anchor(&self, index: usize) -> Option<usize> {
        anchor(index, self.length, self.mode)
    }

    fn pages(&self, index: usize) -> Vec<usize> {
        pages(index, self.length, self.mode)
    }

    fn destination(&self, index: usize, action: Navigation) -> Option<usize> {
        destination(index, self.length, self.mode, action)
    }
}

/// Normalizes a file index to the first index of its view.
///
/// In book mode the cover is index 0 alone and every other spread starts at an
/// odd index, so a clicked even page maps back to the previous odd one.
pub fn anchor(index: usize, length: usize, mode: ReadingMode) -> Option<usize> {
    if index >= length {
        return None;
    }
    Some(match mode {
        ReadingMode::Single => index,
        ReadingMode::Book if index == 0 => 0,
        ReadingMode::Book if index % 2 == 1 => index,
        ReadingMode::Book => index - 1,
    })
}

/// Files shown by the view containing `index`, in reading order.
///
/// Book spreads are right page first: `[1, 2]` means file 1 is on the right.
pub fn pages(index: usize, length: usize, mode: ReadingMode) -> Vec<usize> {
    let Some(first) = anchor(index, length, mode) else {
        return Vec::new();
    };
    match mode {
        ReadingMode::Single => vec![first],
        ReadingMode::Book if first == 0 => vec![0],
        ReadingMode::Book if first + 1 < length => vec![first, first + 1],
        ReadingMode::Book => vec![first],
    }
}

/// Destination view for `action` from the view containing `index`.
///
/// Returns `None` at the ends (no wrapping) and when the target would be the
/// current view.
pub fn destination(
    index: usize,
    length: usize,
    mode: ReadingMode,
    action: Navigation,
) -> Option<usize> {
    let current = anchor(index, length, mode)?;
    match action {
        Navigation::Previous => match mode {
            ReadingMode::Single => current.checked_sub(1),
            ReadingMode::Book => match current {
                0 => None,
                1 => Some(0),
                _ => Some(current - 2),
            },
        },
        Navigation::Next => match mode {
            ReadingMode::Single => {
                let next = current + 1;
                (next < length).then_some(next)
            }
            ReadingMode::Book => {
                let next = if current == 0 { 1 } else { current + 2 };
                (next < length).then_some(next)
            }
        },
        Navigation::JumpFiles(delta) => {
            let target = (current as i128 + delta as i128).clamp(0, length as i128 - 1) as usize;
            let target = anchor(target, length, mode)?;
            (target != current).then_some(target)
        }
    }
}

/// Sequential navigation requests, computed against the queued tail.
///
/// Only actions and destination indices are stored; no decoded pixels or owning resources.
#[derive(Default)]
pub struct NavigationQueue {
    pending: VecDeque<(Navigation, usize)>,
    active: Option<usize>,
    generation: u64,
}

impl NavigationQueue {
    /// Queues the destination of `action` and reports whether it was accepted.
    ///
    /// The step is computed from the queued tail, else the active target, else
    /// `base_index`. Boundary no-ops are ignored rather than queued.
    pub fn enqueue(
        &mut self,
        action: Navigation,
        base_index: usize,
        length: usize,
        mode: ReadingMode,
    ) -> bool {
        self.enqueue_with(action, base_index, &FixedLayout { length, mode })
    }

    pub fn enqueue_with(
        &mut self,
        action: Navigation,
        base_index: usize,
        layout: &dyn PageLayout,
    ) -> bool {
        let current = self
            .pending
            .back()
            .map(|&(_, target)| target)
            .or(self.active)
            .unwrap_or(base_index);
        let Some(target) = layout.destination(current, action) else {
            return false;
        };
        self.pending.push_back((action, target));
        true
    }

    /// Replays pending actions from the corrected base, leaving active work unchanged.
    pub fn retarget(&mut self, base: usize, layout: &dyn PageLayout) {
        let mut current = base;
        self.pending.retain_mut(|(action, target)| {
            let Some(next) = layout.destination(current, *action) else {
                return false;
            };
            *target = next;
            current = next;
            true
        });
    }

    /// Pops the next target once the previous one has completed and renders it.
    pub fn start_next(&mut self) -> Option<usize> {
        if self.active.is_some() {
            return None;
        }
        let (_, next) = self.pending.pop_front()?;
        self.active = Some(next);
        Some(next)
    }

    /// Marks the active target as committed if the index matches.
    ///
    /// Does not start the next item: the UI waits for its own frame barrier.
    pub fn complete(&mut self, index: usize) -> bool {
        if self.active == Some(index) {
            self.active = None;
            true
        } else {
            false
        }
    }

    /// Drops all queued and active work and invalidates callbacks.
    pub fn clear(&mut self) {
        self.pending.clear();
        self.active = None;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Generation used to reject callbacks from before a [`Self::clear`].
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Number of queued targets, excluding the active one.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Whether nothing is queued or active.
    pub fn is_idle(&self) -> bool {
        self.active.is_none() && self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::Navigation::*;
    use super::*;
    use crate::settings::ReadingMode::{Book, Single};

    #[test]
    fn retarget_visits_new_wide_page_and_preserves_active_and_generation() {
        use crate::comic::comic_layout;
        use std::collections::{BTreeMap, HashMap};

        let original = comic_layout(6, &HashMap::new(), &BTreeMap::new(), true);
        let updated = comic_layout(6, &HashMap::from([(2, (400, 100))]), &BTreeMap::new(), true);
        for active in [false, true] {
            let mut queue = NavigationQueue::default();
            queue.clear();
            let generation = queue.generation();
            assert!(queue.enqueue_with(Next, 0, &original));
            assert!(queue.enqueue_with(Next, 0, &original));
            assert_eq!(queue.pending, [(Next, 1), (Next, 3)]);
            if active {
                assert_eq!(queue.start_next(), Some(1));
            }
            queue.retarget(if active { 1 } else { 0 }, &updated);
            assert_eq!(queue.generation(), generation);
            if active {
                assert_eq!(queue.active, Some(1));
                assert_eq!(queue.start_next(), None);
                assert!(queue.complete(1));
            } else {
                assert_eq!(queue.start_next(), Some(1));
                assert!(queue.complete(1));
            }
            assert_eq!(queue.start_next(), Some(2));
            assert!(queue.complete(2));
            assert!(queue.is_idle());
        }
    }

    #[test]
    fn retarget_drops_boundary_and_jump_noops_without_reordering() {
        let mut queue = NavigationQueue::default();
        for action in [Next, Next, Previous, JumpFiles(-1)] {
            assert!(queue.enqueue(action, 0, 5, Single));
        }
        queue.retarget(
            1,
            &FixedLayout {
                length: 2,
                mode: Single,
            },
        );
        assert_eq!(queue.pending, [(Previous, 0)]);
        assert_eq!(queue.start_next(), Some(0));
        assert!(queue.complete(0));
        assert!(queue.is_idle());

        assert!(queue.enqueue(Next, 0, 5, Single));
        queue.retarget(
            0,
            &FixedLayout {
                length: 0,
                mode: Single,
            },
        );
        assert!(queue.is_idle());
    }

    #[test]
    fn fixed_layout_retarget_preserves_actions_and_targets() {
        for mode in [Single, Book] {
            let layout = FixedLayout { length: 16, mode };
            let mut queue = NavigationQueue::default();
            for action in [Next, Next, Previous, JumpFiles(10), JumpFiles(-10)] {
                assert!(queue.enqueue(action, 0, layout.length, mode));
            }
            let expected = queue.pending.clone();
            queue.retarget(0, &layout);
            assert_eq!(queue.pending, expected);
            assert_eq!(queue.start_next(), Some(1));
            let expected = queue.pending.clone();
            queue.retarget(1, &layout);
            assert_eq!(queue.pending, expected);
            assert_eq!(queue.active, Some(1));
        }
    }

    #[test]
    fn fixed_layout_matches_existing_navigation_functions() {
        for length in [0, 1, 2, 3, 6] {
            for mode in [Single, Book] {
                let layout = FixedLayout { length, mode };
                assert_eq!(layout.length(), length);
                for index in 0..=length {
                    assert_eq!(layout.anchor(index), anchor(index, length, mode));
                    assert_eq!(layout.pages(index), pages(index, length, mode));
                    for action in [Next, Previous, JumpFiles(-10), JumpFiles(0), JumpFiles(10)] {
                        assert_eq!(
                            layout.destination(index, action),
                            destination(index, length, mode, action)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn first_next_waits_for_render_completion() {
        let mut queue = NavigationQueue::default();
        assert!(queue.is_idle());
        assert!(queue.enqueue(Next, 0, 10, Single));
        assert!(!queue.is_idle());
        assert_eq!(queue.pending_len(), 1);

        assert_eq!(queue.start_next(), Some(1));
        assert_eq!(queue.start_next(), None, "only one active target");
        assert_eq!(queue.pending_len(), 0);
        assert!(!queue.is_idle());
        assert!(queue.complete(1));
        assert!(queue.is_idle());
    }

    #[test]
    fn queued_tail_drives_mixed_directions() {
        let mut queue = NavigationQueue::default();
        assert!(queue.enqueue(Next, 0, 10, Single)); // 0 -> 1
        assert!(queue.enqueue(Next, 0, 10, Single)); // tail 1 -> 2
        assert!(queue.enqueue(Previous, 0, 10, Single)); // tail 2 -> 1
        assert!(queue.enqueue(Next, 0, 10, Single)); // tail 1 -> 2
        assert_eq!(queue.pending_len(), 4);

        let mut rendered = Vec::new();
        while let Some(index) = queue.start_next() {
            rendered.push(index);
            assert!(queue.complete(index));
        }
        assert_eq!(rendered, vec![1, 2, 1, 2]);
        assert!(queue.is_idle());
    }

    #[test]
    fn active_target_is_the_base_when_nothing_is_queued() {
        let mut queue = NavigationQueue::default();
        assert!(queue.enqueue(Next, 0, 10, Single));
        assert_eq!(queue.start_next(), Some(1));
        assert!(queue.enqueue(Next, 0, 10, Single)); // active 1 -> 2
        assert_eq!(queue.start_next(), None);
        assert!(queue.complete(1));
        assert_eq!(queue.start_next(), Some(2));
    }

    #[test]
    fn clipping_never_wraps_and_boundaries_are_not_queued() {
        assert_eq!(destination(0, 0, Single, Next), None);
        assert_eq!(destination(0, 1, Single, Previous), None);
        assert_eq!(destination(0, 1, Single, Next), None);
        assert_eq!(destination(4, 5, Single, Next), None);
        assert_eq!(destination(0, 5, Single, Previous), None);
        assert_eq!(destination(1, 5, Single, Next), Some(2));
        assert_eq!(destination(1, 5, Single, Previous), Some(0));

        let mut queue = NavigationQueue::default();
        assert!(!queue.enqueue(Previous, 0, 10, Single));
        assert!(queue.is_idle());
        assert!(!queue.enqueue(Next, 9, 10, Single));
        assert!(queue.is_idle());
        assert!(!queue.enqueue(JumpFiles(0), 3, 10, Single));
        assert!(queue.is_idle());
        assert!(!queue.enqueue(JumpFiles(-10), 0, 10, Single));
        assert!(queue.is_idle());
        assert!(!queue.enqueue(JumpFiles(10), 9, 10, Single));
        assert!(queue.is_idle());
    }

    #[test]
    fn book_cover_spreads_and_tail_page() {
        let length = 6;
        for (index, expected) in [
            (0, vec![0]),
            (1, vec![1, 2]),
            (2, vec![1, 2]),
            (3, vec![3, 4]),
            (4, vec![3, 4]),
            (5, vec![5]),
        ] {
            assert_eq!(pages(index, length, Book), expected, "index {index}");
        }
        assert!(pages(6, length, Book).is_empty());
        assert!(pages(0, 0, Book).is_empty());

        assert_eq!(destination(0, length, Book, Next), Some(1));
        assert_eq!(destination(1, length, Book, Next), Some(3));
        assert_eq!(destination(3, length, Book, Next), Some(5));
        assert_eq!(destination(5, length, Book, Next), None);
        assert_eq!(destination(5, length, Book, Previous), Some(3));
        assert_eq!(destination(3, length, Book, Previous), Some(1));
        assert_eq!(destination(1, length, Book, Previous), Some(0));
        assert_eq!(destination(0, length, Book, Previous), None);
        // A clicked even page belongs to the previous odd-anchored spread.
        assert_eq!(destination(2, length, Book, Next), Some(3));
        assert_eq!(anchor(2, length, Book), Some(1));
        assert_eq!(anchor(4, length, Book), Some(3));
    }

    #[test]
    fn jump_files_clamps_then_normalizes() {
        assert_eq!(destination(0, 6, Single, JumpFiles(10)), Some(5));
        assert_eq!(destination(5, 6, Single, JumpFiles(-10)), Some(0));
        assert_eq!(destination(3, 6, Single, JumpFiles(10)), Some(5));
        assert_eq!(destination(3, 6, Single, JumpFiles(-10)), Some(0));

        // Jump ten files, clamp to index 6, then normalize to spread 5.
        assert_eq!(destination(0, 7, Book, JumpFiles(10)), Some(5));
        // From spread 3, jump back one file: 2 normalizes to spread 1.
        assert_eq!(destination(4, 7, Book, JumpFiles(-1)), Some(1));
        assert_eq!(destination(6, 7, Book, JumpFiles(0)), None);
    }

    #[test]
    fn start_next_requires_matching_completion() {
        let mut queue = NavigationQueue::default();
        assert!(queue.enqueue(Next, 0, 10, Single));
        assert!(queue.enqueue(Next, 0, 10, Single));

        assert_eq!(queue.start_next(), Some(1));
        assert_eq!(queue.start_next(), None);
        assert!(!queue.complete(2), "only the active index completes");
        assert!(!queue.complete(0));
        assert_eq!(queue.pending_len(), 1);

        assert!(queue.complete(1));
        assert_eq!(queue.start_next(), Some(2));
        assert!(queue.complete(2));
        assert!(queue.is_idle());
    }

    #[test]
    fn clear_invalidates_generation_and_stale_completions() {
        let mut queue = NavigationQueue::default();
        let generation = queue.generation();
        assert!(queue.enqueue(Next, 0, 10, Single));
        assert!(queue.enqueue(Next, 0, 10, Single));
        assert_eq!(queue.start_next(), Some(1));

        queue.clear();
        assert_eq!(queue.generation(), generation + 1);
        assert!(queue.is_idle());
        assert_eq!(queue.pending_len(), 0);
        assert_eq!(queue.start_next(), None);
        assert!(!queue.complete(1), "old target is stale");

        queue.clear();
        assert_eq!(queue.generation(), generation + 2);
    }
}
