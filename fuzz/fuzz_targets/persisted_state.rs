#![no_main]

use libfuzzer_sys::fuzz_target;
use rift_core::{config, outbound, persistence::decode, profile, Error};

fuzz_target!(|data: &[u8]| {
    if let Ok(store) = profile::decode(data) {
        // A hostile file must not reach the sidecar config: every accepted outbound is one the
        // URI parser could have built, and the row fields are the ones it implies.
        for server in &store.servers {
            let endpoint = outbound::inspect(&server.outbound).expect("accepted outbound");
            assert_eq!(endpoint.protocol, server.protocol);
            assert_eq!(endpoint.host, server.host);
            assert_eq!(endpoint.port, server.port);
            let node = rift_core::connection::Connection::from_legacy(&server.outbound)
                .expect("accepted neutral/legacy model is bounded");
            let _ = node.xray_outbound("synthetic-fuzz-candidate");
        }
        let _ = rift_core::launch::LaunchPlan::build(
            &store,
            "0123456789abcdef0123456789abcdef",
            &rift_core::geodata::GeoSet::none(),
            rift_core::config::Resolver::Bridge(1),
            65535,
            "synthetic-fuzz-handoff-01234567890123456789",
        );
        let _ = config::generate(&store, "0123456789abcdef0123456789abcdef");
        match profile::encode(&store) {
            Ok(encoded) => {
                let restored = profile::decode(&encoded).expect("serialized profile decodes again");
                assert_eq!(
                    serde_json::to_vec(&restored).expect("accepted profile serializes"),
                    serde_json::to_vec(&store).expect("accepted profile serializes")
                );
            }
            // Compact input just under the cap can canonicalize to a few bytes more than it.
            Err(Error::Invalid(message)) if message == profile::too_large() => {}
            Err(error) => panic!("accepted profile does not serialize: {error}"),
        }
    }

    let Ok(store) = decode(data) else {
        return;
    };

    assert_eq!(store.version, 2);
    assert!(store.settings.validate().is_ok());
    let _ = store.available_servers();
    let _ = store.auto_candidates(0);
    let _ = store.auto_candidates(u64::MAX);
    // Every consumer of an accepted state must reject, not panic on, whatever decode let in.
    let _ = config::generate(&store, "0123456789abcdef0123456789abcdef");

    // An accepted state must survive being written and read back unchanged.
    let written = serde_json::to_vec(&store).expect("accepted state serializes");
    let value: serde_json::Value = serde_json::from_slice(&written).unwrap();
    for source in value["subscriptions"].as_array().unwrap() {
        assert!(source.get("enabled").is_none());
    }
    let reread = decode(&written).expect("serialized state decodes again");
    assert_eq!(
        serde_json::to_vec(&reread).expect("accepted state serializes"),
        written
    );
});
