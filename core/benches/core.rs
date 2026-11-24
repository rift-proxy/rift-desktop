//! Criterion wall-clock benchmarks for the core hot paths.
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use rift_core::{
    config,
    fast::{self, Watched},
    jitter::Jitter,
    parser::parse_uri,
    pool::Tolerance,
    proxy_channel::ProxyHandoff,
    selection,
    subscription::{merge, parse_subscription},
};
use std::{hint::black_box, sync::RwLock};
use tokio_util::sync::CancellationToken;

mod common;
use common as fixtures;
use fixtures::{InstantProbe, SERVERS};

fn parsing(c: &mut Criterion) {
    let vmess = fixtures::vmess();
    c.bench_function("parse_uri/vless_reality", |b| {
        b.iter(|| parse_uri(black_box(fixtures::VLESS_REALITY), None).unwrap())
    });
    c.bench_function("parse_uri/vmess", |b| {
        b.iter(|| parse_uri(black_box(&vmess), None).unwrap())
    });
    let body = fixtures::subscription(SERVERS);
    c.bench_function("parse_subscription/2000", |b| {
        b.iter(|| parse_subscription(black_box(&body), "sub").unwrap())
    });
}

fn store(c: &mut Criterion) {
    let store = fixtures::store(SERVERS);
    let body = fixtures::subscription(SERVERS);
    c.bench_function("merge/2000", |b| {
        b.iter_batched(
            || parse_subscription(&body, "sub").unwrap(),
            |parsed| merge(black_box(&store), "sub", parsed),
            BatchSize::LargeInput,
        )
    });
    c.bench_function("config_generate/2000", |b| {
        b.iter(|| {
            config::generate(black_box(&store), "bench-secret-0123456789012345678901").unwrap()
        })
    });
}

fn select(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let servers = fixtures::servers(200);
    c.bench_function("selection_run/full/200", |b| {
        b.iter(|| {
            runtime.block_on(selection::run(
                &InstantProbe,
                black_box(&servers),
                None,
                10,
                &CancellationToken::new(),
                |_| {},
            ))
        })
    });
}

fn fast_check(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    // The pool: three servers, each with a delay far above what the fake answers, so no second look.
    let servers = fixtures::servers(3);
    let watched: Vec<Watched> = servers
        .iter()
        .map(|server| Watched {
            server,
            base_ms: Some(1000),
        })
        .collect();
    let tolerance = Tolerance {
        percent: 50,
        floor_ms: 50,
        cap_ms: 1500,
    };
    c.bench_function("fast_check/3", |b| {
        b.iter(|| {
            runtime.block_on(fast::run_fast(
                &InstantProbe,
                black_box(&watched),
                Some(&servers[0].id),
                tolerance,
                3,
                &Jitter::none(),
                &CancellationToken::new(),
                |_| {},
            ))
        })
    });
}

fn proxy_handoff_snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("proxy_handoff_snapshot");
    for count in [1, 500, 2000] {
        let store = fixtures::store(count);
        let channel = RwLock::new(Some(
            ProxyHandoff::new(25000, "bench-secret-0123456789012345678901".into(), &store).unwrap(),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &channel,
            |b, channel| {
                b.iter(|| black_box(channel.read().unwrap().clone()));
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    parsing,
    store,
    select,
    fast_check,
    proxy_handoff_snapshot
);
criterion_main!(benches);
