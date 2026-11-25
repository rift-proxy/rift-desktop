import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { checkEnvironment, checkVersions, repository, tagVersion, validateAssets, verifySignature } from "./release-check.mjs";
import { prepareUpdateAssets } from "./prepare-update-assets.mjs";
import { fixture } from "./release-fixture.mjs";


test("stable tags and repository names reject ambiguous or injectable values", () => {
  assert.equal(tagVersion("v1.2.3"), "1.2.3");
  for (const bad of ["1.2.3", "v01.2.3", "v1.2.3-beta.1", "v1.2.3+build", "v1.2.3\n", "v1.2", "v1.2.3/../x"])
    assert.throws(() => tagVersion(bad));
  assert.equal(repository("public-owner/rift-desktop"), "public-owner/rift-desktop");
  for (const bad of [null, "", "owner/repo/extra", "owner/..", "owner/repo\n", "https://github.com/owner/repo", "owner/repo?token=x"])
    assert.throws(() => repository(bad));
});

test("all manifests and local packages in both locks must agree", (t) => {
  const f = fixture(t);
  assert.equal(checkVersions("v1.2.3", f.base), "1.2.3");
  for (const file of ["Cargo.toml", "desktop/package.json", "desktop/src-tauri/tauri.conf.json", "Cargo.lock", "fuzz/Cargo.lock"]) {
    const before = readFileSync(path.join(f.base, file), "utf8");
    f.put(file, before.replace("1.2.3", "1.2.4"));
    assert.throws(() => checkVersions("v1.2.3", f.base), /mismatch|must have version/);
    f.put(file, before);
  }
  f.put("fuzz/Cargo.lock", '[[package]]\nname = "rift-core"\nversion = "1.2.3"\n');
  assert.throws(() => checkVersions("v1.2.3", f.base), /rift-fuzz/);
});

test("release environment requires signing key and exact repository feed without disclosing credentials", () => {
  const env = { TAURI_SIGNING_PRIVATE_KEY: "private-key", GITHUB_REPOSITORY: "public-owner/rift-desktop", RIFT_UPDATE_ENDPOINT: "https://github.com/public-owner/rift-desktop/releases/latest/download/latest.json" };
  const before = { ...env };
  assert.equal(checkEnvironment(env), env.RIFT_UPDATE_ENDPOINT);
  for (const RIFT_UPDATE_ENDPOINT of ["https://updates.release-host.net/latest.json", "https://github.com/foreign/repo/releases/latest/download/latest.json", "http://github.com/public-owner/rift-desktop/releases/latest/download/latest.json"])
    assert.throws(() => checkEnvironment({ ...env, RIFT_UPDATE_ENDPOINT }), /preserve/);
  for (const RIFT_UPDATE_ENDPOINT of [undefined, "", " \t\n"])
    assert.throws(() => checkEnvironment({ ...env, RIFT_UPDATE_ENDPOINT }), /Missing RIFT_UPDATE_ENDPOINT/);
  for (const TAURI_SIGNING_PRIVATE_KEY of [undefined, "", " \t"])
    assert.throws(() => checkEnvironment({ ...env, TAURI_SIGNING_PRIVATE_KEY }), /Missing TAURI_SIGNING_PRIVATE_KEY/);
  assert.deepEqual(env, before);
});

test("real Ed25519 signatures authenticate installer, key ID and trusted comment", (t) => {
  const f = fixture(t);
  for (const mode of ["Ed", "ED"])
    assert.equal(verifySignature(f.data, f.signature(f.data, mode), f.pubkey), "1.2.3");
  assert.throws(() => verifySignature(Buffer.from("tampered"), f.signature(), f.pubkey), /verification failed/);
  const envelope = Buffer.from(f.signature(), "base64").toString().replace("timestamp:1234", "timestamp:9999");
  assert.throws(() => verifySignature(f.data, Buffer.from(envelope).toString("base64"), f.pubkey), /verification failed/);
  const other = fixture(t);
  assert.throws(() => verifySignature(f.data, f.signature(), other.pubkey), /verification failed/);
  for (const bad of ["", "garbage", Buffer.from("not minisign").toString("base64")])
    assert.throws(() => verifySignature(f.data, bad, f.pubkey));
});


test("validation fails closed for asset and manifest tampering", (t) => {
  for (const mutate of [
    (f) => f.put("release/source.js.map", "private source"),
    (f) => f.put("release/Rift_1.2.3_x64.pdb", "private symbols"),
    (f) => f.put("release/Rift_1.2.3_x64.symbols.json", "{}"),
    (f) => f.put("release/Rift_1.2.3_x64-setup.exe", Buffer.concat([Buffer.from("MZ"), Buffer.alloc(256)])),
    (f, m) => { m.version = "1.2.4"; },
    (f, m) => { m.platforms["windows-x86_64"].url = "https://evil.invalid/setup.exe"; },
    (f, m) => { m.platforms["windows-x86_64"].signature = "garbage"; },
    (f, m) => { m.platforms["windows-aarch64"] = m.platforms["windows-x86_64"]; },
    (f, m) => { m.pub_date = "invalid"; },
  ]) {
    const f = fixture(t);
    prepareUpdateAssets("v1.2.3", f.bundle, f.output, f.options);
    const m = JSON.parse(readFileSync(path.join(f.output, "latest.json")));
    mutate(f, m);
    f.put("release/latest.json", JSON.stringify(m));
    assert.throws(() => validateAssets("v1.2.3", f.output, f.options));
  }
});

test("publication date must be a real RFC 3339 date with time and offset", (t) => {
  const f = fixture(t);
  prepareUpdateAssets("v1.2.3", f.bundle, f.output, f.options);
  const manifest = JSON.parse(readFileSync(path.join(f.output, "latest.json")));
  for (const date of ["2026-11-04", "2026-11-04T12:00:00", "2026-02-30T12:00:00Z", "2026-11-04T25:00:00Z"]) {
    manifest.pub_date = date;
    f.put("release/latest.json", JSON.stringify(manifest));
    assert.throws(() => validateAssets("v1.2.3", f.output, f.options), /manifest/);
  }
  for (const date of ["2024-02-29T12:00:00Z", "2026-11-04T15:00:00.123+03:00"]) {
    manifest.pub_date = date;
    f.put("release/latest.json", JSON.stringify(manifest));
    assert.deepEqual(validateAssets("v1.2.3", f.output, f.options),
      ["Rift_1.2.3_x64-setup.exe", "Rift_1.2.3_x64-setup.exe.sig", "latest.json"]);
  }
});

test("executable CLI exits nonzero on malformed tag and unknown commands", () => {
  for (const args of [["versions", "v1.2.3-beta"], ["unknown"], ["validate", "v1.2.3", "assets"], ["stage", "v1.2.3", "bundle", "assets"]]) {
    const result = spawnSync(process.execPath, [path.join(import.meta.dirname, "release-check.mjs"), ...args], { encoding: "utf8" });
    assert.equal(result.status, 1);
    assert.ok(result.stderr.length > 0);
  }
});

test("asset checks leave directory entries, contents and modification times unchanged", (t) => {
  const f = fixture(t);
  prepareUpdateAssets("v1.2.3", f.bundle, f.output, f.options);
  const snapshot = () => readdirSync(f.output).sort().map((name) => {
    const file = path.join(f.output, name);
    return { name, data: readFileSync(file), mtime: statSync(file).mtimeMs };
  });
  const before = snapshot();
  validateAssets("v1.2.3", f.output, f.options);
  assert.deepEqual(snapshot(), before);
});

test("environment CLI confirms configuration without printing environment values", () => {
  const result = spawnSync(process.execPath, [path.join(import.meta.dirname, "release-check.mjs"), "environment"], {
    encoding: "utf8",
    env: {
      PATH: process.env.PATH,
      ...(process.env.SystemRoot ? { SystemRoot: process.env.SystemRoot } : {}),
      GITHUB_REPOSITORY: "public-owner/rift-desktop",
      TAURI_SIGNING_PRIVATE_KEY: "test-only-private-key",
      RIFT_UPDATE_ENDPOINT: "https://github.com/public-owner/rift-desktop/releases/latest/download/latest.json",
    },
  });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout.trim(), "Release environment validated.");
  assert.equal(result.stderr, "");
});

test("environment CLI refuses updater-disabled builds", () => {
  for (const endpoint of [undefined, "", " \t"]) {
    const env = {
      PATH: process.env.PATH,
      ...(process.env.SystemRoot ? { SystemRoot: process.env.SystemRoot } : {}),
      GITHUB_REPOSITORY: "public-owner/rift-desktop",
      TAURI_SIGNING_PRIVATE_KEY: "test-only-private-key",
      ...(endpoint === undefined ? {} : { RIFT_UPDATE_ENDPOINT: endpoint }),
    };
    const before = { ...env };
    const result = spawnSync(process.execPath, [path.join(import.meta.dirname, "release-check.mjs"), "environment"], { encoding: "utf8", env });
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, /Missing RIFT_UPDATE_ENDPOINT/);
    assert.equal(result.stdout, "");
    assert.deepEqual(env, before);
  }
});
