#!/usr/bin/env node
// Capture matching frontend JS/maps and audited native PDBs as plaintext artifacts.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { constants, copyFileSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { checkVersions, repository, releaseAssets } from "./release-check.mjs";
import { positiveId, sha } from "./pipeline-source.mjs";

const root = path.resolve(import.meta.dirname, "..");
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
class SymbolsError extends Error {}
const fail = (message) => { throw new SymbolsError(message); };
function runTool(run, command, args, options = {}) {
  try { return run(command, args, { stdio: "pipe", encoding: "utf8", maxBuffer: 8 << 20, timeout: 120_000, ...options }); }
  catch { fail(`Required symbol tool ${command} failed; raw output is suppressed`); }
}
export function checkNativeTools({ run = execFileSync } = {}) {
  for (const tool of ["llvm-pdbutil", "llvm-readobj"]) if (!/LLVM version\s+\d+/i.test(runTool(run, tool, ["--version"]))) fail("LLVM PDB and PE inspection tools are required before signing");
}
function identifier(text, guidLabel, ageLabel) {
  const guids = [...text.matchAll(new RegExp(`^\\s*${guidLabel}:\\s*\\{?([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\\}?\\s*$`, "gmi"))];
  const ages = [...text.matchAll(new RegExp(`^\\s*${ageLabel}:\\s*([0-9]+)\\s*$`, "gm"))];
  if (guids.length !== 1 || ages.length !== 1 || !Number.isSafeInteger(Number(ages[0][1])) || Number(ages[0][1]) > 0xffffffff) fail("Expected exactly one valid native GUID and age");
  return { guid: guids[0][1].replaceAll("-", "").toLowerCase(), age: Number(ages[0][1]) };
}
export function matchingNativeIdentity(pdbSummary, peDebug) {
  const pdb = identifier(pdbSummary, "GUID", "Age");
  const pe = identifier(peDebug, "PDBGUID", "PDBAge");
  if (pdb.guid !== pe.guid || pdb.age !== pe.age) fail("PDB and executable debug GUID/age do not match");
  return pdb;
}
function regular(file) {
  const stat = lstatSync(file);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size <= 0 || stat.size > 1024 ** 3) fail("Symbols must be nonempty regular files below 1 GiB (no symlinks)");
  return stat;
}
function label(file) {
  const name = path.basename(file);
  if (!/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(name) || name === "manifest.json") fail("Symbol archive filenames must be safe basenames");
  return name;
}
function walk(directory, prefix = "") {
  const stat = lstatSync(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink()) fail("Symbol input directories must be regular directories (no symlinks)");
  const result = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const file = path.join(directory, entry.name), relative = prefix ? `${prefix}/${entry.name}` : entry.name;
    const info = lstatSync(file);
    if (info.isSymbolicLink()) fail("Symbol inputs must not contain symlinks");
    if (info.isDirectory()) result.push(...walk(file, relative));
    else { regular(file); result.push({ file, relative }); }
  }
  return result;
}
function identity(env, base) {
  const version = JSON.parse(readFileSync(path.join(base, "desktop/src-tauri/tauri.conf.json"), "utf8")).version;
  checkVersions(`v${version}`, base);
  return { schema: 1, repository: repository(env.GITHUB_REPOSITORY), source_sha: sha(env.SOURCE_SHA || env.GITHUB_SHA), version, run_id: positiveId(env.GITHUB_RUN_ID), run_attempt: positiveId(env.GITHUB_RUN_ATTEMPT) };
}
export function symbolPlan(kind, first, second, { env = process.env, base = root, run = execFileSync } = {}) {
  const manifest = { ...identity(env, base), kind };
  if (kind === "native") {
    regular(first); regular(second);
    if (!first.toLowerCase().endsWith(".pdb") || !second.toLowerCase().endsWith(".exe")) fail("Native symbols require exactly one PDB and its executable");
    const pdbSummary = runTool(run, "llvm-pdbutil", ["dump", "--summary", first]);
    const peDebug = runTool(run, "llvm-readobj", ["--coff-debug-directory", second]);
    manifest.debug_id = matchingNativeIdentity(pdbSummary, peDebug);
    return { manifest, inputs: [first, second].map((file) => ({ file, name: label(file) })) };
  }
  if (kind !== "frontend") fail("Expected native or frontend symbols");
  const mapsRoot = path.resolve(first), distRoot = path.resolve(second);
  if (mapsRoot === distRoot || mapsRoot.startsWith(`${distRoot}${path.sep}`) || distRoot.startsWith(`${mapsRoot}${path.sep}`)) fail("Source maps must be captured outside the frontend distribution");
  const maps = walk(first), dist = walk(second);
  if (!maps.length || maps.some(({ relative }) => !relative.endsWith(".js.map"))) fail("Expected nonempty JavaScript source maps only");
  if (dist.some(({ relative }) => relative.endsWith(".map"))) fail("Frontend distribution must not contain source maps");
  const javascript = dist.filter(({ relative }) => relative.endsWith(".js"));
  if (!javascript.length || JSON.stringify(javascript.map(({ relative }) => relative).sort()) !== JSON.stringify(maps.map(({ relative }) => relative.slice(0, -4)).sort())) fail("Every JavaScript output must have exactly one captured source map");
  const inputs = [], pairs = [];
  for (const map of maps) {
    const js = javascript.find(({ relative }) => relative === map.relative.slice(0, -4));
    let contents;
    try { contents = JSON.parse(readFileSync(map.file, "utf8")); } catch { fail("Source map JSON is invalid (contents suppressed)"); }
    if (contents.version !== 3 || contents.file !== path.basename(js.file)) fail("Source map must identify the matching JavaScript basename");
    inputs.push({ file: js.file, name: label(js.file) }, { file: map.file, name: label(map.file) });
    pairs.push({ javascript: label(js.file), map: label(map.file) });
  }
  manifest.pairs = pairs;
  return { manifest, inputs };
}
export function publishNativeSymbols(pdb, executable, output, { env = process.env, base = root, run = execFileSync } = {}) {
  const plan = symbolPlan("native", pdb, executable, { env, base, run });
  const assets = releaseAssets(plan.manifest.version);
  const installer = path.join(output, assets.installer);
  regular(installer);
  const metadata = (file) => {
    const bytes = readFileSync(file);
    return { sha256: hash(bytes), size: bytes.length };
  };
  const manifest = { ...plan.manifest, source_tree: sha(env.SOURCE_TREE), pdb: assets.pdb, files: {
    [assets.pdb]: metadata(pdb), [assets.installer]: metadata(installer),
  }, executable: metadata(executable) };
  for (const name of [assets.pdb, assets.provenance]) {
    try { lstatSync(path.join(output, name)); fail("Refusing to replace existing native symbols"); }
    catch (error) { if (error instanceof SymbolsError || error.code !== "ENOENT") throw error; }
  }
  // No executable copies or third-party PDB glob: only this matching application PDB.
  writeFileSync(path.join(output, assets.pdb), readFileSync(pdb), { flag: "wx" });
  writeFileSync(path.join(output, assets.provenance), `${JSON.stringify(manifest, null, 2)}\n`, { flag: "wx" });
  return manifest;
}
export function archiveFrontendSymbols(first, second, output, { env = process.env, base = root, run = execFileSync } = {}) {
  if (!output.endsWith(".tar")) fail("Frontend symbols output must end in .tar");
  try { lstatSync(output); fail("Refusing to replace an existing symbol archive"); }
  catch (error) { if (error instanceof SymbolsError) throw error; if (error.code !== "ENOENT") throw error; }
  const plan = symbolPlan("frontend", first, second, { env, base, run });
  const names = plan.inputs.map(({ name }) => name);
  if (new Set(names).size !== names.length) fail("Symbol basenames must be unique");
  const temporary = mkdtempSync(path.join(tmpdir(), "rift-symbols-"));
  const stage = path.join(temporary, "files");
  try {
    mkdirSync(stage, { mode: 0o700 });
    plan.manifest.files = {};
    for (const { file, name } of plan.inputs) {
      regular(file);
      copyFileSync(file, path.join(stage, name));
      const bytes = readFileSync(path.join(stage, name));
      plan.manifest.files[name] = { sha256: hash(bytes), size: bytes.length };
    }
    writeFileSync(path.join(stage, "manifest.json"), `${JSON.stringify(plan.manifest, null, 2)}\n`, { mode: 0o600, flag: "wx" });
    const tar = path.join(temporary, "symbols.tar");
    // No recursive paths, directory entries, links or user-provided tar switches: only these copied files.
    runTool(run, "tar", ["-cf", tar, "-C", stage, "--", ...names, "manifest.json"]);
    regular(tar);
    copyFileSync(tar, output, constants.COPYFILE_EXCL);
    return { files: names.length, kind: "frontend", source: plan.manifest.source_sha };
  } finally { rmSync(temporary, { recursive: true, force: true }); }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [command, first, second, output, ...extra] = process.argv.slice(2);
    if (command === "check-native-tools" && !first) checkNativeTools();
    else if (command === "native" && first && second && output && !extra.length) {
      publishNativeSymbols(first, second, output);
      console.log("Captured matching application PDB and native provenance.");
    } else if (command === "frontend" && first && second && output && !extra.length) {
      const result = archiveFrontendSymbols(first, second, output);
      console.log(`Captured ${result.kind} symbols: ${result.files} files and build provenance.`);
    } else fail("Usage: symbols-archive.mjs check-native-tools | native <pdb> <exe> <public-dir> | frontend <maps-dir> <dist-dir> <new.tar>");
  } catch (error) { console.error(error instanceof SymbolsError ? error.message : "Symbol archive failed; verify required inputs, manifests and output directory (raw diagnostics suppressed)"); process.exitCode = 1; }
}
