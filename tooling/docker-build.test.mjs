import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { EventEmitter } from "node:events";
import { MANIFEST_NAME, OUTPUT_MARKER } from "./build-manifest.mjs";
import {
  BUILD_USER,
  CACHE_VOLUMES,
  DEFAULT_OUT,
  FORWARDED_ENV,
  IMAGE,
  OUTPUT_ENTRIES,
  RELEASE_BUNDLE_CONFIG,
  SECRETS_ENV_FILE,
  archiveCommand,
  archivePlan,
  assertOutputUsable,
  clearPreviousOutput,
  forwardSignals,
  imageBuildArgs,
  isLoopbackProxy,
  parseArgs,
  prepareOutput,
  proxyArgs,
  runArgs,
  runPipeline,
  treeish,
} from "./docker-build.mjs";

const COMMIT = "c".repeat(40);
const run = (overrides = {}) => ({
  out: "/o",
  rootDir: "/a",
  release: false,
  env: {},
  hasDotEnv: false,
  commit: COMMIT,
  ...overrides,
});

const rootDir = path.resolve(import.meta.dirname, "..");
const dockerfile = readFileSync(path.join(rootDir, "docker/build.Dockerfile"), "utf8");
const entrypoint = readFileSync(path.join(rootDir, "docker/build-entrypoint.sh"), "utf8");

test("arguments: output directory, release and rebuild, nothing else", () => {
  assert.deepEqual(parseArgs([]), { out: null, release: false, rebuild: false });
  assert.deepEqual(parseArgs(["--out", "x", "--release", "--rebuild"]), {
    out: "x",
    release: true,
    rebuild: true,
  });
  assert.throws(() => parseArgs(["--out"]), /needs a directory/);
  assert.throws(() => parseArgs(["--shell"]), /unknown argument/);
});

test("the image receives the caller's numeric ids and never a user name", () => {
  const args = imageBuildArgs({ uid: 1234, gid: 4321, rebuild: false });
  assert.deepEqual(args.slice(args.indexOf("--build-arg")).slice(0, 4), [
    "--build-arg",
    "BUILD_UID=1234",
    "--build-arg",
    "BUILD_GID=4321",
  ]);
  assert.ok(!args.join(" ").includes("BUILD_USER"));
  assert.ok(!args.includes("--no-cache"));
  assert.ok(imageBuildArgs({ uid: 1, gid: 1, rebuild: true }).includes("--no-cache"));
});

test("the image is built from a tar on stdin, so the recipe is the commit's and not the working tree's", () => {
  const args = imageBuildArgs({ uid: 1, gid: 1, rebuild: false });
  // `-` is the context read from stdin; the Dockerfile is a name inside that tar, not a host path.
  assert.equal(args.at(-1), "-");
  assert.equal(args[args.indexOf("--file") + 1], "build.Dockerfile");
  assert.ok(!args.some((arg) => path.isAbsolute(arg)));
});

test("the image build does not take this process's proxy: Docker's own configuration decides", () => {
  const args = imageBuildArgs({ uid: 1, gid: 1, rebuild: false });
  assert.ok(!args.some((arg) => /proxy/i.test(arg)));
  assert.ok(!args.includes("--network"));
});

test("the container is unprivileged and mounts caches, the output and nothing from the source tree", () => {
  const args = runArgs(run());
  assert.equal(args.at(-1), IMAGE);
  assert.ok(args.includes("--rm") && args.includes("--interactive"));
  // PID 1 that forwards signals: the entry point script handles none itself.
  assert.ok(args.includes("--init"));
  assert.ok(args.includes(`RIFT_BUILD_COMMIT=${COMMIT}`));
  assert.deepEqual(args.slice(args.indexOf("--cap-drop"), args.indexOf("--cap-drop") + 2), [
    "--cap-drop",
    "ALL",
  ]);
  assert.ok(args.includes("no-new-privileges"));
  const volumes = args.flatMap((arg, i) => (args[i - 1] === "--volume" ? [arg] : []));
  assert.deepEqual(volumes, [...CACHE_VOLUMES.map(([n, t]) => `${n}:${t}`), "/o:/out"]);
  for (const [, target] of CACHE_VOLUMES)
    assert.ok(target === "/work/target" || target.startsWith(`/home/${BUILD_USER}/`), target);
  assert.ok(!args.some((arg) => arg.includes("/a:") || arg === "/a"));
});

test("secrets are forwarded by name only, and .env is mounted read-only when present", () => {
  const env = {
    TAURI_SIGNING_PRIVATE_KEY: "key-material",
    UNRELATED: "x",
  };
  const args = runArgs(run({ env, hasDotEnv: true }));
  assert.ok(args.includes("TAURI_SIGNING_PRIVATE_KEY"));
  assert.ok(!args.includes("RIFT_UPDATE_ENDPOINT"), "unset variables are not forwarded");
  assert.ok(!args.includes("UNRELATED"));
  assert.ok(!args.join(" ").includes("key-material"));
  // Mounted outside /work: install scripts of dependencies must not see it.
  assert.ok(args.includes(`${path.join("/a", ".env")}:${SECRETS_ENV_FILE}:ro`));
  assert.ok(!SECRETS_ENV_FILE.startsWith("/work"));
  assert.ok(FORWARDED_ENV.includes("TAURI_SIGNING_PRIVATE_KEY_PASSWORD"));
  assert.ok(!runArgs(run({ env })).join(" ").includes(".env"));
});

test("the release bundle config is merged only for --release", () => {
  const plain = runArgs(run());
  const release = runArgs(run({ release: true }));
  assert.ok(!plain.join(" ").includes("RIFT_BUNDLE_CONFIG"));
  assert.ok(release.includes(`RIFT_BUNDLE_CONFIG=${RELEASE_BUNDLE_CONFIG}`));
});

test("the archived tree is the directory itself, whether it is a subfolder or the repository root", () => {
  assert.equal(treeish("project/"), "HEAD:project");
  assert.equal(treeish(""), "HEAD:");
  assert.equal(treeish("project/", COMMIT), `${COMMIT}:project`);
  // The image recipe is a subdirectory of that tree, at the same commit.
  assert.deepEqual(archiveCommand("project/", "/top", COMMIT, "docker").args.slice(-1), [`${COMMIT}:project/docker`]);
  assert.deepEqual(archiveCommand("", "/top", COMMIT, "docker").args.slice(-1), [`${COMMIT}:docker`]);
  assert.deepEqual(archiveCommand("project/", "/top").args.slice(-1), ["HEAD:project"]);
});

test("the Dockerfile pins its inputs and takes no host identity", () => {
  assert.match(dockerfile, /^ARG RUST_IMAGE=rust:\d+\.\d+\.\d+-\w+$/m);
  assert.match(dockerfile, /^ARG NODE_IMAGE=node:\d+\.\d+\.\d+-\w+-slim$/m);
  assert.match(dockerfile, /^ARG CARGO_XWIN_VERSION=\d+\.\d+\.\d+$/m);
  assert.ok(dockerfile.includes(`ARG BUILD_USER=${BUILD_USER}`));
  assert.match(dockerfile, /cargo install --locked --version/);
  const manager = JSON.parse(readFileSync(path.join(rootDir, "desktop/package.json"), "utf8")).packageManager;
  assert.ok(dockerfile.includes(`ARG PNPM_VERSION=${manager.replace("pnpm@", "")}`), manager);
  // No host user name or home directory is an input.
  assert.ok(!/\$\{?(USER|LOGNAME|HOME)\b/.test(dockerfile.replace(/\$\{BUILD_USER\}/g, "")));
  // No part of the host source tree is copied in: only the two files next to the Dockerfile.
  const copies = dockerfile.split("\n").filter((line) => /^(COPY|ADD)\b/.test(line) && !line.includes("--from="));
  assert.deepEqual(copies, ["COPY --chmod=0755 build-entrypoint.sh /usr/local/bin/rift-build"]);
  assert.match(dockerfile, /^USER \$\{BUILD_USER\}$/m);
});

test("the entry point installs from the lockfile, builds with the cross runner and reads source from stdin", () => {
  assert.match(entrypoint, /^set -eu$/m);
  // -m: extracted files are newer than the cargo state of the persistent target volume, whatever
  // the commit's own dates are, so an older commit built after a newer one is not "fresh".
  assert.match(entrypoint, /tar -x -m -f - -C \/work/);
  assert.match(entrypoint, /pnpm install --frozen-lockfile/);
  assert.match(entrypoint, /node tooling\/tauri-build\.mjs --runner cargo-xwin --target x86_64-pc-windows-msvc/);
  // The wrapper (path remapping) is the build entry, not a bare `tauri build`.
  assert.ok(!/(^|\s)pnpm .*tauri build/.test(entrypoint));
  assert.ok(!entrypoint.includes("--no-frozen-lockfile"));
});

test("git archive of a subdirectory is not empty, nested or when the project is the repository root", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "docker-build-test-"));
  const git = (cwd, ...args) =>
    spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.invalid", ...args], {
      cwd,
      encoding: "utf8",
    });
  const listing = (prefix, toplevel, cwd = toplevel) => {
    const { args } = archiveCommand(prefix, toplevel);
    const tar = spawnSync("git", args, { cwd, maxBuffer: 1 << 24 });
    assert.equal(tar.status, 0, String(tar.stderr));
    const list = spawnSync("tar", ["-t", "-f", "-"], { input: tar.stdout, encoding: "utf8" });
    return list.stdout.split("\n").filter((entry) => entry && !entry.endsWith("/"));
  };
  try {
    mkdirSync(path.join(dir, "project", "core"), { recursive: true });
    writeFileSync(path.join(dir, "project", "core", "lib.rs"), "x");
    writeFileSync(path.join(dir, "outside.md"), "private");
    git(dir, "init", "-q");
    git(dir, "add", "-A");
    git(dir, "commit", "-q", "-m", "x");
    assert.deepEqual(listing("project/", dir), ["core/lib.rs"]);
    // The command always runs at the top level: from inside `project/` git limits the archive to
    // that directory again and `HEAD:project` comes out empty.
    assert.equal(archiveCommand("project/", dir).cwd, dir);
    assert.deepEqual(listing("project/", dir, path.join(dir, "project")), []);
    // When the directory is the repository root, the tree is the whole repository.
    assert.deepEqual(listing("", dir).sort(), ["outside.md", "project/core/lib.rs"]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a proxy is forwarded by name; one on the host's loopback needs the host network", () => {
  assert.deepEqual(proxyArgs({}), []);
  const loopback = proxyArgs({ HTTPS_PROXY: "http://user:pw@127.0.0.1:10809", NO_PROXY: "localhost" });
  assert.deepEqual(loopback.slice(0, 2), ["--network", "host"]);
  assert.ok(loopback.includes("HTTPS_PROXY") && loopback.includes("NO_PROXY"));
  assert.ok(loopback.includes("NODE_USE_ENV_PROXY=1"));
  assert.ok(!loopback.join(" ").includes("pw") && !loopback.join(" ").includes("10809"));
  for (const url of ["http://localhost:3128", "socks5://[::1]:1080", "http://127.1.2.3/"])
    assert.ok(proxyArgs({ http_proxy: url }).includes("host"), url);
  const remote = proxyArgs({ HTTPS_PROXY: "http://proxy.example.invalid:3128" });
  assert.ok(!remote.includes("--network"));
  assert.ok(remote.includes("HTTPS_PROXY") && remote.includes("NODE_USE_ENV_PROXY=1"));
  // A host that merely starts with "localhost" is not loopback.
  assert.ok(!proxyArgs({ HTTPS_PROXY: "http://localhost.example.invalid:1" }).includes("--network"));
});

test("runArgs adds the proxy options and keeps the image name last", () => {
  const args = runArgs(run({ env: { HTTPS_PROXY: "http://127.0.0.1:1" } }));
  assert.equal(args.at(-1), IMAGE);
  assert.ok(args.includes("host") && args.includes("HTTPS_PROXY"));
});

test("loopback proxies are recognised however they are written, and only those", () => {
  for (const url of [
    "http://127.0.0.1:10809",
    "127.0.0.1:10809",
    "localhost:3128",
    "LOCALHOST:3128",
    "http://0.0.0.0:3128",
    "http://127.1:3128",
    "socks5://[::1]:1080",
    "http://user:p@ss@127.0.0.1:1",
    "http://127.0.0.1",
    "http://localhost.:3128",
    "127.0.0.1.",
    "::1",
    "http://[::1]",
  ])
    assert.equal(isLoopbackProxy(url), true, url);
  for (const url of [
    "http://proxy.example.invalid:3128",
    "http://localhost.example.invalid:1",
    "http://evil.example/?x=@127.0.0.1",
    "http://evil.example#@localhost/",
    "http://10.0.0.1:3128",
    "http://127.0.0.1.example.invalid:1",
    "http://127.0.0.256:3128",
    "http://127.0.0.1.:1.example.invalid",
    "",
  ])
    assert.equal(isLoopbackProxy(url), false, url);
});

test("a container that exits without reading its input ends the pipeline instead of hanging it", async () => {
  // 60 MB cannot fit in a pipe: a writer that is not stopped blocks for ever.
  const archive = spawn(process.execPath, ["-e", "process.stdout.write(Buffer.alloc(60e6))"], {
    stdio: ["ignore", "pipe", "inherit"],
  });
  const container = spawn("sh", ["-c", "exit 3"], { stdio: ["pipe", "inherit", "inherit"] });
  let timer;
  let result;
  try {
    result = await Promise.race([
      runPipeline(archive, container),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error("pipeline hung")), 15_000);
      }),
    ]);
  } finally {
    clearTimeout(timer);
    // A pipeline that did hang must not leave its writer (60 MB into a pipe nobody reads) behind
    // to keep the test process alive.
    archive.kill();
    container.kill();
  }
  assert.equal(result.containerCode, 3);
  assert.notEqual(result.archiveCode, 0);
});

test("the pipeline reports both exit codes on success and when the archive fails", async () => {
  const container = () => spawn("sh", ["-c", "cat > /dev/null"], { stdio: ["pipe", "inherit", "inherit"] });
  const ok = await runPipeline(
    spawn(process.execPath, ["-e", "process.stdout.write('tar')"], { stdio: ["ignore", "pipe", "inherit"] }),
    container(),
  );
  assert.deepEqual(ok, { archiveCode: 0, containerCode: 0 });
  const failed = await runPipeline(
    spawn(process.execPath, ["-e", "process.exit(1)"], { stdio: ["ignore", "pipe", "inherit"] }),
    container(),
  );
  assert.deepEqual(failed, { archiveCode: 1, containerCode: 0 });
});

test("dependency scripts and downloads run without the secrets, only the build gets them", () => {
  const install = entrypoint.indexOf("without_secrets pnpm install --frozen-lockfile");
  const sidecar = entrypoint.indexOf("without_secrets node tooling/sidecar.mjs");
  const copy = entrypoint.indexOf("cp /run/rift-build/.env /work/.env");
  const build = entrypoint.indexOf("node tooling/tauri-build.mjs");
  assert.ok(install > 0 && sidecar > install && copy > sidecar && build > copy);
  for (const name of FORWARDED_ENV)
    assert.ok(entrypoint.includes(`-u ${name}`), name);
  assert.match(entrypoint, /rm -f \/work\/\.env/);
  // Only what this script itself produced is replaced in the output directory.
  assert.match(
    entrypoint,
    /^rm -rf \/out\/bundle\/nsis \/out\/rift-desktop\.exe \/out\/frontend \/out\/symbols \/out\/build-manifest\.json$/m,
  );
  assert.ok(!/rm -rf \/out\/bundle( |$)/m.test(entrypoint));
});

test("an exclusion list alone is not a proxy and never selects the host network", () => {
  assert.deepEqual(proxyArgs({ NO_PROXY: "127.0.0.1" }), []);
  assert.deepEqual(proxyArgs({ no_proxy: "localhost" }), []);
  const remote = proxyArgs({ HTTPS_PROXY: "http://proxy.corp.invalid:3128", NO_PROXY: "localhost,127.0.0.1" });
  assert.ok(!remote.includes("--network"));
  assert.ok(remote.includes("HTTPS_PROXY") && remote.includes("NO_PROXY"));
  assert.ok(proxyArgs({ HTTPS_PROXY: "http://127.0.0.1:1", NO_PROXY: "x" }).includes("host"));
});

test("a previous build's output is removed before a new build, and nothing else is", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "docker-build-test-"));
  try {
    for (const entry of OUTPUT_ENTRIES) {
      mkdirSync(path.dirname(path.join(dir, entry)), { recursive: true });
      writeFileSync(path.join(dir, entry), "old");
    }
    writeFileSync(path.join(dir, "keep.txt"), "mine");
    mkdirSync(path.join(dir, "bundle", "other"), { recursive: true });
    clearPreviousOutput(dir);
    for (const entry of OUTPUT_ENTRIES) assert.equal(existsSync(path.join(dir, entry)), false, entry);
    assert.equal(readFileSync(path.join(dir, "keep.txt"), "utf8"), "mine");
    assert.equal(existsSync(path.join(dir, "bundle", "other")), true);
    clearPreviousOutput(path.join(dir, "missing")); // nothing to remove is not an error
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the entry point removes .env however the build ends and keeps the frontend bundle", () => {
  assert.match(entrypoint, /trap 'rm -f \/work\/\.env' EXIT/);
  assert.match(entrypoint, /cp -r \/work\/desktop\/dist \/out\/frontend/);
});

test("the entry point keeps the debug file and the source maps beside the build, outside the shipped bundle", () => {
  // The maps are made outside dist (which the executable embeds) before the build starts...
  const maps = entrypoint.indexOf("export RIFT_MAPS_DIR=/work/desktop/dist-maps");
  const build = entrypoint.indexOf("node tooling/tauri-build.mjs");
  assert.ok(maps > 0 && maps < build);
  // ...and copied after it; a missing PDB fails the build (set -e), it is never skipped.
  assert.match(entrypoint, /cp "\$release\/rift_desktop\.pdb" \/out\/symbols\/rift_desktop\.pdb/);
  assert.match(entrypoint, /cp -r "\$RIFT_MAPS_DIR" \/out\/symbols\/maps/);
  assert.ok(entrypoint.indexOf("/out/symbols/maps") < entrypoint.indexOf("build-manifest.mjs write"));
  assert.ok(entrypoint.includes("set -eu"));
  assert.ok(OUTPUT_ENTRIES.includes("symbols"));
});

test("the entry point refuses an unmarked output, builds with a locked Cargo.lock and a clean bundle directory, and writes the manifest last", () => {
  const marker = entrypoint.indexOf("/out/.rift-build-output");
  const install = entrypoint.indexOf("pnpm install --frozen-lockfile");
  const locked = entrypoint.indexOf("cargo fetch --locked --target x86_64-pc-windows-msvc");
  const build = entrypoint.indexOf("node tooling/tauri-build.mjs");
  const stale = entrypoint.indexOf('rm -rf "/work/$release/bundle"');
  const copyExe = entrypoint.indexOf("cp \"$release/rift-desktop.exe\"");
  const manifest = entrypoint.indexOf('node tooling/build-manifest.mjs write /out "$RIFT_BUILD_COMMIT"');
  assert.ok(marker > 0 && marker < install, "the marker is checked before anything is written");
  assert.ok(locked > install && locked < build, "the lock is verified before the build");
  assert.ok(stale > 0 && stale < build, "installers of earlier builds are removed first");
  assert.ok(manifest > copyExe, "the manifest is the last write");
  assert.ok(entrypoint.includes("RIFT_BUILD_COMMIT is not set"));
  assert.ok(entrypoint.includes("-u TAURI_SIGNING_PRIVATE_KEY"));
  assert.equal(OUTPUT_MARKER, ".rift-build-output");
  assert.equal(MANIFEST_NAME, "build-manifest.json");
});

test("the build user is created with --non-unique, so a caller's gid that exists in the image still works", () => {
  assert.match(dockerfile, /groupadd --non-unique --gid "\$\{BUILD_GID\}"/);
  assert.match(dockerfile, /useradd --non-unique --uid "\$\{BUILD_UID\}"/);
});

test("the output directory must be new, empty or an earlier build's: anything else is refused untouched", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "docker-build-test-"));
  try {
    assertOutputUsable(path.join(dir, "missing"));
    const empty = path.join(dir, "empty");
    mkdirSync(empty);
    assertOutputUsable(empty);
    const foreign = path.join(dir, "foreign");
    mkdirSync(path.join(foreign, "frontend"), { recursive: true });
    writeFileSync(path.join(foreign, "frontend", "mine.txt"), "mine");
    assert.throws(() => prepareOutput(foreign), /not empty and is not the output of an earlier build/);
    assert.equal(readFileSync(path.join(foreign, "frontend", "mine.txt"), "utf8"), "mine");
    assert.equal(existsSync(path.join(foreign, OUTPUT_MARKER)), false);
    // An earlier output carries the marker and is replaced.
    const earlier = path.join(dir, "earlier");
    prepareOutput(earlier);
    assert.equal(existsSync(path.join(earlier, OUTPUT_MARKER)), true);
    mkdirSync(path.join(earlier, "frontend"));
    writeFileSync(path.join(earlier, "rift-desktop.exe"), "old");
    writeFileSync(path.join(earlier, MANIFEST_NAME), "{}");
    prepareOutput(earlier);
    assert.deepEqual(readdirSync(earlier), [OUTPUT_MARKER]);
    assert.ok(OUTPUT_ENTRIES.includes(MANIFEST_NAME));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("signals received by the build are passed on to docker, and the handlers can be removed", () => {
  const proc = new EventEmitter();
  const killed = [];
  const child = (name) => ({ kill: (signal) => killed.push([name, signal]) });
  const stop = forwardSignals([child("archive"), child("docker")], proc);
  proc.emit("SIGINT");
  proc.emit("SIGTERM");
  assert.deepEqual(killed, [
    ["archive", "SIGINT"],
    ["docker", "SIGINT"],
    ["archive", "SIGTERM"],
    ["docker", "SIGTERM"],
  ]);
  stop();
  assert.equal(proc.listenerCount("SIGINT") + proc.listenerCount("SIGTERM"), 0);
});

// The tool itself against a stand-in `docker`: it records what it was asked and plays the part of
// the container. `git` is the real one: the tool archives the commit of this very checkout.
function harness() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "docker-build-main-"));
  const bin = path.join(dir, "bin");
  mkdirSync(bin);
  writeFileSync(
    path.join(bin, "docker"),
    [
      "#!/bin/sh",
      'printf \'%s\\n\' "$*" >> "$STUB_LOG"',
      "cat > /dev/null",
      'case "$1:${STUB_MODE:-}" in',
      "  build:build-fails) exit 1 ;;",
      "  run:run-fails) exit 3 ;;",
      // Stays in the container until it is signalled, and says so; `ready` only once the handler
      // is installed, so that a signal sent earlier cannot be mistaken for a missed one.
      "  run:hang) trap 'echo TERM >> \"$STUB_LOG\"; exit 143' TERM; echo ready >> \"$STUB_LOG\"; sleep 30 & wait $! ;;",
      "  run:*) for a in \"$@\"; do case $a in *:/out) out=${a%:/out} ;; esac; done",
      '    mkdir -p "$out/bundle/nsis" "$out/frontend"',
      '    echo exe > "$out/rift-desktop.exe"; echo setup > "$out/bundle/nsis/x-setup.exe"; echo html > "$out/frontend/index.html"',
      "    ;;",
      "esac",
      "exit 0",
      "",
    ].join("\n"),
    { mode: 0o755 },
  );
  const log = path.join(dir, "docker.log");
  writeFileSync(log, "");
  const environment = (mode) => {
    const env = { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH}`, STUB_LOG: log, STUB_MODE: mode };
    for (const name of ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy", "NO_PROXY", "no_proxy"])
      delete env[name];
    return env;
  };
  const script = path.join(import.meta.dirname, "docker-build.mjs");
  const exec = (args, mode = "") =>
    spawnSync(process.execPath, [script, ...args], { encoding: "utf8", env: environment(mode) });
  const start = (args, mode = "") => spawn(process.execPath, [script, ...args], { env: environment(mode), stdio: "ignore" });
  const calls = () => readFileSync(log, "utf8").split("\n").filter(Boolean);
  return { dir, exec, start, calls };
}

test("a build asks docker to build the image from stdin, then to run it on the same commit", () => {
  const { dir, exec, calls } = harness();
  try {
    const out = path.join(dir, "out");
    const result = exec(["--out", out]);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /artifacts: /);
    const [build, container] = calls();
    assert.match(build, /^build .*--file build\.Dockerfile .* -$/);
    const head = spawnSync("git", ["rev-parse", "HEAD"], { cwd: rootDir, encoding: "utf8" }).stdout.trim();
    assert.match(container, /^run --rm --interactive --init /);
    assert.ok(container.includes(`RIFT_BUILD_COMMIT=${head}`));
    assert.ok(container.includes(`${out}:/out`));
    assert.equal(existsSync(path.join(out, OUTPUT_MARKER)), true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a failed image build leaves the previous output exactly as it was and never starts a container", () => {
  const { dir, exec, calls } = harness();
  try {
    const out = path.join(dir, "out");
    mkdirSync(out);
    writeFileSync(path.join(out, OUTPUT_MARKER), "");
    writeFileSync(path.join(out, "rift-desktop.exe"), "previous build");
    const result = exec(["--out", out], "build-fails");
    assert.equal(result.status, 1);
    assert.match(result.stderr, /docker build failed/);
    assert.equal(readFileSync(path.join(out, "rift-desktop.exe"), "utf8"), "previous build");
    assert.equal(calls().length, 1);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a failing container is a failed build, and a foreign directory is refused before docker is called", () => {
  const { dir, exec, calls } = harness();
  try {
    const failed = exec(["--out", path.join(dir, "out")], "run-fails");
    assert.equal(failed.status, 1);
    assert.match(failed.stderr, /build failed \(exit 3\)/);
    const foreign = path.join(dir, "foreign");
    mkdirSync(path.join(foreign, "frontend"), { recursive: true });
    writeFileSync(path.join(foreign, "frontend", "mine.txt"), "mine");
    const before = calls().length;
    const refused = exec(["--out", foreign]);
    assert.equal(refused.status, 1);
    assert.match(refused.stderr, /not empty and is not the output of an earlier build/);
    assert.equal(calls().length, before, "docker was not asked to do anything");
    assert.equal(readFileSync(path.join(foreign, "frontend", "mine.txt"), "utf8"), "mine");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the default output is dist/windows-x64 under the project root", () => {
  assert.equal(DEFAULT_OUT.split(path.sep).join("/"), "dist/windows-x64");
});

test("stopping the build signals the container too, not only this process", async () => {
  const { dir, start, calls } = harness();
  try {
    const child = start(["--out", path.join(dir, "out")], "hang");
    const closed = new Promise((resolve) => child.on("close", (code, signal) => resolve({ code, signal })));
    const deadline = Date.now() + 20_000;
    while (!calls().includes("ready") && Date.now() < deadline)
      await new Promise((resolve) => setTimeout(resolve, 50));
    assert.ok(calls().some((line) => line.startsWith("run ")), "the container was started");
    assert.ok(calls().includes("ready"), "the container is waiting for a signal");
    child.kill("SIGTERM");
    const { code } = await Promise.race([
      closed,
      new Promise((_, reject) => setTimeout(() => reject(new Error("the build did not stop")), 20_000)),
    ]).finally(() => child.kill("SIGKILL"));
    assert.ok(calls().includes("TERM"), "docker received the signal");
    assert.equal(code, 1, "and the build reports failure");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the image recipe and the source are archived from the same commit", () => {
  const plan = archivePlan("project/", "/top", COMMIT);
  assert.deepEqual(plan.image.args.slice(-1), [`${COMMIT}:project/docker`]);
  assert.deepEqual(plan.source.args.slice(-1), [`${COMMIT}:project`]);
  assert.equal(plan.image.cwd, "/top");
  assert.equal(plan.source.cwd, "/top");
  assert.ok(!JSON.stringify(plan).includes("HEAD"));
});
