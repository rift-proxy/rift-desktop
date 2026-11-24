#![no_main]

use libfuzzer_sys::fuzz_target;
use rift_core::sidecar_log::{classify, SingBoxLog};

fuzz_target!(|data: &[u8]| {
    // A line of the log of sing-box, whose words come from the network (a server answers with
    // what it likes): nothing may panic, and what is kept of a line is never more than the three
    // facts and the id of a server, which is 64 lowercase hex digits and nothing else.
    let line = String::from_utf8_lossy(data);
    if let Some(entry) = classify(&line) {
        if let Some(id) = entry.server {
            assert_eq!(id.len(), 64);
            assert!(id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        }
    }
    if let Some(entry) = rift_core::xray_log::classify(&line) {
        assert!(entry.server.is_none());
        assert_eq!(entry.whom, rift_core::sidecar_log::Whom::Other);
    }
    let log = SingBoxLog::default();
    log.record(&line, 1);
    log.record(&line, 2);
    let summary = log.summary();
    assert!(summary.lines <= 2);
    assert!(summary.groups.iter().all(|group| group.count <= 2));
    assert!(summary.servers.keys().all(|id| id.len() == 64));
    let notes = log.notes(10);
    assert!(notes.len() <= summary.groups.len());
    for note in notes {
        assert!(note.starts_with("sing-box "));
    }
});
