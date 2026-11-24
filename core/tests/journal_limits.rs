//! What the journal keeps (`journal::trim`): at most a day, 100 KiB and 5000 entries, cut off
//! the start, whatever the entries are.
use proptest::prelude::*;
use rift_core::{
    application::{Category, Entry},
    journal::{trim, MAX_AGE_SECS, MAX_BYTES, MAX_ENTRIES},
};

const NOW: u64 = 1_000_000;

fn entry(at: u64, message: String) -> Entry {
    Entry {
        at,
        level: "info".into(),
        message,
        details: None,
        crash_id: None,
        failure_code: None,
        category: Category::Checks,
    }
}
fn file_bytes(journal: &[Entry]) -> usize {
    serde_json::to_vec(journal).unwrap().len()
}
fn key(journal: &[Entry]) -> Vec<(u64, String)> {
    journal.iter().map(|e| (e.at, e.message.clone())).collect()
}

/// Entries in the order they were written: ages from two days ago to now, sizes from nothing to
/// 4 KB, so that any of the limits can be the one that cuts.
fn journal() -> impl Strategy<Value = Vec<Entry>> {
    prop::collection::vec((0..=2 * MAX_AGE_SECS, 0usize..4000), 0..80).prop_map(|mut raw| {
        raw.sort_by_key(|(age, _)| std::cmp::Reverse(*age));
        raw.into_iter()
            .map(|(age, len)| entry(NOW - age, "x".repeat(len)))
            .collect()
    })
}

proptest! {
    #[test]
    fn what_is_kept_is_the_newest_part_inside_every_limit(input in journal()) {
        let aged: Vec<Entry> = input
            .iter()
            .filter(|e| e.at + MAX_AGE_SECS >= NOW)
            .cloned()
            .collect();
        let mut kept = input;
        trim(&mut kept, NOW);

        prop_assert!(kept.len() <= MAX_ENTRIES);
        prop_assert!(kept.iter().all(|e| e.at + MAX_AGE_SECS >= NOW));
        // Only the start is cut: what stays is the end of the entries young enough.
        prop_assert!(kept.len() <= aged.len());
        prop_assert_eq!(key(&kept), key(&aged[aged.len() - kept.len()..]));
        // Inside the size limit, except for the one newest entry that is too big alone.
        prop_assert!(kept.len() <= 1 || file_bytes(&kept) <= MAX_BYTES);
        // Nothing more is cut than has to be: the next older entry would not fit.
        if kept.len() < aged.len() {
            let one_more = &aged[aged.len() - kept.len() - 1..];
            prop_assert!(file_bytes(one_more) > MAX_BYTES || one_more.len() > MAX_ENTRIES);
        }
        // The newest entry is never the one lost to the size.
        prop_assert_eq!(kept.is_empty(), aged.is_empty());
    }

    #[test]
    fn trimming_twice_changes_nothing_more(input in journal()) {
        let mut once = input;
        trim(&mut once, NOW);
        let mut twice = once.clone();
        trim(&mut twice, NOW);
        prop_assert_eq!(key(&once), key(&twice));
    }

    #[test]
    fn a_journal_inside_the_limits_is_left_alone(len in 0usize..50) {
        let journal: Vec<Entry> = (0..len as u64)
            .map(|at| entry(NOW - at, format!("event {at}")))
            .rev()
            .collect();
        let mut trimmed = journal.clone();
        trim(&mut trimmed, NOW);
        prop_assert_eq!(key(&journal), key(&trimmed));
    }
}
