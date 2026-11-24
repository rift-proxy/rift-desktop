//! Spread of periodic intervals, and the other chance that keeps the checks of one client from
//! looking alike.
//!
//! Every periodic timer (selection, subscription refresh and its retries, update checks)
//! waits its interval plus a random offset of up to [`SPREAD`] either way, never less than
//! [`FLOOR`]: fixed intervals make many clients hit a provider at the same moment and make
//! the checks of one client easy to recognise. Deadlines of single requests (timeouts) are
//! limits, not schedules, and are not spread.
//!
//! The same source gives the random pause between the measurements of two different servers
//! ([`Jitter::pause`]) and a random order of the servers ([`Jitter::shuffle`]): a burst of
//! connections to every server of a subscription, in the order of the subscription, at the same
//! interval, is what makes the list of servers easy to read from outside.
use std::{sync::Arc, time::Duration};

/// How far a periodic interval may move in either direction.
pub const SPREAD: Duration = Duration::from_secs(10);
/// The shortest wait a spread interval can produce.
pub const FLOOR: Duration = Duration::from_secs(1);

const SPREAD_MS: i64 = SPREAD.as_millis() as i64;

/// The shortest pause between the measurements of two different servers.
pub const PAUSE_MIN: Duration = Duration::from_millis(100);
/// The longest one.
pub const PAUSE_MAX: Duration = Duration::from_secs(5);
/// What the pauses of one pass over many servers add up to, on average, at most: with hundreds of
/// servers the longest pause shrinks, so a subscription of two thousand servers is not measured
/// for hours.
pub const PAUSE_BUDGET: Duration = Duration::from_secs(120);

/// Where the offset in milliseconds, within `-SPREAD..=SPREAD`, comes from.
pub trait Source: Send + Sync {
    fn offset_ms(&self) -> i64;
    /// A pause in milliseconds within `min..=max`. A source that says nothing gives no pause.
    fn pause_ms(&self, _min: u64, _max: u64) -> u64 {
        0
    }
    /// The key a server is sorted by to put the servers in random order. Equal keys keep the order
    /// the servers came in, so a source that says nothing leaves the order alone.
    fn order_key(&self) -> u64 {
        0
    }
    /// Whether `order_key` is worth asking for at all; a source that leaves the order alone says no
    /// and the servers are not even sorted.
    fn orders(&self) -> bool {
        false
    }
}

/// Operating-system randomness (the random bits of a v4 UUID), uniform over the whole range.
struct Os;
impl Source for Os {
    fn offset_ms(&self) -> i64 {
        let id = uuid::Uuid::new_v4();
        let bytes = id.as_bytes();
        let raw = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        i64::from(raw % (2 * SPREAD_MS as u32 + 1)) - SPREAD_MS
    }
    fn pause_ms(&self, min: u64, max: u64) -> u64 {
        let id = uuid::Uuid::new_v4();
        let bytes = id.as_bytes();
        let raw = u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        min + raw % (max.saturating_sub(min) + 1)
    }
    fn order_key(&self) -> u64 {
        let id = uuid::Uuid::new_v4();
        let bytes = id.as_bytes();
        u64::from_be_bytes([
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ])
    }
    fn orders(&self) -> bool {
        true
    }
}

struct Fixed(i64);

/// The same pause every time, whatever the bounds, in the order the servers came in.
struct Pausing(u64);
impl Source for Pausing {
    fn offset_ms(&self) -> i64 {
        0
    }
    fn pause_ms(&self, _min: u64, _max: u64) -> u64 {
        self.0
    }
}
impl Source for Fixed {
    fn offset_ms(&self) -> i64 {
        self.0
    }
}

#[derive(Clone)]
pub struct Jitter(Arc<dyn Source>);
impl Jitter {
    /// Production: a fresh uniform offset for every deadline.
    pub fn random() -> Self {
        Self(Arc::new(Os))
    }
    /// No spread: exact intervals. For tests that assert exact deadlines.
    pub fn none() -> Self {
        Self::fixed(0)
    }
    /// The same offset (clamped to the spread) every time. For deterministic tests.
    pub fn fixed(offset_ms: i64) -> Self {
        Self(Arc::new(Fixed(offset_ms)))
    }
    /// No spread and the same pause between servers every time, in the order they came in. For
    /// tests that count the pauses.
    pub fn pausing(pause: Duration) -> Self {
        Self(Arc::new(Pausing(pause.as_millis() as u64)))
    }
    /// Any source of offsets, for tests that need a scripted sequence.
    pub fn with_source(source: Arc<dyn Source>) -> Self {
        Self(source)
    }
    /// `base` moved by a fresh offset: within `base ± SPREAD`, at least [`FLOOR`].
    pub fn spread(&self, base: Duration) -> Duration {
        apply(base, self.0.offset_ms())
    }
    /// The wait before the measurement of the next of `servers` servers begins: between
    /// [`PAUSE_MIN`] and [`PAUSE_MAX`], and short enough for a pass over all of them to take about
    /// max([`PAUSE_BUDGET`], servers × [`PAUSE_MIN`]) in pauses on average. Zero without a source that gives pauses.
    pub fn pause(&self, servers: usize) -> Duration {
        let min = PAUSE_MIN.as_millis() as u64;
        let longest = PAUSE_MAX.as_millis() as u64;
        let share =
            ((2 * PAUSE_BUDGET.as_millis() as u64) / servers.max(1) as u64).saturating_sub(min);
        Duration::from_millis(self.0.pause_ms(min, share.clamp(min, longest)))
    }
    /// `items` in a fresh random order (the order they came in, without a source that gives one).
    pub fn shuffle<T>(&self, items: &mut [T]) {
        if self.0.orders() {
            items.sort_by_cached_key(|_| self.0.order_key());
        }
    }
}
impl std::fmt::Debug for Jitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Jitter")
    }
}
impl Default for Jitter {
    fn default() -> Self {
        Self::random()
    }
}

/// `base + offset_ms` with the offset clamped to `±SPREAD`, never below [`FLOOR`].
pub fn apply(base: Duration, offset_ms: i64) -> Duration {
    let offset = i128::from(offset_ms.clamp(-SPREAD_MS, SPREAD_MS));
    let millis = (base.as_millis() as i128 + offset).max(FLOOR.as_millis() as i128);
    Duration::from_millis(u64::try_from(millis).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    struct Maximum;
    impl Source for Maximum {
        fn offset_ms(&self) -> i64 {
            0
        }
        fn pause_ms(&self, _min: u64, max: u64) -> u64 {
            max
        }
    }
    #[test]
    fn the_average_pause_budget_accounts_for_the_required_minimum() {
        let jitter = Jitter::with_source(Arc::new(Maximum));
        for count in [1, 10, 100, 1000, 1200, 2000, 5000] {
            let maximum = jitter.pause(count).as_millis();
            let minimum = PAUSE_MIN.as_millis();
            assert!((minimum..=PAUSE_MAX.as_millis()).contains(&maximum));
            let budget = PAUSE_BUDGET.as_millis().max(count as u128 * minimum);
            assert!(
                (minimum + maximum) * count as u128 <= 2 * budget,
                "{count} servers exceed the mean budget"
            );
        }
    }
}
