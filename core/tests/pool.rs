use proptest::prelude::*;
use rift_core::{
    model::Settings,
    pool::{self, Health, Ranked, Tolerance},
};

fn ranked(id: &str, host: &str, ms: u64) -> Ranked {
    Ranked {
        id: id.into(),
        host: host.into(),
        ms,
    }
}
fn ids(pool: &[Ranked]) -> Vec<&str> {
    pool.iter().map(|r| r.id.as_str()).collect()
}
fn tolerance() -> Tolerance {
    Tolerance::from_settings(&Settings::default())
}

#[test]
fn the_pool_is_the_best_of_the_ranking_up_to_its_size() {
    let ranking = [
        ranked("a", "h1", 100),
        ranked("b", "h2", 120),
        ranked("c", "h3", 130),
        ranked("d", "h4", 140),
    ];
    assert_eq!(ids(&pool::build(&ranking, 3)), ["a", "b", "c"]);
    assert_eq!(ids(&pool::build(&ranking, 1)), ["a"]);
    assert_eq!(ids(&pool::build(&ranking, 5)), ["a", "b", "c", "d"]);
    assert!(pool::build(&[], 3).is_empty());
}

#[test]
fn the_pool_holds_one_server_per_host() {
    // Three rows of one machine are not three reserves: when it falls they all fall.
    let ranking = [
        ranked("a", "h1", 100),
        ranked("b", "h1", 105),
        ranked("c", "H1", 110),
        ranked("d", "h2", 120),
        ranked("e", "h2", 125),
        ranked("f", "h3", 130),
        ranked("g", "h4", 135),
    ];
    assert_eq!(ids(&pool::build(&ranking, 3)), ["a", "d", "f"]);
    // The host is compared without regard to case.
    assert_eq!(ids(&pool::build(&ranking, 2)), ["a", "d"]);
}

#[test]
fn a_ranking_of_one_host_gives_a_pool_of_one() {
    let ranking = [ranked("a", "h1", 100), ranked("b", "h1", 105)];
    assert_eq!(ids(&pool::build(&ranking, 3)), ["a"]);
}

#[test]
fn how_many_servers_are_measured_again_before_the_choice() {
    let hosts = |names: &[&str]| -> Vec<String> { names.iter().map(|n| (*n).to_owned()).collect() };
    // At least five, the pool size and two more: the pool is chosen among the steady ones.
    let many: Vec<String> = (0..20).map(|i| format!("h{i}")).collect();
    assert_eq!(pool::refine_count(&many, 3), 5);
    assert_eq!(pool::refine_count(&many, 5), 7);
    assert_eq!(pool::refine_count(&many, 1), 5);
    // Fewer servers than that: all of them.
    assert_eq!(pool::refine_count(&hosts(&["a", "b"]), 3), 2);
    assert_eq!(pool::refine_count(&[], 3), 0);
    // The fastest five are one machine: go on until the pool can have its hosts.
    let crowded = hosts(&["a", "a", "a", "a", "a", "b", "b", "c", "d"]);
    assert_eq!(
        pool::refine_count(&crowded, 3),
        8,
        "a, b and c are in the first eight"
    );
    // The hosts run out: everything there is, but not past twice the usual number.
    let one = hosts(&["a", "a", "a", "a", "a", "a", "a", "a"]);
    assert_eq!(pool::refine_count(&one, 3), 8);
    let crowd: Vec<String> = (0..50).map(|_| "a".to_owned()).collect();
    assert_eq!(pool::refine_count(&crowd, 3), 10, "twice five");
    assert_eq!(pool::refine_count(&crowd, 5), 14, "twice seven");
}

#[test]
fn the_tolerance_grows_by_a_share_but_not_beyond_the_cap_nor_below_the_floor() {
    let t = tolerance(); // 50 %, floor 50 ms (the switch threshold), cap 1500 ms
    assert_eq!(t.limit(120), 180, "50 % of 120");
    assert_eq!(
        t.limit(20),
        70,
        "a small delay still has room for the floor"
    );
    assert_eq!(t.limit(100), 150);
    assert_eq!(t.limit(1000), 1500, "50 % would be 1500, the cap");
    assert_eq!(t.limit(1200), 1500, "50 % would be 1800, the cap");
    assert_eq!(t.limit(1480), 1530, "above the cap the floor still applies");
    assert_eq!(
        t.limit(2000),
        2050,
        "the cap is a ceiling on the limit, not on the growth: a base above it gets only the floor"
    );
    assert_eq!(t.limit(5000), 5050);
}

#[test]
fn the_tolerance_follows_the_settings() {
    let settings = Settings {
        degrade_percent: 100,
        degrade_cap_ms: 400,
        switch_threshold_ms: 10,
        ..Settings::default()
    };
    let t = Tolerance::from_settings(&settings);
    assert_eq!(t.limit(100), 200);
    assert_eq!(t.limit(300), 400, "cap");
    assert_eq!(t.limit(5), 15, "floor");
}

#[test]
fn an_answer_at_the_limit_is_still_the_old_answer() {
    let t = tolerance();
    assert!(!t.exceeded(120, 180));
    assert!(t.exceeded(120, 181));
    assert!(!t.exceeded(120, 90), "faster is never worse");
}

#[test]
fn a_first_answer_within_the_limit_needs_no_second_look() {
    let t = tolerance();
    assert!(!pool::needs_confirmation(Some(120), Some(150), t));
    assert!(!pool::needs_confirmation(Some(120), Some(180), t));
    assert!(pool::needs_confirmation(Some(120), Some(181), t));
    assert!(
        pool::needs_confirmation(Some(120), None, t),
        "silence is looked at twice"
    );
    // Without a delay to compare with, the server is measured until it has one.
    assert!(pool::needs_confirmation(None, Some(100), t));
    assert!(pool::needs_confirmation(None, None, t));
}

#[test]
fn a_noisy_first_answer_is_forgiven_when_the_middle_of_three_is_fine() {
    let t = tolerance();
    let j = pool::judge(Some(120), &[Some(400), Some(130), Some(125)], t);
    assert_eq!(j.health, Health::Fine);
    assert_eq!(j.median_ms, Some(130));
}

#[test]
fn a_server_that_stays_slow_is_worse() {
    let t = tolerance();
    let j = pool::judge(Some(120), &[Some(400), Some(380), Some(390)], t);
    assert_eq!(
        j.health,
        Health::Worse {
            ms: 390,
            limit_ms: 180
        }
    );
    assert_eq!(j.median_ms, Some(390));
}

#[test]
fn two_answers_of_three_are_judged_by_the_worse_of_them() {
    let t = tolerance();
    // One miss: the slower of the two answers decides, as in the comparison of servers.
    let j = pool::judge(Some(120), &[None, Some(130), Some(200)], t);
    assert_eq!(
        j.health,
        Health::Worse {
            ms: 200,
            limit_ms: 180
        }
    );
    let j = pool::judge(Some(120), &[Some(130), None, Some(170)], t);
    assert_eq!(j.health, Health::Fine);
}

#[test]
fn a_server_with_fewer_than_two_answers_is_lost() {
    let t = tolerance();
    assert_eq!(
        pool::judge(Some(120), &[None, None, None], t).health,
        Health::Lost
    );
    assert_eq!(
        pool::judge(Some(120), &[Some(100), None, None], t).health,
        Health::Lost
    );
    assert_eq!(
        pool::judge(Some(120), &[None, Some(100), None], t).median_ms,
        None
    );
}

#[test]
fn a_server_without_a_delay_to_compare_with_gets_one_from_its_answers() {
    let t = tolerance();
    let j = pool::judge(None, &[Some(300), Some(310), Some(320)], t);
    assert_eq!(j.health, Health::Fine);
    assert_eq!(
        j.median_ms,
        Some(310),
        "the middle becomes its delay to compare with"
    );
    assert_eq!(
        pool::judge(None, &[None, None, Some(5)], t).health,
        Health::Lost
    );
}

#[test]
fn a_judgement_of_one_answer_that_needs_no_confirmation_is_fine() {
    let t = tolerance();
    let j = pool::judge(Some(120), &[Some(140)], t);
    assert_eq!(j.health, Health::Fine);
    assert_eq!(j.median_ms, Some(140));
}

proptest! {
    #[test]
    fn the_limit_never_drops_below_the_floor_and_never_exceeds_cap_or_floor(
        base in 1u64..20_000,
        percent in 10u64..=300,
        floor in 0u64..=10_000,
        cap in 200u64..=10_000,
    ) {
        let t = Tolerance { percent, floor_ms: floor, cap_ms: cap };
        let limit = t.limit(base);
        prop_assert!(limit >= base + floor);
        prop_assert!(limit <= (base + floor).max(cap));
    }

    #[test]
    fn a_slower_base_never_tolerates_less(
        base in 1u64..20_000,
        more in 0u64..5_000,
        percent in 10u64..=300,
        floor in 0u64..=1_000,
        cap in 200u64..=10_000,
    ) {
        let t = Tolerance { percent, floor_ms: floor, cap_ms: cap };
        prop_assert!(t.limit(base + more) >= t.limit(base));
    }

    #[test]
    fn the_pool_never_has_two_servers_of_one_host_nor_more_than_asked(
        hosts in proptest::collection::vec(0u8..6, 0..30),
        size in 1usize..=5,
    ) {
        let ranking: Vec<Ranked> = hosts
            .iter()
            .enumerate()
            .map(|(i, h)| ranked(&format!("s{i}"), &format!("h{h}"), 100 + i as u64))
            .collect();
        let pool = pool::build(&ranking, size);
        prop_assert!(pool.len() <= size);
        let mut seen = std::collections::HashSet::new();
        for member in &pool {
            prop_assert!(seen.insert(member.host.to_lowercase()));
        }
        // Nothing better was left out for a host that is already there.
        let distinct: std::collections::HashSet<_> = hosts.iter().collect();
        prop_assert_eq!(pool.len(), size.min(distinct.len()));
        // The pool keeps the order of the ranking.
        let order: Vec<u64> = pool.iter().map(|m| m.ms).collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        prop_assert_eq!(order, sorted);
    }

    #[test]
    fn a_confirmed_server_is_never_worse_than_its_limit_allows(
        base in 1u64..5_000,
        a in 1u64..10_000,
        b in 1u64..10_000,
        c in 1u64..10_000,
    ) {
        let t = tolerance();
        let judgement = pool::judge(Some(base), &[Some(a), Some(b), Some(c)], t);
        let mut sorted = [a, b, c];
        sorted.sort_unstable();
        match judgement.health {
            Health::Fine => prop_assert!(sorted[1] <= t.limit(base)),
            Health::Worse { ms, limit_ms } => {
                prop_assert_eq!(ms, sorted[1]);
                prop_assert_eq!(limit_ms, t.limit(base));
                prop_assert!(ms > limit_ms);
            }
            Health::Lost => prop_assert!(false, "three answers are never lost"),
        }
    }
}
