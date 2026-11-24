import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

// Tauri v2 capability files are the whole permission surface of the WebView. The profile
// export/import commands rely on native dialogs, a user-chosen path and file reads/writes that
// all stay in Rust (custom commands), so no dialog, fs, shell, http or opener permission may
// ever be granted to a window. These tests fail if a later change adds one.
const root = join(import.meta.dirname, "../desktop");
const directory = join(root, "src-tauri/capabilities");
const capabilities = readdirSync(directory)
  .filter((name) => name.endsWith(".json"))
  .sort()
  .map((name) => ({ name, ...JSON.parse(readFileSync(join(directory, name), "utf8")) }));

test("only the main window has a capability: errors are a toast, not a second window", () => {
  assert.deepEqual(
    capabilities.map(({ name, identifier, windows }) => [name, identifier, windows]),
    [["main.json", "main", ["main"]]],
  );
  for (const capability of capabilities) {
    assert.equal(capability.remote, undefined, `${capability.name} must not open remote URLs`);
    assert.equal(capability.webviews, undefined, `${capability.name} must stay window-scoped`);
  }
});

test("every capability grants only event listen and unlisten", () => {
  for (const capability of capabilities) {
    assert.deepEqual(
      [...capability.permissions].sort(),
      ["core:event:allow-listen", "core:event:allow-unlisten"],
      `${capability.name} permissions changed: review the security impact first`,
    );
  }
});

test("the dialog plugin is Rust-side only", () => {
  const granted = capabilities.flatMap(({ permissions }) => permissions);
  assert.equal(granted.filter((permission) => /^(dialog|fs|shell|http|opener):/.test(permission)).length, 0);
  const manifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  const dependencies = { ...manifest.dependencies, ...manifest.devDependencies };
  assert.equal(
    Object.keys(dependencies).filter((name) => /^@tauri-apps\/plugin-(dialog|fs|shell|http|opener)$/.test(name)).length,
    0,
    "the frontend must not bundle a plugin API it has no permission for",
  );
});
