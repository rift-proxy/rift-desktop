import assert from "node:assert/strict";
import test from "node:test";
import { coverageTotals } from "./coverage-summary.mjs";

function report() {
  const metric = { count: 10, covered: 8, notcovered: 2, percent: 80 };
  return { data: [{ files: [{ filename: "/private/source.rs", text: "SENSITIVE-CANARY" }],
    totals: { lines: { ...metric }, functions: { ...metric }, regions: { ...metric },
      secret: "SENSITIVE-CANARY" } }] };
}

test("coverage publication retains numeric totals but no paths, source or unknown strings", () => {
  const input = report();
  input.data[0].totals.lines.path = "/private/SENSITIVE-CANARY";
  const output = coverageTotals(input);
  assert.equal(output.metrics.lines.count, 10);
  assert.equal(output.metrics.functions.percent, 80);
  assert.equal(JSON.stringify(output).includes("SENSITIVE-CANARY"), false);
  assert.equal(JSON.stringify(output).includes("/private/"), false);
});

test("missing or nonnumeric coverage cannot become successful evidence", () => {
  for (const mutate of [
    (value) => { value.data = []; },
    (value) => { value.data.push(value.data[0]); },
    (value) => { delete value.data[0].totals.lines; },
    (value) => { value.data[0].totals.lines.count = "10"; },
    (value) => { value.data[0].totals.regions.percent = 101; },
    (value) => { value.data[0].totals.functions.covered = -1; },
    (value) => { value.data[0].totals.lines.percent = NaN; },
  ]) {
    const input = report(); mutate(input);
    assert.throws(() => coverageTotals(input));
  }
});
