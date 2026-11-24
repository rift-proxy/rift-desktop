/// <reference types="node" />
import { parse } from "yaml";
import type { Plugin } from "vite";

/** A message: one string, or the forms of a plural keyed by the CLDR category of the count. */
export type Entry = string | Record<string, string>;
export type Catalog = Record<string, Entry>;

const FORMS = ["zero", "one", "two", "few", "many", "other"];

/**
 * The groups of a catalog that are not the page's: the texts of the core (`core`), of the Tauri
 * shell (`shell`) and of the installer (`installer`). They live in the same file, so that one
 * file is one language, and `tooling/locales-sync.mjs` turns them into what Rust and NSIS read.
 * The page gets none of them.
 */
export const NOT_THE_PAGES = ["core", "shell", "installer"];

const isMap = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/**
 * Turns the text of a catalog file into one flat object with dotted keys. The file is grouped for
 * the translator (`settings: { autostart: { label: … } }`); the interface asks for
 * `settings.autostart.label`. A map whose keys are all plural categories and has `other` is one
 * message with several forms. Anything else than a string (a number, a list, an empty value) is
 * refused with the key it sits under: such a value would show as `undefined` on a screen.
 */
export function flattenCatalog(source: string, file = "catalog"): Catalog {
  const out: Catalog = {};
  const walk = (node: unknown, path: string[]) => {
    const key = path.join(".");
    if (typeof node === "string") {
      out[key] = node;
    } else if (
      isMap(node) &&
      Object.keys(node).length > 0 &&
      Object.keys(node).every((k) => FORMS.includes(k))
    ) {
      if (!("other" in node)) throw Error(`${file}: «${key}» has no «other»`);
      for (const [form, text] of Object.entries(node)) {
        if (typeof text !== "string")
          throw Error(`${file}: «${key}.${form}» is not a string`);
      }
      out[key] = node as Record<string, string>;
    } else if (isMap(node)) {
      if (Object.keys(node).length === 0)
        throw Error(`${file}: «${key || "(root)"}» is an empty map`);
      for (const [name, child] of Object.entries(node)) {
        if (name === "" || name.includes("."))
          throw Error(`${file}: «${key}» has the key «${name}»`);
        walk(child, [...path, name]);
      }
    } else {
      throw Error(`${file}: «${key || "(root)"}» is not text`);
    }
  };
  walk(parse(source), []);
  return out;
}

/** The catalog without the groups of `NOT_THE_PAGES`. */
export function forThePage(catalog: Catalog): Catalog {
  return Object.fromEntries(
    Object.entries(catalog).filter(
      ([key]) => !NOT_THE_PAGES.includes(key.split(".")[0]),
    ),
  );
}

/**
 * `import ru from "./locales/ru.yml"`: the catalog is read when the page is built, so the
 * YAML parser is not part of the program and the texts need no request.
 */
export function yamlCatalog(): Plugin {
  return {
    name: "rift-yaml-catalog",
    enforce: "pre",
    transform(code, id) {
      const file = id.split("?")[0];
      if (!file.endsWith(".yml")) return null;
      return {
        code: `export default ${JSON.stringify(forThePage(flattenCatalog(code, file)))};`,
        map: null,
      };
    },
  };
}
