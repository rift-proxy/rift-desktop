import assert from "node:assert/strict";
import test from "node:test";
import { mutationSummary } from "./mutation-summary.mjs";

const valid = () => ({ cargo_mutants_version: "27.1.0", total_mutants: 5, caught: 2, missed: 1, timeout: 1, unviable: 1, success: 0 });

test("exports only fixed tool identity and allowlisted numeric counters", () => {
  const source = { ...valid(), outcomes: [{ log_path: "private-process-log", diff_path: "private-diff", summary: "raw-command-output" }], start_time: "private-time", extra: "untrusted-text" };
  assert.deepEqual(mutationSummary(source), {
    schema: 1, tool: "cargo-mutants", version: "27.1.0", total_mutants: 5,
    caught: 2, missed: 1, timeout: 1, unviable: 1, success: 0,
  });
});

test("missing, malformed and unsupported outcomes cannot produce misleading aggregates", () => {
  for (const input of [null, {}, { ...valid(), cargo_mutants_version: "unknown" },
    { ...valid(), caught: "2" }, { ...valid(), timeout: -1 },
    { ...valid(), total_mutants: Number.MAX_SAFE_INTEGER + 1 }, { ...valid(), missed: 1.5 }])
    assert.throws(() => mutationSummary(input));
});
