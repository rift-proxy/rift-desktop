#![no_main]

use libfuzzer_sys::fuzz_target;
use rift_core::dns::{accept, answer_addresses, empty_response, parse_question};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fuzz_target!(|data: &[u8]| {
    // A query read from a loopback connection and an answer read from a resolver: nothing may
    // panic, and what is read is what the bridge promises to answer.
    let response = empty_response(data, 2);
    assert!(response.len() >= 12);
    if data.len() >= 2 {
        assert_eq!(&response[..2], &data[..2]);
    }
    if let Some(question) = parse_question(data) {
        assert!(question.name.len() <= 253);
        let answer = answer_addresses(
            data,
            &[
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ],
        )
        .expect("a question that was read is answered");
        // The answer repeats the question and lists both addresses (16 and 28 bytes of record).
        assert_eq!(&answer[..2], &data[..2]);
        assert_eq!(&answer[6..8], &[0, 2]);
        assert!(answer.len() > 12 + 16 + 28);
    }
    // The first byte splits the input into a query and an answer to it.
    if let Some((&cut, rest)) = data.split_first() {
        let at = usize::from(cut).min(rest.len());
        let (query, answer) = rest.split_at(at);
        if let Ok(accepted) = accept(query, answer.to_vec()) {
            // An accepted answer is a response of the query it answers, with its id.
            assert!(accepted.len() >= 12);
            assert_eq!(&accepted[..2], &query[..2]);
            assert_ne!(accepted[2] & 0x80, 0);
            assert!(matches!(accepted[3] & 0x0F, 0 | 3));
        }
    }
});
