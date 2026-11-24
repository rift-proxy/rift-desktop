//! Spread of periodic intervals (`jitter`): bounds, floor and the production source.
use proptest::prelude::*;
use rift_core::jitter::{apply, Jitter, Source, FLOOR, PAUSE_MAX, PAUSE_MIN, SPREAD};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

#[test]
fn offsets_move_the_base_by_at_most_ten_seconds() {
    let base = Duration::from_secs(60);
    assert_eq!(apply(base, -10_000), Duration::from_secs(50));
    assert_eq!(apply(base, 0), base);
    assert_eq!(apply(base, 10_000), Duration::from_secs(70));
    assert_eq!(apply(base, 2_500), Duration::from_millis(62_500));
    // The spread cannot be exceeded by a source that misbehaves.
    assert_eq!(apply(base, 3_600_000), Duration::from_secs(70));
    assert_eq!(apply(base, i64::MIN), Duration::from_secs(50));
}

#[test]
fn the_fifteen_second_check_becomes_five_to_twenty_five() {
    let base = Duration::from_secs(15);
    assert_eq!(apply(base, -10_000), Duration::from_secs(5));
    assert_eq!(apply(base, 10_000), Duration::from_secs(25));
}

#[test]
fn a_short_interval_never_drops_below_the_floor() {
    assert_eq!(apply(Duration::from_secs(5), -10_000), FLOOR);
    assert_eq!(apply(Duration::ZERO, -10_000), FLOOR);
    assert_eq!(apply(Duration::from_secs(11), -10_000), FLOOR);
    assert_eq!(
        apply(Duration::from_secs(12), -10_000),
        Duration::from_secs(2)
    );
}

#[test]
fn no_jitter_keeps_exact_intervals() {
    let jitter = Jitter::none();
    for seconds in [1, 15, 60, 3600] {
        let base = Duration::from_secs(seconds);
        assert_eq!(jitter.spread(base), base);
    }
}

#[test]
fn the_production_source_covers_the_whole_range_and_stays_inside_it() {
    let jitter = Jitter::random();
    let base = Duration::from_secs(60);
    let mut distinct = HashSet::new();
    let (mut low, mut high, mut total) = (u128::MAX, 0u128, 0u128);
    const DRAWS: u128 = 4000;
    for _ in 0..DRAWS {
        let waited = jitter.spread(base);
        assert!(
            waited >= base - SPREAD && waited <= base + SPREAD,
            "{waited:?}"
        );
        distinct.insert(waited);
        low = low.min(waited.as_millis());
        high = high.max(waited.as_millis());
        total += waited.as_millis();
    }
    assert!(
        distinct.len() > 1000,
        "only {} distinct waits",
        distinct.len()
    );
    assert!(low < 52_000, "the lower end is reachable, got {low}");
    assert!(high > 68_000, "the upper end is reachable, got {high}");
    let mean = total / DRAWS;
    assert!(
        (57_000..=63_000).contains(&mean),
        "the offsets are centred, mean {mean}"
    );
}

proptest! {
    #[test]
    fn any_base_and_offset_stay_within_the_spread_and_above_the_floor(
        base_ms in 0u64..=700_000_000,
        offset in any::<i64>(),
    ) {
        let base = Duration::from_millis(base_ms);
        let waited = apply(base, offset);
        prop_assert!(waited >= FLOOR);
        prop_assert!(waited >= base.saturating_sub(SPREAD));
        prop_assert!(waited <= base + SPREAD);
    }
}

/// Remembers the bounds it was asked for and always answers the longest pause.
struct Longest(Mutex<Vec<(u64, u64)>>);
impl Source for Longest {
    fn offset_ms(&self) -> i64 {
        0
    }
    fn pause_ms(&self, min: u64, max: u64) -> u64 {
        self.0.lock().unwrap().push((min, max));
        max
    }
}

#[test]
fn the_longest_pause_shrinks_with_the_number_of_servers_so_a_pass_stays_bounded() {
    let source = Arc::new(Longest(Mutex::new(vec![])));
    let jitter = Jitter::with_source(source.clone());
    // (servers, the longest pause in ms)
    for (servers, longest) in [
        (0, 5_000),
        (1, 5_000),
        (30, 5_000),
        (48, 4_900),
        (49, 4_797),
        (100, 2_300),
        (2_000, 100),
        (100_000, 100),
    ] {
        assert_eq!(
            jitter.pause(servers),
            Duration::from_millis(longest),
            "{servers} servers"
        );
    }
    let bounds = source.0.lock().unwrap().clone();
    assert!(bounds
        .iter()
        .all(|(min, _)| *min == PAUSE_MIN.as_millis() as u64));
    assert!(bounds
        .iter()
        .all(|(_, max)| *max <= PAUSE_MAX.as_millis() as u64));
}

#[test]
fn the_production_pause_is_random_and_within_the_bounds() {
    let jitter = Jitter::random();
    let drawn: Vec<Duration> = (0..300).map(|_| jitter.pause(10)).collect();
    assert!(drawn.iter().all(|d| *d >= PAUSE_MIN && *d <= PAUSE_MAX));
    assert!(
        drawn.iter().collect::<HashSet<_>>().len() > 100,
        "not constant"
    );
    // Short and long ones both occur: the whole range is used.
    assert!(drawn.iter().any(|d| *d < Duration::from_secs(1)));
    assert!(drawn.iter().any(|d| *d > Duration::from_secs(4)));
    // With hundreds of servers no pause is longer than the share of the budget.
    assert!((0..300).all(|_| jitter.pause(2_000) <= Duration::from_millis(100)));
}

#[test]
fn without_a_source_that_gives_pauses_there_are_none_and_the_order_is_kept() {
    for jitter in [Jitter::none(), Jitter::fixed(7_000)] {
        assert_eq!(jitter.pause(10), Duration::ZERO);
        let mut items: Vec<u32> = (0..50).collect();
        jitter.shuffle(&mut items);
        assert_eq!(items, (0..50).collect::<Vec<_>>());
    }
    assert_eq!(
        Jitter::pausing(Duration::from_secs(3)).pause(1_000_000),
        Duration::from_secs(3)
    );
}

#[test]
fn the_production_order_is_a_random_permutation() {
    let jitter = Jitter::random();
    let original: Vec<u32> = (0..40).collect();
    let mut seen = HashSet::new();
    for _ in 0..5 {
        let mut items = original.clone();
        jitter.shuffle(&mut items);
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, original, "nothing lost, nothing added");
        seen.insert(items);
    }
    assert!(seen.len() > 1, "five draws gave one and the same order");
    assert!(!seen.contains(&original), "the order of the subscription");
}
