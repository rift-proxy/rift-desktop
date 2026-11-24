import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { checkContent } from "./content-check.mjs";

function fixture(t, files) {
  const root = mkdtempSync(path.join(tmpdir(), "rift-content-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const [name, contents] of Object.entries(files)) {
    const file = path.join(root, name);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, contents);
  }
  return root;
}

test("resolves standalone root, relative, reference, image and Cyrillic heading links", (t) => {
  const root = fixture(t, {
    "README.md": "# Home\n[guide](docs/guide.md#данные-и-обновления)\n[again][guide]\n[guide]: </docs/guide.md#repeat-1>\n![image](assets/icon.svg)\n",
    "docs/guide.md": "# Данные и обновления\n## Repeat\n## Repeat\n[home](/README.md#home)\n<a id=\"explicit\"></a>\n[self](#explicit)\n[shortcut]\n[shortcut]: ../README.md\n",
    "assets/icon.svg": "<svg/>",
  });
  const report = checkContent(root);
  assert.deepEqual(report.failures, []);
  assert.equal(report.markdownFiles, 2);
  assert.equal(report.internalLinks, 6);
});

test("ignores external destinations, fenced examples, inline code and generated files", (t) => {
  const root = fixture(t, {
    "README.md": "# Home\n[external](https://example.invalid/missing#gone)\n[mail](mailto:team@example.invalid)\n```md\n[bad](missing.md)\n```\n~~~\n[bad](missing.md)\n~~~\n`[bad](missing.md)`\n<!-- [bad](missing.md) -->\n",
    "desktop/node_modules/pkg/README.md": "[bad](missing.md)",
    "target/README.md": "[bad](missing.md)",
    "desktop/storybook-static/README.md": "[bad](missing.md)",
  });
  assert.deepEqual(checkContent(root), { markdownFiles: 1, internalLinks: 0, rustReferenceChecked: false, failures: [] });
});

test("fails for an existing prose link whose file was renamed", (t) => {
  const root = fixture(t, { "README.md": "[install](docs/installation.md)\n", "docs/install.md": "# Install\n" });
  assert.match(checkContent(root).failures.join("\n"), /README\.md:1:.*Missing internal target/);
});

test("fails when a real target heading was renamed", (t) => {
  const root = fixture(t, { "README.md": "[install](docs/install.md#old-heading)\n", "docs/install.md": "# New heading\n" });
  assert.match(checkContent(root).failures.join("\n"), /Missing internal anchor/);
});

test("resolves setext, escaped parentheses, percent encoding and link titles", (t) => {
  const root = fixture(t, {
    "README.md": "[guide](docs/guide\\(copy\\).md#next \"Title\")\n[space](<docs/space name.md#heading>)\n[encoded](docs/space%20name.md#heading)\n",
    "docs/guide(copy).md": "Next\n====\n",
    "docs/space name.md": "# Heading\n",
  });
  assert.deepEqual(checkContent(root).failures, []);
});

test("does not invent anchors from code spans or prose attributes", (t) => {
  const root = fixture(t, {
    "README.md": '# Home\n`<a id="code">`\nAn attribute id="prose" is not HTML.\n[code](#code)\n[prose](#prose)\n<a id="real"></a>\n[real](#real)\n',
  });
  const report = checkContent(root);
  assert.equal(report.failures.length, 2);
  assert.match(report.failures[0], /#code: Missing internal anchor/);
  assert.match(report.failures[1], /#prose: Missing internal anchor/);
});

test("resolves duplicate heading slugs that collide with an earlier suffixed heading", (t) => {
  const root = fixture(t, {
    "README.md": "# Repeat\n# Repeat-1\n# Repeat\n[third](#repeat-2)\n[second](#repeat-1)\n",
  });
  assert.deepEqual(checkContent(root).failures, []);
});

test("rejects links escaping the project root and undefined references", (t) => {
  const root = fixture(t, { "README.md": "[outside](../outside-file.md)\n[missing][unknown]\n" });
  const failures = checkContent(root).failures;
  assert.equal(failures.length, 2);
  assert.match(failures[0], /escapes project root/);
  assert.match(failures[1], /Undefined link reference/);
});

test("compares parsed Rust JSON and fails on a profile contract change", (t) => {
  const reference = { profile_version: 2, unchecked_server: { checked_at: null } };
  const root = fixture(t, {
    "README.md": "# Home\n",
    "desktop/src/testing/core-contract.json": JSON.stringify(reference),
    "current.json": JSON.stringify({ unchecked_server: { checked_at: null }, profile_version: 2 }, null, 2),
    "drift.json": JSON.stringify({ ...reference, profile_version: 3 }),
  });
  assert.deepEqual(checkContent(root, { referencePath: path.join(root, "current.json") }).failures, []);
  const report = checkContent(root, { referencePath: path.join(root, "drift.json") });
  assert.equal(report.rustReferenceChecked, true);
  assert.match(report.failures.join("\n"), /Rust story contract differs/);
});

test("CLI returns failure and saves a separate machine-readable report", (t) => {
  const root = fixture(t, { "README.md": "[broken](missing.md)\n" });
  const reportPath = path.join(root, "reports/content.json");
  const result = spawnSync(process.execPath, [path.join(import.meta.dirname, "content-check.mjs"), "--root", root, "--report", reportPath], { encoding: "utf8" });
  assert.equal(result.status, 1, result.stderr);
  const report = JSON.parse(result.stdout);
  assert.equal(report.failures.length, 1);
  assert.equal(report.markdownFiles, 1);
  assert.deepEqual(JSON.parse(readFileSync(reportPath, "utf8")), report);
});
