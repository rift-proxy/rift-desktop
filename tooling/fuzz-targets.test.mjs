import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const root = new URL("..", import.meta.url).pathname;
const helper = join(root, "tooling", "fuzz-targets.sh");
const PERMANENT = [
  "subscription",
  "persisted_state",
  "ipc_payload",
  "clash_api",
  "helper_frame",
  "dns_query",
  "sidecar_log",
  "journal_details",
];

// Builds a throwaway project/fuzz with a fake `cargo` and runs fuzz_replay_targets in it.
// `options.list`: what `cargo fuzz list` prints; `options.listStatus`: its exit status;
// `options.seeds`: target -> seed files (an empty array is an empty directory, a missing
// key is a missing directory); `options.regressions`: targets with a regressions directory;
// `options.runStatus`: exit status of `cargo fuzz run`.
function replay(t, options) {
  const directory = mkdtempSync(join(tmpdir(), "rift-fuzz-targets-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const fuzz = join(directory, "project", "fuzz");
  mkdirSync(fuzz, { recursive: true });
  const seeds = options.seeds ?? Object.fromEntries(PERMANENT.map((name) => [name, ["a.json"]]));
  for (const [target, files] of Object.entries(seeds)) {
    mkdirSync(join(fuzz, "seeds", target), { recursive: true });
    for (const file of files) writeFileSync(join(fuzz, "seeds", target, file), "{}");
  }
  for (const target of options.regressions ?? []) {
    mkdirSync(join(fuzz, "regressions", target), { recursive: true });
    writeFileSync(join(fuzz, "regressions", target, "crash.json"), "{}");
  }
  const calls = join(directory, "calls.log");
  const bin = join(directory, "bin");
  mkdirSync(bin);
  const listing = join(directory, "list.txt");
  writeFileSync(listing, (options.list ?? PERMANENT).map((name) => `${name}\n`).join(""));
  const fake = join(bin, "cargo");
  writeFileSync(
    fake,
    `#!/bin/bash
echo "$*" >> "${calls}"
if [ "$1 $2" = "fuzz list" ]; then
  cat "${listing}"
  exit ${options.listStatus ?? 0}
fi
exit ${options.runStatus ?? 0}
`,
  );
  chmodSync(fake, 0o755);
  const result = spawnSync(
    "/bin/bash",
    ["-c", `source "$1"; cd "$2" && fuzz_replay_targets`, "bash", helper, fuzz],
    { encoding: "utf8", env: { ...process.env, PATH: `${bin}:${process.env.PATH}` } },
  );
  let runs = [];
  try {
    runs = readFileSync(calls, "utf8")
      .trim()
      .split("\n")
      .filter((line) => line.startsWith("fuzz run "));
  } catch {
    // cargo was never called.
  }
  return { result, runs };
}

test("replays every permanent target with its seeds and regressions only", (t) => {
  const { result, runs } = replay(t, { regressions: ["persisted_state"] });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(runs, [
    "fuzz run subscription seeds/subscription -- -runs=0",
    "fuzz run persisted_state seeds/persisted_state regressions/persisted_state -- -runs=0",
    "fuzz run ipc_payload seeds/ipc_payload -- -runs=0",
    "fuzz run clash_api seeds/clash_api -- -runs=0",
    "fuzz run helper_frame seeds/helper_frame -- -runs=0",
    "fuzz run dns_query seeds/dns_query -- -runs=0",
    "fuzz run sidecar_log seeds/sidecar_log -- -runs=0",
    "fuzz run journal_details seeds/journal_details -- -runs=0",
  ]);
});

test("a failing cargo fuzz list is a failure, not an empty loop", (t) => {
  const { result, runs } = replay(t, { listStatus: 1 });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /cargo fuzz list failed/);
  assert.deepEqual(runs, []);
});

test("an empty target list is a failure", (t) => {
  const { result, runs } = replay(t, { list: [] });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /no targets/);
  assert.deepEqual(runs, []);
});

test("every permanent target must be listed", (t) => {
  for (const missing of PERMANENT) {
    const list = PERMANENT.filter((name) => name !== missing);
    const { result, runs } = replay(t, { list });
    assert.equal(result.status, 1, `${missing} dropped from the list`);
    assert.match(result.stderr, new RegExp(`${missing} is not built`));
    assert.deepEqual(runs, []);
  }
});

test("a target without a seeds directory fails", (t) => {
  const { result } = replay(t, {
    seeds: { subscription: ["a.txt"], persisted_state: ["a.json"] },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /ipc_payload has no seed corpus/);
});

test("an existing but empty seeds directory fails", (t) => {
  const { result, runs } = replay(t, {
    seeds: { subscription: [], persisted_state: ["a.json"], ipc_payload: ["a.json"] },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /subscription has an empty seed corpus/);
  assert.deepEqual(runs, []);
});

test("a seeds directory holding only a placeholder dotfile counts as empty", (t) => {
  const { result } = replay(t, {
    seeds: { subscription: [".gitkeep"], persisted_state: ["a.json"], ipc_payload: ["a.json"] },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /subscription has an empty seed corpus/);
});

test("a crashing replay fails the step", (t) => {
  const { result, runs } = replay(t, { runStatus: 77 });
  assert.equal(result.status, 1);
  assert.equal(runs.length, 1, "stops at the first failing target");
});


