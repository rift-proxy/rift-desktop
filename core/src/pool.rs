//! The servers kept ready between full checks, and how a fast check judges them.
//!
//! Everything here is a pure function of numbers: what the probes found is passed in, what to do
//! about it comes out. The checks themselves (`selection`) and the clock (`Schedule`) stay where
//! they are.
use crate::{model::Settings, selection::median};
use std::collections::HashSet;

/// A server of the last full check that answered reliably, with the middle of its answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ranked {
    pub id: String,
    pub host: String,
    pub ms: u64,
}

/// The first `size` servers of `ranking` (fastest first), at most one per host: three rows of one
/// machine are not three reserves, they fall together.
pub fn build(ranking: &[Ranked], size: usize) -> Vec<Ranked> {
    let mut hosts = HashSet::new();
    let mut pool = Vec::with_capacity(size.min(ranking.len()));
    for server in ranking {
        if pool.len() == size {
            break;
        }
        if hosts.insert(server.host.to_lowercase()) {
            pool.push(server.clone());
        }
    }
    pool
}

/// How many of the fastest servers by their first answer are measured again before the choice.
/// `hosts` are their hosts in that order. At least five, and the pool size and two more, because the
/// pool is chosen among the steady ones; more while the ones taken do not yet hold as many hosts as
/// the pool wants, but never twice as many as that: a list that crowds one machine with rows must
/// not make every server of it be measured three times.
pub fn refine_count(hosts: &[String], pool_size: usize) -> usize {
    let base = 5.max(pool_size + 2);
    let most = hosts.len().min(2 * base);
    let mut count = hosts.len().min(base);
    let distinct = |count: usize| {
        hosts[..count]
            .iter()
            .map(|host| host.to_lowercase())
            .collect::<HashSet<_>>()
            .len()
    };
    while count < most && distinct(count) < pool_size {
        count += 1;
    }
    count
}

/// How far a delay may grow before the server counts as worse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tolerance {
    /// Growth by this share (percent) of the delay the server had when the pool was built...
    pub percent: u64,
    /// ...but never less than this many milliseconds: a small delay shakes by more than its share...
    pub floor_ms: u64,
    /// ...and the share never lifts the limit above this many milliseconds in total, whatever it
    /// says; the floor can (a base above this has `base + floor`).
    pub cap_ms: u64,
}

impl Tolerance {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            percent: settings.degrade_percent,
            floor_ms: settings.switch_threshold_ms,
            cap_ms: settings.degrade_cap_ms,
        }
    }
    /// The slowest answer still counted as the old one:
    /// `max(base + floor, min(base · (1 + percent / 100), cap))`.
    pub fn limit(&self, base_ms: u64) -> u64 {
        let grown = u128::from(base_ms) * u128::from(100 + self.percent) / 100;
        let grown = u64::try_from(grown).unwrap_or(u64::MAX).min(self.cap_ms);
        grown.max(base_ms.saturating_add(self.floor_ms))
    }
    pub fn exceeded(&self, base_ms: u64, ms: u64) -> bool {
        ms > self.limit(base_ms)
    }
}

/// What a fast check made of one server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Answers as it did.
    Fine,
    /// Answers, but slower than the tolerance allows: the middle of its answers and the limit.
    Worse { ms: u64, limit_ms: u64 },
    /// Does not answer.
    Lost,
}

/// A verdict and the middle of the answers it rests on. The middle is the new delay to compare with
/// when the server had none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Judgement {
    pub health: Health,
    pub median_ms: Option<u64>,
}

/// Whether the first answer of a fast check is enough: only an answer within the tolerance of a
/// known delay is. Silence, a slower answer and a server with nothing to compare with are measured
/// twice more, because one answer is noise.
pub fn needs_confirmation(base_ms: Option<u64>, first: Option<u64>, tolerance: Tolerance) -> bool {
    match (base_ms, first) {
        (Some(base), Some(ms)) => tolerance.exceeded(base, ms),
        _ => true,
    }
}

/// The verdict on a server from its answers (`None` is a miss): the first one alone when it needed
/// no confirmation, otherwise the three of them. The middle of the answers decides, as it does when
/// servers are compared: two answers count by the worse of them, fewer than two mean lost.
pub fn judge(base_ms: Option<u64>, samples: &[Option<u64>], tolerance: Tolerance) -> Judgement {
    if let [Some(ms)] = *samples {
        if !needs_confirmation(base_ms, Some(ms), tolerance) {
            return Judgement {
                health: Health::Fine,
                median_ms: Some(ms),
            };
        }
    }
    let Some(middle) = median(samples.iter().flatten().copied().collect()) else {
        return Judgement {
            health: Health::Lost,
            median_ms: None,
        };
    };
    let health = match base_ms {
        Some(base) if tolerance.exceeded(base, middle) => Health::Worse {
            ms: middle,
            limit_ms: tolerance.limit(base),
        },
        _ => Health::Fine,
    };
    Judgement {
        health,
        median_ms: Some(middle),
    }
}
