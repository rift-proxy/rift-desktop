import assert from "node:assert/strict";
import { readFileSync, rmSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { validateAssets } from "./release-check.mjs";
import { prepareUpdateAssets } from "./prepare-update-assets.mjs";
import { fixture } from "./release-fixture.mjs";
test("stage writes only three verified assets and correct Windows updater URL", (t) => {
  const f = fixture(t);
  const names = prepareUpdateAssets("v1.2.3", f.bundle, f.output, f.options);
  for (const name of names.filter((name) => name !== "latest.json"))
    assert.deepEqual(readFileSync(path.join(f.output, name)), readFileSync(path.join(f.bundle, name)));
  assert.deepEqual(names, ["Rift_1.2.3_x64-setup.exe", "Rift_1.2.3_x64-setup.exe.sig", "latest.json"]);
  const manifest = JSON.parse(readFileSync(path.join(f.output, "latest.json")));
  assert.equal(manifest.platforms["windows-x86_64"].url, "https://github.com/public-owner/rift-desktop/releases/download/v1.2.3/Rift_1.2.3_x64-setup.exe");
  assert.deepEqual(validateAssets("v1.2.3", f.output, f.options), names);
  assert.throws(() => prepareUpdateAssets("v1.2.3", f.bundle, f.output, f.options), /EEXIST/);
});

test("stage refuses wrong key, wrong version, missing signatures and extra symbol files", (t) => {
  for (const mutate of [
    (f) => f.put("bundle/debug.pdb", "private symbols"),
    (f) => rmSync(path.join(f.bundle, "Rift_1.2.3_x64-setup.exe.sig")),
    (f) => f.put("bundle/Rift_1.2.3_x64-setup.exe.sig", "garbage"),
    (f) => f.put("bundle/Rift_1.2.4_x64-setup.exe", f.data),
    (f) => f.put("bundle/Rift_1.2.3_x64-setup.exe", Buffer.alloc(256)),
  ]) {
    const f = fixture(t);
    mutate(f);
    assert.throws(() => prepareUpdateAssets("v1.2.3", f.bundle, f.output, f.options));
  }
});

test("staging and final validation reject renamed older signed releases and unbound signatures", (t) => {
  for (const comment of ["timestamp:1234", "timestamp:1234\tversion:1.2.2",
    "timestamp:1234\tversion:1.2.3\tversion:1.2.3"]) {
    const f = fixture(t);
    prepareUpdateAssets("v1.2.3", f.bundle, f.output, f.options);
    const signature = f.signature(f.data, "ED", comment);
    f.put("bundle/Rift_1.2.3_x64-setup.exe.sig", signature);
    assert.throws(() => prepareUpdateAssets("v1.2.3", f.bundle, path.join(f.base, "renamed"), f.options), /version/i);
    f.put("release/Rift_1.2.3_x64-setup.exe.sig", signature);
    const manifest = JSON.parse(readFileSync(path.join(f.output, "latest.json")));
    manifest.platforms["windows-x86_64"].signature = signature;
    f.put("release/latest.json", JSON.stringify(manifest));
    assert.throws(() => validateAssets("v1.2.3", f.output, f.options), /version/i);
  }
});

test("preparer CLI rejects missing, malformed and surplus arguments", () => {
  for (const args of [[], ["v1.2.3", "bundle"], ["v1.2.3-beta", "bundle", "output"], ["v1.2.3", "bundle", "output", "extra"]]) {
    const result = spawnSync(process.execPath, [path.join(import.meta.dirname, "prepare-update-assets.mjs"), ...args], { encoding: "utf8" });
    assert.equal(result.status, 1);
    assert.ok(result.stderr.length > 0);
  }
});

