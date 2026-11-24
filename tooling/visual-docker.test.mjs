import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

for (const reporting of [false, true]) {
  test(`visual Docker ${reporting ? "forwards" : "does not add"} explicitly selected JSON and HTML reports`, { skip: process.platform === "win32" }, (t) => {
    const directory = mkdtempSync(path.join(os.tmpdir(), "visual-docker-test-"));
    t.after(() => rmSync(directory, { recursive: true, force: true }));
    const record = path.join(directory, "args.txt");
    const docker = path.join(directory, "docker");
    writeFileSync(docker, `#!/bin/sh\nprintf '%s\\n' "$@" > "${record}"\n`);
    chmodSync(docker, 0o755);
    const env = { ...process.env, PATH: `${directory}${path.delimiter}${process.env.PATH}` };
    delete env.PLAYWRIGHT_JSON_OUTPUT_FILE;
    delete env.PLAYWRIGHT_HTML_OUTPUT_DIR;
    delete env.PLAYWRIGHT_HTML_OPEN;
    if (reporting) {
      env.PLAYWRIGHT_JSON_OUTPUT_FILE = "reports/ci-raw/visual.json";
      env.PLAYWRIGHT_HTML_OUTPUT_DIR = "playwright-report";
      env.PLAYWRIGHT_HTML_OPEN = "never";
    }
    const result = spawnSync(process.execPath, [
      path.join(import.meta.dirname, "visual-docker.mjs"),
      "--reporter=json,html",
      "--output=test-results/visual",
      "--update-snapshots=none",
    ], { env, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    const args = readFileSync(record, "utf8").trim().split("\n");
    assert.equal(args.includes("PLAYWRIGHT_JSON_OUTPUT_FILE"), reporting);
    assert.ok(args.includes("--reporter=json,html"));
    assert.equal(args.includes("PLAYWRIGHT_HTML_OUTPUT_DIR"), reporting);
    assert.equal(args.includes("PLAYWRIGHT_HTML_OPEN"), reporting);
    assert.ok(args.includes("--update-snapshots=none"));
    assert.ok(args.includes("--output=test-results/visual"));
    assert.ok(args.some((arg) => /^mcr\.microsoft\.com\/playwright:v[\d.]+-noble$/.test(arg)));
    assert.ok(!args.includes("--update-snapshots"));
    const forwarded = args.filter((_, index) => args[index - 1] === "--env");
    assert.deepEqual(forwarded, reporting
      ? ["HOME=/tmp", "CI", "PLAYWRIGHT_JSON_OUTPUT_FILE", "PLAYWRIGHT_HTML_OUTPUT_DIR", "PLAYWRIGHT_HTML_OPEN"]
      : ["HOME=/tmp", "CI"]);
  });
}
