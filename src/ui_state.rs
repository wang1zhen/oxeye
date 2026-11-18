use crate::{
    navigation::{Navigation, NavigationQueue, PageLayout},
    settings::{AppSettings, ComicArrowDirection, ReadingMode},
};
#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
pub fn selected_index(selected: Option<&Path>, siblings: &[PathBuf]) -> Option<usize> {
    let selected = selected?;
    siblings.iter().position(|path| path == selected)
}

pub fn horizontal_navigation(right: bool, direction: ComicArrowDirection) -> Navigation {
    if right == (direction == ComicArrowDirection::RightNext) {
        Navigation::Next
    } else {
        Navigation::Previous
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackKind {
    Success,
    Progress,
    Error,
}

#[derive(Clone)]
pub struct Notice {
    pub kind: FeedbackKind,
    pub text: String,
}

#[derive(Default)]
pub struct Feedback {
    pub notice: Option<Notice>,
    generation: u64,
}

impl Feedback {
    pub fn replace(&mut self, kind: FeedbackKind, text: String) -> u64 {
        self.generation += 1;
        self.notice = Some(Notice { kind, text });
        self.generation
    }

    pub fn dismiss(&mut self) {
        self.generation += 1;
        self.notice = None;
    }

    pub fn expire(&mut self, generation: u64) -> bool {
        if generation != self.generation
            || !self
                .notice
                .as_ref()
                .is_some_and(|notice| notice.kind == FeedbackKind::Success)
        {
            return false;
        }
        self.dismiss();
        true
    }
}

pub fn restore_defaults(draft: &mut AppSettings) {
    *draft = AppSettings::default();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavigationTicket {
    pub index: usize,
    generation: u64,
    revision: u64,
}

#[derive(Default)]
pub struct NavigationFlow {
    queue: NavigationQueue,
    active: Option<NavigationTicket>,
    painted: bool,
}

impl NavigationFlow {
    pub fn enqueue(
        &mut self,
        action: Navigation,
        base: usize,
        length: usize,
        mode: ReadingMode,
    ) -> bool {
        self.queue.enqueue(action, base, length, mode)
    }

    pub fn enqueue_with(
        &mut self,
        action: Navigation,
        base: usize,
        layout: &dyn PageLayout,
    ) -> bool {
        self.queue.enqueue_with(action, base, layout)
    }

    pub fn retarget(&mut self, base: usize, layout: &dyn PageLayout) {
        self.queue.retarget(base, layout);
    }

    pub fn is_idle(&self) -> bool {
        self.queue.is_idle()
    }

    pub fn pending_len(&self) -> usize {
        self.queue.pending_len()
    }

    pub fn start_next(&mut self, revision: u64) -> Option<NavigationTicket> {
        let index = self.queue.start_next()?;
        let ticket = NavigationTicket {
            index,
            generation: self.queue.generation(),
            revision,
        };
        self.active = Some(ticket);
        self.painted = false;
        Some(ticket)
    }

    pub fn painted(&mut self, revision: u64) -> Option<NavigationTicket> {
        let ticket = self.active.filter(|ticket| ticket.revision == revision)?;
        if self.painted {
            return None;
        }
        self.painted = true;
        Some(ticket)
    }

    pub fn crossed_frame(&mut self, ticket: NavigationTicket) -> bool {
        if self.active != Some(ticket)
            || !self.painted
            || self.queue.generation() != ticket.generation
        {
            return false;
        }
        self.active = None;
        self.painted = false;
        self.queue.complete(ticket.index)
    }

    pub fn failed(&mut self, revision: u64) -> bool {
        let Some(ticket) = self.active.filter(|ticket| ticket.revision == revision) else {
            return false;
        };
        self.active = None;
        self.painted = false;
        self.queue.complete(ticket.index)
    }

    pub fn clear(&mut self) {
        self.queue.clear();
        self.active = None;
        self.painted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retarget_preserves_active_ticket_and_paint_barrier() {
        let mut flow = NavigationFlow::default();
        assert!(flow.enqueue(Navigation::Next, 0, 6, ReadingMode::Book));
        assert!(flow.enqueue(Navigation::Next, 0, 6, ReadingMode::Book));
        let first = flow.start_next(10).unwrap();
        assert_eq!(first.index, 1);
        assert_eq!(flow.painted(10), Some(first));
        flow.retarget(
            1,
            &crate::navigation::FixedLayout {
                length: 6,
                mode: ReadingMode::Single,
            },
        );
        assert_eq!(flow.pending_len(), 1);
        assert!(flow.start_next(11).is_none());
        assert!(flow.crossed_frame(first));
        let next = flow.start_next(11).unwrap();
        assert_eq!(next.index, 2);
        assert!(!flow.crossed_frame(next));
        assert_eq!(flow.painted(11), Some(next));
        assert!(flow.crossed_frame(next));
        assert!(flow.is_idle());
    }

    #[test]
    fn book_fifo_waits_for_whole_spread_paint_and_uses_file_jump_normalization() {
        let mut flow = NavigationFlow::default();
        for action in [
            Navigation::Next,
            Navigation::Next,
            Navigation::Previous,
            Navigation::JumpFiles(10),
        ] {
            assert!(flow.enqueue(action, 0, 16, ReadingMode::Book));
        }
        let mut seen = Vec::new();
        for revision in 1..=4 {
            let ticket = flow.start_next(revision).unwrap();
            seen.push(ticket.index);
            assert!(!flow.crossed_frame(ticket));
            flow.painted(revision).unwrap();
            assert!(flow.start_next(revision + 1).is_none());
            assert!(flow.crossed_frame(ticket));
        }
        assert_eq!(seen, vec![1, 3, 1, 11]);
        assert!(flow.is_idle());
    }

    #[test]
    fn configurable_horizontal_arrows_do_not_change_logical_page_navigation() {
        assert_eq!(
            horizontal_navigation(true, ComicArrowDirection::RightNext),
            Navigation::Next
        );
        assert_eq!(
            horizontal_navigation(false, ComicArrowDirection::RightNext),
            Navigation::Previous
        );
        assert_eq!(
            horizontal_navigation(true, ComicArrowDirection::LeftNext),
            Navigation::Previous
        );
        assert_eq!(
            horizontal_navigation(false, ComicArrowDirection::LeftNext),
            Navigation::Next
        );
        for direction in [
            ComicArrowDirection::RightNext,
            ComicArrowDirection::LeftNext,
        ] {
            let mut flow = NavigationFlow::default();
            assert!(flow.enqueue(Navigation::Next, 0, 6, ReadingMode::Book));
            assert_eq!(flow.start_next(1).unwrap().index, 1);
            assert_eq!(
                horizontal_navigation(direction == ComicArrowDirection::RightNext, direction),
                Navigation::Next
            );
        }
    }

    #[test]
    fn navigation_context_rejects_old_directory_until_selected_context_arrives() {
        let old: Vec<PathBuf> = ["X/A.png", "X/next.png"].map(PathBuf::from).into();
        let displayed = old[0].clone();
        let selected = PathBuf::from("Y/B.png");
        let mut flow = NavigationFlow::default();
        assert_eq!(selected_index(Some(&displayed), &old), Some(0));
        let old_base = selected_index(Some(&selected), &old);
        assert_eq!(old_base, None);
        if let Some(base) = old_base {
            flow.enqueue(Navigation::Next, base, old.len(), ReadingMode::Single);
        }
        assert!(flow.is_idle());
        let new: Vec<PathBuf> = ["Y/B.png", "Y/next.png"].map(PathBuf::from).into();
        let base = selected_index(Some(&selected), &new).unwrap();
        assert!(flow.enqueue(Navigation::Next, base, new.len(), ReadingMode::Single));
        let ticket = flow.start_next(1).unwrap();
        assert_eq!(new[ticket.index], PathBuf::from("Y/next.png"));
    }

    #[test]
    fn navigation_context_preserves_same_directory_pending_and_listed_corrupt_selection() {
        let siblings: Vec<PathBuf> = ["X/A.png", "X/B.png", "X/C.png", "X/corrupt.png"]
            .map(PathBuf::from)
            .into();
        let selected = &siblings[1];
        let base = selected_index(Some(selected), &siblings).unwrap();
        let mut flow = NavigationFlow::default();
        for action in [Navigation::Next, Navigation::Previous, Navigation::Next] {
            assert!(flow.enqueue(action, base, siblings.len(), ReadingMode::Single));
        }
        let mut seen = Vec::new();
        for revision in 1..=3 {
            let ticket = flow.start_next(revision).unwrap();
            seen.push(ticket.index);
            flow.painted(revision).unwrap();
            assert!(flow.crossed_frame(ticket));
        }
        assert_eq!(seen, vec![2, 1, 2]);
        let corrupt_base = selected_index(Some(&siblings[3]), &siblings).unwrap();
        assert!(flow.enqueue(
            Navigation::Previous,
            corrupt_base,
            siblings.len(),
            ReadingMode::Single
        ));
        assert_eq!(flow.start_next(4).unwrap().index, 2);
    }

    #[test]
    fn only_current_success_notice_expires() {
        let mut feedback = Feedback::default();
        let old = feedback.replace(FeedbackKind::Success, "Saved".into());
        let progress = feedback.replace(FeedbackKind::Progress, "Saving".into());
        assert!(!feedback.expire(old));
        assert!(!feedback.expire(progress));
        let error = feedback.replace(FeedbackKind::Error, "Not saved".into());
        assert!(!feedback.expire(old));
        assert!(!feedback.expire(error));
        feedback.dismiss();
        let new = feedback.replace(FeedbackKind::Success, "Saved again".into());
        assert!(!feedback.expire(old));
        assert!(feedback.expire(new));
        assert!(feedback.notice.is_none());
    }

    #[test]
    fn restore_changes_only_draft_until_apply_and_cancel_keeps_active() {
        let mut active = AppSettings::default();
        active.prefetch.before = 2;
        active.thumbnails_visible = false;
        let original = active.clone();
        {
            let mut draft = active.clone();
            restore_defaults(&mut draft);
            assert_eq!(draft, AppSettings::default());
        }
        assert_eq!(active, original);
        let mut draft = active.clone();
        restore_defaults(&mut draft);
        active = draft;
        assert_eq!(active, AppSettings::default());
    }

    #[test]
    fn every_accepted_target_waits_for_paint_and_frame_boundary() {
        let mut flow = NavigationFlow::default();
        for _ in 0..100 {
            assert!(flow.enqueue(Navigation::Next, 0, 101, ReadingMode::Single));
        }
        let mut seen = Vec::new();
        for revision in 1..=100 {
            let ticket = flow.start_next(revision).unwrap();
            seen.push(ticket.index);
            assert!(flow.start_next(revision + 1).is_none());
            assert!(!flow.crossed_frame(ticket));
            assert_eq!(flow.painted(revision), Some(ticket));
            assert!(flow.start_next(revision + 1).is_none());
            assert!(flow.crossed_frame(ticket));
            assert!(!flow.crossed_frame(ticket));
        }
        assert_eq!(seen, (1..=100).collect::<Vec<_>>());
        assert!(flow.is_idle());
    }

    #[test]
    fn cancellation_rejects_old_frame_callback_and_failures_continue() {
        let mut flow = NavigationFlow::default();
        assert!(flow.enqueue(Navigation::Next, 0, 10, ReadingMode::Single));
        let old = flow.start_next(1).unwrap();
        flow.painted(1).unwrap();
        flow.clear();
        assert!(flow.enqueue(Navigation::Next, 3, 10, ReadingMode::Single));
        assert!(flow.enqueue(Navigation::Next, 3, 10, ReadingMode::Single));
        let new = flow.start_next(2).unwrap();
        assert!(!flow.crossed_frame(old));
        assert!(!flow.failed(1));
        assert!(flow.failed(2));
        let following = flow.start_next(3).unwrap();
        assert_eq!((new.index, following.index), (4, 5));
        flow.painted(3).unwrap();
        assert!(flow.crossed_frame(following));
        assert!(flow.is_idle());
        assert!(flow.enqueue(Navigation::Next, 0, 10, ReadingMode::Single));
        let old = flow.start_next(4).unwrap();
        flow.painted(4).unwrap();
        flow.clear();
        assert!(flow.enqueue(Navigation::Next, 0, 10, ReadingMode::Single));
        let replacement = flow.start_next(4).unwrap();
        flow.painted(4).unwrap();
        assert!(!flow.crossed_frame(old));
        assert!(flow.crossed_frame(replacement));
    }

    #[test]
    fn file_jumps_clamp_and_preserve_mixed_input_order() {
        let mut flow = NavigationFlow::default();
        for action in [
            Navigation::Next,
            Navigation::JumpFiles(10),
            Navigation::JumpFiles(10),
            Navigation::JumpFiles(-10),
            Navigation::Previous,
            Navigation::JumpFiles(-10),
        ] {
            assert!(flow.enqueue(action, 0, 16, ReadingMode::Single));
        }
        assert!(!flow.enqueue(Navigation::JumpFiles(-10), 0, 16, ReadingMode::Single));
        let mut seen = Vec::new();
        for revision in 1..=6 {
            let ticket = flow.start_next(revision).unwrap();
            seen.push(ticket.index);
            flow.painted(revision).unwrap();
            assert!(flow.crossed_frame(ticket));
        }
        assert_eq!(seen, vec![1, 11, 15, 5, 4, 0]);
        assert!(flow.is_idle());
    }
}
