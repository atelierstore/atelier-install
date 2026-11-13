//! The work the test does, and how much of it each second saw.

use std::collections::VecDeque;

/// The steps in one piece of work: a fraction of a millisecond's worth, so
/// the clock is read often enough to see a pause where it happens.
pub const PIECE: u64 = 100_000;

/// How many past seconds the test keeps, for the page's chart.
pub const KEPT: usize = 120;

/// Work that only needs the processor, as hashing an app's files does when
/// Atelier's installer signs it.
pub struct Work(u64);

impl Work {
    pub fn new(seed: u64) -> Self {
        Work(seed | 1)
    }

    /// Does one piece, and says how many steps that was.
    pub fn piece(&mut self) -> u64 {
        let mut x = self.0;
        for _ in 0..PIECE {
            x ^= x >> 33;
            x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
            x ^= x >> 29;
            x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        }
        self.0 = std::hint::black_box(x);
        PIECE
    }
}

/// How many steps each second saw, counted from the test's start.
pub struct History {
    started_ms: u64,
    /// The second being counted, 0 being the first, and its steps so far.
    second: u64,
    steps: u64,
    /// The seconds before it, newest last, `KEPT` at most.
    past: VecDeque<u64>,
    /// How many seconds are over, and the most steps one of them saw.
    over: u64,
    best: u64,
}

impl History {
    pub fn starting_at(now_ms: u64) -> Self {
        History {
            started_ms: now_ms,
            second: 0,
            steps: 0,
            past: VecDeque::with_capacity(KEPT),
            over: 0,
            best: 0,
        }
    }

    /// Counts `steps` done by `now_ms`. Seconds that passed with nothing
    /// counted saw no steps: the program wasn't running.
    pub fn add(&mut self, now_ms: u64, steps: u64) {
        let second = now_ms.saturating_sub(self.started_ms) / 1000;
        if second > self.second {
            self.end_second(self.steps);
            let empty = second - self.second - 1;
            for _ in 0..empty.min(KEPT as u64) {
                self.end_second(0);
            }
            self.over += empty.saturating_sub(KEPT as u64);
            self.second = second;
            self.steps = 0;
        }
        self.steps += steps;
    }

    fn end_second(&mut self, steps: u64) {
        if self.past.len() == KEPT {
            self.past.pop_front();
        }
        self.past.push_back(steps);
        self.over += 1;
        self.best = self.best.max(steps);
    }

    /// How many seconds are over.
    pub fn seconds(&self) -> u64 {
        self.over
    }

    /// The most steps one second saw.
    pub fn best(&self) -> u64 {
        self.best
    }

    /// `steps` in a second as a share of the best second's, 0 to 100.
    pub fn percent(&self, steps: u64) -> u8 {
        if self.best == 0 {
            return 0;
        }
        ((steps.saturating_mul(100) + self.best / 2) / self.best).min(100) as u8
    }

    /// The steps a second saw on average over the last `seconds` that are
    /// over, or over as many as there are. None before the first is over.
    pub fn lately(&self, seconds: usize) -> Option<u64> {
        let seconds = seconds.min(self.past.len());
        if seconds == 0 {
            return None;
        }
        let sum: u64 = self.past.iter().rev().take(seconds).sum();
        Some(sum / seconds as u64)
    }

    /// Each kept second as a share of the best one, newest last.
    pub fn chart(&self) -> Vec<u8> {
        self.past.iter().map(|&steps| self.percent(steps)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piece_of_work_changes_its_state() {
        let mut work = Work::new(7);
        assert_eq!(work.piece(), PIECE);
        let after_one = work.0;
        work.piece();
        assert_ne!(work.0, after_one);
    }

    #[test]
    fn counts_each_second_from_the_start() {
        let mut history = History::starting_at(10_000);
        assert_eq!(history.lately(3), None);
        history.add(10_100, 400);
        history.add(10_900, 600);
        // The first second is over when something is counted in the next.
        assert_eq!(history.seconds(), 0);
        history.add(11_000, 500);
        assert_eq!(history.seconds(), 1);
        assert_eq!(history.best(), 1_000);
        history.add(12_500, 250);
        assert_eq!(history.chart(), [100, 50]);
        assert_eq!(history.lately(1), Some(500));
        assert_eq!(history.lately(3), Some(750));
    }

    #[test]
    fn seconds_with_nothing_counted_saw_no_steps() {
        let mut history = History::starting_at(0);
        history.add(500, 1_000);
        // Nothing for five seconds: the program was stopped.
        history.add(6_200, 10);
        assert_eq!(history.seconds(), 6);
        assert_eq!(history.chart(), [100, 0, 0, 0, 0, 0]);
        assert_eq!(history.lately(5), Some(0));
    }

    #[test]
    fn keeps_the_last_two_minutes_through_a_long_stop() {
        let mut history = History::starting_at(0);
        history.add(500, 1_000);
        history.add(3_600_000, 10);
        assert_eq!(history.seconds(), 3_600);
        assert_eq!(history.chart().len(), KEPT);
        assert!(history.chart().iter().all(|&share| share == 0));
        assert_eq!(history.best(), 1_000);
    }

    #[test]
    fn a_clock_that_goes_back_counts_in_the_same_second() {
        let mut history = History::starting_at(5_000);
        history.add(5_500, 100);
        history.add(4_000, 100);
        assert_eq!(history.seconds(), 0);
        history.add(6_000, 1);
        assert_eq!(history.best(), 200);
    }

    #[test]
    fn a_share_of_the_best_second_stays_within_100() {
        let mut history = History::starting_at(0);
        assert_eq!(history.percent(5), 0);
        history.add(100, 200);
        history.add(1_100, 1);
        assert_eq!(history.percent(100), 50);
        assert_eq!(history.percent(199), 100);
        assert_eq!(history.percent(1), 1);
        assert_eq!(history.percent(400), 100);
    }
}
