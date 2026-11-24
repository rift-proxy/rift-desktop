import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { summarizeFrontendEvidence } from "./frontend-evidence.mjs";

const marker = "DO_NOT_RETAIN_LOG_OR_PATH";

test("coverage evidence keeps only aggregate numeric metrics", () => {
  const metric = { total: 12, covered: 9, skipped: 0, pct: 75, log: marker };
  const output = summarizeFrontendEvidence("coverage", {
    total: { lines: metric, statements: metric, functions: metric, branches: metric, raw: marker },
    [marker]: { source: marker },
  });
  assert.deepEqual(output.totals.lines, { total: 12, covered: 9, skipped: 0, pct: 75 });
  assert.deepEqual(Object.keys(output.totals), ["lines", "statements", "functions", "branches"]);
  assert.ok(!JSON.stringify(output).includes(marker));
});

function coverageReport(metric) {
  return { total: Object.fromEntries(["lines", "statements", "functions", "branches"].map((name) => [name, metric])) };
}

test("coverage counts are safe integers within totals and percentages match Istanbul", () => {
  const valid = { total: 12, covered: 9, skipped: 0, pct: 75 };
  for (const metric of ["lines", "statements", "functions", "branches"]) {
    for (const key of ["total", "covered", "skipped"]) {
      for (const value of [0.5, Number.MAX_SAFE_INTEGER + 1, -1, NaN, Infinity, "1", null]) {
        const report = coverageReport(valid);
        report.total[metric] = { ...valid, [key]: value };
        assert.throws(() => summarizeFrontendEvidence("coverage", report), /Invalid/);
      }
    }
    for (const invalid of [
      { covered: 13 }, { skipped: 13 }, { pct: 101 }, { pct: 74.99 },
      { total: 0, covered: 1, pct: 100 }, { total: 0, skipped: 1, covered: 0, pct: 100 },
      { total: 0, covered: 0, pct: 0 },
    ]) {
      const report = coverageReport(valid);
      report.total[metric] = { ...valid, ...invalid };
      assert.throws(() => summarizeFrontendEvidence("coverage", report), /Inconsistent/);
    }
  }
  for (const metric of [
    { total: 0, covered: 0, skipped: 0, pct: 100 },
    { total: 3, covered: 1, skipped: 0, pct: 33.33 },
    { total: 3, covered: 2, skipped: 0, pct: 66.66 },
    { total: 3, covered: 2, skipped: 1, pct: 66.66 },
    { total: 1, covered: 0, skipped: 0, pct: 0 },
    { total: 1, covered: 1, skipped: 0, pct: 100 },
  ]) {
    const output = summarizeFrontendEvidence("coverage", coverageReport(metric));
    assert.deepEqual(output.totals.lines, metric);
  }
});

for (const kind of ["e2e", "visual"]) {
  test(`${kind} counts are integers but millisecond durations may be fractional`, () => {
    const stats = { expected: 5, unexpected: 1, flaky: 0, skipped: 2, duration: 12.5 };
    for (const key of ["expected", "unexpected", "flaky", "skipped"]) {
      for (const value of [0.5, Number.MAX_SAFE_INTEGER + 1]) {
        assert.throws(() => summarizeFrontendEvidence(kind, { stats: { ...stats, [key]: value } }), /Invalid frontend evidence count/);
      }
    }
    assert.equal(summarizeFrontendEvidence(kind, { stats }).durationMs, 12.5);
  });
}

test("CLI rejects malformed counts and coverage before creating output, retaining valid aggregates", (t) => {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-frontend-evidence-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const input = path.join(directory, "input.json");
  const entry = fileURLToPath(new URL("./frontend-evidence.mjs", import.meta.url));
  const valid = { total: 3, covered: 2, skipped: 0, pct: 66.66 };
  const fixtures = [
    ["coverage", coverageReport({ ...valid, total: 3.5 })],
    ["coverage", coverageReport({ ...valid, covered: 4 })],
    ["coverage", coverageReport({ ...valid, skipped: 4 })],
    ["coverage", coverageReport({ ...valid, pct: 101 })],
    ["coverage", coverageReport({ ...valid, pct: 66.67 })],
    ...["e2e", "visual"].map(kind => [kind, { stats: { expected: 0.5, unexpected: 0, flaky: 0, skipped: 0, duration: 1.25 } }]),
  ];
  for (const [index, [kind, report]] of fixtures.entries()) {
    const output = path.join(directory, `rejected-${index}`, "evidence.json");
    writeFileSync(input, JSON.stringify(report));
    const result = spawnSync(process.execPath, [entry, kind, input, output], { encoding: "utf8" });
    assert.notEqual(result.status, 0, result.stdout);
    assert.ok(!existsSync(path.dirname(output)));
  }
  const output = path.join(directory, "accepted", "evidence.json");
  writeFileSync(input, JSON.stringify(coverageReport(valid)));
  const result = spawnSync(process.execPath, [entry, "coverage", input, output], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(readFileSync(output, "utf8")), {
    schema: 1, kind: "coverage", totals: coverageReport(valid).total,
  });
});

test("mutation evidence discards sources, paths, test names and failure text", () => {
  const output = summarizeFrontendEvidence("mutation", {
    files: {
      [marker]: {
        source: marker,
        mutants: [
          { status: "Killed", statusReason: marker, replacement: marker },
          { status: "Survived", statusReason: marker },
          { status: "Killed", testsCompleted: 3, killedBy: [marker] },
        ],
      },
    },
    testFiles: { [marker]: { source: marker } },
    rawLogs: marker,
  });
  assert.equal(output.total, 3);
  assert.equal(output.counts.Killed, 2);
  assert.equal(output.counts.Survived, 1);
  assert.ok(!JSON.stringify(output).includes(marker));
});

for (const kind of ["e2e", "visual"]) {
  test(`${kind} evidence discards stdout, stderr, traces and error messages`, () => {
    const output = summarizeFrontendEvidence(kind, {
      config: { rootDir: marker, metadata: { raw: marker } },
      stats: { expected: 5, unexpected: 1, flaky: 0, skipped: 2, duration: 12.5, raw: marker },
      suites: [{ title: marker, tests: [{ stdout: marker, stderr: marker, trace: marker }] }],
      errors: [{ message: marker }],
    });
    assert.deepEqual(output, {
      schema: 1,
      kind,
      counts: { expected: 5, unexpected: 1, flaky: 0, skipped: 2 },
      durationMs: 12.5,
    });
    assert.ok(!JSON.stringify(output).includes(marker));
  });
}

test("missing, malformed and unknown evidence fails instead of reporting a pass", () => {
  for (const value of [undefined, null, [], "raw text"]) {
    assert.throws(() => summarizeFrontendEvidence("coverage", value));
  }
  assert.throws(() => summarizeFrontendEvidence("coverage", {}));
  assert.throws(() => summarizeFrontendEvidence("mutation", { files: {} }));
  assert.throws(() => summarizeFrontendEvidence("mutation", {
    files: { file: { mutants: [{ status: marker }] } },
  }));
  for (const value of [marker, null, -1, Infinity, NaN]) {
    assert.throws(() => summarizeFrontendEvidence("visual", {
      stats: { expected: value, unexpected: 0, flaky: 0, skipped: 0, duration: 1 },
    }));
  }
  assert.throws(() => summarizeFrontendEvidence(marker, {}));
});
