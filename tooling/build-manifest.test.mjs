import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  MANIFEST_NAME,
  OUTPUT_MARKER,
  listFiles,
  manifestProblems,
  writeManifest,
} from "./build-manifest.mjs";

const COMMIT = "a".repeat(40);
const OTHER = "b".repeat(40);

function output(files = {}) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "build-manifest-test-"));
  const all = {
    "rift-desktop.exe": "exe",
    "bundle/nsis/Rift_0.1.0_x64-setup.exe": "installer",
    "frontend/index.html": "<html>",
    ...files,
  };
  for (const [file, content] of Object.entries(all)) {
    mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    writeFileSync(path.join(dir, file), content);
  }
  return dir;
}
const finish = (dir) => rmSync(dir, { recursive: true, force: true });

test("a complete build of the expected commit has no problems", () => {
  const dir = output();
  try {
    writeManifest(dir, COMMIT);
    assert.deepEqual(manifestProblems(dir, COMMIT), []);
    assert.deepEqual(manifestProblems(dir), []);
  } finally {
    finish(dir);
  }
});

test("the manifest and the marker are not part of the listing", () => {
  const dir = output();
  try {
    writeFileSync(path.join(dir, OUTPUT_MARKER), "");
    writeManifest(dir, COMMIT);
    assert.ok(!listFiles(dir).includes(MANIFEST_NAME));
    assert.ok(!listFiles(dir).includes(OUTPUT_MARKER));
    assert.deepEqual(manifestProblems(dir, COMMIT), []);
  } finally {
    finish(dir);
  }
});

test("the symbols are not part of the manifest, so moving them to the release archive keeps the check green", () => {
  const dir = output({
    "symbols/rift_desktop.pdb": "pdb",
    "symbols/maps/assets/index-ab12.js.map": "{}",
  });
  try {
    assert.ok(!listFiles(dir).some((file) => file.startsWith("symbols")));
    writeManifest(dir, COMMIT);
    assert.deepEqual(manifestProblems(dir, COMMIT), []);
    rmSync(path.join(dir, "symbols"), { recursive: true });
    assert.deepEqual(manifestProblems(dir, COMMIT), []);
  } finally {
    finish(dir);
  }
});

test("a build that did not finish has no manifest", () => {
  const dir = output();
  try {
    assert.match(manifestProblems(dir, COMMIT)[0], /did not finish/);
  } finally {
    finish(dir);
  }
});

test("an output built from another commit is stale", () => {
  const dir = output();
  try {
    writeManifest(dir, OTHER);
    assert.match(manifestProblems(dir, COMMIT).join("\n"), /built from bbbbbbbbbbbb, not from aaaaaaaaaaaa/);
  } finally {
    finish(dir);
  }
});

test("each required part is checked: executable, installer and frontend", () => {
  for (const [missing, pattern] of [
    ["rift-desktop.exe", /rift-desktop\.exe is not in the build/],
    ["bundle/nsis/Rift_0.1.0_x64-setup.exe", /no NSIS installer/],
    ["frontend/index.html", /frontend bundle is not in the build/],
  ]) {
    const dir = output();
    try {
      rmSync(path.join(dir, missing));
      writeManifest(dir, COMMIT);
      assert.match(manifestProblems(dir, COMMIT).join("\n"), pattern, missing);
    } finally {
      finish(dir);
    }
  }
});

test("a file changed, removed or added after the build is reported", () => {
  const dir = output();
  try {
    writeManifest(dir, COMMIT);
    writeFileSync(path.join(dir, "rift-desktop.exe"), "tampered");
    writeFileSync(path.join(dir, "bundle/nsis/Old_0.0.1_x64-setup.exe"), "old installer");
    rmSync(path.join(dir, "frontend/index.html"));
    const problems = manifestProblems(dir, COMMIT).join("\n");
    assert.match(problems, /rift-desktop\.exe changed after the build/);
    assert.match(problems, /Old_0\.0\.1_x64-setup\.exe is not part of the build/);
    assert.match(problems, /frontend\/index\.html is listed but missing/);
  } finally {
    finish(dir);
  }
});

test("an unreadable or foreign manifest is a problem, not a pass", () => {
  const dir = output();
  try {
    writeFileSync(path.join(dir, MANIFEST_NAME), "{not json");
    assert.match(manifestProblems(dir, COMMIT)[0], /missing or unreadable/);
    writeFileSync(path.join(dir, MANIFEST_NAME), JSON.stringify({ schema: 99, files: {} }));
    assert.match(manifestProblems(dir, COMMIT)[0], /unknown format/);
  } finally {
    finish(dir);
  }
});

test("only a full commit SHA is written", () => {
  const dir = output();
  try {
    assert.throws(() => writeManifest(dir, "HEAD"), /40-digit/);
    assert.throws(() => writeManifest(dir, ""), /40-digit/);
  } finally {
    finish(dir);
  }
});

test("the command line writes and checks, and exits non-zero on a problem", () => {
  const dir = output();
  const script = path.join(import.meta.dirname, "build-manifest.mjs");
  const run = (...args) => spawnSync(process.execPath, [script, ...args], { encoding: "utf8" });
  try {
    assert.equal(run("write", dir, COMMIT).status, 0);
    assert.equal(run("check", dir, COMMIT).status, 0);
    const stale = run("check", dir, OTHER);
    assert.equal(stale.status, 1);
    assert.match(stale.stderr, /build again/);
    assert.equal(run("write", dir).status, 2);
  } finally {
    finish(dir);
  }
});
