#!/usr/bin/env node
// Explicit local version edits only; release-check.mjs remains the version contract.
import { chmodSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { checkVersions, tagVersion } from "./release-check.mjs";

const root = path.resolve(import.meta.dirname, "..");
const files = ["Cargo.toml", "desktop/package.json", "desktop/src-tauri/tauri.conf.json", "Cargo.lock", "fuzz/Cargo.lock", "desktop/src/testing/core-contract.json"];
const localNames = new Set(["rift-core", "rift-desktop", "rift-fuzz"]);
const lines = (text) => [...text.matchAll(/[^\n]*(?:\n|$)/g)].filter((match) => match[0]);
const content = (line) => line.replace(/\r?\n$/, "");
const blank = (line) => /^[ \t]*(?:#.*)?$/.test(line);

function replaceSpans(text, edits) {
  for (const { start, end, value } of edits.sort((a, b) => b.start - a.start))
    text = text.slice(0, start) + value + text.slice(end);
  return text;
}

function workspaceVersion(text, next) {
  const headers = [...text.matchAll(/^\[workspace\.package\][ \t]*(?:#[^\r\n]*)?\r?$/gm)];
  if (headers.length !== 1) throw new Error("Expected one [workspace.package]");
  const start = headers[0].index + headers[0][0].length;
  const tail = text.slice(start);
  const boundary = tail.search(/^\[/m);
  const block = boundary === -1 ? tail : tail.slice(0, boundary);
  const keys = new Set();
  let slot;
  // Deliberately support the simple scalar workspace.package table used here,
  // not alternate quoted/dotted keys or multiline TOML strings.
  for (const line of lines(block)) {
    const raw = content(line[0]);
    if (blank(raw)) continue;
    const field = /^([A-Za-z_][A-Za-z0-9_-]*)[ \t]*=[ \t]*("[^"\\\r\n]*"|true|false|[0-9]+)[ \t]*(?:#.*)?$/.exec(raw);
    if (!field || keys.has(field[1])) throw new Error("Unsupported or duplicate workspace.package field");
    keys.add(field[1]);
    if (field[1] === "version") {
      if (!field[2].startsWith('"')) throw new Error("Expected a string workspace.package.version");
      const offset = start + line.index + raw.indexOf('"');
      slot = { start: offset, end: offset + field[2].length, value: JSON.parse(field[2]) };
    }
  }
  if (!slot) throw new Error("Missing workspace.package.version");
  const current = tagVersion(`v${slot.value}`);
  return { current, text: replaceSpans(text, [{ ...slot, value: JSON.stringify(next ?? current) }]) };
}

function jsonVersion(text, next, key = "version") {
  const parsed = JSON.parse(text);
  if (!parsed || Array.isArray(parsed) || typeof parsed !== "object") throw new Error("Expected a JSON object manifest");
  let depth = 0;
  const slots = [];
  // JSON.parse validates syntax; token offsets let us preserve all formatting
  // and nested version fields, including escaped top-level property names.
  for (const token of text.matchAll(/"(?:\\[\s\S]|[^"\\])*"|[{}\[\]]/g)) {
    if (token[0] === "{" || token[0] === "[") depth++;
    else if (token[0] === "}" || token[0] === "]") depth--;
    else if (depth === 1 && JSON.parse(token[0]) === key) {
      const after = text.slice(token.index + token[0].length);
      const value = /^[ \t\r\n]*:[ \t\r\n]*("(?:\\[\s\S]|[^"\\])*")/.exec(after);
      if (value) {
        const start = token.index + token[0].length + value[0].length - value[1].length;
        slots.push({ start, end: start + value[1].length, value: JSON.parse(value[1]) === next ? value[1] : JSON.stringify(next) });
      } else if (/^[ \t\r\n]*:/.test(after)) throw new Error(`Expected a string ${key}`);
    }
  }
  if (slots.length !== 1) throw new Error(`Expected one top-level JSON ${key}`);
  return replaceSpans(text, slots);
}

function lockVersion(text, current, next) {
  const packages = [];
  let entry;
  let array;
  let lastComma = true;
  let format;
  for (const line of lines(text)) {
    const raw = content(line[0]);
    if (blank(raw)) continue;
    if (array) {
      if (/^[ \t]*\][ \t]*(?:#.*)?$/.test(raw)) { array = undefined; continue; }
      const dependency = /^[ \t]*"([^"\\\r\n]+)"(,?)[ \t]*(?:#.*)?$/.exec(raw);
      if (!dependency || !lastComma) throw new Error("Unsupported lock dependency array");
      const start = line.index + raw.indexOf('"') + 1;
      array.push({ text: dependency[1], start, end: start + dependency[1].length });
      lastComma = dependency[2] === ",";
      continue;
    }
    if (/^\[\[package\]\][ \t]*$/.test(raw)) {
      entry = { fields: new Map(), dependencies: [] };
      packages.push(entry);
      continue;
    }
    if (!entry) {
      const header = /^version[ \t]*=[ \t]*([34])[ \t]*(?:#.*)?$/.exec(raw);
      if (!header || format) throw new Error("Expected Cargo lock format 3 or 4");
      format = header[1];
      continue;
    }
    const field = /^(name|version|source|checksum|dependencies)[ \t]*=[ \t]*(.*?)[ \t]*$/.exec(raw);
    if (!field || entry.fields.has(field[1])) throw new Error("Unsupported or duplicate lock field");
    entry.fields.set(field[1], true);
    if (field[1] === "dependencies") {
      if (/^\[\][ \t]*(?:#.*)?$/.test(field[2])) continue;
      if (!/^\[[ \t]*(?:#.*)?$/.test(field[2])) throw new Error("Unsupported lock dependency array");
      array = entry.dependencies;
      lastComma = true;
    } else {
      const value = /^"([^"\\\r\n]+)"[ \t]*(?:#.*)?$/.exec(field[2]);
      if (!value) throw new Error("Expected a simple lock string");
      entry[field[1]] = value[1];
      if (field[1] === "version") {
        const start = line.index + raw.indexOf('"') + 1;
        entry.slot = { start, end: start + value[1].length };
      }
    }
  }
  if (array || !format || !packages.length) throw new Error("Incomplete Cargo lock");
  const locals = new Map();
  const identities = new Set();
  for (const item of packages) {
    if (!item.name || !item.version) throw new Error("Missing lock package name/version");
    const identity = JSON.stringify([item.name, item.version, item.source]);
    if (identities.has(identity)) throw new Error("Duplicate lock package");
    identities.add(identity);
    if (!item.source) locals.set(item.name, item.version);
  }
  const edits = [];
  for (const item of packages) {
    if (!item.source && (item.name === "rift-core" || item.name === "rift-desktop")) {
      if (item.version !== current) throw new Error("Inconsistent local lock version");
      edits.push({ ...item.slot, value: next });
    }
    for (const dependency of item.dependencies) {
      const ref = /^([A-Za-z0-9_-]+)(?: ([^ ()]+)(?: \(([^()]+)\))?)?$/.exec(dependency.text);
      if (!ref) throw new Error("Unsupported lock dependency reference");
      if (!localNames.has(ref[1])) continue;
      if (!locals.has(ref[1]) || ref[3] || (ref[2] && ref[2] !== locals.get(ref[1])))
        throw new Error(`Inconsistent local dependency reference: ${dependency.text}`);
      if (ref[2] && ref[1] !== "rift-fuzz")
        edits.push({ ...dependency, value: `${ref[1]} ${next}` });
    }
  }
  return replaceSpans(text, edits);
}

function validatePlan(version, plan, fuzzManifest) {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-version-check-"));
  try {
    for (const [name, text] of [...plan, ["fuzz/Cargo.toml", fuzzManifest]]) {
      const file = path.join(directory, name);
      mkdirSync(path.dirname(file), { recursive: true });
      writeFileSync(file, text);
    }
    checkVersions(`v${version}`, directory);
  } finally { rmSync(directory, { recursive: true, force: true }); }
}

function install(base, originals, plan, modes) {
  const staged = [];
  const installed = [];
  const retained = new Set();
  try {
    // Finish every write before the first replacement. Keep same-filesystem
    // originals for rollback if an ordinary replacement fails midway through.
    for (const [name, text] of plan) {
      const file = path.join(base, name);
      const directory = mkdtempSync(path.join(path.dirname(file), ".rift-version-"));
      const item = { file, directory, replacement: path.join(directory, "next"), backup: path.join(directory, "original") };
      staged.push(item);
      writeFileSync(item.replacement, text);
      writeFileSync(item.backup, originals.get(name));
      chmodSync(item.replacement, modes.get(name));
      chmodSync(item.backup, modes.get(name));
    }
    for (const [name, text] of originals)
      if (readFileSync(path.join(base, name), "utf8") !== text) throw new Error(`File changed while preparing version: ${name}`);
    for (const item of staged) {
      renameSync(item.replacement, item.file);
      installed.push(item);
    }
  } catch (error) {
    const failures = [];
    for (const item of installed.reverse()) {
      try { renameSync(item.backup, item.file); }
      catch (rollback) {
        retained.add(item.directory);
        failures.push(new Error(`Could not restore ${item.file}; original retained at ${item.backup}`, { cause: rollback }));
      }
    }
    if (failures.length) throw new AggregateError([error, ...failures], failures.map((failure) => failure.message).join("; "), { cause: error });
    throw error;
  } finally {
    for (const item of staged)
      if (!retained.has(item.directory)) rmSync(item.directory, { recursive: true, force: true });
  }
  // Renames are per-file, not a crash-atomic transaction across all paths.
}

export function setVersion(version, base = root) {
  if (typeof version !== "string") throw new Error("Expected an explicit stable version X.Y.Z");
  tagVersion(`v${version}`);
  const originals = new Map();
  const modes = new Map();
  for (const name of [...files, "fuzz/Cargo.toml"]) {
    const file = path.join(base, name);
    const stat = lstatSync(file);
    if (!stat.isFile()) throw new Error(`Expected a regular file: ${name}`);
    originals.set(name, readFileSync(file, "utf8"));
    modes.set(name, stat.mode & 0o777);
  }
  const workspace = workspaceVersion(originals.get("Cargo.toml"), version);
  checkVersions(`v${workspace.current}`, base);
  const reference = originals.get("desktop/src/testing/core-contract.json");
  if (JSON.parse(reference).app_version !== workspace.current)
    throw new Error("Version mismatch in desktop/src/testing/core-contract.json");
  const plan = new Map([
    ["Cargo.toml", workspace.text],
    ...files.slice(1, 3).map((name) => [name, jsonVersion(originals.get(name), version)]),
    ...files.slice(3, 5).map((name) => [name, lockVersion(originals.get(name), workspace.current, version)]),
    ["desktop/src/testing/core-contract.json", jsonVersion(reference, version, "app_version")],
  ]);
  if ([...plan].every(([name, text]) => originals.get(name) === text)) return version;
  validatePlan(version, plan, originals.get("fuzz/Cargo.toml"));
  install(base, originals, plan, modes);
  return version;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length !== 3) throw new Error("Usage: node tooling/version.mjs X.Y.Z");
    console.log(setVersion(process.argv[2]));
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
