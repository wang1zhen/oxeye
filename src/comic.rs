//! Immutable right-to-left comic spreads, indexed by catalog position.

use std::collections::{BTreeMap, HashMap};

use crate::navigation::{Navigation, PageLayout};
use crate::progress::PageOverride;

/// Zero dimensions are unknown; widening u32 before multiplying avoids overflow.
pub fn is_wide(width: u32, height: u32) -> bool {
    height > 0 && width > height && u64::from(width) * 5 > u64::from(height) * 6
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComicLayout {
    spreads: Vec<Vec<usize>>,
    spread_for_page: Vec<usize>,
}

/// The cover is always single, even with a manual Spread override.
pub fn comic_layout(
    page_count: usize,
    dimensions: &HashMap<usize, (u32, u32)>,
    overrides: &BTreeMap<usize, PageOverride>,
    auto_wide: bool,
) -> ComicLayout {
    let auto_single = |index| {
        auto_wide
            && dimensions
                .get(&index)
                .is_some_and(|&(width, height)| is_wide(width, height))
    };
    let mut layout = ComicLayout {
        spreads: Vec::new(),
        spread_for_page: vec![0; page_count],
    };
    let mut index = 0;
    while index < page_count {
        let mode = overrides.get(&index).copied().unwrap_or_default();
        let single = index == 0
            || mode == PageOverride::Single
            || (mode == PageOverride::Auto && auto_single(index));
        let pair = !single
            && index + 1 < page_count
            && (mode == PageOverride::Spread
                || (overrides.get(&(index + 1)) != Some(&PageOverride::Single)
                    && !auto_single(index + 1)));
        let end = index + if pair { 2 } else { 1 };
        layout.spread_for_page[index..end].fill(layout.spreads.len());
        layout.spreads.push((index..end).collect());
        index = end;
    }
    layout
}

impl ComicLayout {
    pub fn page_count(&self) -> usize {
        self.spread_for_page.len()
    }

    pub fn spread_count(&self) -> usize {
        self.spreads.len()
    }

    /// Zero-based spread ordinal, including for the second page of a pair.
    pub fn spread_of(&self, index: usize) -> Option<usize> {
        self.spread_for_page.get(index).copied()
    }

    pub fn anchor(&self, index: usize) -> Option<usize> {
        Some(self.spreads[self.spread_of(index)?][0])
    }

    /// Pages in reading order: right page, then left page.
    pub fn pages(&self, index: usize) -> Vec<usize> {
        self.spread_of(index)
            .map(|spread| self.spreads[spread].clone())
            .unwrap_or_default()
    }

    pub fn first(&self) -> Option<usize> {
        self.spreads.first().map(|pages| pages[0])
    }

    pub fn last(&self) -> Option<usize> {
        self.spreads.last().map(|pages| pages[0])
    }

    /// File jumps start at the containing spread's anchor, like FixedLayout.
    pub fn destination(&self, index: usize, action: Navigation) -> Option<usize> {
        let spread = self.spread_of(index)?;
        let current = self.spreads[spread][0];
        match action {
            Navigation::Previous => self
                .spreads
                .get(spread.checked_sub(1)?)
                .map(|pages| pages[0]),
            Navigation::Next => self.spreads.get(spread + 1).map(|pages| pages[0]),
            Navigation::JumpFiles(delta) => {
                let target = (current as i128 + delta as i128)
                    .clamp(0, self.page_count() as i128 - 1) as usize;
                let target = self.anchor(target)?;
                (target != current).then_some(target)
            }
        }
    }
}

impl PageLayout for ComicLayout {
    fn length(&self) -> usize {
        self.page_count()
    }

    fn anchor(&self, index: usize) -> Option<usize> {
        ComicLayout::anchor(self, index)
    }

    fn pages(&self, index: usize) -> Vec<usize> {
        ComicLayout::pages(self, index)
    }

    fn destination(&self, index: usize, action: Navigation) -> Option<usize> {
        ComicLayout::destination(self, index, action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::{FixedLayout, NavigationQueue};
    use crate::settings::ReadingMode;
    use crate::ui_state::NavigationFlow;

    #[test]
    fn wide_threshold_is_strict_and_handles_unknown_or_maximum_dimensions() {
        assert!(!is_wide(0, 0));
        assert!(!is_wide(100, 0));
        assert!(!is_wide(0, 100));
        assert!(!is_wide(100, 100));
        assert!(!is_wide(120, 100));
        assert!(is_wide(121, 100));
        assert!(!is_wide(100, 121));
        assert!(is_wide(u32::MAX, 1));
        assert!(!is_wide(u32::MAX, u32::MAX));
        assert!(is_wide(u32::MAX, u32::MAX / 2));
    }

    #[test]
    fn ordinary_page_counts_match_fixed_book_layout_and_cover_every_page_once() {
        for length in [0, 1, 2, 3, 6] {
            let layout = comic_layout(length, &HashMap::new(), &BTreeMap::new(), true);
            let fixed = FixedLayout {
                length,
                mode: ReadingMode::Book,
            };
            assert_eq!(layout.page_count(), length);
            assert_eq!(
                layout.spread_count(),
                if length == 0 { 0 } else { 1 + length / 2 }
            );
            assert_eq!(layout.first(), (length > 0).then_some(0));
            assert_eq!(
                layout.last(),
                length.checked_sub(1).and_then(|index| fixed.anchor(index))
            );
            let mut visited = Vec::new();
            for pages in &layout.spreads {
                visited.extend_from_slice(pages);
            }
            assert_eq!(visited, (0..length).collect::<Vec<_>>());
            for index in 0..=length {
                assert_eq!(layout.anchor(index), fixed.anchor(index));
                assert_eq!(layout.pages(index), fixed.pages(index));
                for action in [
                    Navigation::Previous,
                    Navigation::Next,
                    Navigation::JumpFiles(-10),
                    Navigation::JumpFiles(0),
                    Navigation::JumpFiles(10),
                ] {
                    assert_eq!(
                        layout.destination(index, action),
                        fixed.destination(index, action)
                    );
                }
            }
        }
    }

    #[test]
    fn cover_wide_current_wide_next_and_unknown_dimensions() {
        let dimensions = HashMap::from([(0, (400, 100)), (2, (400, 100)), (5, (0, 0))]);
        let overrides = BTreeMap::from([(0, PageOverride::Spread)]);
        let layout = comic_layout(6, &dimensions, &overrides, true);
        assert_eq!(
            layout.spreads,
            [vec![0], vec![1], vec![2], vec![3, 4], vec![5]]
        );
        assert_eq!(layout.anchor(4), Some(3));
        assert_eq!(layout.spread_of(4), Some(3));
        assert_eq!(layout.pages(4), [3, 4]);
        assert_eq!(layout.spread_of(6), None);
        let disabled = comic_layout(6, &dimensions, &overrides, false);
        assert_eq!(disabled.spreads, [vec![0], vec![1, 2], vec![3, 4], vec![5]]);
        let wide_first = comic_layout(4, &HashMap::from([(1, (400, 100))]), &BTreeMap::new(), true);
        assert_eq!(wide_first.spreads, [vec![0], vec![1], vec![2, 3]]);
    }

    #[test]
    fn manual_single_spread_and_tail_follow_pairing_precedence() {
        let single = comic_layout(
            6,
            &HashMap::new(),
            &BTreeMap::from([(2, PageOverride::Single)]),
            false,
        );
        assert_eq!(
            single.spreads,
            [vec![0], vec![1], vec![2], vec![3, 4], vec![5]]
        );
        let forced = comic_layout(
            6,
            &HashMap::from([(2, (400, 100))]),
            &BTreeMap::from([
                (1, PageOverride::Spread),
                (2, PageOverride::Single),
                (5, PageOverride::Spread),
            ]),
            true,
        );
        assert_eq!(forced.spreads, [vec![0], vec![1, 2], vec![3, 4], vec![5]]);
        for (mode, expected) in [
            (PageOverride::Spread, vec![vec![0], vec![1, 2]]),
            (PageOverride::Single, vec![vec![0], vec![1], vec![2]]),
            (PageOverride::Auto, vec![vec![0], vec![1], vec![2]]),
        ] {
            let wide = comic_layout(
                3,
                &HashMap::from([(1, (400, 100)), (2, (400, 100))]),
                &BTreeMap::from([(1, mode)]),
                true,
            );
            assert_eq!(wide.spreads, expected, "{mode:?}");
        }
        let auto = comic_layout(
            3,
            &HashMap::new(),
            &BTreeMap::from([(1, PageOverride::Auto)]),
            true,
        );
        assert_eq!(auto.spreads, [vec![0], vec![1, 2]]);
    }

    #[test]
    fn navigation_and_queued_tail_use_actual_spreads_without_wrapping() {
        let layout = comic_layout(7, &HashMap::from([(2, (400, 100))]), &BTreeMap::new(), true);
        assert_eq!(
            layout.spreads,
            [vec![0], vec![1], vec![2], vec![3, 4], vec![5, 6]]
        );
        assert_eq!(layout.destination(0, Navigation::Previous), None);
        assert_eq!(layout.destination(6, Navigation::Next), None);
        assert_eq!(layout.destination(4, Navigation::Next), Some(5));
        assert_eq!(layout.destination(4, Navigation::Previous), Some(2));
        assert_eq!(layout.destination(4, Navigation::JumpFiles(0)), None);
        assert_eq!(layout.destination(4, Navigation::JumpFiles(-1)), Some(2));
        assert_eq!(
            layout.destination(0, Navigation::JumpFiles(isize::MAX)),
            Some(5)
        );
        assert_eq!(
            layout.destination(6, Navigation::JumpFiles(isize::MIN)),
            Some(0)
        );
        assert_eq!(layout.destination(7, Navigation::Next), None);
        let mut queue = NavigationQueue::default();
        for action in [
            Navigation::Next,
            Navigation::Next,
            Navigation::Next,
            Navigation::Previous,
        ] {
            assert!(queue.enqueue_with(action, 0, &layout));
        }
        for expected in [1, 2, 3, 2] {
            assert_eq!(queue.start_next(), Some(expected));
            assert!(queue.complete(expected));
        }
        assert!(queue.is_idle());
    }

    #[test]
    fn navigation_flow_retains_paint_barrier_for_variable_spreads() {
        let layout = comic_layout(6, &HashMap::from([(1, (400, 100))]), &BTreeMap::new(), true);
        let mut flow = NavigationFlow::default();
        assert!(flow.enqueue_with(Navigation::Next, 0, &layout));
        assert!(flow.enqueue_with(Navigation::Next, 0, &layout));
        assert_eq!(flow.pending_len(), 2);
        let first = flow.start_next(10).unwrap();
        assert_eq!(first.index, 1);
        assert!(flow.start_next(11).is_none());
        assert!(!flow.crossed_frame(first));
        assert_eq!(flow.painted(10), Some(first));
        assert!(flow.crossed_frame(first));
        let second = flow.start_next(11).unwrap();
        assert_eq!(second.index, 2);
        flow.clear();
        assert!(!flow.crossed_frame(second));
        assert!(flow.is_idle());
    }
}
