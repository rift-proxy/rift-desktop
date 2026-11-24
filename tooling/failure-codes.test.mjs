import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { flattenCatalog } from "../desktop/yamlCatalog.ts";

// A failure code that Rust can send but the frontend has no text for would reach the user as
// the generic fallback. The wire names come from `FailureCode::as_str` and the labels from the
// group `failure` of the default language catalog (`failureText` asks for `failure.<code>`), both
// read as source so a new variant cannot be added on one side only.
const root = join(import.meta.dirname, "..");
const rust = readFileSync(join(root, "core/src/failure.rs"), "utf8");
const catalog = flattenCatalog(
  readFileSync(join(root, "desktop/src/locales/ru.yml"), "utf8"),
  "ru.yml",
);

const wireNames = [...rust.matchAll(/Self::\w+ => "([a-z_]+)"/g)]
  .map((match) => match[1])
  .filter((name) => name !== "unknown")
  .sort();

const labelled = Object.keys(catalog)
  .map((key) => /^failure\.([a-z][a-z_]*)$/.exec(key)?.[1])
  .filter((name) => name !== undefined && name !== "unknown")
  .sort();

test("the parsed sources are not empty", () => {
  assert.ok(wireNames.length >= 20, `only ${wireNames.length} wire names parsed`);
  assert.ok(labelled.length >= 20, `only ${labelled.length} labels parsed`);
});

test("every failure code of the core has a frontend text, and no text is orphaned", () => {
  assert.deepEqual(labelled, wireNames);
});

// Rust leaves `Entry.failure_code` out of the JSON of an ordinary journal entry
// (`skip_serializing_if`), while `ServerView` and `SubscriptionView` always send the key
// (`null` without a failure). The TypeScript mirror must say the same, or fixtures and
// tests describe a payload the app never receives.
const application = readFileSync(join(root, "core/src/application.rs"), "utf8");
const model = readFileSync(join(root, "core/src/model.rs"), "utf8");
const types = readFileSync(join(root, "desktop/src/backend/types.ts"), "utf8");

function rustOmitsKey(source, structName) {
  const body = [...source.matchAll(/pub struct (\w+) \{([\s\S]*?)\n\}/g)].find(
    (match) => match[1] === structName,
  )?.[2];
  assert.ok(body, `${structName} not found`);
  const field = body.match(
    /(?<attributes>(?:\s*#\[[^\n]*\])*)\s*pub failure_code: Option<FailureCode>/,
  );
  assert.ok(field, `${structName}.failure_code not found`);
  return field.groups.attributes.includes("skip_serializing_if");
}

function tsKeyIsOptional(interfaceName) {
  const body = [
    ...types.matchAll(/export interface (\w+) \{([\s\S]*?)\n\}/g),
  ].find((match) => match[1] === interfaceName)?.[2];
  assert.ok(body, `${interfaceName} not found`);
  const field = body.match(/^\s*failure_code(\?)?:\s*string \| null;/m);
  assert.ok(field, `${interfaceName}.failure_code not found`);
  return field[1] === "?";
}

test("the TypeScript failure_code of each view is optional exactly when Rust omits the key", () => {
  assert.equal(rustOmitsKey(application, "Entry"), true);
  assert.equal(rustOmitsKey(application, "SubscriptionView"), false);
  assert.equal(rustOmitsKey(model, "ServerView"), false);
  assert.equal(tsKeyIsOptional("Entry"), true);
  assert.equal(tsKeyIsOptional("Subscription"), false);
  assert.equal(tsKeyIsOptional("Server"), false);
});
