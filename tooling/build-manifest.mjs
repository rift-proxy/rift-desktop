#!/usr/bin/env node
// Manifest of a Docker build's output directory: the commit it was built from and the SHA-256 of
// every file, written last by the build (docker/build-entrypoint.sh). It makes the directory
// self-describing: a copy that stopped half way, an installer left over from an earlier build or
// an output of another commit is told apart from a complete, current one.
//
//   node tooling/build-manifest.mjs write <dir> <commit>   write <dir>/build-manifest.json
//   node tooling/build-manifest.mjs check <dir> [commit]   list problems; exit 1 if there are any
import { createHash } from "node:crypto";
import { readFileSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import path from "node:path";

export const MANIFEST_NAME = "build-manifest.json";
/** Empty file that marks a directory as the output of this build (and so as safe to clear). */
export const OUTPUT_MARKER = ".rift-build-output";
const SCHEMA = 1;
const SETUP = /^bundle\/nsis\/[^/]+-setup\.exe$/;
const isDebugArtifact = (file) => /(^|\/)symbols(\/|$)|\.(pdb|map)$/i.test(file);

const digest = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");

/** Files below `dir`, relative and `/`-separated, without the manifest and marker. */
export function listFiles(dir, prefix = "") {
  const found = [];
  for (const entry of readdirSync(path.join(dir, prefix), { withFileTypes: true })) {
    const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) found.push(...listFiles(dir, relative));
    else if (prefix || (entry.name !== MANIFEST_NAME && entry.name !== OUTPUT_MARKER))
      found.push(relative);
  }
  return found.sort();
}

export function buildManifest(dir, commit) {
  const files = {};
  for (const file of listFiles(dir)) {
    if (isDebugArtifact(file)) throw new Error(`${file} is a forbidden debug artifact`);
    files[file] = digest(path.join(dir, file));
  }
  return { schema: SCHEMA, commit, files };
}

export function writeManifest(dir, commit) {
  if (!/^[0-9a-f]{40}$/.test(commit)) throw new Error("commit must be a full 40-digit SHA");
  const manifest = buildManifest(dir, commit);
  writeFileSync(path.join(dir, MANIFEST_NAME), `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}

/** What is wrong with `dir` as the output of a complete build (of `commit`, when given). */
export function manifestProblems(dir, commit) {
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(path.join(dir, MANIFEST_NAME), "utf8"));
  } catch {
    return [`${MANIFEST_NAME} is missing or unreadable: the build did not finish`];
  }
  if (manifest?.schema !== SCHEMA || typeof manifest.files !== "object" || !manifest.files)
    return [`${MANIFEST_NAME} has an unknown format`];
  const problems = [];
  if (commit && manifest.commit !== commit)
    problems.push(
      `built from ${String(manifest.commit).slice(0, 12)}, not from ${commit.slice(0, 12)}: build again`,
    );
  const listed = Object.keys(manifest.files);
  if (!listed.includes("rift-desktop.exe")) problems.push("rift-desktop.exe is not in the build");
  if (!listed.some((file) => SETUP.test(file))) problems.push("no NSIS installer in the build");
  if (!listed.some((file) => file.startsWith("frontend/")))
    problems.push("the frontend bundle is not in the build");
  let present;
  try {
    present = new Set(listFiles(dir));
  } catch {
    return [...problems, "the output directory cannot be read"];
  }
  for (const file of new Set([...listed, ...present]))
    if (isDebugArtifact(file)) problems.push(`${file} is a forbidden debug artifact`);
  for (const file of listed) {
    if (!present.has(file)) problems.push(`${file} is listed but missing`);
    else if (digest(path.join(dir, file)) !== manifest.files[file])
      problems.push(`${file} changed after the build`);
  }
  for (const file of present)
    if (!(file in manifest.files)) problems.push(`${file} is not part of the build (left over?)`);
  return problems;
}

// `import.meta.filename` is the resolved path, `argv[1]` as typed: through a symlink they differ.
const invokedDirectly = (() => {
  try {
    return realpathSync(process.argv[1]) === import.meta.filename;
  } catch {
    return false;
  }
})();
if (invokedDirectly) {
  const [command, dir, commit] = process.argv.slice(2);
  try {
    if (command === "write" && dir && commit) {
      const { files } = writeManifest(dir, commit);
      console.log(`manifest: ${Object.keys(files).length} files`);
    } else if (command === "check" && dir) {
      const problems = manifestProblems(dir, commit);
      for (const problem of problems) console.error(`FAIL ${problem}`);
      process.exitCode = problems.length ? 1 : 0;
    } else {
      console.error("usage: build-manifest.mjs write <dir> <commit> | check <dir> [commit]");
      process.exitCode = 2;
    }
  } catch (error) {
    console.error(`error: ${error.message}`);
    process.exitCode = 2;
  }
}
