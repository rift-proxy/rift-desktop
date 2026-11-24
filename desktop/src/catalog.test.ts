import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { describe, it, expect } from "vitest";
import ru from "./locales/ru.yml";
import { CATALOGS, DEFAULT_LOCALE } from "./i18n";

// The texts of the interface live in `locales/ru.yml`. These checks keep the code and the file
// from drifting apart: a key the code asks for must exist, and a text nobody asks for must go.
const src = import.meta.dirname;
const files = (readdirSync(src, { recursive: true }) as string[])
  .filter((f) => /\.(ts|tsx)$/.test(f))
  .filter((f) => !/\.(test|stories)\.|(^|[\\/])(testing|locales)[\\/]/.test(f))
  .map((f) => ({ file: f, code: readFileSync(path.join(src, f), "utf8") }));
const code = files.map((f) => f.code).join("\n");
const keys = Object.keys(ru);

/** `t("a.b")` and friends: the key written out in the call. */
const literalKeys = [
  ...code.matchAll(/\bt\(\s*"([a-z0-9_.]+)"/g),
  ...code.matchAll(/\bt\(\s*\n?\s*(?:[^"`()]*\?\s*)?"([a-z0-9_.]+)"/g),
].map((m) => m[1]);
/** `tFor("failure", code)` and `` t(`nav.${id}`) ``: a key made of a prefix and a value. */
const prefixes = [
  ...code.matchAll(/\btFor\(\s*"([a-z0-9_.]+)"/g),
  ...code.matchAll(/\bt\(\s*`([a-z0-9_.]+\.)\$\{/g),
].map((m) => (m[1].endsWith(".") ? m[1] : `${m[1]}.`));
/** Any quoted string that is a full key: a key kept in a table and given to `t` later. */
const quoted = new Set([...code.matchAll(/"([a-z0-9_.]+)"/g)].map((m) => m[1]));

describe("the text catalog", () => {
  it("has every key the code asks for", () => {
    const missing = literalKeys.filter((k) => !keys.includes(k));
    expect(missing).toEqual([]);
  });
  it("has keys under every prefix the code builds keys from", () => {
    const empty = prefixes.filter((p) => !keys.some((k) => k.startsWith(p)));
    expect(empty).toEqual([]);
  });
  it("has no text that nothing asks for", () => {
    const unused = keys.filter(
      (k) =>
        !literalKeys.includes(k) &&
        !quoted.has(k) &&
        !prefixes.some((p) => k.startsWith(p)),
    );
    expect(unused).toEqual([]);
  });
  it("keeps every placeholder a plain word and every plural complete", () => {
    for (const [key, entry] of Object.entries(ru)) {
      const forms = typeof entry === "string" ? [entry] : Object.values(entry);
      for (const text of forms) {
        expect(text, key).not.toMatch(/\{[^\w}]|\{\}|\{\w*$/);
        expect(text.trim(), key).not.toBe("");
      }
      if (typeof entry !== "string") expect(entry.other, key).toBeTruthy();
    }
  });
  it("is read by the screens: the first start shows no key", () => {
    // A key shown instead of a text is the one way a missing entry reaches a person.
    for (const text of Object.values(ru).flatMap((e) =>
      typeof e === "string" ? [e] : Object.values(e),
    ))
      expect(text).not.toMatch(/^[a-z_]+(\.[a-z0-9_]+)+$/);
  });
  it("has the same keys and placeholders in every language as in the default one", () => {
    const placeholders = (entry: string | Record<string, string>) => [
      ...new Set(
        (typeof entry === "string" ? [entry] : Object.values(entry))
          .flatMap((text) => [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]))
          .sort(),
      ),
    ];
    const reference = CATALOGS[DEFAULT_LOCALE];
    for (const [code, catalog] of Object.entries(CATALOGS)) {
      expect(Object.keys(catalog).sort(), code).toEqual(
        Object.keys(reference).sort(),
      );
      for (const [key, entry] of Object.entries(catalog))
        // A plural form may drop a placeholder in a language whose form has no use of it, but not invent one.
        expect(
          placeholders(entry).filter(
            (p) => !placeholders(reference[key]).includes(p),
          ),
          `${code}: ${key}`,
        ).toEqual([]);
    }
  });
});
