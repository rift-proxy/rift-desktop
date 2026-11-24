// Rift-owned tooling. Imported scripts/ is deliberately not used.
import { createHash } from "node:crypto";
import { realpathSync } from "node:fs";
import { mkdir, writeFile, readFile, copyFile, chmod } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";

export const sha256 = (bytes) =>
  createHash("sha256").update(bytes).digest("hex");

// Shared by the downloader and release verifier; no second digest allowlist.
export const CORE_SPEC = {
  "sing-box": {
    version: "1.14.1",
    windows: "b838de45bd0b2e6ddbed1977e4745622f7dffab3b293807ff4c6b1b640fed909",
    linux: "9334a1c1fe97234911afaedd37226667ebdb3dab2abe383f2c4ae1387706dad9",
  },
  xray: {
    version: "26.10.21",
    windows: "c77a00eb84fb93ae044c2d771a1cabcc033b81e487253fbc35b12df6879c1e15",
    linux: "a27e3f95ab6ce270956d0cecd32eda4686a811afc71c64a6a734a3dede14883e",
  },
};

export function extractArchive(ext, archive, cache) {
  if (ext === "zip") {
    if (process.platform === "win32")
      execFileSync("powershell", [
        "-NoProfile",
        "-Command",
        // A pwsh parent can leave PowerShell 7 modules in Windows PowerShell's search path.
        `$ErrorActionPreference = 'Stop'; Import-Module (Join-Path $PSHOME 'Modules/Microsoft.PowerShell.Archive/Microsoft.PowerShell.Archive.psd1') -Force; Expand-Archive -LiteralPath '${archive.replaceAll("'", "''")}' -DestinationPath '${cache.replaceAll("'", "''")}' -Force`,
      ]);
    else execFileSync("unzip", ["-o", "-q", archive, "-d", cache]);
  } else execFileSync("tar", ["-xzf", archive, "-C", cache]);
}

// Puts a verified core at `dest`. A cached binary is trusted only if its digest matches;
// otherwise the pinned archive is downloaded, its digest is checked before anything is
// extracted, and the extracted binary's digest is checked before it replaces `dest`. Any
// mismatch throws and leaves `dest` as it was. `download`, `extract` and `log` are injectable
// for tests.
export async function ensureSidecar({
  dest,
  cache,
  name,
  ext,
  suffix,
  extractedRelative = `${name}/sing-box${suffix}`,
  archiveDigest,
  binaryDigest,
  url,
  download = fetch,
  extract = extractArchive,
  log = console.log,
}) {
  const cachedBinary = await readFile(dest).catch(() => null);
  if (cachedBinary && sha256(cachedBinary) === binaryDigest) {
    await chmod(dest, 0o755);
    log(`${dest}\nCached binary SHA-256 verified: ${binaryDigest}`);
    return "cached";
  }
  await mkdir(cache, { recursive: true });
  const archive = resolve(cache, `${name}.${ext}`);
  const response = await download(url);
  if (!response.ok) throw Error(`Download failed: ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (sha256(bytes) !== archiveDigest) throw Error("Archive SHA-256 mismatch");
  await writeFile(archive, bytes);
  extract(ext, archive, cache);
  const extractedPath = resolve(cache, extractedRelative);
  const extracted = await readFile(extractedPath);
  if (sha256(extracted) !== binaryDigest)
    throw Error("Binary SHA-256 mismatch");
  await copyFile(extractedPath, dest);
  await chmod(dest, 0o755);
  log(`${dest}\nBinary SHA-256 verified: ${binaryDigest}`);
  return "downloaded";
}

if (process.argv[1] && realpathSync(process.argv[1]) === import.meta.filename) {
  if (["-h", "--help"].includes(process.argv[2])) {
    console.log(
      "Usage: node tooling/sidecar.mjs [windows|linux] [all|sing-box|xray]",
    );
    process.exit(0);
  }
  const platform =
    process.argv[2] ?? (process.platform === "win32" ? "windows" : "linux");
  const core = process.argv[3] ?? "all";
  if (!["all", "sing-box", "xray"].includes(core))
    throw Error("Use all, sing-box or xray");
  const assets = {
    windows: [
      "zip",
      "5197f16d492d93202dc623622149a6ed040f8eca263128f91d603f2b901baa89",
      CORE_SPEC["sing-box"].windows,
      "x86_64-pc-windows-msvc",
      ".exe",
    ],
    linux: [
      "tar.gz",
      "12cb2816b52febb356f6a885b740cc8758c3f30b8ae0ca8edba80f0d2d35343f",
      CORE_SPEC["sing-box"].linux,
      "x86_64-unknown-linux-gnu",
      "",
    ],
  };
  if (!assets[platform]) throw Error("Use windows or linux");
  const [ext, archiveDigest, binaryDigest, triple, suffix] = assets[platform];
  const name = `sing-box-${CORE_SPEC["sing-box"].version}-${platform}-amd64`;
  const root = resolve(import.meta.dirname, "..");
  if (core !== "xray")
    await ensureSidecar({
      dest: resolve(
        root,
        `desktop/src-tauri/binaries/sing-box-${triple}${suffix}`,
      ),
      cache: resolve(root, ".tools", name),
      name,
      ext,
      suffix,
      archiveDigest,
      binaryDigest,
      url: `https://github.com/SagerNet/sing-box/releases/download/v${CORE_SPEC["sing-box"].version}/${name}.${ext}`,
    });
  if (core !== "sing-box") {
    const xray = {
      linux: [
        "0eec3ed4163c273d8c8bf821f7d219e257921cf584aa3fb970715e458e631978",
        CORE_SPEC.xray.linux,
      ],
      windows: [
        "fc58c39ee2f2717b4c54d28d374362389e5319b20ae44aea76ea45a5243ec827",
        CORE_SPEC.xray.windows,
      ],
    };
    const [archiveDigest, binaryDigest] = xray[platform];
    const asset = `Xray-${platform}-64`;
    await ensureSidecar({
      dest: resolve(root, `desktop/src-tauri/binaries/xray-${triple}${suffix}`),
      cache: resolve(
        root,
        ".tools",
        `xray-${CORE_SPEC.xray.version}-${platform}`,
      ),
      name: asset,
      ext: "zip",
      suffix,
      extractedRelative: `xray${suffix}`,
      archiveDigest,
      binaryDigest,
      url: `https://github.com/XTLS/Xray-core/releases/download/v${CORE_SPEC.xray.version}/${asset}.zip`,
    });
  }
}
