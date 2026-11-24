// Included by core_gungraun.rs on Linux only (Valgrind is Linux-only).
use gungraun::{
    library_benchmark, library_benchmark_group, main, Callgrind, CallgrindMetrics, EventKind,
    LibraryBenchmarkConfig,
};
use rift_core::{
    config,
    model::{Server, Store},
    parser::parse_uri,
    selection,
    subscription::{merge, parse_subscription, ParsedSubscription},
};
use std::hint::black_box;
use tokio_util::sync::CancellationToken;

use common as fixtures;
use fixtures::{InstantProbe, SERVERS};

#[library_benchmark]
#[bench::vless_reality(fixtures::VLESS_REALITY.to_owned())]
#[bench::vmess(fixtures::vmess())]
fn bench_parse_uri(uri: String) -> Server {
    black_box(parse_uri(black_box(&uri), None).unwrap())
}

#[library_benchmark]
#[bench::servers_2000(fixtures::subscription(SERVERS))]
fn bench_parse_subscription(body: String) -> ParsedSubscription {
    black_box(parse_subscription(black_box(&body), "sub").unwrap())
}

fn merge_input() -> (Store, ParsedSubscription) {
    (
        fixtures::store(SERVERS),
        parse_subscription(&fixtures::subscription(SERVERS), "sub").unwrap(),
    )
}
#[library_benchmark]
#[bench::servers_2000(merge_input())]
fn bench_merge(input: (Store, ParsedSubscription)) -> Store {
    black_box(merge(black_box(&input.0), "sub", input.1))
}

#[library_benchmark]
#[bench::servers_2000(fixtures::store(SERVERS))]
fn bench_config_generate(store: Store) -> serde_json::Value {
    black_box(config::generate(black_box(&store), "bench-secret-0123456789012345678901").unwrap())
}

fn selection_input() -> (tokio::runtime::Runtime, Vec<Server>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    (runtime, fixtures::servers(200))
}
#[library_benchmark]
#[bench::initial_200(selection_input())]
fn bench_selection_initial(input: (tokio::runtime::Runtime, Vec<Server>)) -> String {
    let (runtime, servers) = input;
    let outcome = runtime.block_on(selection::run(
        &InstantProbe,
        black_box(&servers),
        None,
        10,
        &CancellationToken::new(),
        |_| {},
    ));
    black_box(outcome.unwrap().selected)
}

library_benchmark_group!(
    name = core,
    benchmarks = [
        bench_parse_uri,
        bench_parse_subscription,
        bench_merge,
        bench_config_generate,
        bench_selection_initial
    ]
);

main!(
    config = LibraryBenchmarkConfig::default().tool(
        Callgrind::default()
            .args(["cache-sim=yes"])
            .format([CallgrindMetrics::All])
            .soft_limits([(EventKind::Ir, 10f64)])
    ),
    library_benchmark_groups = core
);
