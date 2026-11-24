#!/usr/bin/env node
// Rift-owned tooling: `tauri build` with build-machine paths removed from rift.exe.
// Rust keeps source paths for panic messages; without remapping the installer would carry
// e.g. C:\Users\<name>\.cargo\registry\... . Arguments are passed to `tauri build`.
// (Cargo `trim-paths` does the same but is not stable in the pinned toolchain.) C code built
// by cc (aws-lc-sys) keeps its __FILE__ paths under CARGO_HOME. When cross-building with
// clang-cl (cargo-xwin) the same prefixes are passed through CL_FLAGS, which cargo-xwin
// forwards to the target C compiler only. MSVC `cl.exe` on a Windows host has no equivalent
// option: there, build with a CARGO_HOME outside the user profile.
import { lstatSync, readdirSync, realpathSync } from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const home = os.homedir();
// cross-spawn lives in desktop/node_modules (its own package.json); createRequire resolves
// it from there regardless of where this script itself is invoked from.
const spawnSync = createRequire(path.join(root, "desktop/package.json"))(
  "cross-spawn",
).sync;

/** `from`, `to` pairs; rustc lets the last matching prefix win, so specific ones go last. */
export function prefixes(env = process.env) {
  return [
    [home, "~"],
    [env.RUSTUP_HOME ?? path.join(home, ".rustup"), "/rustup"],
    [env.CARGO_HOME ?? path.join(home, ".cargo"), "/cargo"],
    [root, "/rift"],
  ];
}
export const remaps = (env) =>
  prefixes(env).map(([from, to]) => `--remap-path-prefix=${from}=${to}`);
/**
 * `CL_FLAGS` for cargo-xwin's clang-cl: the same prefixes as `remaps`, as `-ffile-prefix-map`.
 * cargo-xwin splits the value on whitespace, so a prefix containing whitespace cannot be
 * expressed and is skipped here (rustc still remaps it); the artifact scan catches what is left.
 * Empty on a Windows host, where the C compiler is not clang-cl.
 */
export function clFlags(env = process.env, platform = process.platform) {
  if (platform === "win32") return env.CL_FLAGS ?? "";
  const maps = prefixes(env)
    .filter(([from]) => !/\s/.test(from))
    .map(([from, to]) => `/clang:-ffile-prefix-map=${from}=${to}`);
  return [env.CL_FLAGS, ...maps].filter(Boolean).join(" ");
}
// An env var, not a forwarded CLI arg: pnpm's `--` passthrough and PowerShell's quoting of a
// path argument are both unreliable across the shells CI and local builds use (the
// manual release command sets this to merge in createUpdaterArtifacts; a plain
// plain build without signing secrets must not do — nothing could satisfy it).
export const bundleConfigArgs = (env = process.env) =>
  env.RIFT_BUNDLE_CONFIG ? ["--config", env.RIFT_BUNDLE_CONFIG] : [];

/**
 * CI downloads the already-checked bundle from this run instead of rebuilding it with native
 * signing credentials present. Refuse incomplete bundles, symlinks and source maps before
 * disabling the hook; neither native consumer should ship a different frontend or raw maps.
 */
export function prebuiltFrontendConfigArgs(
  env = process.env,
  directory = path.join(root, "desktop/dist"),
) {
  if (env.RIFT_PREBUILT_FRONTEND === undefined) return [];
  if (env.RIFT_PREBUILT_FRONTEND !== "1")
    throw new Error("RIFT_PREBUILT_FRONTEND must be 1 when set");
  const check = (entry) => {
    const stat = lstatSync(entry);
    if (stat.isSymbolicLink())
      throw new Error("Prebuilt frontend must not contain symbolic links");
    if (!stat.isDirectory() && !stat.isFile())
      throw new Error("Prebuilt frontend must contain only files and directories");
    return stat;
  };
  if (!check(directory).isDirectory())
    throw new Error("Prebuilt frontend must be a directory");
  const index = check(path.join(directory, "index.html"));
  if (!index.isFile() || index.size === 0)
    throw new Error("Prebuilt frontend must contain a nonempty index.html");
  const assets = path.join(directory, "assets");
  if (!check(assets).isDirectory() || readdirSync(assets).length === 0)
    throw new Error("Prebuilt frontend must contain a nonempty assets directory");
  let hasJavaScript = false;
  const walk = (entry) => {
    const stat = check(entry);
    if (stat.isDirectory()) {
      for (const name of readdirSync(entry)) walk(path.join(entry, name));
    } else if (entry.toLowerCase().endsWith(".map")) {
      throw new Error("Prebuilt frontend must not contain source maps");
    } else if (
      entry.startsWith(`${assets}${path.sep}`) &&
      entry.toLowerCase().endsWith(".js") &&
      stat.size > 0
    ) {
      hasJavaScript = true;
    }
  };
  walk(directory);
  if (!hasJavaScript)
    throw new Error("Prebuilt frontend must contain a nonempty JavaScript asset");
  return ["--config", JSON.stringify({ build: { beforeBuildCommand: "" } })];
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
  // Encoded form (0x1f-separated) keeps paths with spaces intact.
  const existing =
    process.env.CARGO_ENCODED_RUSTFLAGS?.split("\x1f") ??
    (process.env.RUSTFLAGS ?? "").split(/\s+/);
  const rustflags = [...existing, ...remaps()].filter(Boolean).join("\x1f");
  const result = spawnSync(
    "pnpm",
    [
      "--dir",
      "desktop",
      "tauri",
      "build",
      ...bundleConfigArgs(),
      ...prebuiltFrontendConfigArgs(),
      ...process.argv.slice(2),
    ],
    {
      cwd: root,
      stdio: "inherit",
      env: {
        ...process.env,
        CARGO_ENCODED_RUSTFLAGS: rustflags,
        ...(clFlags() ? { CL_FLAGS: clFlags() } : {}),
      },
    },
  );
  process.exitCode = result.status ?? 1;
}
