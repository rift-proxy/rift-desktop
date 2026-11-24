//! Gungraun (Valgrind Callgrind) benchmarks: instruction counts, cache misses (cache sim) and
//! estimated cycles per call. Deterministic, so a >10% instruction increase over the previous
//! run's baseline fails the comparison. Allocations are measured by the Heaptrack step: DHAT
//! output of Valgrind 3.26 is not parsed by gungraun-runner 0.19.4.
// Gungraun hands each #[bench] setup value to the benchmark by value.
#![allow(clippy::needless_pass_by_value)]

#[cfg(target_os = "linux")]
mod common;
#[cfg(target_os = "linux")]
include!("gungraun/linux.rs");

/// Valgrind does not run on Windows/macOS; keep the target compiling there.
#[cfg(not(target_os = "linux"))]
fn main() {}
