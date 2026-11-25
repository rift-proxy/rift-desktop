import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

const desktop = path.resolve(import.meta.dirname, "../desktop");
const explicit = "explicit-process-value";

// Each real consumer runs cold in its own process and synthetic project: Vite and
// Storybook may set process.env, so neither the test runner nor another case is shared.
const consumer = `
import { createRequire } from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { runInNewContext } from "node:vm";
const options = JSON.parse(process.argv[1]);
const require = createRequire(path.join(options.desktop, "package.json"));
let result;
if (options.kind === "storybook-env") {
  const { loadEnvs } = await import(pathToFileURL(require.resolve("storybook/internal/common")));
  const env = await loadEnvs({ production: options.command === "build" });
  result = { env: env.raw, stringified: env.stringified };
} else {
  const { loadConfigFromFile, resolveConfig, build } = await import(pathToFileURL(require.resolve("vite")));
  const file = options.kind === "vite" ? "vite.config.ts"
    : options.kind === "mutation" ? "vitest.mutation.config.ts"
    : options.kind === "storybook-final" ? ".storybook/main.ts" : "vitest.config.ts";
  const loaded = await loadConfigFromFile(
    { command: options.command, mode: options.mode },
    path.join(options.desktop, file), process.cwd(), "silent",
  );
  if (!loaded) throw new Error("Consumer configuration did not load");
  let config = loaded.config;
  if (options.kind.startsWith("vitest-project-")) {
    const name = options.kind.slice("vitest-project-".length);
    config = config.test.projects.find((project) => project.test.name === name);
    if (!config) throw new Error("Vitest project did not load");
  }
  if (options.kind === "storybook-final") {
    // Deliberately begin with file loading enabled, as a builder/addon could.
    // The real final hook must enforce the boundary for both server and build.
    config = await config.viteFinal({ root: process.cwd(), envDir: process.cwd() }, {
      configType: options.command === "build" ? "PRODUCTION" : "DEVELOPMENT",
    });
  }
  const inline = {
    ...config, root: process.cwd(), configFile: false, logLevel: "silent",
    cacheDir: path.join(process.cwd(), "cache"),
  };
  const resolved = await resolveConfig(inline, options.command, options.mode,
    options.command === "build" ? "production" : "development");
  result = { env: resolved.env, production: resolved.isProduction };
  if (options.bundle) {
    const output = await build({
      ...inline, mode: options.mode,
      build: {
        ...config.build, write: false,
        rollupOptions: { input: path.join(process.cwd(), "entry.js") },
      },
    });
    const context = {};
    for (const item of output.output) {
      if (item.type === "chunk") runInNewContext(item.code, context);
    }
    result.bundle = context.__riftEnvironment;
  }
}
result.process = Object.fromEntries([
  "NODE_ENV", "VITE_USER_NODE_ENV", "BROWSER", "BROWSER_ARGS",
  "VITE_RIFT_ENV_FILE_ONLY", "STORYBOOK_RIFT_ENV_FILE_ONLY", "RIFT_ENV_FILE_ONLY",
  "RIFT_ENV_UNRELATED",
].map((name) => [name, process.env[name] ?? null]));
console.log("RIFT_ENV_PROBE=" + JSON.stringify(result));
`;

function runConsumer(t, options) {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-frontend-env-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  writeFileSync(path.join(directory, "package.json"), '{"private":true,"type":"module"}\n');
  const fileNodeEnv = options.command === "build" ? "development" : "production";
  for (const name of [".env", ".env.local", `.env.${options.mode}`, `.env.${options.mode}.local`]) {
    writeFileSync(path.join(directory, name), [
      `NODE_ENV=${fileNodeEnv}`,
      "BROWSER=dotenv-browser",
      "BROWSER_ARGS=dotenv-browser-args",
      "VITE_RIFT_ENV_FILE_ONLY=dotenv-vite",
      "VITE_RIFT_ENV_EXPLICIT=dotenv-vite-override",
      "STORYBOOK_RIFT_ENV_FILE_ONLY=dotenv-storybook",
      "STORYBOOK_RIFT_ENV_EXPLICIT=dotenv-storybook-override",
      "RIFT_ENV_FILE_ONLY=dotenv-unprefixed",
      "",
    ].join("\n"));
  }
  writeFileSync(path.join(directory, "entry.js"), `globalThis.__riftEnvironment = {
    fileOnly: import.meta.env.VITE_RIFT_ENV_FILE_ONLY,
    explicit: import.meta.env.VITE_RIFT_ENV_EXPLICIT,
    development: import.meta.env.DEV,
    production: import.meta.env.PROD,
    nodeEnv: process.env.NODE_ENV,
  };\n`);
  const env = {
    ...process.env,
    VITE_RIFT_ENV_EXPLICIT: explicit,
    STORYBOOK_RIFT_ENV_EXPLICIT: explicit,
    RIFT_ENV_UNRELATED: explicit,
  };
  // Only the isolated child's probe inputs are reset. Preserve NODE_OPTIONS and
  // unrelated caller/toolchain configuration; this is not a global env sanitizer.
  for (const name of [
    "NODE_ENV", "VITE_USER_NODE_ENV", "VITEST", "BROWSER", "BROWSER_ARGS",
    "VITE_RIFT_ENV_FILE_ONLY", "STORYBOOK_RIFT_ENV_FILE_ONLY", "RIFT_ENV_FILE_ONLY",
  ]) delete env[name];
  if (options.nodeEnv) env.NODE_ENV = options.nodeEnv;
  const run = spawnSync(process.execPath, [
    "--input-type=module", "-e", consumer, JSON.stringify({ ...options, desktop }),
  ], { cwd: directory, env, encoding: "utf8", timeout: 60_000 });
  assert.equal(run.status, 0, `${run.error ?? ""}\n${run.stdout}\n${run.stderr}`);
  const line = run.stdout.split(/\r?\n/).find((value) => value.startsWith("RIFT_ENV_PROBE="));
  assert.ok(line, run.stdout);
  const result = JSON.parse(line.slice("RIFT_ENV_PROBE=".length));
  for (const name of [
    "VITE_USER_NODE_ENV", "BROWSER", "BROWSER_ARGS", "VITE_RIFT_ENV_FILE_ONLY",
    "STORYBOOK_RIFT_ENV_FILE_ONLY", "RIFT_ENV_FILE_ONLY",
  ]) assert.equal(result.process[name], null, name);
  assert.equal(result.process.RIFT_ENV_UNRELATED, explicit);
  return result;
}

for (const kind of [
  "vite", "vitest-root", "vitest-project-unit", "vitest-project-storybook", "mutation", "storybook-final",
]) {
  for (const [command, mode] of [["build", "production"], ["serve", "development"], ["serve", "custom"]]) {
    test(`${kind} ${command}/${mode} resolves process-only Vite environment`, (t) => {
      const result = runConsumer(t, { kind, command, mode });
      assert.equal(result.env.VITE_RIFT_ENV_FILE_ONLY, undefined);
      assert.equal(result.env.VITE_RIFT_ENV_EXPLICIT, explicit);
      assert.equal(result.env.MODE, mode);
      assert.equal(result.production, command === "build");
      assert.equal(result.env.PROD, command === "build");
      assert.equal(result.env.DEV, command !== "build");
      assert.equal(result.process.NODE_ENV, command === "build" ? "production" : "development");
    });
  }
}

for (const nodeEnv of [undefined, "development"]) {
  test(`production bundle ignores dotenv and preserves ${nodeEnv ? "explicit" : "default"} NODE_ENV`, (t) => {
    const result = runConsumer(t, { kind: "vite", command: "build", mode: "production", bundle: true, nodeEnv });
    assert.deepEqual(result.bundle, {
      explicit,
      development: nodeEnv === "development",
      production: nodeEnv !== "development",
      nodeEnv: nodeEnv ?? "production",
    });
  });
}

for (const [command, mode] of [["build", "production"], ["serve", "development"]]) {
  test(`Storybook core ${command} never imports dotenv into its process or prefixed environment`, (t) => {
    const result = runConsumer(t, { kind: "storybook-env", command, mode });
    assert.equal(result.env.NODE_ENV, mode);
    assert.equal(result.env.STORYBOOK_RIFT_ENV_EXPLICIT, explicit);
    assert.equal(result.stringified.STORYBOOK_RIFT_ENV_EXPLICIT, JSON.stringify(explicit));
    assert.equal(result.env.STORYBOOK_RIFT_ENV_FILE_ONLY, undefined);
    assert.equal(result.env.RIFT_ENV_UNRELATED, undefined);
    assert.equal(result.env.VITE_RIFT_ENV_EXPLICIT, undefined);
    assert.equal(result.process.NODE_ENV, null);
  });
}

test("Storybook core preserves explicit NODE_ENV without exposing unrelated process values", (t) => {
  const result = runConsumer(t, {
    kind: "storybook-env", command: "build", mode: "production", nodeEnv: "test",
  });
  assert.equal(result.env.NODE_ENV, "test");
  assert.equal(result.process.NODE_ENV, "test");
  assert.equal(result.env.STORYBOOK_RIFT_ENV_EXPLICIT, explicit);
  assert.equal(result.env.STORYBOOK_RIFT_ENV_FILE_ONLY, undefined);
  assert.equal(result.env.RIFT_ENV_UNRELATED, undefined);
});
