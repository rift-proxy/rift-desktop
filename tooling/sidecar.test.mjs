import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ensureSidecar, sha256 } from "./sidecar.mjs";

test("flat Xray archives use the same verified extraction boundary", async (t) => {
  const { dest, calls, options } = setup(t, {});
  options.extractedRelative = "xray.exe";
  options.extract = (ext, archivePath, cache) => {
    calls.extractions += 1;
    assert.equal(readFileSync(archivePath).toString(), ARCHIVE.toString());
    writeFileSync(join(cache, "xray.exe"), GOOD);
  };
  assert.equal(await ensureSidecar(options), "downloaded");
  assert.equal(calls.extractions, 1);
  assert.equal(readFileSync(dest).toString(), GOOD.toString());
});

test("a corrupt flat Xray binary cannot replace a cached executable", async (t) => {
  const { dest, options } = setup(t, { cached: TAMPERED });
  options.extractedRelative = "xray.exe";
  options.extract = (_ext, _archivePath, cache) =>
    writeFileSync(join(cache, "xray.exe"), TAMPERED);
  await assert.rejects(ensureSidecar(options), /Binary SHA-256 mismatch/);
  assert.equal(readFileSync(dest).toString(), TAMPERED.toString());
});

const NAME = "sing-box-test-linux-amd64";
const GOOD = Buffer.from("verified sing-box binary");
const TAMPERED = Buffer.from("tampered cached binary");
const ARCHIVE = Buffer.from("pretend archive bytes");

// A scratch layout with the destination directory, a cache directory and fakes that count
// downloads and extractions. `extracted` is what the fake extractor puts at <cache>/<name>/sing-box.
function setup(
  t,
  { cached, extracted = GOOD, archive = ARCHIVE, status = 200 } = {},
) {
  const directory = mkdtempSync(join(tmpdir(), "rift-sidecar-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const dest = join(directory, "binaries", "sing-box");
  mkdirSync(join(directory, "binaries"));
  if (cached) writeFileSync(dest, cached);
  const calls = { downloads: [], extractions: 0, logs: [] };
  return {
    dest,
    calls,
    options: {
      dest,
      cache: join(directory, "cache", NAME),
      name: NAME,
      ext: "tar.gz",
      suffix: "",
      archiveDigest: sha256(ARCHIVE),
      binaryDigest: sha256(GOOD),
      url: "https://example.invalid/sing-box.tar.gz",
      download: async (url) => {
        calls.downloads.push(url);
        return {
          ok: status === 200,
          status,
          arrayBuffer: async () => archive,
        };
      },
      extract: (ext, archivePath, cache) => {
        calls.extractions += 1;
        assert.equal(readFileSync(archivePath).toString(), archive.toString());
        mkdirSync(join(cache, NAME), { recursive: true });
        writeFileSync(join(cache, NAME, "sing-box"), extracted);
      },
      log: (line) => calls.logs.push(line),
    },
  };
}

test("a cached binary with the pinned digest is used without touching the network", async (t) => {
  const { dest, calls, options } = setup(t, { cached: GOOD });
  chmodSync(dest, 0o644);
  assert.equal(await ensureSidecar(options), "cached");
  assert.deepEqual(calls.downloads, []);
  assert.equal(calls.extractions, 0);
  assert.match(calls.logs.join("\n"), /Cached binary SHA-256 verified/);
  assert.equal(readFileSync(dest).toString(), GOOD.toString());
  if (process.platform !== "win32")
    assert.equal(statSync(dest).mode & 0o111, 0o111);
});

test("a cached binary with another digest is not trusted and is replaced by a verified download", async (t) => {
  const { dest, calls, options } = setup(t, { cached: TAMPERED });
  assert.equal(await ensureSidecar(options), "downloaded");
  assert.deepEqual(calls.downloads, [options.url]);
  assert.equal(calls.extractions, 1);
  assert.equal(readFileSync(dest).toString(), GOOD.toString());
  assert.doesNotMatch(calls.logs.join("\n"), /Cached/);
});

test("a missing binary is downloaded, verified and installed", async (t) => {
  const { dest, calls, options } = setup(t);
  assert.equal(await ensureSidecar(options), "downloaded");
  assert.equal(calls.downloads.length, 1);
  assert.equal(readFileSync(dest).toString(), GOOD.toString());
});

test("an archive with the wrong digest is rejected before extraction and leaves dest alone", async (t) => {
  const { dest, calls, options } = setup(t, {
    cached: TAMPERED,
    archive: Buffer.from("substituted archive"),
  });
  await assert.rejects(ensureSidecar(options), /Archive SHA-256 mismatch/);
  assert.equal(
    calls.extractions,
    0,
    "nothing is extracted from an unverified archive",
  );
  assert.equal(readFileSync(dest).toString(), TAMPERED.toString());
});

test("an extracted binary with the wrong digest is rejected and leaves dest alone", async (t) => {
  const { dest, calls, options } = setup(t, {
    cached: TAMPERED,
    extracted: Buffer.from("different binary inside a correct archive"),
  });
  await assert.rejects(ensureSidecar(options), /Binary SHA-256 mismatch/);
  assert.equal(calls.extractions, 1);
  assert.equal(readFileSync(dest).toString(), TAMPERED.toString());
});

test("a rejected extracted binary does not create dest when there was none", async (t) => {
  const { dest, options } = setup(t, {
    extracted: Buffer.from("different binary inside a correct archive"),
  });
  await assert.rejects(ensureSidecar(options), /Binary SHA-256 mismatch/);
  assert.equal(existsSync(dest), false);
});

test("a failed download is an error, not a silent cache miss", async (t) => {
  const { dest, calls, options } = setup(t, { cached: TAMPERED, status: 503 });
  await assert.rejects(ensureSidecar(options), /Download failed: 503/);
  assert.equal(calls.extractions, 0);
  assert.equal(readFileSync(dest).toString(), TAMPERED.toString());
});

test(
  "a real tar.gz archive is extracted and its binary verified end to end",
  { skip: process.platform === "win32" },
  async (t) => {
    const directory = mkdtempSync(join(tmpdir(), "rift-sidecar-tar-"));
    t.after(() => rmSync(directory, { recursive: true, force: true }));
    mkdirSync(join(directory, "src", NAME), { recursive: true });
    writeFileSync(join(directory, "src", NAME, "sing-box"), GOOD);
    const archivePath = join(directory, "archive.tar.gz");
    execFileSync("tar", [
      "-czf",
      archivePath,
      "-C",
      join(directory, "src"),
      NAME,
    ]);
    const archive = readFileSync(archivePath);
    const dest = join(directory, "binaries", "sing-box");
    mkdirSync(join(directory, "binaries"));
    const result = await ensureSidecar({
      dest,
      cache: join(directory, "cache", NAME),
      name: NAME,
      ext: "tar.gz",
      suffix: "",
      archiveDigest: sha256(archive),
      binaryDigest: sha256(GOOD),
      url: "https://example.invalid/sing-box.tar.gz",
      download: async () => ({
        ok: true,
        status: 200,
        arrayBuffer: async () => archive,
      }),
      log: () => {},
    });
    assert.equal(result, "downloaded");
    assert.equal(readFileSync(dest).toString(), GOOD.toString());
  },
);

test("the sidecar entry point still runs through a symlink", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "rift-sidecar-entry-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const entry = join(directory, "sidecar.mjs");
  try {
    symlinkSync(
      fileURLToPath(new URL("./sidecar.mjs", import.meta.url)),
      entry,
    );
  } catch (error) {
    if (error?.code === "EPERM") return t.skip("file symlinks unavailable");
    throw error;
  }
  const result = spawnSync(process.execPath, [entry, "--help"], {
    encoding: "utf8",
  });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /Usage: node tooling\/sidecar\.mjs/);
});
