import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

const root = path.resolve(import.meta.dirname, "..");
const desktop = path.join(root, "desktop");

test("a failed frontend assertion still produces coverage without turning the run green", { timeout: 120_000 }, (t) => {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-coverage-failure-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  symlinkSync(path.join(desktop, "node_modules"), path.join(directory, "node_modules"), process.platform === "win32" ? "junction" : "dir");
  writeFileSync(path.join(directory, "branch.mjs"), "export const choose = (active) => active ? 1 : 2;\n");
  writeFileSync(path.join(directory, "failure.test.mjs"), `
import { choose } from "./branch.mjs";
it("retains the failed assertion", () => expect(choose(true)).toBe(2));
`);
  const config = path.join(directory, "vitest.config.ts");
  writeFileSync(config, `
import config from ${JSON.stringify(path.join(desktop, "vitest.config.ts").replaceAll("\\", "/"))};
export default {
  ...config,
  root: ${JSON.stringify(directory)},
  test: {
    ...config.test,
    projects: [{
      envDir: false,
      test: { name: "failure-probe", globals: true, environment: "node", include: ["failure.test.mjs"] },
    }],
    coverage: {
      ...config.test.coverage,
      include: ["branch.mjs"],
      exclude: [],
      reporter: ["json-summary"],
      reportsDirectory: ${JSON.stringify(path.join(directory, "coverage"))},
    },
  },
};
`);
  const result = spawnSync(process.execPath, [path.join(desktop, "node_modules/vitest/vitest.mjs"), "run", "--config", config, "--coverage"], {
    cwd: desktop,
    encoding: "utf8",
    timeout: 90_000,
    env: { ...process.env, CI: "true", STORYBOOK_DISABLE_TELEMETRY: "1" },
  });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /retains the failed assertion/);
  const summary = JSON.parse(readFileSync(path.join(directory, "coverage/coverage-summary.json"), "utf8"));
  assert.ok(summary.total.lines.total > 0);
  assert.ok(summary.total.lines.covered > 0);
  assert.ok(summary.total.branches.pct < 100);
});
