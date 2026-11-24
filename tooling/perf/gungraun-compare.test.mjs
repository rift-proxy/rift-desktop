import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { compareInstructions, instructionCounts } from "./gungraun-compare.mjs";

const summary = (instructions = 100, overrides = {}) => ({
  version: "6", module_path: "core_gungraun::core::bench_parse_uri", id: "vmess",
  profiles: [{ tool: "Callgrind", summaries: { total: { summary: {
    Callgrind: { Ir: { metrics: { Left: { Int: instructions } }, diffs: null } },
  } } } }], ...overrides,
});
function home(t, values) {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-gungraun-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  values.forEach((value, index) => {
    const folder = path.join(directory, String(index));
    mkdirSync(folder);
    writeFileSync(path.join(folder, "summary.json"), JSON.stringify(value));
  });
  return directory;
}

test("reads fresh instruction counts by stable benchmark identity, not filesystem path", (t) => {
  const counts = instructionCounts(home(t, [summary(), summary(200, { id: "vless_reality" })]));
  assert.equal(counts.size, 2);
  assert.equal(counts.get(JSON.stringify(["core_gungraun::core::bench_parse_uri", "vmess"])), 100);
});

test("strict greater-than-ten-percent regression with exact integer boundary", () => {
  const base = new Map([["a", 100]]);
  assert.equal(compareInstructions(base, new Map([["a", 110]]))[0].regressed, false);
  assert.equal(compareInstructions(base, new Map([["a", 111]]))[0].regressed, true);
  assert.equal(compareInstructions(base, new Map([["a", 50]]))[0].regressed, false);
});

test("missing, empty or changed inventories cannot silently establish a new baseline", (t) => {
  assert.throws(() => instructionCounts(path.join(home(t, []), "missing")));
  assert.throws(() => instructionCounts(home(t, [])), /No Gungraun summaries/);
  for (const [base, current] of [
    [new Map(), new Map()],
    [new Map([["a", 100]]), new Map([["b", 100]])],
    [new Map([["a", 100]]), new Map([["a", 100], ["b", 100]])],
    [new Map([["a", 100], ["b", 100]]), new Map([["a", 100]])],
  ]) assert.throws(() => compareInstructions(base, current), /Incompatible/);
});

test("rejects stale comparison output, absent metrics, duplicates and unsupported schema", (t) => {
  const stale = summary();
  stale.profiles[0].summaries.total.summary.Callgrind.Ir.metrics = { Both: [{ Int: 100 }, { Int: 90 }] };
  for (const values of [
    [summary(0)], [summary(-1)], [summary(1.5)], [summary(Number.MAX_SAFE_INTEGER + 1)],
    [summary(100, { version: "5" })], [summary(100, { profiles: [] })],
    [summary(), summary()], [stale],
  ]) assert.throws(() => instructionCounts(home(t, values)));
});
