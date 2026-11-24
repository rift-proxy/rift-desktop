//! Servers that did not answer, and when each of them is asked again.
//!
//! One list, no degrees. A server that failed to answer twice, with no answer between, is put in
//! it and is left out of the cycles of selection (the first miss only makes the next one count: a
//! single lost answer is noise); it is asked again alone, after a wait of its own that grows with
//! every failure in a row: five minutes, ten, fifteen, up to an hour, and an hour from then on. One
//! answer takes the server out of the list and forgets the misses. Memory only: a new connection
//! starts with no list.
//!
//! Measuring a server that has not answered for days, again and again, serves nothing and shows
//! the observer one more address of the subscription each time.
use crate::jitter::Jitter;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio::time::Instant;

/// The wait grows by this much with every failure in a row...
pub const STEP: Duration = Duration::from_secs(5 * 60);
/// ...up to this much, which is also the wait of a server that has not answered for hours.
pub const LONGEST: Duration = Duration::from_secs(60 * 60);
/// Misses in a row, with no answer between, that put a server in the list.
pub const MISSES_TO_LIST: u32 = 2;

/// How long to wait after the `failures`-th failure in a row (the periodic spread comes on top).
pub fn wait(failures: u32) -> Duration {
    STEP.saturating_mul(failures.max(1)).min(LONGEST)
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    /// Misses in a row; the list starts at [`MISSES_TO_LIST`].
    misses: u32,
    /// When the server is asked again; none while it is not in the list.
    next_at: Option<Instant>,
}
#[derive(Debug, Default)]
pub struct Dead {
    entries: HashMap<String, Entry>,
}
impl Dead {
    pub fn new() -> Self {
        Self::default()
    }
    /// The server did not answer. The second miss in a row puts it in the list, and every later one
    /// makes the wait before the next question longer than the last.
    pub fn failed(&mut self, id: &str, now: Instant, jitter: &Jitter) {
        let entry = self.entries.entry(id.to_owned()).or_insert(Entry {
            misses: 0,
            next_at: None,
        });
        entry.misses = entry.misses.saturating_add(1);
        if entry.misses >= MISSES_TO_LIST {
            let failures = entry.misses - MISSES_TO_LIST + 1;
            entry.next_at = Some(now + jitter.spread(wait(failures)));
        }
    }
    /// The server answered: it is out of the list and its misses are forgotten.
    pub fn answered(&mut self, id: &str) {
        self.entries.remove(id);
    }
    /// Asking the servers could not be done (no network): the same wait again, no new failure.
    pub fn postponed(&mut self, ids: &[String], now: Instant, jitter: &Jitter) {
        for id in ids {
            if let Some(entry) = self.entries.get_mut(id) {
                if entry.next_at.is_some() {
                    let failures = entry.misses - MISSES_TO_LIST + 1;
                    entry.next_at = Some(now + jitter.spread(wait(failures)));
                }
            }
        }
    }
    /// Whether the server is in the list (a single miss is not enough).
    pub fn contains(&self, id: &str) -> bool {
        self.next_at(id).is_some()
    }
    /// Misses in a row of a server, in the list or not.
    pub fn misses(&self, id: &str) -> u32 {
        self.entries.get(id).map_or(0, |entry| entry.misses)
    }
    /// Failures in a row of a server in the list; none for one that is not.
    pub fn failures(&self, id: &str) -> u32 {
        if self.contains(id) {
            self.misses(id) - MISSES_TO_LIST + 1
        } else {
            0
        }
    }
    /// When the server is asked again, if it is in the list.
    pub fn next_at(&self, id: &str) -> Option<Instant> {
        self.entries.get(id).and_then(|entry| entry.next_at)
    }
    /// The servers of the list whose time has come, the longest waiting first.
    pub fn due(&self, now: Instant) -> Vec<String> {
        let mut due: Vec<(&String, Instant)> = self
            .entries
            .iter()
            .filter_map(|(id, entry)| entry.next_at.map(|at| (id, at)))
            .filter(|(_, at)| *at <= now)
            .collect();
        due.sort_by(|(a, at_a), (b, at_b)| at_a.cmp(at_b).then_with(|| a.cmp(b)));
        due.into_iter().map(|(id, _)| id.clone()).collect()
    }
    /// Forgets the servers that are not in `known` any more (removed, or their subscription is).
    pub fn keep_only(&mut self, known: &HashSet<&str>) {
        self.entries.retain(|id, _| known.contains(id.as_str()));
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    /// Servers in the list.
    pub fn len(&self) -> usize {
        self.entries
            .values()
            .filter(|e| e.next_at.is_some())
            .count()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
