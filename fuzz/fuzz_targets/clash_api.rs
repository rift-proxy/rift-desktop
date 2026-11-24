#![no_main]

use libfuzzer_sys::fuzz_target;
use rift_core::clash::{
    parse_connections, parse_traffic_line, unix_from_rfc3339, view, MAX_CONNECTIONS,
};
use std::time::{Duration, UNIX_EPOCH};

fuzz_target!(|data: &[u8]| {
    // The answers of the sing-box Clash API are read from loopback, but they are still bytes
    // from another program: no panic, and what reaches the screen is bounded and clean.
    let _ = parse_traffic_line(data);
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = unix_from_rfc3339(text);
    }
    if let Ok(fetched) = parse_connections(data) {
        assert!(fetched.items.len() <= MAX_CONNECTIONS);
        assert!(fetched.items.len() <= fetched.total);
        let servers = vec![("a".to_owned(), "name\u{7}\u{1b}[0m".to_owned())];
        let shown = view(
            fetched,
            &servers,
            UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        );
        for row in shown.items {
            assert!(row.host.chars().count() <= 253);
            assert!(!row.host.chars().any(char::is_control));
            if let Some(process) = &row.process {
                assert!(process.chars().count() <= 128);
                assert!(!process.contains(['/', '\\']), "a path reached the screen");
                assert!(!process.chars().any(char::is_control));
            }
            if let Some(server) = &row.server {
                assert!(server.chars().count() <= 120);
                assert!(!server.chars().any(char::is_control));
            }
        }
    }
});
