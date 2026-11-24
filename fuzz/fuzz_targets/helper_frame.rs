#![no_main]

use libfuzzer_sys::fuzz_target;
use rift_core::{
    config,
    elevation::{
        helper::vet,
        protocol::{decode, frame_length, read_frame, FromHelper, ToHelper, MAX_FRAME},
    },
    model::Mode,
    outbound,
};

fuzz_target!(|data: &[u8]| {
    // Slice reads are immediately ready, so this exercises the actual framing parser without
    // starting a runtime for every fuzz input (including EOF in either header or body).
    let _ = futures::executor::block_on(read_frame::<_, ToHelper>(&mut &data[..]));
    // The elevated helper reads these bytes from a channel: nothing may panic, a length is
    // refused before it is believed, and a store that is vetted is one the config builder and
    // the outbound allowlist both accept.
    if data.len() >= 4 {
        let header = [data[0], data[1], data[2], data[3]];
        match frame_length(header) {
            Ok(length) => assert!(length <= MAX_FRAME),
            Err(_) => assert!(u32::from_be_bytes(header) as usize > MAX_FRAME),
        }
    }
    let _ = decode::<FromHelper>(data);
    if let Ok(ToHelper::Start { store, secret }) = decode::<ToHelper>(data) {
        if vet(&store, &secret).is_ok() {
            assert_ne!(store.settings.mode, Mode::Proxy);
            assert_eq!(secret.len(), 32);
            for server in &store.servers {
                outbound::inspect(&server.outbound).expect("a vetted outbound is on the allowlist");
            }
            let _ = config::generate(&store, &secret);
        }
    }
});
