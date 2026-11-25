import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import os from "node:os";
import { createRequire } from "node:module";
import path from "node:path";
import {
  bundleConfigArgs,
  clFlags,
  prebuiltFrontendConfigArgs,
  remaps,
} from "./tauri-build.mjs";

test("specific prefixes come after the home directory, so they win in rustc", () => {
  const flags = remaps({ CARGO_HOME: "/c/cargo home", RUSTUP_HOME: "/r" });
  assert.equal(flags.length, 4);
  assert.match(flags[0], /^--remap-path-prefix=.+=~$/);
  assert.equal(flags[1], "--remap-path-prefix=/r=/rustup");
  // Spaces stay inside one flag: the wrapper passes CARGO_ENCODED_RUSTFLAGS.
  assert.equal(flags[2], "--remap-path-prefix=/c/cargo home=/cargo");
  assert.match(flags[3], /=\/rift$/);
});

test("clang-cl gets the same prefixes, specific ones last, appended to the caller's CL_FLAGS", () => {
  const flags = clFlags(
    { CARGO_HOME: "/c/cargo", RUSTUP_HOME: "/r", CL_FLAGS: "-DX" },
    "linux",
  ).split(" ");
  assert.equal(flags[0], "-DX");
  const maps = flags.slice(1);
  for (const map of maps) assert.match(map, /^\/clang:-ffile-prefix-map=.+=.+$/);
  // rustup and cargo are always present; the home directory and the checkout only when their
  // paths contain no whitespace, which the machine running the test decides.
  assert.ok(maps.includes("/clang:-ffile-prefix-map=/r=/rustup"));
  assert.ok(maps.includes("/clang:-ffile-prefix-map=/c/cargo=/cargo"));
  const order = maps.map((map) => map.split("=").at(-1));
  assert.ok(order.indexOf("/rustup") < order.indexOf("/cargo"));
  if (order.includes("~")) assert.equal(order[0], "~", "the home directory is the most general");
  if (order.includes("/rift")) assert.equal(order.at(-1), "/rift", "the checkout is the most specific");
});

test("a prefix with whitespace cannot be split into a C flag and is left to rustc alone", () => {
  const flags = clFlags({ CARGO_HOME: "/c/cargo home", RUSTUP_HOME: "/r" }, "linux");
  assert.ok(!flags.includes("cargo home"));
  assert.ok(flags.includes("=/rustup"));
  assert.ok(!flags.includes("=/cargo"));
});

test("a Windows host keeps CL_FLAGS untouched: its C compiler is not clang-cl", () => {
  assert.equal(clFlags({ CL_FLAGS: "-DX" }, "win32"), "-DX");
  assert.equal(clFlags({}, "win32"), "");
});

test("bundle config override is opt-in via env, not forwarded as a bare CLI arg", () => {
  assert.deepEqual(bundleConfigArgs({}), []);
  assert.deepEqual(
    bundleConfigArgs({ RIFT_BUNDLE_CONFIG: "src-tauri/tauri.release.conf.json" }),
    ["--config", "src-tauri/tauri.release.conf.json"],
  );
});

function prebuiltFixture(t) {
  const directory = mkdtempSync(path.join(os.tmpdir(), "tauri-prebuilt-test-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  mkdirSync(path.join(directory, "assets"));
  writeFileSync(path.join(directory, "index.html"), '<script src="./assets/app.js"></script>');
  writeFileSync(path.join(directory, "assets/app.js"), 'console.log("fixture");');
  return directory;
}

test("ordinary builds leave the frontend hook enabled without reading dist", () => {
  assert.deepEqual(prebuiltFrontendConfigArgs({}, "/missing/frontend"), []);
});

test("prebuilt mode accepts only its explicit opt-in value", () => {
  for (const value of ["", "0", "true", "yes"]) {
    assert.throws(
      () => prebuiltFrontendConfigArgs({ RIFT_PREBUILT_FRONTEND: value }, "/missing/frontend"),
      /must be 1/,
    );
  }
});

test("validated prebuilt bundle explicitly disables the hook after the release override", (t) => {
  const directory = prebuiltFixture(t);
  const env = {
    RIFT_PREBUILT_FRONTEND: "1",
    RIFT_BUNDLE_CONFIG: "src-tauri/tauri.release.conf.json",
  };
  const args = [...bundleConfigArgs(env), ...prebuiltFrontendConfigArgs(env, directory)];
  assert.deepEqual(args.slice(0, 2), ["--config", env.RIFT_BUNDLE_CONFIG]);
  assert.deepEqual(args.slice(2), [
    "--config",
    JSON.stringify({ build: { beforeBuildCommand: "" } }),
  ]);
});

for (const entry of ["index.html", "assets"]) {
  test(`prebuilt mode fails when ${entry} is missing`, (t) => {
    const directory = prebuiltFixture(t);
    rmSync(path.join(directory, entry), { recursive: true });
    assert.throws(() => prebuiltFrontendConfigArgs({ RIFT_PREBUILT_FRONTEND: "1" }, directory));
  });
}

test("prebuilt mode rejects an empty index or assets directory", (t) => {
  const directory = prebuiltFixture(t);
  writeFileSync(path.join(directory, "index.html"), "");
  assert.throws(
    () => prebuiltFrontendConfigArgs({ RIFT_PREBUILT_FRONTEND: "1" }, directory),
    /nonempty index/,
  );
  writeFileSync(path.join(directory, "index.html"), "<html></html>");
  rmSync(path.join(directory, "assets/app.js"));
  assert.throws(
    () => prebuiltFrontendConfigArgs({ RIFT_PREBUILT_FRONTEND: "1" }, directory),
    /nonempty assets/,
  );
});

for (const kind of ["empty child directory", "empty JavaScript", "CSS only", "JavaScript outside assets"]) {
  test(`prebuilt mode rejects assets containing ${kind}`, (t) => {
    const directory = prebuiltFixture(t);
    rmSync(path.join(directory, "assets/app.js"));
    if (kind === "empty child directory") mkdirSync(path.join(directory, "assets/empty"));
    if (kind === "empty JavaScript") writeFileSync(path.join(directory, "assets/app.js"), "");
    if (kind === "CSS only") writeFileSync(path.join(directory, "assets/app.css"), "body{}");
    if (kind === "JavaScript outside assets") {
      mkdirSync(path.join(directory, "assets/empty"));
      writeFileSync(path.join(directory, "app.js"), "console.log(1)");
    }
    assert.throws(
      () => prebuiltFrontendConfigArgs({ RIFT_PREBUILT_FRONTEND: "1" }, directory),
      /nonempty JavaScript asset/,
    );
  });
}

test("prebuilt mode accepts a nonempty nested JavaScript asset", (t) => {
  const directory = prebuiltFixture(t);
  rmSync(path.join(directory, "assets/app.js"));
  mkdirSync(path.join(directory, "assets/chunks"));
  writeFileSync(path.join(directory, "assets/chunks/app.js"), "console.log(1)");
  assert.equal(prebuiltFrontendConfigArgs({ RIFT_PREBUILT_FRONTEND: "1" }, directory).length, 2);
});

for (const entry of ["app.map", "assets/app.js.MAP"]) {
  test(`prebuilt mode rejects source maps at ${entry}`, (t) => {
    const directory = prebuiltFixture(t);
    writeFileSync(path.join(directory, entry), "{}");
    assert.throws(
      () => prebuiltFrontendConfigArgs({ RIFT_PREBUILT_FRONTEND: "1" }, directory),
      /source maps/,
    );
  });
}

test("prebuilt mode rejects a symlink root and nested asset", { skip: process.platform === "win32" }, (t) => {
  const directory = prebuiltFixture(t);
  const linkedRoot = path.join(directory, "linked-root");
  symlinkSync(directory, linkedRoot, "dir");
  assert.throws(
    () => prebuiltFrontendConfigArgs({ RIFT_PREBUILT_FRONTEND: "1" }, linkedRoot),
    /symbolic links/,
  );
  rmSync(linkedRoot);
  symlinkSync(path.join(directory, "assets/app.js"), path.join(directory, "assets/linked.js"));
  assert.throws(
    () => prebuiltFrontendConfigArgs({ RIFT_PREBUILT_FRONTEND: "1" }, directory),
    /symbolic links/,
  );
});

test("a real run hands the tauri CLI both the rustc remaps and the C compiler's CL_FLAGS", { skip: process.platform === "win32" }, () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "tauri-build-test-"));
  try {
    // A stand-in for `pnpm` that records what the wrapper would have given `tauri build`.
    const record = path.join(dir, "record.txt");
    writeFileSync(
      path.join(dir, "pnpm"),
      `#!/bin/sh\nprintf 'ARGS=%s\\nCL=%s\\nRUSTFLAGS=%s\\n' "$*" "$CL_FLAGS" "$CARGO_ENCODED_RUSTFLAGS" > "${record}"\n`,
    );
    chmodSync(path.join(dir, "pnpm"), 0o755);
    const result = spawnSync(process.execPath, [path.join(import.meta.dirname, "tauri-build.mjs"), "--bundles", "nsis"], {
      env: {
        ...process.env,
        PATH: `${dir}${path.delimiter}${process.env.PATH}`,
        CL_FLAGS: "-DX",
        // Fixed, so that a home directory with whitespace on this machine changes nothing.
        HOME: path.join(dir, "home"),
        CARGO_HOME: "/c/cargo",
        RUSTUP_HOME: "/r",
      },
      encoding: "utf8",
    });
    assert.equal(result.status, 0, result.stderr);
    const text = readFileSync(record, "utf8");
    assert.match(text, /^ARGS=--dir desktop tauri build --bundles nsis$/m);
    assert.match(text, /^CL=-DX .*\/clang:-ffile-prefix-map=.*=\/cargo/m);
    // The home directory is the prefix of everything under /home/<user> in C `__FILE__` strings.
    assert.ok(text.includes(`/clang:-ffile-prefix-map=${path.join(dir, "home")}=~`), text);
    assert.match(text, /^RUSTFLAGS=.*--remap-path-prefix=.*\/home=~/m);
    assert.match(text, /^RUSTFLAGS=.*--remap-path-prefix=.*=\/cargo/m);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the installed Tauri CLI passes only process values to its build hook despite adjacent dotenv files", (t) => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "tauri-cli-env-test-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const native = path.join(dir, "src-tauri");
  mkdirSync(path.join(native, "src"), { recursive: true });
  writeFileSync(path.join(dir, "package.json"), JSON.stringify({
    name: "rift-isolated-env-consumer", version: "0.0.0", private: true,
  }));
  writeFileSync(path.join(native, "Cargo.toml"),
    '[package]\nname = "rift-isolated-env-consumer"\nversion = "0.0.0"\nedition = "2021"\n');
  writeFileSync(path.join(native, "src/main.rs"), "fn main() {}\n");
  writeFileSync(path.join(native, "tauri.conf.json"), JSON.stringify({
    productName: "RiftIsolatedEnvConsumer",
    version: "0.0.0",
    identifier: "test.rift.environment",
    build: { beforeBuildCommand: "node probe.cjs", frontendDist: "../dist" },
  }));
  // Abort at the real hook: no native compilation, bundling or signing is needed.
  writeFileSync(path.join(dir, "probe.cjs"), `
    require("node:fs").writeFileSync("record.json", JSON.stringify({
      fileOnly: process.env.RIFT_TAURI_ENV_FILE_ONLY ?? null,
      explicit: process.env.RIFT_TAURI_ENV_EXPLICIT,
      encodedRustflags: process.env.CARGO_ENCODED_RUSTFLAGS,
    }));
    process.exit(63);
  `);
  for (const base of [dir, native]) {
    for (const name of [
      ".env", ".env.local", ".env.production", ".env.production.local",
      ".env.development", ".env.development.local",
    ]) {
      writeFileSync(path.join(base, name),
        "RIFT_TAURI_ENV_FILE_ONLY=dotenv-only\nRIFT_TAURI_ENV_EXPLICIT=dotenv-override\n");
    }
  }
  const require = createRequire(path.resolve(import.meta.dirname, "../desktop/package.json"));
  const cli = path.join(path.dirname(require.resolve("@tauri-apps/cli/package.json")), "tauri.js");
  const env = {
    ...process.env,
    CARGO_NET_OFFLINE: "true",
    RIFT_TAURI_ENV_EXPLICIT: "explicit-process-value",
    CARGO_ENCODED_RUSTFLAGS: "--cfg=environment_probe\x1f--remap-path-prefix=/synthetic=/source",
  };
  delete env.RIFT_TAURI_ENV_FILE_ONLY;
  const result = spawnSync(process.execPath, [cli, "build", "--no-bundle"], {
    cwd: dir, env, encoding: "utf8", timeout: 30_000,
  });
  assert.equal(result.status, 1, `${result.error ?? ""}\n${result.stdout}\n${result.stderr}`);
  assert.match(result.stderr, /beforeBuildCommand .*failed with exit code 63/);
  assert.deepEqual(JSON.parse(readFileSync(path.join(dir, "record.json"), "utf8")), {
    fileOnly: null,
    explicit: "explicit-process-value",
    encodedRustflags: env.CARGO_ENCODED_RUSTFLAGS,
  });
});
