import assert from "node:assert/strict";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import {
  compare,
  expand,
  format,
  measure,
  validateComponentEvidence,
} from "./size-budget.mjs";

function fixture() {
  const base = mkdtempSync(path.join(tmpdir(), "rift-size-"));
  mkdirSync(path.join(base, "dist/assets"), { recursive: true });
  writeFileSync(path.join(base, "dist/assets/a.js"), "x".repeat(100));
  writeFileSync(path.join(base, "dist/assets/b.js"), "x".repeat(300));
  writeFileSync(path.join(base, "dist/assets/c.css"), "x".repeat(50));
  return base;
}

test("globs, sums and largest file; first existing candidate wins", () => {
  const base = fixture();
  assert.equal(expand("dist/assets/*.js", base).length, 2);
  assert.deepEqual(expand("dist/missing/*.js", base), []);
  assert.equal(measure({ paths: ["dist/assets/*.js"] }, base), 400);
  assert.equal(
    measure({ paths: ["dist/assets/*.js"], largest: true }, base),
    300,
  );
  assert.equal(measure({ paths: ["nope.exe", "dist/assets/c.css"] }, base), 50);
  assert.equal(measure({ paths: ["nope.exe"] }, base), null);
});

const budgets = {
  tolerance: 0.02,
  artifacts: {
    js: { bytes: 1000 },
    exe: { bytes: 1000, tolerance: 0.05 },
    css: { bytes: 1000 },
    installer: { bytes: 1000 },
    fresh: { bytes: 0 },
  },
};

test("growth over the tolerance fails; per-artefact tolerance and shrink are reported", () => {
  const rows = compare(budgets, {
    js: 1021,
    exe: 1040,
    css: 900,
    installer: null,
    fresh: 5,
  });
  const by = Object.fromEntries(rows.map((r) => [r.name, r]));
  assert.equal(by.js.status, "grew");
  assert.equal(by.js.fail, true);
  assert.equal(by.exe.status, "ok");
  assert.equal(by.css.status, "shrank");
  assert.equal(by.css.fail, false);
  assert.equal(by.installer.status, "missing");
  assert.equal(by.installer.fail, false);
  assert.equal(by.fresh.status, "new");
  assert.match(format(rows), /FAIL  js: .*\+2\.10%.*budget/);
});

test("the exact tolerance passes, required missing and report-only growth behave", () => {
  const rows = compare(
    budgets,
    { js: 1020, exe: 2000, css: 1000, installer: null, fresh: null },
    { require: ["installer"], reportOnly: ["exe"] },
  );
  const by = Object.fromEntries(rows.map((r) => [r.name, r]));
  assert.equal(by.js.fail, false);
  assert.equal(by.exe.status, "grew");
  assert.equal(by.exe.fail, false);
  assert.equal(by.installer.fail, true);
  assert.match(format(rows), /warn  exe/);
  assert.match(format(rows), /FAIL  installer: not built/);
});

test("measured Xray allowance preserves the original installer base and all other budgets", () => {
  const proof = JSON.parse(
    readFileSync(
      new URL(
        "./installer-component-cost.json",
        import.meta.url,
      ),
    ),
  );
  const current = JSON.parse(
    readFileSync(new URL("./budgets.json", import.meta.url)),
  );
  validateComponentEvidence(current);
  const result = compare(current, {
    installer: proof.full_installer.bytes,
    rift_exe: 23176192 * 2,
  });
  const installer = result.find((r) => r.name === "installer");
  assert.equal(current.artifacts.installer.bytes, 25386081);
  assert.equal(installer.baseBaseline, 25386081);
  assert.equal(installer.componentAllowance, 9811158);
  assert.equal(installer.baseline, 35197239);
  assert.equal(installer.fail, false);
  assert.equal(result.find((r) => r.name === "rift_exe").fail, true);
  assert.match(format([installer]), /original base .*measured Xray/);
  const tooBig = compare(current, {
    installer: Math.round(installer.baseline * 1.03) + 1,
  }).find((r) => r.name === "installer");
  assert.equal(tooBig.fail, true);
  for (const mutate of [
    (c) => {
      c.artifacts.installer.component_allowances[0].bytes++;
    },
    (c) => {
      c.artifacts.installer.bytes++;
    },
    (c) => {
      c.artifacts.installer.component_allowances[0].binary_sha256 = "0".repeat(
        64,
      );
    },
    (c) => {
      c.artifacts.installer.component_allowances[0].bytes = -1;
    },
  ]) {
    const bad = structuredClone(current);
    mutate(bad);
    assert.throws(() => {
      validateComponentEvidence(bad);
      compare(bad, { installer: proof.full_installer.bytes });
    });
  }
});

test("size-budget CLI rejects malformed update arguments without changing the baseline", (t) => {
  const base = fixture();
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const tooling = path.join(base, "tooling");
  const perf = path.join(tooling, "perf");
  mkdirSync(perf, { recursive: true });
  copyFileSync(new URL("./size-budget.mjs", import.meta.url), path.join(perf, "size-budget.mjs"));
  copyFileSync(new URL("../sidecar.mjs", import.meta.url), path.join(tooling, "sidecar.mjs"));
  const budgetFile = path.join(perf, "budgets.json");
  const original = `${JSON.stringify({
    tolerance: 0.02,
    artifacts: { js: { bytes: 1, paths: ["dist/assets/*.js"] } },
  }, null, 2)}\n`;
  writeFileSync(budgetFile, original);
  for (const args of [
    ["--update", "--require"],
    ["--update", "--report-only"],
    ["--update", "--require", "--report-only", "js"],
    ["--update", "--require", "unknown"],
    ["--update", "--report-only", "js,"],
    ["--update", "--typo"],
    ["--update", "--update"],
    ["--update", "--require", "js", "--require", "js"],
    ["--update", "unexpected"],
  ]) {
    const result = spawnSync(process.execPath, [path.join(perf, "size-budget.mjs"), ...args], { encoding: "utf8" });
    assert.ifError(result.error);
    assert.equal(result.status, 1, JSON.stringify(args));
    assert.equal(readFileSync(budgetFile, "utf8"), original, JSON.stringify(args));
  }
  const valid = spawnSync(process.execPath, [
    path.join(perf, "size-budget.mjs"), "--update", "--require", "js", "--report-only", "js",
  ], { encoding: "utf8" });
  assert.ifError(valid.error);
  assert.equal(valid.status, 0, valid.stderr);
  const updated = JSON.parse(readFileSync(budgetFile, "utf8"));
  assert.equal(updated.artifacts.js.bytes, 400);
  assert.equal(updated.tolerance, 0.02);
});
