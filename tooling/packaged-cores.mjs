#!/usr/bin/env node
// Verify the bytes actually shipped, not only the downloader cache or archive inventory.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  lstatSync,
  readdirSync,
  readFileSync,
  mkdtempSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { CORE_SPEC } from "./sidecar.mjs";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(import.meta.dirname, "..");
const digest = (file) =>
  createHash("sha256").update(readFileSync(file)).digest("hex");
const LICENSES = [
  "LICENSE",
  "sing-box-LICENSE",
  "xray-LICENSE",
  "tao-LICENSE",
  "THIRD-PARTY-NOTICES.md",
];

function files(dir) {
  return readdirSync(dir).flatMap((name) => {
    const file = path.join(dir, name),
      stat = lstatSync(file);
    if (stat.isSymbolicLink()) throw Error("Package contains a symlink");
    return stat.isDirectory() ? files(file) : [file];
  });
}

export function inspectTree(
  dir,
  { root = ROOT, spec = CORE_SPEC, requireMain = false, portable = false } = {},
) {
  const found = files(dir);
  if (found.some((file) => /\.(?:pdb|map)$/i.test(file) || /^\.env(?:\.|$)/i.test(path.basename(file))))
    throw Error("Package contains private symbols, source maps or environment files");
  const expected = Object.fromEntries([
    ...Object.entries(spec).map(([name, core]) => [
      `${name}.exe`,
      core.windows,
    ]),
    ...LICENSES.map((name) => [name, digest(path.join(root, name))]),
  ]);
  const checked = {};
  let entryDirectory = dir;
  for (const [name, hash] of Object.entries(expected)) {
    const matches = found.filter(
      (file) => path.basename(file).toLowerCase() === name.toLowerCase(),
    );
    if (matches.length !== 1) throw Error(`Package needs exactly one ${name}`);
    if (digest(matches[0]) !== hash)
      throw Error(`Package digest mismatch: ${name}`);
    checked[name] = hash;
  }
  if (requireMain) {
    const main = found.filter((file) =>
      ["rift.exe", "rift-desktop.exe"].includes(
        path.basename(file).toLowerCase(),
      ),
    );
    if (main.length !== 1)
      throw Error("Package needs exactly one Rift PE entrypoint");
    const bytes = readFileSync(main[0]);
    const pe = bytes.length >= 64 ? bytes.readUInt32LE(0x3c) : bytes.length;
    if (
      bytes.subarray(0, 2).toString() !== "MZ" ||
      pe + 26 > bytes.length ||
      bytes.subarray(pe, pe + 4).toString("binary") !== "PE\0\0" ||
      bytes.readUInt16LE(pe + 4) !== 0x8664 ||
      bytes.readUInt16LE(pe + 24) !== 0x20b ||
      !(bytes.readUInt16LE(pe + 22) & 2) ||
      bytes.readUInt16LE(pe + 22) & 0x2000
    )
      throw Error("Rift entrypoint is not an x64 PE executable");
    checked[path.basename(main[0])] = digest(main[0]);
    entryDirectory = path.dirname(main[0]);
    for (const name of Object.keys(spec)) {
      const sidecar = found.find(
        (file) => path.basename(file).toLowerCase() === `${name}.exe`.toLowerCase(),
      );
      if (path.dirname(sidecar) !== entryDirectory)
        throw Error(`Package runtime sidecar must be beside Rift entrypoint: ${name}.exe`);
    }
  }
  if (portable) {
    const allowed = new Set([
      ...Object.keys(checked).map((name) => name.toLowerCase()),
      "sha256sums.txt",
    ]);
    if (
      found.some(
        (file) =>
          path.dirname(file) !== entryDirectory ||
          !allowed.has(path.basename(file).toLowerCase()),
      )
    )
      throw Error("Portable package contains unexpected files or user data");
  }
  return checked;
}

export function validateArchiveListing(listing) {
  const entries = listing.split(/^----------\r?$/m)[1];
  if (!entries) throw Error("Cannot inspect package archive listing");
  let size = 0,
    count = 0;
  for (const block of entries.trim().split(/\r?\n\r?\n/)) {
    const name = /^Path = (.+)$/m
      .exec(block)?.[1]
      ?.trim()
      .replaceAll("\\", "/");
    if (
      !name ||
      name.startsWith("/") ||
      /^[A-Za-z]:/.test(name) ||
      name.split("/").includes("..") ||
      /^(Symbolic Link|Hard Link) = .+/m.test(block) ||
      /^Attributes = .*l[rwx-]{9}/m.test(block)
    )
      throw Error("Unsafe package archive entry");
    size += Number(/^Size = (\d+)$/m.exec(block)?.[1] ?? 0);
    if (++count > 10_000 || size > 1024 ** 3)
      throw Error("Package archive exceeds inspection budget");
  }
}

export function inspectPackage(target, options = {}) {
  if (lstatSync(target).isSymbolicLink())
    throw Error("Package cannot be a symlink");
  if (lstatSync(target).isDirectory()) return inspectTree(target, options);
  const directory = mkdtempSync(path.join(tmpdir(), "rift-core-package-"));
  try {
    const listing = execFileSync("7z", ["l", "-slt", target], {
      encoding: "utf8",
      maxBuffer: 8 << 20,
      timeout: 60_000,
    });
    if (
      (/-setup\.exe$/i.test(target) && !/^Type = Nsis\r?$/m.test(listing)) ||
      (/\.zip$/i.test(target) && !/^Type = zip\r?$/m.test(listing))
    )
      throw Error("Package container type does not match installer/ZIP");
    validateArchiveListing(listing);
    execFileSync("7z", ["x", "-y", `-o${directory}`, target], {
      stdio: "pipe",
      timeout: 60_000,
    });
    return inspectTree(directory, options);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [installer, ...extra] = process.argv.slice(2);
    if (!installer || extra.length || !installer.endsWith("-setup.exe")) throw Error("Usage: node tooling/packaged-cores.mjs <installer-setup.exe>");
    console.log(JSON.stringify(inspectPackage(path.resolve(installer), { requireMain: true }), null, 2));
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
