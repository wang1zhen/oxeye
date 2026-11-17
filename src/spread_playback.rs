//! Independent GIF clocks for up to two pages in a Book spread.
//!
//! Pure coordination only: no window, texture, or framework types. Each page
//! owns its own [`Playback`]; this module reports frames, shared deadlines, and
//! pause/resume/restart policy for the group.

use std::time::Duration;

use crate::model::LoopMode;
use crate::viewer::Playback;

/// GIF timing for one visible page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageTiming {
    pub frame_count: usize,
    pub delays: Vec<Duration>,
    pub loop_mode: LoopMode,
}

struct Page {
    timing: Option<PageTiming>,
    playback: Option<Playback>,
}

/// Playback state for the one or two pages visible in a spread.
pub struct SpreadPlayback {
    pages: Vec<Page>,
}

impl SpreadPlayback {
    /// Builds clocks for each visible page.
    ///
    /// `Some(timing)` with more than one frame and at least one delay is
    /// animated; everything else is a static page with no clock.
    pub fn new(pages: impl IntoIterator<Item = Option<PageTiming>>, now: Duration) -> Self {
        let pages = pages
            .into_iter()
            .map(|timing| match timing {
                Some(timing) if timing.frame_count > 1 && !timing.delays.is_empty() => {
                    debug_assert_eq!(
                        timing.frame_count,
                        timing.delays.len(),
                        "animated timing must have one delay per frame"
                    );
                    let playback =
                        Playback::new(timing.delays.iter().copied(), timing.loop_mode, now);
                    Page {
                        timing: Some(timing),
                        playback: Some(playback),
                    }
                }
                _ => Page {
                    timing: None,
                    playback: None,
                },
            })
            .collect();
        Self { pages }
    }

    /// Number of visible pages.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Current frame for `page`; invalid indices use page 0.
    pub fn frame(&self, page: usize, now: Duration) -> usize {
        self.pages
            .get(page)
            .or_else(|| self.pages.first())
            .and_then(|page| page.playback.as_ref())
            .map_or(0, |playback| playback.position(now).frame)
    }

    /// Current frames for every visible page.
    pub fn targets(&self, now: Duration) -> Vec<usize> {
        (0..self.pages.len())
            .map(|page| self.frame(page, now))
            .collect()
    }

    /// Earliest next-frame deadline among running clocks.
    pub fn next_deadline(&self, now: Duration) -> Option<Duration> {
        self.pages
            .iter()
            .filter_map(|page| page.playback.as_ref())
            .filter_map(|playback| playback.position(now).next)
            .min()
    }

    /// Whether any animated page is currently playing.
    pub fn any_playing(&self, now: Duration) -> bool {
        self.pages
            .iter()
            .filter_map(|page| page.playback.as_ref())
            .any(|playback| playback.playing(now))
    }

    /// Whether every animated page is finished (true when none are animated).
    pub fn all_finished(&self, now: Duration) -> bool {
        self.pages
            .iter()
            .filter_map(|page| page.playback.as_ref())
            .all(|playback| playback.position(now).finished)
    }

    /// Pauses every clock at `now`; finished clocks keep their last frame.
    pub fn pause(&mut self, now: Duration) {
        for playback in self
            .pages
            .iter_mut()
            .filter_map(|page| page.playback.as_mut())
        {
            playback.pause(now);
        }
    }

    /// Resumes only clocks that have not finished.
    ///
    /// Finished clocks stay on their last frame. Use [`Self::restart_all`] for
    /// an explicit replay.
    pub fn resume(&mut self, now: Duration) {
        for playback in self
            .pages
            .iter_mut()
            .filter_map(|page| page.playback.as_mut())
        {
            if !playback.position(now).finished {
                playback.play(now);
            }
        }
    }

    /// Recreates every animated clock from frame 0 at `now`.
    pub fn restart_all(&mut self, now: Duration) {
        for page in &mut self.pages {
            if let Some(timing) = &page.timing {
                page.playback = Some(Playback::new(
                    timing.delays.iter().copied(),
                    timing.loop_mode,
                    now,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(delays: &[u64], loop_mode: LoopMode) -> PageTiming {
        PageTiming {
            frame_count: delays.len(),
            delays: delays.iter().copied().map(Duration::from_millis).collect(),
            loop_mode,
        }
    }

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn reports_frames_and_earliest_deadline_for_different_delays() {
        let spread = SpreadPlayback::new(
            [
                Some(timing(&[20, 30, 50], LoopMode::Infinite)),
                Some(timing(&[100, 100], LoopMode::Infinite)),
            ],
            Duration::ZERO,
        );

        assert_eq!(spread.targets(Duration::ZERO), [0, 0]);
        assert_eq!(spread.next_deadline(Duration::ZERO), Some(ms(20)));

        assert_eq!(spread.frame(0, ms(20)), 1);
        assert_eq!(spread.frame(1, ms(20)), 0);
        assert_eq!(spread.next_deadline(ms(20)), Some(ms(50)));

        assert_eq!(spread.frame(0, ms(50)), 2);
        assert_eq!(spread.targets(ms(50)), [2, 0]);
        assert_eq!(spread.next_deadline(ms(50)), Some(ms(100)));

        assert_eq!(spread.targets(ms(100)), [0, 1]);
        assert_eq!(spread.next_deadline(ms(100)), Some(ms(120)));
    }

    #[test]
    fn finite_page_finishes_while_infinite_partner_continues() {
        let spread = SpreadPlayback::new(
            [
                Some(timing(&[20, 30, 50], LoopMode::Once)),
                Some(timing(&[100, 100], LoopMode::Infinite)),
            ],
            Duration::ZERO,
        );

        assert_eq!(spread.targets(ms(99)), [2, 0]);
        assert_eq!(spread.targets(ms(100)), [2, 1]);
        assert_eq!(
            spread.frame(0, ms(500)),
            2,
            "finished page keeps last frame"
        );
        assert!(spread.any_playing(ms(100)));
        assert!(!spread.all_finished(ms(100)));
        assert_eq!(spread.next_deadline(ms(100)), Some(ms(200)));
        assert_eq!(spread.next_deadline(ms(300)), Some(ms(400)));
    }

    #[test]
    fn finite_one_repeats_then_finishes_while_infinite_partner_continues() {
        let finite = SpreadPlayback::new(
            [Some(timing(&[20, 30, 50], LoopMode::Finite(1)))],
            Duration::ZERO,
        );
        assert_eq!(finite.targets(ms(99)), [2]);
        assert!(!finite.all_finished(ms(199)));
        assert_eq!(finite.targets(ms(200)), [2], "last frame retained");
        assert!(finite.all_finished(ms(200)));
        assert_eq!(finite.next_deadline(ms(200)), None);

        let paired = SpreadPlayback::new(
            [
                Some(timing(&[20, 30, 50], LoopMode::Finite(1))),
                Some(timing(&[100, 100], LoopMode::Infinite)),
            ],
            Duration::ZERO,
        );
        assert_eq!(paired.targets(Duration::ZERO), [0, 0]);
        assert_eq!(paired.targets(ms(100)), [0, 1], "second cycle restarts A");
        assert_eq!(paired.targets(ms(120)), [1, 1]);
        assert_eq!(paired.targets(ms(150)), [2, 1]);
        assert_eq!(paired.targets(ms(200)), [2, 0], "A finished; B continues");
        assert_eq!(paired.frame(0, ms(250)), 2, "finished A keeps last frame");
        assert!(paired.any_playing(ms(200)));
        assert!(!paired.all_finished(ms(200)));
        assert_eq!(paired.next_deadline(ms(200)), Some(ms(300)));
    }

    #[test]
    fn pause_and_resume_preserve_positions_without_restarting_finished_page() {
        let mut spread = SpreadPlayback::new(
            [
                Some(timing(&[20, 30, 50], LoopMode::Once)),
                Some(timing(&[100, 100], LoopMode::Infinite)),
            ],
            Duration::ZERO,
        );

        spread.pause(ms(115));
        assert_eq!(spread.targets(ms(500)), [2, 1]);
        assert!(!spread.any_playing(ms(500)));
        assert_eq!(spread.next_deadline(ms(500)), None);

        spread.resume(ms(500));
        assert_eq!(
            spread.targets(ms(500)),
            [2, 1],
            "finished A stays last; B keeps elapsed"
        );
        assert!(spread.any_playing(ms(500)));
        assert_eq!(spread.next_deadline(ms(500)), Some(ms(585)));
        assert_eq!(spread.frame(0, ms(585)), 2, "A was not restarted");
        assert_eq!(spread.frame(1, ms(585)), 0);
    }

    #[test]
    fn all_finished_then_restart_resets_both_pages() {
        let mut spread = SpreadPlayback::new(
            [
                Some(timing(&[20, 30, 50], LoopMode::Once)),
                Some(timing(&[100, 100], LoopMode::Once)),
            ],
            Duration::ZERO,
        );

        assert_eq!(spread.targets(ms(250)), [2, 1]);
        assert!(spread.all_finished(ms(250)));
        assert!(!spread.any_playing(ms(250)));
        assert_eq!(spread.next_deadline(ms(250)), None);

        spread.restart_all(ms(250));
        assert_eq!(spread.targets(ms(250)), [0, 0]);
        assert!(spread.any_playing(ms(250)));
        assert!(!spread.all_finished(ms(250)));
        assert_eq!(spread.next_deadline(ms(250)), Some(ms(270)));
        assert_eq!(spread.frame(0, ms(270)), 1);
        assert_eq!(spread.frame(1, ms(270)), 0);
    }

    #[test]
    fn static_page_has_no_clock_and_does_not_block_finished() {
        let spread = SpreadPlayback::new(
            [None, Some(timing(&[100, 100], LoopMode::Once))],
            Duration::ZERO,
        );
        assert_eq!(spread.frame(0, Duration::ZERO), 0);
        assert_eq!(spread.targets(ms(50)), [0, 0]);
        assert_eq!(spread.next_deadline(ms(50)), Some(ms(100)));
        assert!(!spread.all_finished(ms(50)));
        assert_eq!(spread.targets(ms(500)), [0, 1]);
        assert!(!spread.any_playing(ms(500)));
        assert!(spread.all_finished(ms(500)));
        assert_eq!(spread.next_deadline(ms(500)), None);

        let static_timings = SpreadPlayback::new(
            [
                Some(PageTiming {
                    frame_count: 1,
                    delays: vec![ms(100)],
                    loop_mode: LoopMode::Infinite,
                }),
                Some(PageTiming {
                    frame_count: 2,
                    delays: Vec::new(),
                    loop_mode: LoopMode::Infinite,
                }),
            ],
            Duration::ZERO,
        );
        assert_eq!(static_timings.targets(ms(1_000)), [0, 0]);
        assert_eq!(static_timings.next_deadline(ms(1_000)), None);
        assert!(!static_timings.any_playing(ms(1_000)));
        assert!(static_timings.all_finished(ms(1_000)));
    }

    #[test]
    fn zero_delays_use_playback_normalization_without_same_instant_deadline() {
        let spread =
            SpreadPlayback::new([Some(timing(&[0, 0], LoopMode::Infinite))], Duration::ZERO);
        assert_eq!(spread.frame(0, Duration::ZERO), 0);
        assert_eq!(spread.next_deadline(Duration::ZERO), Some(ms(10)));
        assert_eq!(spread.frame(0, ms(10)), 1);
        assert_eq!(spread.next_deadline(ms(10)), Some(ms(20)));
        assert_eq!(spread.frame(0, ms(20)), 0);
        assert_eq!(spread.next_deadline(ms(20)), Some(ms(30)));
    }

    #[test]
    fn invalid_page_indices_clamp_to_page_zero() {
        let spread = SpreadPlayback::new(
            [
                Some(timing(&[20, 30], LoopMode::Once)),
                Some(timing(&[100], LoopMode::Once)),
            ],
            Duration::ZERO,
        );
        assert_eq!(spread.frame(99, ms(25)), spread.frame(0, ms(25)));
        assert_eq!(spread.page_count(), 2);
        assert_eq!(spread.targets(ms(25)).len(), 2);

        let empty = SpreadPlayback::new(std::iter::empty(), Duration::ZERO);
        assert_eq!(empty.page_count(), 0);
        assert_eq!(empty.frame(7, ms(10)), 0);
        assert!(empty.targets(ms(10)).is_empty());
        assert_eq!(empty.next_deadline(ms(10)), None);
        assert!(!empty.any_playing(ms(10)));
        assert!(empty.all_finished(ms(10)));
    }
}
