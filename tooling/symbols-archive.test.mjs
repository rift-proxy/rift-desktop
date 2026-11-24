import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { lstatSync, mkdirSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { archiveFrontendSymbols, checkNativeTools, matchingNativeIdentity, publishNativeSymbols, symbolPlan } from "./symbols-archive.mjs";
import { fixture } from "./release-fixture.mjs";
import { prepareUpdateAssets } from "./prepare-update-assets.mjs";
import { releaseAssets } from "./release-check.mjs";

const guid = "01234567-89AB-CDEF-0123-456789ABCDEF";
const pdbSummary = `Summary\n  GUID: {${guid}}\n  Age: 1\n  private source path intentionally not emitted\n`;
const peDebug = `CodeView {{\n PDBGUID: {${guid.toLowerCase()}}\n PDBAge: 1\n PDBFileName: private path intentionally not emitted\n}}`;
const env = { GITHUB_SHA: "a".repeat(40), SOURCE_TREE: "c".repeat(40), GITHUB_REPOSITORY: "public-owner/rift-desktop", GITHUB_RUN_ID: "12", GITHUB_RUN_ATTEMPT: "2" };
function symbolFixture(t) {
  const f = fixture(t);
  const maps = path.join(f.base, "maps"), dist = path.join(f.base, "dist");
  mkdirSync(path.join(maps, "assets"), { recursive: true });
  mkdirSync(path.join(dist, "assets"), { recursive: true });
  const js = path.join(dist, "assets/app-test.js"), map = path.join(maps, "assets/app-test.js.map");
  writeFileSync(js, "console.log('test-only');");
  writeFileSync(map, JSON.stringify({ version: 3, file: "app-test.js", sources: ["/private/source.ts"], sourcesContent: ["test-only source"], names: [], mappings: "AAAA" }));
  const pdb = path.join(f.base, "rift_desktop.pdb"), exe = path.join(f.base, "rift-desktop.exe");
  writeFileSync(pdb, "test-only PDB; real identity extraction is mocked in this unit test");
  writeFileSync(exe, "MZ test-only executable");
  return { ...f, maps, dist, js, map, pdb, exe };
}
function tools() {
  const run = (command, args) => {
    if (command === "llvm-pdbutil") return args[0] === "--version" ? "LLVM version 21.1.8" : pdbSummary;
    if (command === "llvm-readobj") return args[0] === "--version" ? "LLVM version 21.1.8" : peDebug;
    assert.fail(`Unexpected tool ${command}`);
  };
  return { run };
}

test("native tool checks reject unavailable or invalid LLVM without exposing raw diagnostics", () => {
  assert.throws(() => checkNativeTools({ run: () => "not LLVM" }), /LLVM PDB/);
  assert.throws(() => checkNativeTools({ run: () => { throw new Error("raw credentials must never escape"); } }), (error) => !error.message.includes("credentials"));
});
test("native identity matches normalized GUID and decimal age but rejects mismatches/ambiguity", () => {
  assert.deepEqual(matchingNativeIdentity(pdbSummary, peDebug), { guid: guid.replaceAll("-", "").toLowerCase(), age: 1 });
  for (const [pdb, pe] of [[pdbSummary.replace("Age: 1", "Age: 2"), peDebug], [pdbSummary.replace("01234567", "11234567"), peDebug], ["missing", peDebug], [pdbSummary, "missing"], [pdbSummary + pdbSummary, peDebug]]) assert.throws(() => matchingNativeIdentity(pdb, pe));
});
test("native PDB publication binds its executable, installer and source", (t) => {
  const f = symbolFixture(t), { run } = tools();
  const output = f.output;
  prepareUpdateAssets("v1.2.3", f.bundle, output, f.options);
  const record = publishNativeSymbols(f.pdb, f.exe, output, { env, base: f.base, run });
  const a = releaseAssets("1.2.3");
  assert.deepEqual(readFileSync(path.join(output, a.pdb)), readFileSync(f.pdb));
  assert.equal(record.files[a.pdb].sha256, createHash("sha256").update(readFileSync(f.pdb)).digest("hex"));
  assert.equal(record.source_sha, env.GITHUB_SHA);
  assert.equal(record.run_id, 12);
  assert.equal(record.run_attempt, 2);
  assert.deepEqual(record.debug_id, { guid: guid.replaceAll("-", "").toLowerCase(), age: 1 });
  assert.equal(JSON.stringify(record).includes(f.base), false);
  assert.throws(() => publishNativeSymbols(f.pdb, f.exe, output, { env, base: f.base, run }), /replace/);
});
test("frontend tar preserves matching original JS/maps and their source/run hash manifest", (t) => {
  const f = symbolFixture(t), archive = path.join(f.base, "frontend.tar"), extracted = path.join(f.base, "extracted");
  archiveFrontendSymbols(f.maps, f.dist, archive, { env, base: f.base });
  mkdirSync(extracted);
  execFileSync("tar", ["-xf", archive, "-C", extracted]);
  const manifest = JSON.parse(readFileSync(path.join(extracted, "manifest.json"), "utf8"));
  assert.deepEqual(manifest.pairs, [{ javascript: "app-test.js", map: "app-test.js.map" }]);
  for (const [name, file] of [["app-test.js", f.js], ["app-test.js.map", f.map]]) {
    assert.deepEqual(readFileSync(path.join(extracted, name)), readFileSync(file));
    assert.equal(manifest.files[name].sha256, createHash("sha256").update(readFileSync(file)).digest("hex"));
  }
  assert.equal(manifest.source_sha, env.GITHUB_SHA);
  assert.equal(manifest.run_id, 12);
  const original = readFileSync(archive);
  assert.throws(() => archiveFrontendSymbols(f.maps, f.dist, archive, { env, base: f.base }), /replace/);
  assert.deepEqual(readFileSync(archive), original);
});
test("frontend maps must stay outside dist and match every JavaScript output exactly", async (t) => {
  for (const name of ["inside dist", "map in dist", "missing pair", "wrong map file", "invalid map", "extra nonmap"]) await t.test(name, (t) => {
    const f = symbolFixture(t), { run } = tools();
    let maps = f.maps;
    if (name === "inside dist") maps = path.join(f.dist, "maps");
    if (name === "map in dist") writeFileSync(path.join(f.dist, "assets/leaked.js.map"), "private");
    if (name === "missing pair") writeFileSync(path.join(f.dist, "assets/extra.js"), "test-only");
    if (name === "wrong map file") writeFileSync(f.map, JSON.stringify({ version: 3, file: "wrong.js" }));
    if (name === "invalid map") writeFileSync(f.map, "invalid private contents");
    if (name === "extra nonmap") writeFileSync(path.join(f.maps, "unrelated.pdb"), "private");
    assert.throws(() => symbolPlan("frontend", maps, f.dist, { env, base: f.base, run }));
  });
});
test("symlinks and mismatched PDBs are rejected and failed archiving leaves no output", async (t) => {
  await t.test("map directory symlink", (t) => {
    const f = symbolFixture(t), { run } = tools(), link = path.join(f.base, "maps-link");
    symlinkSync(f.maps, link, process.platform === "win32" ? "junction" : "dir");
    assert.throws(() => symbolPlan("frontend", link, f.dist, { env, base: f.base, run }), /no symlinks/);
  });
  await t.test("native debug ID mismatch", (t) => {
    const f = symbolFixture(t), tool = tools();
    assert.throws(() => symbolPlan("native", f.pdb, f.exe, { env, base: f.base, run: (cmd, args, opts) => cmd === "llvm-readobj" ? peDebug.replace("PDBAge: 1", "PDBAge: 2") : tool.run(cmd, args, opts) }), /do not match/);
  });
  await t.test("tar failure", (t) => {
    const f = symbolFixture(t), output = path.join(f.base, "failed.tar");
    let staging;
    const run = (command, args) => {
      assert.equal(command, "tar");
      staging = args[args.indexOf("-C") + 1];
      throw new Error("raw sensitive diagnostics");
    };
    assert.throws(() => archiveFrontendSymbols(f.maps, f.dist, output, { env, base: f.base, run }), /raw output is suppressed/);
    assert.throws(() => lstatSync(output), /ENOENT/);
    assert.throws(() => lstatSync(staging), /ENOENT/);
  });
});
