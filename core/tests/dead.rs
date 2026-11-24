//! The list of servers that did not answer: one list, a server enters it at the second miss in a
//! row, a wait of its own for each server that grows by five minutes with every failure in a row up
//! to an hour, one answer to leave.
use rift_core::{
    dead::{wait, Dead, LONGEST, MISSES_TO_LIST, STEP},
    jitter::Jitter,
};
use std::{collections::HashSet, time::Duration};
use tokio::time::Instant;

const MINUTE: Duration = Duration::from_secs(60);

#[test]
fn the_wait_grows_by_five_minutes_per_failure_and_stops_at_an_hour() {
    assert_eq!(STEP, 5 * MINUTE);
    assert_eq!(LONGEST, 60 * MINUTE);
    for (failures, minutes) in [
        (0, 5),
        (1, 5),
        (2, 10),
        (3, 15),
        (11, 55),
        (12, 60),
        (13, 60),
        (1000, 60),
        (u32::MAX, 60),
    ] {
        assert_eq!(wait(failures), minutes * MINUTE, "{failures} failures");
    }
}

#[test]
fn one_miss_is_noise_and_the_second_in_a_row_puts_the_server_in_the_list() {
    assert_eq!(MISSES_TO_LIST, 2);
    let start = Instant::now();
    let jitter = Jitter::none();
    let mut dead = Dead::new();
    dead.failed("A", start, &jitter);
    assert!(!dead.contains("A"), "one miss is not enough");
    assert_eq!(dead.misses("A"), 1);
    assert_eq!(dead.failures("A"), 0);
    assert_eq!(dead.next_at("A"), None);
    assert!(dead.is_empty());
    assert!(
        dead.due(start + 1000 * MINUTE).is_empty(),
        "nothing to ask again yet"
    );
    dead.failed("A", start + MINUTE, &jitter);
    assert!(dead.contains("A"));
    assert_eq!(dead.failures("A"), 1);
    assert_eq!(
        dead.next_at("A"),
        Some(start + 6 * MINUTE),
        "five minutes from the second miss"
    );
    assert_eq!(dead.len(), 1);
}

#[test]
fn an_answer_between_two_misses_forgets_the_first() {
    let start = Instant::now();
    let jitter = Jitter::none();
    let mut dead = Dead::new();
    dead.failed("A", start, &jitter);
    dead.answered("A");
    assert_eq!(dead.misses("A"), 0);
    dead.failed("A", start, &jitter);
    assert!(
        !dead.contains("A"),
        "the miss before the answer does not count"
    );
    dead.failed("A", start, &jitter);
    assert!(dead.contains("A"));
}

#[test]
fn a_server_is_asked_again_after_its_own_wait_and_each_failure_makes_the_wait_longer() {
    let start = Instant::now();
    let jitter = Jitter::none();
    let mut dead = Dead::new();
    dead.failed("A", start, &jitter);
    dead.failed("A", start, &jitter);
    assert_eq!(dead.next_at("A"), Some(start + 5 * MINUTE));
    assert!(dead
        .due(start + 5 * MINUTE - Duration::from_secs(1))
        .is_empty());
    assert_eq!(dead.due(start + 5 * MINUTE), ["A"]);
    // Asked again, no answer: ten minutes, then fifteen...
    let mut now = start + 5 * MINUTE;
    for (failures, minutes) in [(2, 10), (3, 15), (4, 20)] {
        dead.failed("A", now, &jitter);
        assert_eq!(dead.failures("A"), failures);
        assert_eq!(dead.next_at("A"), Some(now + minutes * MINUTE));
        now += minutes * MINUTE;
    }
    // ...and an hour at most, however long it goes on.
    for _ in 0..20 {
        dead.failed("A", now, &jitter);
    }
    assert_eq!(dead.next_at("A"), Some(now + 60 * MINUTE));
    assert_eq!(dead.len(), 1);
}

#[test]
fn every_server_has_a_wait_of_its_own() {
    let start = Instant::now();
    let jitter = Jitter::none();
    let mut dead = Dead::new();
    for _ in 0..4 {
        dead.failed("old", start, &jitter);
    }
    dead.failed("new", start, &jitter);
    dead.failed("new", start, &jitter);
    assert_eq!(dead.next_at("new"), Some(start + 5 * MINUTE));
    assert_eq!(dead.next_at("old"), Some(start + 15 * MINUTE));
    // The one that failed fewer times is due first; the other skips that time.
    assert_eq!(dead.due(start + 5 * MINUTE), ["new"]);
    assert_eq!(dead.due(start + 10 * MINUTE), ["new"]);
    assert_eq!(dead.due(start + 15 * MINUTE), ["new", "old"]);
    assert!(dead.due(start).is_empty());
}

#[test]
fn one_answer_takes_the_server_out_and_the_next_failures_start_over() {
    let start = Instant::now();
    let jitter = Jitter::none();
    let mut dead = Dead::new();
    for _ in 0..7 {
        dead.failed("A", start, &jitter);
    }
    assert_eq!(dead.failures("A"), 6);
    dead.answered("A");
    assert!(!dead.contains("A"));
    assert_eq!(dead.failures("A"), 0);
    assert_eq!(dead.next_at("A"), None);
    assert!(dead.due(start + 1000 * MINUTE).is_empty());
    dead.failed("A", start, &jitter);
    dead.failed("A", start, &jitter);
    assert_eq!(dead.next_at("A"), Some(start + 5 * MINUTE));
    // Answering is harmless for a server that was never in the list.
    dead.answered("B");
    assert_eq!(dead.len(), 1);
}

#[test]
fn a_pass_that_could_not_be_made_moves_the_time_and_counts_no_failure() {
    let start = Instant::now();
    let jitter = Jitter::none();
    let mut dead = Dead::new();
    for _ in 0..3 {
        dead.failed("A", start, &jitter);
    }
    dead.failed("B", start, &jitter);
    dead.failed("B", start, &jitter);
    dead.failed("one miss", start, &jitter);
    let later = start + 10 * MINUTE;
    dead.postponed(
        &["A".into(), "gone".into(), "one miss".into()],
        later,
        &jitter,
    );
    assert_eq!(dead.failures("A"), 2);
    assert_eq!(dead.next_at("A"), Some(later + 10 * MINUTE));
    assert_eq!(
        dead.next_at("B"),
        Some(start + 5 * MINUTE),
        "B was not named"
    );
    assert!(!dead.contains("gone"), "nothing is added by postponing");
    assert!(
        !dead.contains("one miss"),
        "nor a server that is not in the list"
    );
    assert_eq!(dead.misses("one miss"), 1);
}

#[test]
fn the_spread_of_periodic_waits_applies_and_servers_that_are_gone_are_forgotten() {
    let start = Instant::now();
    let mut dead = Dead::new();
    dead.failed("A", start, &Jitter::fixed(10_000));
    dead.failed("A", start, &Jitter::fixed(10_000));
    assert_eq!(
        dead.next_at("A"),
        Some(start + 5 * MINUTE + Duration::from_secs(10))
    );
    for id in ["B", "C"] {
        dead.failed(id, start, &Jitter::none());
        dead.failed(id, start, &Jitter::none());
    }
    dead.keep_only(&HashSet::from(["A", "C", "never listed"]));
    assert!(dead.contains("A") && dead.contains("C") && !dead.contains("B"));
    assert_eq!(dead.len(), 2);
    dead.clear();
    assert!(dead.is_empty());
}

#[test]
fn the_server_waiting_longest_is_asked_first() {
    let start = Instant::now();
    let jitter = Jitter::none();
    let mut dead = Dead::new();
    for (id, at) in [("late", 3), ("early", 0), ("same-b", 1), ("same-a", 1)] {
        dead.failed(id, start + at * MINUTE, &jitter);
        dead.failed(id, start + at * MINUTE, &jitter);
    }
    assert_eq!(
        dead.due(start + 60 * MINUTE),
        ["early", "same-a", "same-b", "late"]
    );
}
