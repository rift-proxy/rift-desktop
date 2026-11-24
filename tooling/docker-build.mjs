#!/usr/bin/env node
// Builds the Windows installer in a container (docker/build.Dockerfile) from the committed tree,
// so that no path or user name of this machine can end up in the artifacts.
//
//   node tooling/docker-build.mjs [--out <dir>] [--release] [--rebuild]
//
//   --out <dir>   where rift-desktop.exe, bundle/nsis/ and frontend/ are written (default
//                 dist/windows-x64). The directory must be new, empty or an earlier output of
//                 this build (it holds an empty `.rift-build-output` file): only then are
//                 rift-desktop.exe, bundle/nsis, frontend and build-manifest.json replaced.
//                 Legacy symbols directories must be moved aside manually; they are not deleted.
//   --release     also merge src-tauri/tauri.release.conf.json (signed updater artifacts; needs
//                 TAURI_SIGNING_PRIVATE_KEY and its password in the environment)
//   --rebuild     rebuild the image without Docker's layer cache
//
// Both the image recipe (docker/) and the source are `git archive` of the same commit, streamed
// through stdin: uncommitted changes are not built, and the host checkout is not mounted.
// RIFT_UPDATE_ENDPOINT and the updater signing key are passed by name, so their values
// never appear on a command line. A build ends with build-manifest.json in the
// output: the commit and the SHA-256 of every file (tooling/build-manifest.mjs).
import { spawn, spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import { MANIFEST_NAME, OUTPUT_MARKER } from "./build-manifest.mjs";

const root = path.resolve(import.meta.dirname, "..");

export const IMAGE = "rift-build:local";
export const BUILD_USER = "testerek";
/** Where a build writes by default, relative to the project root. */
export const DEFAULT_OUT = path.join("dist", "windows-x64");
/** Named volumes that keep downloads and build state between builds. None holds the source tree;
 * `rift-build-target` is /work/target, where build.rs leaves the update endpoint it embeds. */
export const CACHE_VOLUMES = [
  ["rift-build-target", "/work/target"],
  ["rift-build-cargo", `/home/${BUILD_USER}/.cargo`],
  ["rift-build-cache", `/home/${BUILD_USER}/.cache`],
  ["rift-build-pnpm", `/home/${BUILD_USER}/.local/share/pnpm`],
];
/** Environment variables forwarded by name (the value is read from this process by docker). */
export const FORWARDED_ENV = [
  "RIFT_UPDATE_ENDPOINT",
  "TAURI_SIGNING_PRIVATE_KEY",
  "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
];
export const RELEASE_BUNDLE_CONFIG = "src-tauri/tauri.release.conf.json";
/** Standard proxy variables, forwarded by name like the secrets (a proxy URL may carry a login). */
export const PROXY_URL_ENV = [
  "HTTP_PROXY",
  "HTTPS_PROXY",
  "ALL_PROXY",
  "http_proxy",
  "https_proxy",
  "all_proxy",
];
/** Exclusion lists: forwarded together with a proxy, never a reason to configure one. */
export const NO_PROXY_ENV = ["NO_PROXY", "no_proxy"];
export const PROXY_ENV = [...PROXY_URL_ENV, ...NO_PROXY_ENV];
/** What a build leaves in the output directory (relative to it); replaced as a whole each time. */
export const OUTPUT_ENTRIES = [
  "rift-desktop.exe",
  "bundle/nsis",
  "frontend",
  MANIFEST_NAME,
];

/** Whether a proxy URL points at the host's own loopback (any scheme, or none: `127.0.0.1:3128`).
 * Credentials may contain `@`, so the host is what follows the last `@` of the authority; a
 * `?`, `#` or `/` ends the authority, so `http://evil.example/?x=@127.0.0.1` is not loopback. */
export function isLoopbackProxy(value) {
  const authority = value.trim().replace(/^[a-z][a-z0-9+.-]*:\/\//i, "").split(/[/?#]/, 1)[0];
  const hostPort = authority.slice(authority.lastIndexOf("@") + 1);
  let host;
  if (hostPort.startsWith("[")) host = hostPort.slice(1, hostPort.indexOf("]"));
  else if (hostPort.split(":").length > 2) host = hostPort; // bare IPv6, no port
  else host = hostPort.replace(/:\d*$/, "");
  host = host.toLowerCase().replace(/\.$/, ""); // `localhost.` is the same name
  if (host === "localhost" || host === "::1" || host === "0.0.0.0") return true;
  const octets = host.split(".");
  return (
    /^127(\.\d{1,3}){0,3}$/.test(host) && octets.every((octet) => Number(octet) <= 255)
  );
}

/** The proxy URL variables that are set, and whether one of them is on the host's loopback. */
function proxyState(env) {
  const urls = PROXY_URL_ENV.filter((name) => env[name]);
  return {
    urls,
    names: [...urls, ...NO_PROXY_ENV.filter((name) => env[name])],
    loopback: urls.some((name) => isLoopbackProxy(env[name])),
  };
}

/**
 * Network options for a build behind a proxy. A proxy on the host's loopback is unreachable
 * from a container's own network, so such a build shares the host network, which means that
 * build and install scripts can reach the host's loopback services (README, "Building in
 * Docker"); Node needs NODE_USE_ENV_PROXY=1 to make `fetch` (sidecar downloads) use
 * the proxy.
 */
export function proxyArgs(env) {
  const { urls, names, loopback } = proxyState(env);
  if (urls.length === 0) return [];
  return [
    ...(loopback ? ["--network", "host"] : []),
    ...names.flatMap((name) => ["--env", name]),
    "--env",
    "NODE_USE_ENV_PROXY=1",
  ];
}

export function parseArgs(argv) {
  const options = { out: null, release: false, rebuild: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--release") options.release = true;
    else if (arg === "--rebuild") options.rebuild = true;
    else if (arg === "--out") {
      options.out = argv[(i += 1)];
      if (!options.out) throw new Error("--out needs a directory");
    } else throw new Error(`unknown argument: ${arg}`);
  }
  return options;
}

/** `docker build` of an image whose context (the docker/ directory of one commit) arrives on
 * stdin as a tar: the Dockerfile and the entry point are those of the commit being built. The
 * proxy of this process is deliberately not passed on: as a build argument it changes the cache
 * key of every layer and fails the build when the proxy cannot be reached from the build
 * network. A proxy the image build needs is Docker's own setting (`proxies` in its config.json). */
export function imageBuildArgs({ uid, gid, rebuild }) {
  return [
    "build",
    ...(rebuild ? ["--no-cache"] : []),
    "--file",
    "build.Dockerfile",
    "--tag",
    IMAGE,
    "--build-arg",
    `BUILD_UID=${uid}`,
    "--build-arg",
    `BUILD_GID=${gid}`,
    "-",
  ];
}

export function runArgs({ out, release, env, commit }) {
  return [
    "run",
    "--rm",
    "--interactive",
    // PID 1 that forwards signals and reaps children: without it the entry point script, which
    // handles none, would ignore SIGINT/SIGTERM and the build would go on after Ctrl-C.
    "--init",
    "--cap-drop",
    "ALL",
    "--security-opt",
    "no-new-privileges",
    ...CACHE_VOLUMES.flatMap(([name, target]) => ["--volume", `${name}:${target}`]),
    "--volume",
    `${out}:/out`,
    ...FORWARDED_ENV.filter((name) => env[name] !== undefined).flatMap((name) => [
      "--env",
      name,
    ]),
    "--env",
    `RIFT_BUILD_COMMIT=${commit}`,
    ...(release ? ["--env", `RIFT_BUNDLE_CONFIG=${RELEASE_BUNDLE_CONFIG}`] : []),
    ...proxyArgs(env),
    IMAGE,
  ];
}

/** `git archive` tree-ish of a directory of one commit, whether it is a subdirectory or the root. */
export const treeish = (prefix, ref = "HEAD") => `${ref}:${prefix.replace(/\/+$/, "")}`;

/** The `git archive` call that yields a directory's committed files at the archive root: this
 * directory (`sub` empty) or one of its subdirectories (`sub`, such as `docker`). It runs from the
 * repository's top level: inside a subdirectory `git archive` restricts the archive to that
 * directory again, and `HEAD:<this directory>` would come out empty. */
export const archiveCommand = (prefix, toplevel, ref = "HEAD", sub = "") => ({
  args: [
    "archive",
    "--format=tar",
    treeish(sub ? path.posix.join(prefix, sub) : prefix, ref),
  ],
  cwd: toplevel,
});

/** The two archives of one commit: the image recipe (`docker/`) and the source tree. */
export function archivePlan(prefix, toplevel, commit) {
  return {
    image: archiveCommand(prefix, toplevel, commit, "docker"),
    source: archiveCommand(prefix, toplevel, commit),
  };
}

/**
 * Streams `archive`'s stdout into `container`'s stdin and resolves with both exit codes. A
 * container that exits without reading everything (docker refused, the daemon is down) breaks the
 * pipe; the writer would then block for ever, so it is stopped as soon as the container is gone.
 */
export function runPipeline(archive, container) {
  return new Promise((resolve) => {
    let archiveCode = null;
    let containerCode = null;
    const settle = () => {
      if (archiveCode !== null && containerCode !== null)
        resolve({ archiveCode, containerCode });
    };
    container.stdin.on("error", () => {});
    archive.stdout.on("error", () => {});
    archive.on("close", (code, signal) => {
      archiveCode = code ?? (signal ? 143 : 1);
      settle();
    });
    container.on("close", (code) => {
      containerCode = code ?? 1;
      if (archiveCode === null) archive.kill();
      settle();
    });
    archive.stdout.pipe(container.stdin);
  });
}

/** Passes SIGINT and SIGTERM received by this process on to `children`, so that stopping the
 * build stops `docker` (which in turn signals the container) instead of leaving it running.
 * Returns the function that removes the handlers. */
export function forwardSignals(children, proc = process, signals = ["SIGINT", "SIGTERM"]) {
  const handlers = signals.map((signal) => {
    const handler = () => {
      for (const child of children) child.kill(signal);
    };
    proc.on(signal, handler);
    return [signal, handler];
  });
  return () => {
    for (const [signal, handler] of handlers) proc.off(signal, handler);
  };
}

/** Removes what an earlier build left, so a build that fails cannot leave a previous success
 * behind to be taken for the current one. Only the entries a build writes are touched. */
export function clearPreviousOutput(out) {
  for (const entry of OUTPUT_ENTRIES)
    rmSync(path.join(out, entry), { recursive: true, force: true });
}

/** Throws unless `out` may be used for a build: new, empty or already marked as a build's output.
 * Any other directory is refused before anything in it is touched, because entries such as
 * `frontend` are common names in a directory that belongs to something else. */
export function assertOutputUsable(out) {
  if (!existsSync(out)) return;
  const entries = readdirSync(out);
  if (entries.length > 0 && !entries.includes(OUTPUT_MARKER))
    throw new Error(
      `${out} is not empty and is not the output of an earlier build: use a new or empty ` +
        "directory (a build replaces rift-desktop.exe, bundle/nsis and frontend plus " +
        `${MANIFEST_NAME} there)`,
    );
  if (entries.includes("symbols"))
    throw new Error(
      `${out} contains legacy symbols output: move symbols aside or use a new output directory`,
    );
}

/** Makes `out` ready for a build and marks it as this build's own. */
export function prepareOutput(out) {
  assertOutputUsable(out);
  mkdirSync(out, { recursive: true });
  writeFileSync(path.join(out, OUTPUT_MARKER), "");
  clearPreviousOutput(out);
}

function git(args) {
  const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
  if (result.status !== 0) throw new Error(`git ${args[0]} failed: ${result.stderr}`);
  return result.stdout;
}

/** Runs `archive` and a docker command that reads its output on stdin; stopping this process
 * stops both. Resolves with the exit codes. */
async function pipeThroughDocker(archiveSpec, dockerArgs) {
  const archive = spawn("git", archiveSpec.args, {
    cwd: archiveSpec.cwd,
    stdio: ["ignore", "pipe", "inherit"],
  });
  const container = spawn("docker", dockerArgs, { stdio: ["pipe", "inherit", "inherit"] });
  const stopForwarding = forwardSignals([archive, container]);
  try {
    return await runPipeline(archive, container);
  } finally {
    stopForwarding();
  }
}

export async function main(argv) {
  const options = parseArgs(argv);
  const out = path.resolve(options.out ?? path.join(root, DEFAULT_OUT));

  // Refused first and cheaply: the image build below takes minutes.
  assertOutputUsable(out);
  const uid = process.getuid?.() ?? 1000;
  // The image's build user takes the caller's ids so that the output belongs to the caller; uid 0
  // would make that user root and the build would no longer be unprivileged.
  if (uid === 0) throw new Error("run as a regular user, not as root");
  if (git(["status", "--porcelain", "--untracked-files=no", "--", "."]).trim())
    console.warn("warning: uncommitted changes are not part of the build (HEAD is archived)");

  // One commit for the image recipe, the source and the manifest: HEAD may move meanwhile.
  const commit = git(["rev-parse", "HEAD"]).trim();
  const prefix = git(["rev-parse", "--show-prefix"]).trim();
  const toplevel = git(["rev-parse", "--show-toplevel"]).trim();

  const plan = archivePlan(prefix, toplevel, commit);
  const image = await pipeThroughDocker(
    plan.image,
    imageBuildArgs({
      uid,
      gid: process.getgid?.() ?? 1000,
      rebuild: options.rebuild,
    }),
  );
  if (image.containerCode !== 0 || image.archiveCode !== 0)
    throw new Error("docker build failed");

  // Only now, when nothing can fail before the container starts, is the previous output removed.
  prepareOutput(out);
  const { containerCode, archiveCode } = await pipeThroughDocker(
    plan.source,
    runArgs({
      out,
      release: options.release,
      env: process.env,
      commit,
    }),
  );
  if (containerCode !== 0) throw new Error(`build failed (exit ${containerCode})`);
  if (archiveCode !== 0) throw new Error("git archive failed");
  console.log(`artifacts: ${out}`);
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
  main(process.argv.slice(2)).catch((error) => {
    console.error(`error: ${error.message}`);
    process.exitCode = 1;
  });
}
