import assert from "node:assert/strict";
import test from "node:test";
import { format, simplify, summarize } from "./cpu-profile-summary.mjs";

test("names lose hashes, generic arguments and closure suffixes", () => {
  assert.equal(
    simplify(
      "rift_core::application::Application::worker::{closure#0}::h0123456789abcdef",
    ),
    "rift_core::application::Application::worker",
  );
  assert.equal(
    simplify(
      "<alloc::vec::Vec<rift_core::model::Server> as core::clone::Clone>::clone",
    ),
    "<alloc::vec::Vec as core::clone::Clone>::clone",
  );
  assert.equal(
    simplify("<rift_core::application::Application>::publish_store"),
    "rift_core::application::Application::publish_store",
  );
});

// Two libraries: the workload binary and libc. Addresses are relative to each library.
const syms = {
  string_table: [
    "perf_workload::main",
    "perf_workload::coordinator::{closure#0}",
    "rift_core::application::Application::worker::{closure#0}",
    "tokio::runtime::block_on",
    "memcmp",
    "perf_workload::scrub_errors",
    "rift_core::journal::scrub_with",
    "rift_core::journal::helper_inlined",
  ],
  data: [
    {
      debug_name: "perf_workload",
      symbol_table: [
        // Like a real profile: main starts with an inlined call (here scrub_errors).
        {
          rva: 0x100,
          size: 0x10,
          symbol: 0,
          frames: [{ function: 5 }, { function: 0 }],
        },
        { rva: 0x200, size: 0x10, symbol: 1 },
        { rva: 0x300, size: 0x10, symbol: 2 },
        { rva: 0x400, size: 0x10, symbol: 3 },
        { rva: 0x500, size: 0x10, symbol: 5 },
        // Inline frames describe the symbol's start address only (innermost first).
        {
          rva: 0x600,
          size: 0x10,
          symbol: 6,
          frames: [{ function: 7 }, { function: 6 }],
        },
      ],
    },
    {
      debug_name: "libc.so.6",
      symbol_table: [{ rva: 0x10, size: 0x10, symbol: 4 }],
    },
  ],
};
// frame -> [func, address]; func -> resource -> lib.
const frames = [
  [0, 0x104], // 0 main, a return address inside it (not its start)
  [0, 0x200], // 1 coordinator
  [0, 0x400], // 2 block_on
  [0, 0x300], // 3 worker
  [1, 0x10], // 4 memcmp
  [0, 0x500], // 5 scrub_errors
  [0, 0x608], // 6 scrub_with, past its start: inline frames do not apply
  [2, 0], // 7 kernel frame, no library
  [0, 0x600], // 8 scrub_with start address: its inline helper applies
];
// stack -> [prefix, frame]
const stacks = [
  [null, 0], // 0 main
  [0, 1], // 1 main > coordinator
  [1, 4], // 2 main > coordinator > memcmp
  [0, 2], // 3 main > block_on
  [3, 3], // 4 main > block_on > worker
  [4, 4], // 5 ... > worker > memcmp
  [0, 5], // 6 main > scrub_errors
  [6, 6], // 7 ... > scrub_with
  [0, 7], // 8 main > kernel
  [0, 8], // 9 main > helper inlined at scrub_with's start
];
const profile = {
  libs: [{ debugName: "perf_workload" }, { debugName: "libc.so.6" }],
  threads: [
    {
      stringArray: ["[kernel]"],
      frameTable: {
        func: frames.map(([f]) => f),
        address: frames.map(([, a]) => a),
      },
      funcTable: { resource: [0, 1, -1], name: [null, null, 0] },
      resourceTable: { lib: [0, 1] },
      stackTable: {
        prefix: stacks.map(([p]) => p),
        frame: stacks.map(([, f]) => f),
      },
      samples: {
        length: 7,
        stack: [2, 5, 5, 2, 7, 8, 9],
        time: [0, 1, 2, 3, 10, 20, 30],
        weight: null,
      },
    },
  ],
};

test("spawned tasks count toward the scenario running at that time", () => {
  const summary = summarize(profile, syms);
  assert.equal(summary.total, 7);
  // main's start-address inline frame (scrub_errors) must not claim every sample.
  assert.deepEqual(summary.scenarios, [
    ["coordinator", 4],
    ["(setup and shutdown)", 2],
    ["scrub_errors", 1],
  ]);
});

test("time is charged to the nearest Rift function, inlined frames included", () => {
  const summary = summarize(profile, syms);
  assert.deepEqual(summary.nearest, [
    ["perf_workload::coordinator", 2],
    ["rift_core::application::Application::worker", 2],
    ["rift_core::journal::scrub_with", 1],
    ["(outside Rift code: startup, runtime)", 1],
    ["rift_core::journal::helper_inlined", 1],
  ]);
  assert.deepEqual(summary.self[0], ["memcmp", 4]);
  assert.match(format(summary), /57\.1%\s+coordinator/);
});

test("an empty recording fails instead of reporting a successful zero-sample profile", () => {
  assert.throws(() => summarize({ threads: [] }, syms), /no CPU samples/);
  const empty = structuredClone(profile);
  empty.threads[0].samples.stack.fill(null);
  assert.throws(() => summarize(empty, syms), /no CPU samples/);
});
