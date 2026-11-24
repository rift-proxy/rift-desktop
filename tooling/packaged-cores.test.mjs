import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  mkdtempSync,
  copyFileSync,
  readFileSync,
  mkdirSync,
  writeFileSync,
  rmSync,
  symlinkSync,
  renameSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  inspectTree,
  inspectPackage,
  validateArchiveListing,
} from "./packaged-cores.mjs";

function fixture(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rift-package-test-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const tree = path.join(root, "package");
  mkdirSync(tree);
  const spec = {};
  for (const name of ["sing-box", "xray"]) {
    const bytes = Buffer.from(`synthetic executable ${name}`);
    spec[name] = { windows: createHash("sha256").update(bytes).digest("hex") };
    writeFileSync(path.join(tree, `${name}.exe`), bytes);
  }
  for (const name of [
    "LICENSE",
    "sing-box-LICENSE",
    "xray-LICENSE",
    "tao-LICENSE",
    "THIRD-PARTY-NOTICES.md",
  ]) {
    writeFileSync(path.join(root, name), `synthetic ${name}`);
    writeFileSync(path.join(tree, name), `synthetic ${name}`);
  }
  return { root, tree, spec };
}

test("package CLI actually executes and missing artifacts fail rather than skip", (t) => {
  const { root } = fixture(t);
  const result = spawnSync(
    process.execPath,
    [
      path.join(import.meta.dirname, "packaged-cores.mjs"),
      path.join(root, "missing-setup.exe"),
    ],
    { encoding: "utf8" },
  );
  assert.ifError(result.error);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /ENOENT/);
});

test("shipped bytes and licenses are checked, including compressed packages", (t) => {
  const { root, tree, spec } = fixture(t);
  assert.equal(Object.keys(inspectTree(tree, { root, spec })).length, 7);
  const zip = path.join(root, "portable.zip");
  execFileSync("7z", ["a", zip, "."], { cwd: tree, stdio: "pipe" });
  assert.deepEqual(
    inspectPackage(zip, { root, spec }),
    inspectTree(tree, { root, spec }),
  );
});

for (const name of [
  "xray.exe",
  "sing-box.exe",
  "xray-LICENSE",
  "THIRD-PARTY-NOTICES.md",
]) {
  test(`a missing or replaced ${name} fails the package gate`, (t) => {
    const { root, tree, spec } = fixture(t),
      file = path.join(tree, name);
    writeFileSync(file, "wrong release");
    assert.throws(() => inspectTree(tree, { root, spec }), /digest mismatch/);
    rmSync(file);
    assert.throws(() => inspectTree(tree, { root, spec }), /exactly one/);
  });
}

function peHeader() {
  const bytes = Buffer.alloc(512);
  bytes.write("MZ");
  bytes.writeUInt32LE(64, 0x3c);
  bytes.write("PE\0\0", 64, "binary");
  bytes.writeUInt16LE(0x8664, 68);
  bytes.writeUInt16LE(2, 86);
  bytes.writeUInt16LE(0x20b, 88);
  return bytes;
}

test("full packages require one x64 executable entrypoint", (t) => {
  const { root, tree, spec } = fixture(t),
    options = { root, spec, requireMain: true };
  assert.throws(() => inspectTree(tree, options), /entrypoint/);
  writeFileSync(path.join(tree, "Rift.exe"), "MZ text is not a PE image");
  assert.throws(() => inspectTree(tree, options), /x64 PE/);
  const bytes = peHeader();
  writeFileSync(path.join(tree, "Rift.exe"), bytes);
  assert.ok(inspectTree(tree, options)["Rift.exe"]);
  bytes.writeUInt16LE(0x14c, 68);
  writeFileSync(path.join(tree, "Rift.exe"), bytes);
  assert.throws(() => inspectTree(tree, options), /x64 PE/);
});

for (const name of ["sing-box", "xray"]) {
  test(`installer rejects ${name} outside the entrypoint directory`, (t) => {
    const { root, tree, spec } = fixture(t);
    writeFileSync(path.join(tree, "Rift.exe"), peHeader());
    mkdirSync(path.join(tree, "unrelated"));
    renameSync(
      path.join(tree, `${name}.exe`),
      path.join(tree, "unrelated", `${name}.exe`),
    );
    assert.throws(
      () => inspectTree(tree, { root, spec, requireMain: true }),
      /runtime sidecar must be beside Rift entrypoint/,
    );
  });
}

test("installer accepts colocated nested executables with separately laid out licenses", (t) => {
  const { root, tree, spec } = fixture(t);
  const bin = path.join(tree, "application");
  mkdirSync(bin);
  writeFileSync(path.join(bin, "Rift.exe"), peHeader());
  for (const name of Object.keys(spec))
    renameSync(path.join(tree, `${name}.exe`), path.join(bin, `${name}.exe`));
  assert.ok(inspectTree(tree, { root, spec, requireMain: true })["Rift.exe"]);
});

test("an actual NSIS container is inspected and a renamed ZIP is rejected", (t) => {
  const { root, tree, spec } = fixture(t);
  writeFileSync(path.join(tree, "Rift.exe"), peHeader());
  const installer = path.join(root, "fixture-setup.exe");
  const script = path.join(root, "fixture.nsi");
  writeFileSync(
    script,
    `Name "Fixture"\nOutFile "${installer}"\nInstallDir "$TEMP\\RiftFixture"\nSection\nSetOutPath "$INSTDIR"\nFile /r "${tree}/*"\nSectionEnd\n`,
  );
  execFileSync("makensis", ["-V2", script], { stdio: "pipe" });
  assert.ok(
    inspectPackage(installer, { root, spec, requireMain: true })["Rift.exe"],
  );
  const renamed = path.join(root, "renamed-setup.exe");
  execFileSync("7z", ["a", "-tzip", renamed, "."], {
    cwd: tree,
    stdio: "pipe",
  });
  assert.throws(
    () => inspectPackage(renamed, { root, spec }),
    /container type/,
  );
});

test("portable inspection rejects data and accepts a single archive root folder", (t) => {
  const { root, tree, spec } = fixture(t);
  writeFileSync(path.join(tree, "Rift.exe"), peHeader());
  const zip = path.join(root, "nested.zip");
  execFileSync("7z", ["a", zip, path.basename(tree)], {
    cwd: root,
    stdio: "pipe",
  });
  assert.ok(
    inspectPackage(zip, { root, spec, requireMain: true, portable: true })[
      "Rift.exe"
    ],
  );
  mkdirSync(path.join(tree, "data"));
  writeFileSync(path.join(tree, "data/state.json"), "synthetic private data");
  assert.throws(
    () => inspectTree(tree, { root, spec, requireMain: true, portable: true }),
    /user data/,
  );
});

test("duplicate executable and symlinks are not accepted", (t) => {
  const { root, tree, spec } = fixture(t);
  mkdirSync(path.join(tree, "nested"));
  writeFileSync(path.join(tree, "nested/xray.exe"), "stale");
  assert.throws(() => inspectTree(tree, { root, spec }), /exactly one/);
  rmSync(path.join(tree, "nested"), { recursive: true });
  symlinkSync(path.join(root, "xray-LICENSE"), path.join(tree, "link"));
  assert.throws(() => inspectTree(tree, { root, spec }), /symlink/);
});

test("unsafe archive paths, links and oversized expansion are refused before extraction", () => {
  for (const entry of [
    "Path = ../escape\nSize = 1",
    "Path = C:\\escape\nSize = 1",
    "Path = /escape\nSize = 1",
    "Path = safe\nSymbolic Link = /outside",
    "Path = safe\nHard Link = other",
    "Path = safe\nSize = 9999999999",
  ])
    assert.throws(() =>
      validateArchiveListing(`header\n----------\n${entry}\n`),
    );
});

test("installer payload cannot contain private symbols, source maps or environment files", (t) => {
  for (const name of ["rift_desktop.pdb", "app.js.map", ".env", ".env.production"]) {
    const { root, tree, spec } = fixture(t);
    writeFileSync(path.join(tree, name), "must stay private");
    assert.throws(() => inspectTree(tree, { root, spec }), /private symbols/);
  }
});
