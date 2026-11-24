//! When checks start: a fast one and a full one, each on its own timer, a cooldown after a switch
//! that only failover bypasses, and the way a lost network and an interrupted check are handled.
use rift_core::{
    jitter::{Jitter, Source},
    selection::{Plan, Reason, Schedule},
};
use std::{sync::Mutex, time::Duration};
use tokio::time::Instant;

const FAST: Duration = Duration::from_secs(60);
const FULL: Duration = Duration::from_secs(600);
fn schedule(jitter: Jitter) -> Schedule {
    Schedule::new(FAST, FULL, jitter)
}
fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}
fn full(reason: Reason) -> Plan {
    Plan::full(reason)
}
fn fast(reason: Reason) -> Plan {
    Plan::fast(reason)
}
/// Offsets in order, then the last one forever.
struct Script(Mutex<Vec<i64>>);
impl Source for Script {
    fn offset_ms(&self) -> i64 {
        let mut offsets = self.0.lock().unwrap();
        if offsets.len() > 1 {
            offsets.remove(0)
        } else {
            offsets[0]
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_requested_check_starts_at_once_and_a_second_one_waits_for_the_first() {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    s.request(full(Reason::Connect));
    assert_eq!(s.begin(start), Some(full(Reason::Connect)));
    assert!(s.running);
    for _ in 0..100 {
        s.request(full(Reason::CheckNow));
        assert_eq!(s.begin(start), None, "one check at a time");
    }
    assert_eq!(s.pending, Some(full(Reason::CheckNow)));
}

#[tokio::test(start_paused = true)]
async fn a_full_check_takes_the_place_of_a_fast_one_and_keeps_its_first_reason() {
    let mut s = schedule(Jitter::none());
    s.request(fast(Reason::Scheduled));
    s.request(full(Reason::PoolDegraded));
    assert_eq!(s.pending, Some(full(Reason::PoolDegraded)));
    // Neither a fast check nor another full check pushes it out.
    s.request(fast(Reason::Scheduled));
    s.request(full(Reason::CheckNow));
    assert_eq!(s.pending, Some(full(Reason::PoolDegraded)));
    // Two fast checks are one.
    let mut s = schedule(Jitter::none());
    s.request(fast(Reason::Scheduled));
    s.request(fast(Reason::SmallList));
    assert_eq!(s.pending, Some(fast(Reason::Scheduled)));
}

#[tokio::test(start_paused = true)]
async fn each_timer_brings_its_own_check_and_the_full_one_comes_first_when_both_are_due() {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    // Both are due at the start; the full one is the one that is run.
    assert_eq!(s.begin(start), Some(full(Reason::Scheduled)));
    s.finish(start);
    assert_eq!(s.begin(start + secs(59)), None);
    assert_eq!(s.begin(start + secs(60)), Some(fast(Reason::Scheduled)));
    s.finish(start + secs(60));
    // The fast check moved only its own timer.
    assert_eq!(s.begin(start + secs(119)), None);
    assert_eq!(s.begin(start + secs(120)), Some(fast(Reason::Scheduled)));
    s.finish(start + secs(120));
    // Ten minutes after the end of the full check, the full one is due again.
    assert_eq!(s.begin(start + secs(600)), Some(full(Reason::Scheduled)));
}

#[tokio::test(start_paused = true)]
async fn the_full_check_is_due_ten_minutes_after_the_end_of_the_last_full_one_and_moves_the_fast_one(
) {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    assert_eq!(s.begin(start), Some(full(Reason::Scheduled)));
    // A full check that takes five minutes: the next is counted from its end.
    let end = start + secs(300);
    s.finish(end);
    assert_eq!(s.deadline(), end + FAST, "the fast one after it");
    // Fast checks go on every minute; at 300 + 600 the full one is due.
    let mut now = end;
    for _ in 0..9 {
        now += FAST;
        assert_eq!(s.begin(now), Some(fast(Reason::Scheduled)), "{now:?}");
        s.finish(now);
    }
    now += FAST;
    assert_eq!(now, end + FULL);
    assert_eq!(s.begin(now), Some(full(Reason::Scheduled)));
}

#[tokio::test(start_paused = true)]
async fn a_fast_check_that_became_a_full_one_restarts_the_full_timer_from_its_end() {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    assert_eq!(s.begin(start), Some(full(Reason::Scheduled)));
    s.finish(start);
    // Five minutes in, the fast check is given to a full one (the network came back, the list is
    // short): the full timer counts from where this ends, not from the check before.
    let at = start + FAST * 5;
    assert_eq!(s.begin(at), Some(fast(Reason::Scheduled)));
    s.became_full();
    let end = at + secs(30);
    s.finish(end);
    let mut now = end;
    for _ in 0..9 {
        now += FAST;
        assert_eq!(s.begin(now), Some(fast(Reason::Scheduled)), "{now:?}");
        s.finish(now);
    }
    now += FAST;
    assert_eq!(now, end + FULL);
    assert_eq!(s.begin(now), Some(full(Reason::Scheduled)));
}

#[tokio::test(start_paused = true)]
async fn a_fast_check_that_stayed_fast_leaves_the_full_timer_alone() {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    assert_eq!(s.begin(start), Some(full(Reason::Scheduled)));
    s.finish(start);
    let mut now = start;
    for _ in 0..9 {
        now += FAST;
        assert_eq!(s.begin(now), Some(fast(Reason::Scheduled)));
        s.finish(now);
    }
    now += FAST;
    assert_eq!(
        s.begin(now),
        Some(full(Reason::Scheduled)),
        "ten minutes from the full one"
    );
}

#[tokio::test(start_paused = true)]
async fn a_switch_holds_every_check_but_failover_until_the_switch_plus_the_interval() {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    assert_eq!(s.begin(start), Some(full(Reason::Scheduled)));
    s.switched(start + secs(7));
    s.finish(start + secs(20));
    // Counted from the end of the check, but never before the cooldown of the switch.
    assert_eq!(s.deadline(), start + secs(80));
    s.request(full(Reason::CheckNow));
    assert_eq!(
        s.begin(start + secs(66)),
        None,
        "cooldown holds a requested check too"
    );
    assert_eq!(
        s.due_in(start + secs(60)),
        secs(7),
        "until the switch plus a minute"
    );
    assert_eq!(s.begin(start + secs(67)), Some(full(Reason::CheckNow)));
}

#[tokio::test(start_paused = true)]
async fn a_long_check_does_not_make_the_next_one_due_the_moment_it_ends() {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    assert_eq!(s.begin(start), Some(full(Reason::Scheduled)));
    // The switch came at the very beginning, the check lasted five minutes.
    s.switched(start + secs(2));
    s.finish(start + secs(300));
    assert_eq!(
        s.deadline(),
        start + secs(360),
        "a minute after the end, not at once"
    );
}

#[tokio::test(start_paused = true)]
async fn failover_starts_a_full_check_now_and_takes_the_place_of_what_was_queued() {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    s.switched(start);
    s.request(full(Reason::CheckNow));
    s.request(fast(Reason::Scheduled));
    assert_eq!(s.begin(start + secs(1)), None, "the cooldown holds");
    assert_eq!(
        s.begin_urgent(Reason::Failover),
        Some(full(Reason::Failover))
    );
    assert_eq!(s.pending, None);
    assert!(s.running);
    assert_eq!(
        s.begin_urgent(Reason::Failover),
        None,
        "not while a check runs"
    );
    assert_eq!(s.begin(start + secs(100)), None);
}

#[tokio::test(start_paused = true)]
async fn the_spread_moves_the_cooldown_and_the_next_deadline_by_up_to_ten_seconds() {
    for (offset, earliest) in [(-10_000, 50), (0, 60), (10_000, 70)] {
        let start = Instant::now();
        let mut s = schedule(Jitter::fixed(offset));
        s.request(full(Reason::Connect));
        assert_eq!(s.begin(start), Some(full(Reason::Connect)));
        s.switched(start);
        s.finish(start);
        assert_eq!(s.deadline(), start + secs(earliest));
        s.request(full(Reason::CheckNow));
        assert_eq!(
            s.begin(start + secs(earliest - 1)),
            None,
            "offset {offset}: cooldown holds until switch + the spread interval"
        );
        assert_eq!(
            s.begin(start + secs(earliest)),
            Some(full(Reason::CheckNow)),
            "offset {offset}"
        );
        assert_eq!(s.due_in(start + secs(earliest - 5)), secs(5));
    }
}

#[tokio::test(start_paused = true)]
async fn every_deadline_draws_its_own_offset() {
    let start = Instant::now();
    // First draw: the switch (cooldown). Second: the fast check that ends. Third: the full one.
    let script = Script(Mutex::new(vec![10_000, -10_000, 4_000]));
    let mut s = schedule(Jitter::with_source(std::sync::Arc::new(script)));
    s.switched(start);
    assert_eq!(s.begin(start + secs(69)), None);
    assert_eq!(s.begin(start + secs(70)), Some(full(Reason::Scheduled)));
    s.finish(start + secs(70));
    assert_eq!(
        s.deadline(),
        start + secs(70 + 50),
        "a fresh offset from the end of the check"
    );
    assert_eq!(s.begin(start + secs(120)), Some(fast(Reason::Scheduled)));
}

#[tokio::test(start_paused = true)]
async fn changing_the_intervals_spreads_the_new_ones_and_keeps_the_cooldown() {
    let start = Instant::now();
    let mut s = schedule(Jitter::fixed(10_000));
    // Both timers run out at the start: begin a check and end it, then they are set from now.
    assert_eq!(s.begin(start), Some(full(Reason::Scheduled)));
    s.finish(start);
    assert_eq!(s.deadline(), start + secs(70));
    s.set_intervals(secs(120), FULL);
    assert_eq!(s.deadline(), start + secs(130));
    s.switched(start + secs(10));
    s.set_intervals(secs(180), FULL);
    assert_eq!(
        s.deadline(),
        start + secs(10 + 190),
        "after the switch plus the interval"
    );
}

#[tokio::test(start_paused = true)]
async fn saving_other_settings_does_not_push_the_full_check_away() {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    assert_eq!(s.begin(start), Some(full(Reason::Scheduled)));
    s.finish(start);
    // A save at minute nine, with the intervals as they were: the full check is still due at ten.
    tokio::time::advance(secs(540)).await;
    s.set_intervals(FAST, FULL);
    assert_eq!(s.begin(start + secs(599)), Some(fast(Reason::Scheduled)));
    s.finish(start + secs(599));
    assert_eq!(s.begin(start + secs(600)), Some(full(Reason::Scheduled)));
    s.finish(start + secs(600));
    // A new full interval does move it, from now.
    tokio::time::advance(secs(60)).await;
    s.set_intervals(FAST, secs(900));
    assert_eq!(s.begin(start + secs(1199)), Some(fast(Reason::Scheduled)));
    s.finish(start + secs(1199));
    // Fifteen minutes from the save at 600 s.
    assert_eq!(s.begin(start + secs(1499)), Some(fast(Reason::Scheduled)));
    s.finish(start + secs(1499));
    assert_eq!(s.begin(start + secs(1500)), Some(full(Reason::Scheduled)));
}

#[tokio::test(start_paused = true)]
async fn a_check_skipped_for_lack_of_network_waits_for_the_next_deadline_and_keeps_an_owed_full_one(
) {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    s.request(full(Reason::Connect));
    let plan = s.begin(start).unwrap();
    s.skipped(plan, start);
    assert!(!s.running);
    assert_eq!(s.deadline(), start + secs(60));
    // Still owed, but not before the interval has passed: no loop on every tick.
    assert_eq!(s.pending, Some(full(Reason::Connect)));
    assert_eq!(s.begin(start + secs(59)), None);
    let plan = s.begin(start + secs(60)).unwrap();
    assert_eq!(plan, full(Reason::Connect));
    // A scheduled one is not owed: its timer brings the next.
    s.skipped(Plan::full(Reason::Scheduled), start + secs(60));
    assert_eq!(s.pending, None);
    assert_eq!(s.deadline(), start + secs(120));
    assert_eq!(s.begin(start + secs(119)), None);
    assert_eq!(s.begin(start + secs(120)), Some(full(Reason::Scheduled)));
}

#[tokio::test(start_paused = true)]
async fn a_fast_check_skipped_for_lack_of_network_leaves_the_full_timer_alone() {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    assert_eq!(s.begin(start), Some(full(Reason::Scheduled)));
    s.finish(start);
    let plan = s.begin(start + secs(60)).unwrap();
    assert_eq!(plan, fast(Reason::Scheduled));
    s.skipped(plan, start + secs(60));
    assert_eq!(s.pending, None, "a fast check is never owed");
    assert_eq!(s.begin(start + secs(119)), None);
    assert_eq!(s.begin(start + secs(120)), Some(fast(Reason::Scheduled)));
    s.finish(start + secs(120));
    // At 600 the full check is due, as if nothing had been skipped.
    assert_eq!(s.begin(start + secs(600)), Some(full(Reason::Scheduled)));
}

#[tokio::test(start_paused = true)]
async fn the_automatic_check_is_due_when_the_sooner_timer_has_run_out() {
    let start = Instant::now();
    let mut s = schedule(Jitter::none());
    assert_eq!(s.begin(start), Some(full(Reason::Scheduled)));
    assert!(!s.automatic_due(start), "not while a check runs");
    s.finish(start);
    assert!(!s.automatic_due(start + secs(59)));
    assert!(s.automatic_due(start + secs(60)));
    assert_eq!(s.due_in(start + secs(10)), secs(50));
}

#[test]
fn every_reason_survives_the_trip_through_a_byte() {
    for code in 1..=13u8 {
        let reason = Reason::from_code(code).unwrap();
        assert_eq!(reason.code(), code);
    }
    assert_eq!(Reason::from_code(0), None);
    assert_eq!(Reason::from_code(14), None);
}
