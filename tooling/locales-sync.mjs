#!/usr/bin/env node
// Turns the catalogs of the interface (`desktop/src/locales/<language>.yml`) into what
// the parts that read no YAML take:
//   - `core/src/locales.json`: the groups `language`, `core` and `shell` of every language, built
//     into the program (`text.rs`);
//   - `desktop/src-tauri/windows/lang/<NSIS language>.nsh`: the `installer` group as the
//     `LangString`s of the installer (`bundle.windows.nsis.customLanguageFiles`);
// and checks that `tauri.conf.json` lists the same installer languages as the files.
// The generated files are committed, so that the product builds from the project root alone.
//
//   node tooling/locales-sync.mjs write   writes them
//   node tooling/locales-sync.mjs check   fails when a file is not what the catalogs make
//   (default: check)
//
// Every language must have the keys and the message-level `{name}` sets of the default one.
import {
  readdirSync,
  readFileSync,
  mkdirSync,
  rmSync,
  writeFileSync,
  existsSync,
} from "node:fs";
import path from "node:path";
import { flattenCatalog, NOT_THE_PAGES } from "../desktop/yamlCatalog.ts";

const root = path.resolve(import.meta.dirname, "..");
export const LOCALES = path.join(root, "desktop/src/locales");
export const CORE_JSON = path.join(root, "core/src/locales.json");
export const NSIS_DIR = path.join(root, "desktop/src-tauri/windows/lang");
export const TAURI_CONF = path.join(root, "desktop/src-tauri/tauri.conf.json");
export const DEFAULT_LANGUAGE = "ru";
/** The groups the core and the shell read. */
export const RUST_GROUPS = ["language", "core", "shell"];

/** An NSIS language is a file name of NSIS (`Russian`, `SimpChinese`): letters and digits, no path. The
 * generator names it `${LANG_RUSSIAN}`, so two names that differ in case are one language. */
const NSIS_NAME = /^[A-Za-z][A-Za-z0-9]*$/;
/** The form Tauri's own language files use for the product name; the catalog writes the NSIS one. */
const TAURI_FORM = /\{\{\s*\w+\s*\}\}/;

const group = (key) => key.split(".")[0];
const placeholders = (entry) => [
  ...new Set(
    (typeof entry === "string" ? [entry] : Object.values(entry)).flatMap(
      (text) => [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]),
    ),
  ),
];

/** @returns {Record<string, Record<string, string | Record<string, string>>>} by language code */
export function readCatalogs(directory = LOCALES) {
  const catalogs = {};
  for (const file of readdirSync(directory)
    .filter((f) => f.endsWith(".yml"))
    .sort()) {
    catalogs[file.slice(0, -4)] = flattenCatalog(
      readFileSync(path.join(directory, file), "utf8"),
      file,
    );
  }
  return catalogs;
}

/** What is wrong with the set of catalogs, as a list of lines. */
export function problems(catalogs) {
  const found = [];
  const reference = catalogs[DEFAULT_LANGUAGE];
  if (!reference)
    return [`no catalog of the default language «${DEFAULT_LANGUAGE}»`];
  const nsisOwner = new Map();
  for (const [code, catalog] of Object.entries(catalogs)) {
    for (const key of [
      "language.name",
      "language.flag",
      "language.tag",
      "installer.nsis_language",
    ])
      if (typeof catalog[key] !== "string" || catalog[key].trim() === "")
        found.push(`${code}: «${key}» is missing`);
    const nsis = catalog["installer.nsis_language"];
    if (typeof nsis === "string" && nsis.trim() !== "") {
      if (!NSIS_NAME.test(nsis))
        found.push(
          `${code}: «installer.nsis_language» is «${nsis}», not the name of an NSIS language`,
        );
      else if (nsisOwner.has(nsis.toUpperCase()))
        found.push(
          `${code}: «installer.nsis_language» is «${nsis}», as in ${nsisOwner.get(nsis.toUpperCase())}`,
        );
      else nsisOwner.set(nsis.toUpperCase(), code);
    }
    for (const [key, entry] of Object.entries(catalog)) {
      if (group(key) !== "installer") continue;
      // NSIS has no plural forms: a `LangString` is one string.
      if (typeof entry !== "string")
        found.push(
          `${code}: «${key}» has plural forms, an installer text is one string`,
        );
      else if (TAURI_FORM.test(entry))
        found.push(
          `${code}: «${key}» has {{…}}, write the NSIS form (\${PRODUCTNAME}) instead`,
        );
    }
    for (const key of Object.keys(reference))
      if (!(key in catalog)) found.push(`${code}: «${key}» is missing`);
    for (const [key, entry] of Object.entries(catalog)) {
      if (!(key in reference)) {
        found.push(`${code}: «${key}» is not in ${DEFAULT_LANGUAGE}`);
        continue;
      }
      const wanted = placeholders(reference[key]);
      const actual = placeholders(entry);
      for (const name of wanted)
        if (!actual.includes(name))
          found.push(`${code}: «${key}» is missing {${name}}`);
      for (const name of actual)
        if (!wanted.includes(name))
          found.push(
            `${code}: «${key}» has {${name}} that ${DEFAULT_LANGUAGE} has not`,
          );
    }
  }
  return found;
}

/** The part of the catalog that Rust reads, in the order of the file. */
export function forRust(catalog) {
  return Object.fromEntries(
    Object.entries(catalog).filter(([key]) => RUST_GROUPS.includes(group(key))),
  );
}

const camel = (name) => name.replace(/_(\w)/g, (_, c) => c.toUpperCase());

/** The text of an NSIS value: `"` and a real newline are written the NSIS way, `$…` is NSIS and stays. */
const nsisValue = (text) =>
  text.replaceAll('"', '$\\"').replaceAll("\n", "$\\n");

/** `LangString name ${LANG_X} "text"` for the group `installer`. */
export function nsisFile(catalog) {
  const language = `\${LANG_${catalog["installer.nsis_language"].toUpperCase()}}`;
  const lines = Object.entries(catalog)
    .filter(
      ([key]) =>
        group(key) === "installer" && key !== "installer.nsis_language",
    )
    .map(
      ([key, text]) =>
        `LangString ${camel(key.slice("installer.".length))} ${language} "${nsisValue(text)}"`,
    );
  return (
    "; Generated by tooling/locales-sync.mjs from the group `installer` of the catalog.\n" +
    "; Do not edit: change desktop/src/locales/*.yml and run `node tooling/locales-sync.mjs write`.\n" +
    lines.join("\n") +
    "\n"
  );
}

/** The files the catalogs make: path → content. */
export function outputs(catalogs) {
  const files = new Map();
  files.set(
    CORE_JSON,
    `${JSON.stringify(
      Object.fromEntries(
        Object.entries(catalogs).map(([code, c]) => [code, forRust(c)]),
      ),
      null,
      2,
    )}\n`,
  );
  for (const catalog of Object.values(catalogs))
    files.set(
      path.join(NSIS_DIR, `${catalog["installer.nsis_language"]}.nsh`),
      nsisFile(catalog),
    );
  return files;
}

/** The installer languages and files `tauri.conf.json` must have for the catalogs. */
export function installerConfig(catalogs) {
  const names = Object.values(catalogs).map(
    (c) => c["installer.nsis_language"],
  );
  return {
    languages: names,
    customLanguageFiles: Object.fromEntries(
      names.map((n) => [n, `windows/lang/${n}.nsh`]),
    ),
    displayLanguageSelector: names.length > 1,
  };
}

/** JSON with the keys of every object in order, so that the order they were written in does not matter. */
const stable = (value) =>
  JSON.stringify(value, (_, v) =>
    v && typeof v === "object" && !Array.isArray(v)
      ? Object.fromEntries(
          Object.entries(v).sort(([a], [b]) => (a < b ? -1 : 1)),
        )
      : v,
  );

/** What is wrong in `tauri.conf.json` against the catalogs. */
export function configProblems(catalogs, config) {
  const nsis = config?.bundle?.windows?.nsis ?? {};
  const wanted = installerConfig(catalogs);
  const found = [];
  for (const [name, value] of Object.entries(wanted))
    if (stable(nsis[name]) !== stable(value))
      found.push(
        `tauri.conf.json bundle.windows.nsis.${name} is ${JSON.stringify(nsis[name])}, the catalogs want ${JSON.stringify(value)}`,
      );
  return found;
}

/** The names of the `LangString`s the installer template and its hooks ask for. */
export function usedByInstaller(
  directory = path.join(root, "desktop/src-tauri/windows"),
) {
  const used = new Set();
  for (const file of ["installer.nsi", "installer-hooks.nsh"])
    for (const m of readFileSync(path.join(directory, file), "utf8").matchAll(
      /\$\(([A-Za-z][A-Za-z0-9]*)\)/g,
    ))
      used.add(m[1]);
  return used;
}

/** What is wrong with the installer's texts against what the template asks for. */
export function installerProblems(catalogs, used = usedByInstaller()) {
  const found = [];
  for (const [code, catalog] of Object.entries(catalogs)) {
    const defined = new Set(
      Object.keys(catalog)
        .filter(
          (k) => group(k) === "installer" && k !== "installer.nsis_language",
        )
        .map((k) => camel(k.slice("installer.".length))),
    );
    for (const name of used)
      if (!defined.has(name))
        found.push(
          `${code}: the installer asks for $(${name}), «installer.${name}» is missing`,
        );
  }
  return found;
}

export function main(argv, directory = LOCALES) {
  const mode = argv[0] ?? "check";
  if (!["write", "check"].includes(mode)) {
    console.error("usage: locales-sync.mjs [write|check]");
    return 2;
  }
  const catalogs = readCatalogs(directory);
  // Only the problems of the catalogs stop a write; the config is edited by hand. Nothing is built
  // from a catalog that has them: a missing `installer.nsis_language` would be a stack trace.
  const blocking = [...problems(catalogs), ...installerProblems(catalogs)];
  const failures = [
    ...blocking,
    ...configProblems(catalogs, JSON.parse(readFileSync(TAURI_CONF, "utf8"))),
  ];
  if (blocking.length) {
    for (const f of mode === "write" ? blocking : failures) console.error(f);
    return 1;
  }
  const files = outputs(catalogs);
  if (mode === "write") {
    mkdirSync(NSIS_DIR, { recursive: true });
    for (const stale of readdirSync(NSIS_DIR))
      if (!files.has(path.join(NSIS_DIR, stale)))
        rmSync(path.join(NSIS_DIR, stale));
    for (const [file, content] of files) writeFileSync(file, content);
    for (const f of configProblems(
      catalogs,
      JSON.parse(readFileSync(TAURI_CONF, "utf8")),
    ))
      console.error(f);
    console.log(
      `Wrote ${files.size} files for ${Object.keys(catalogs).length} language(s).`,
    );
    return 0;
  }
  for (const [file, content] of files) {
    const actual = existsSync(file) ? readFileSync(file, "utf8") : null;
    if (actual !== content)
      failures.push(
        `${path.relative(root, file)} is not what the catalogs make: run \`node tooling/locales-sync.mjs write\``,
      );
  }
  if (existsSync(NSIS_DIR))
    for (const stale of readdirSync(NSIS_DIR))
      if (!files.has(path.join(NSIS_DIR, stale)))
        failures.push(`windows/lang/${stale} belongs to no catalog`);
  for (const f of failures) console.error(f);
  if (failures.length) return 1;
  console.log(
    `The generated files match ${Object.keys(catalogs).length} catalog(s).`,
  );
  return 0;
}

if (process.argv[1] === import.meta.filename)
  process.exit(main(process.argv.slice(2)));
