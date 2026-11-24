//! Shared by the tests: a builder of geosite and geoip database files, the protobuf of the real
//! ones, so no binary fixture is committed.
#![allow(dead_code)]

pub mod clash;
pub mod fake;
pub mod helper;

pub fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}
pub fn bytes_field(number: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = varint(u64::from(number) << 3 | 2);
    out.extend(varint(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}
pub fn varint_field(number: u32, value: u64) -> Vec<u8> {
    let mut out = varint(u64::from(number) << 3);
    out.extend(varint(value));
    out
}
/// `kind`: 0 keyword, 1 regex, 2 domain (suffix), 3 full.
pub fn domain(kind: u64, value: &str) -> Vec<u8> {
    let mut out = varint_field(1, kind);
    out.extend(bytes_field(2, value.as_bytes()));
    // An attribute, which the reader ignores.
    out.extend(bytes_field(
        3,
        &[bytes_field(1, b"cn"), varint_field(2, 1)].concat(),
    ));
    out
}
pub fn site(code: &str, domains: &[(u64, &str)]) -> Vec<u8> {
    let mut out = bytes_field(1, code.as_bytes());
    for (kind, value) in domains {
        out.extend(bytes_field(2, &domain(*kind, value)));
    }
    out
}
pub fn cidr(ip: &[u8], prefix: u64) -> Vec<u8> {
    [bytes_field(1, ip), varint_field(2, prefix)].concat()
}
pub fn ip_entry(code: &str, ranges: &[Vec<u8>]) -> Vec<u8> {
    let mut out = bytes_field(1, code.as_bytes());
    for range in ranges {
        out.extend(bytes_field(2, range));
    }
    out
}
pub fn file(entries: &[Vec<u8>]) -> Vec<u8> {
    entries
        .iter()
        .flat_map(|entry| bytes_field(1, entry))
        .collect()
}
pub fn geosite_file() -> Vec<u8> {
    file(&[
        site(
            "CATEGORY-RU",
            &[
                (2, "yandex.ru"),
                (2, "VK.com"),
                (3, "www.example.ru"),
                (0, "bank"),
                (1, r"^ad[0-9]+\."),
            ],
        ),
        site("GOOGLE", &[(2, "google.com")]),
        site("GEOLOCATION-!CN", &[(2, "example.org")]),
    ])
}
pub fn geoip_file() -> Vec<u8> {
    file(&[
        ip_entry(
            "RU",
            &[
                cidr(&[5, 8, 0, 0], 13),
                cidr(
                    &[0x2a, 0, 0x14, 0x50, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                    32,
                ),
            ],
        ),
        ip_entry(
            "PRIVATE",
            &[cidr(&[10, 0, 0, 0], 8), cidr(&[192, 168, 0, 0], 16)],
        ),
    ])
}
