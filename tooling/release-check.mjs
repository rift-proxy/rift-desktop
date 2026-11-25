#!/usr/bin/env node
// Stable Windows updater releases only. No parent repository files are required.
import { createHash, createPublicKey, verify } from "node:crypto";
import { lstatSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(import.meta.dirname, "..");
const json = (file) => JSON.parse(readFileSync(file, "utf8"));
const stable = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
export function tagVersion(tag) {
  if (typeof tag !== "string" || tag !== tag.trim() || !tag.startsWith("v") || !stable.test(tag.slice(1)))
    throw new Error("Expected a stable tag vX.Y.Z (no prerelease or leading zeroes)");
  return tag.slice(1);
}
function value(block, key) {
  const matches = [...block.matchAll(new RegExp(`^${key}\\s*=\\s*"([^"\\n]+)"\\s*(?:#.*)?$`, "gm"))];
  if (matches.length !== 1) throw new Error(`Expected one TOML ${key}`);
  return matches[0][1];
}
function section(file, name) {
  const text = readFileSync(file, "utf8");
  const blocks = text.split(/^\[/m).filter((block) => block.startsWith(`${name}]`));
  if (blocks.length !== 1) throw new Error(`Expected one [${name}] in ${file}`);
  return blocks[0].slice(name.length + 1);
}
function lockedPackages(file) {
  return readFileSync(file, "utf8").split(/^\[\[package\]\]\s*$/m).slice(1)
    .map((block) => ({ name: value(block, "name"), version: value(block, "version"), external: /^source\s*=/m.test(block) }));
}
export function checkVersions(tag, base = root) {
  const version = tagVersion(tag);
  for (const [file, actual] of [
    ["Cargo.toml", value(section(path.join(base, "Cargo.toml"), "workspace.package"), "version")],
    ["desktop/package.json", json(path.join(base, "desktop/package.json")).version],
    ["desktop/src-tauri/tauri.conf.json", json(path.join(base, "desktop/src-tauri/tauri.conf.json")).version],
  ]) if (actual !== version) throw new Error(`Version mismatch in ${file}: expected ${version}`);
  const fuzzVersion = value(section(path.join(base, "fuzz/Cargo.toml"), "package"), "version");
  for (const [file, expected] of [
    ["Cargo.lock", { "rift-core": version, "rift-desktop": version }],
    ["fuzz/Cargo.lock", { "rift-core": version, "rift-fuzz": fuzzVersion }],
  ]) {
    const packages = lockedPackages(path.join(base, file));
    for (const [name, wanted] of Object.entries(expected)) {
      const found = packages.filter((entry) => entry.name === name);
      if (found.length !== 1 || found[0].version !== wanted || found[0].external)
        throw new Error(`Local package ${name} in ${file} must have version ${wanted}`);
    }
    if (packages.some((entry) => !entry.external && !(entry.name in expected)))
      throw new Error(`Unexpected local package in ${file}`);
  }
  return version;
}
export function repository(value = process.env.GITHUB_REPOSITORY) {
  if (typeof value !== "string" || value !== value.trim() || !/^[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9_.-]+$/.test(value) || /\/(?:\.|\.\.)$/.test(value))
    throw new Error("GITHUB_REPOSITORY must be owner/repository");
  return value;
}
export function releaseAssets(version) {
  return { installer: `Rift_${version}_x64-setup.exe`, manifest: "latest.json" };
}
export function checkEnvironment(env = process.env) {
  if (!env.TAURI_SIGNING_PRIVATE_KEY?.trim()) throw new Error("Missing TAURI_SIGNING_PRIVATE_KEY");
  const expected = `https://github.com/${repository(env.GITHUB_REPOSITORY)}/releases/latest/download/latest.json`;
  const endpoint = env.RIFT_UPDATE_ENDPOINT;
  if (typeof endpoint !== "string" || !endpoint.trim()) throw new Error("Missing RIFT_UPDATE_ENDPOINT; release updater must be explicitly configured");
  if (endpoint !== expected) throw new Error("Updater endpoint must preserve the repository latest.json feed");
  return endpoint;
}
function base64(text) {
  if (typeof text !== "string" || !text || text.length % 4 || !/^[A-Za-z0-9+/]+={0,2}$/.test(text))
    throw new Error("Invalid base64 signature/key");
  const decoded = Buffer.from(text, "base64");
  if (decoded.toString("base64") !== text) throw new Error("Noncanonical base64 signature/key");
  return decoded;
}
// Minisign's ED packet signs BLAKE2b-512(data); the second signature authenticates
// signature bytes + trusted comment. See jedisct1/rust-minisign src/lib.rs::verify.
export function verifySignature(data, encodedSignature, encodedPublicKey) {
  const publicLines = base64(encodedPublicKey).toString("utf8").trimEnd().split(/\r?\n/);
  const lines = base64(encodedSignature).toString("utf8").trimEnd().split(/\r?\n/);
  if (publicLines.length !== 2 || !publicLines[0].startsWith("untrusted comment: ") ||
      lines.length !== 4 || !lines[0].startsWith("untrusted comment: ") || !lines[2].startsWith("trusted comment: "))
    throw new Error("Invalid minisign text envelope");
  const pk = base64(publicLines[1]);
  const sig = base64(lines[1]);
  const global = base64(lines[3]);
  if (pk.length !== 42 || pk.subarray(0, 2).toString() !== "Ed" || sig.length !== 74 ||
      !["Ed", "ED"].includes(sig.subarray(0, 2).toString()) || global.length !== 64 || !pk.subarray(2, 10).equals(sig.subarray(2, 10)))
    throw new Error("Invalid minisign packet or mismatched key ID");
  const key = createPublicKey({ key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), pk.subarray(10)]), format: "der", type: "spki" });
  const payload = sig.subarray(0, 2).toString() === "ED" ? createHash("blake2b512").update(data).digest() : data;
  if (!verify(null, payload, key, sig.subarray(10)) ||
      !verify(null, Buffer.concat([sig.subarray(10), Buffer.from(lines[2].slice("trusted comment: ".length))]), key, global))
    throw new Error("Installer minisign verification failed");
  const versions = lines[2].slice("trusted comment: ".length).split("\t")
    .filter((field) => field.startsWith("version:")).map((field) => field.slice("version:".length));
  if (versions.length !== 1 || !stable.test(versions[0]))
    throw new Error("Installer signature must authenticate one stable app version");
  return versions[0];
}
function files(directory) {
  const names = readdirSync(directory).sort();
  for (const name of names) if (!lstatSync(path.join(directory, name)).isFile())
    throw new Error("Release assets must be regular files (no symlinks or directories)");
  return names;
}
export function installerFiles(directory, version, withManifest) {
  const names = files(directory);
  const { installer, manifest } = releaseAssets(version);
  const expected = [installer, `${installer}.sig`, ...(withManifest ? [manifest] : [])].sort();
  if (JSON.stringify(names) !== JSON.stringify(expected)) throw new Error("Unexpected or missing release assets");
  const data = readFileSync(path.join(directory, installer));
  if (data.length < 64 || data.subarray(0, 2).toString() !== "MZ") throw new Error("Invalid Windows installer");
  const signature = readFileSync(path.join(directory, `${installer}.sig`), "utf8").trim();
  return { installer, data, signature };
}
export function assetUrl(repo, tag, installer) {
  return `https://github.com/${repository(repo)}/releases/download/${tag}/${encodeURIComponent(installer)}`;
}
function validPublicationDate(value) {
  if (typeof value !== "string" ||
      !/^\d{4}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])[Tt]([01]\d|2[0-3]):[0-5]\d:[0-5]\d(?:\.\d+)?(?:[Zz]|[+-]([01]\d|2[0-3]):[0-5]\d)$/.test(value) ||
      !Number.isFinite(Date.parse(value))) return false;
  const [year, month, day] = value.slice(0, 10).split("-").map(Number);
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  return day <= [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month - 1];
}
export function validateAssets(tag, directory, { base = root, repo = process.env.GITHUB_REPOSITORY } = {}) {
  const version = checkVersions(tag, base);
  const names = releaseAssets(version);
  const { installer, data, signature } = installerFiles(directory, version, true);
  if (verifySignature(data, signature, json(path.join(base, "desktop/src-tauri/tauri.conf.json")).plugins.updater.pubkey) !== version)
    throw new Error("Signed installer version does not match release tag");
  const manifest = json(path.join(directory, names.manifest));
  if (JSON.stringify(Object.keys(manifest).sort()) !== JSON.stringify(["notes", "platforms", "pub_date", "version"]) ||
      manifest.version !== version || typeof manifest.notes !== "string" ||
      !validPublicationDate(manifest.pub_date) ||
      JSON.stringify(Object.keys(manifest.platforms ?? {})) !== JSON.stringify(["windows-x86_64"]))
    throw new Error("Invalid updater manifest");
  const platform = manifest.platforms["windows-x86_64"];
  if (JSON.stringify(Object.keys(platform).sort()) !== JSON.stringify(["signature", "url"]) ||
      platform.signature !== signature || platform.url !== assetUrl(repo, tag, installer))
    throw new Error("Updater manifest does not match signed assets/repository");
  return [installer, `${installer}.sig`, names.manifest];
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [command, tag, input, ...extra] = process.argv.slice(2);
    if (command === "environment" && !tag) { checkEnvironment(); console.log("Release environment validated."); }
    else if (command === "versions" && tag && !input) console.log(checkVersions(tag));
    else if (command === "assets" && tag && input && !extra.length) console.log(validateAssets(tag, input).join("\n"));
    else throw new Error("Usage: release-check.mjs versions <tag> | environment | assets <tag> <assets-dir>");
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
