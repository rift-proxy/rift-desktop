#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The headers of a response: the same bytes as a title and as a file name, split in two.
    // An image picked for its QR code is untrusted bytes too.
    let _ = rift_core::qr::read_image(data);
    let (first, second) = data.split_at(data.len() / 2);
    let _ = rift_core::subscription::title_from_values(Some(first), Some(second));
    let _ = rift_core::subscription::title_from_values(Some(data), None);
    let _ = rift_core::subscription::title_from_values(None, Some(data));
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    let _ = rift_core::parser::parse_uri(text, None);
    let _ = rift_core::subscription::parse_subscription(text, "fuzz");
    let _ = rift_core::subscription::usage_from_userinfo(text);
    // Text the user pastes, a QR code or a `rift://` link carries: the same untrusted input.
    let _ = rift_core::link::preview_input(text);
});
