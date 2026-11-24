#!/usr/bin/env node
// Release check: no text of the interface is written in the code.
//
// Every text a person reads lives in `desktop/src/locales/*.yml` and is asked for with
// `t("key")`. This reads the production sources and refuses what puts a text back.
//
// TypeScript (`desktop/src`, the syntax tree):
//   - letters in the text of a JSX element (`<p>Save</p>`);
//   - a string given to a prop that shows text (`label="Save"`, `title`, `placeholder`, ...);
//   - a string an expression puts into the page (`{ok ? "Saved" : "Failed"}`, `{`Rift ${v}`}`);
//   - Cyrillic or CJK letters in any string or template (`"Не удалось"`), wherever it is;
//   - a Latin sentence in any string (`return "Loading failed."`), see `looksLikeSentence`.
// Rust (`core/src`, `desktop/src-tauri/src`, the string literals outside tests):
//   - Cyrillic or CJK letters in any literal, and a Latin sentence;
//   - a key `t("core.x")` / `tr!("shell.x")` that the catalog has not, and a `core`/`shell` key
//     that no code asks for.
// Installer (`windows/installer.nsi`, `windows/installer-hooks.nsh`):
//   - Cyrillic or CJK letters anywhere outside a comment, and a Latin sentence in the literal of
//     `MessageBox`, `DetailPrint` or `Abort`: the installer shows `$(name)`, a `LangString`.
// The `LangString`s themselves, the generated files and `tauri.conf.json` are held by
// `tooling/locales-sync.mjs check`, which the release step runs next to this.
// Comments are not code and are not read. A text that must stay in the code (the name of a
// language in that language, the text a child program prints and we compare with) carries
// `i18n-ignore: <why>` on its line or the line above. Tests keep their literals and are not read.
//
// Not found: a one-word Latin text, and a sentence built from pieces (`"Failed" + ": " + why`).
// Those cannot be told from a key or a code value by syntax; the catalog test
// (`src/catalog.test.ts`) and review hold that line.
//
// Usage: node tooling/hardcoded-text-check.mjs [frontend source directory]
//        (with a directory, only the frontend is read; without, the frontend, Rust and the keys)
import { readdirSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { flattenCatalog } from "../desktop/yamlCatalog.ts";
import { rustStrings } from "./rust-strings.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const defaultSource = path.join(root, "desktop/src");
/** The Rust sources a person can read a text from: the core and the Tauri shell. */
export const RUST_SOURCES = [
  path.join(root, "core/src"),
  path.join(root, "desktop/src-tauri/src"),
];
/** The installer template and its hooks: what the installer shows is `$(name)` of the catalog. */
export const INSTALLER_SOURCES = [
  path.join(root, "desktop/src-tauri/windows/installer.nsi"),
  path.join(root, "desktop/src-tauri/windows/installer-hooks.nsh"),
];
/** The catalog whose keys the Rust code asks for. */
export const DEFAULT_CATALOG = path.join(
  root,
  "desktop/src/locales/ru.yml",
);

/** Props whose string value a person reads. */
export const TEXT_PROPS = new Set([
  "label",
  "title",
  "description",
  "message",
  "confirmLabel",
  "text",
  "error",
  "submitError",
  "hint",
  "heading",
  "caption",
  "tooltip",
  "subtitle",
  "helperText",
  "emptyText",
  "placeholder",
  "alt",
  "aria-label",
  "aria-description",
  "aria-placeholder",
  "aria-roledescription",
]);
const LETTER = /\p{L}/u;
// Cyrillic, Hiragana, Katakana, CJK ideographs and Hangul: the scripts of the interface languages
// that are not Latin.
const NON_LATIN = /[Ѐ-ӿ぀-ヿ㐀-䶿一-鿿가-힯]/u;
const IGNORE = /i18n-ignore:\s*\S/;

/**
 * Whether a string reads as a sentence a person is shown: two words or more, not a code, a path or
 * a pattern, that starts with a capital letter or ends with `.`, `!`, `?`, `…` or `:`. The
 * `{name}` and `${expression}` places are not words. `"muted small"`, `"react-dom/client"` and
 * `"application/json; charset=utf-8"` are not sentences; `"Loading failed"` and `"Try again."` are.
 */
export function looksLikeSentence(text) {
  const bare = text
    .replace(/\$\{[^}]*\}/g, "")
    .replace(/\{[^{}]*\}/g, "")
    .trim();
  if (!/\s/.test(bare)) return false;
  if ((bare.match(/\p{L}+/gu) ?? []).length < 2) return false;
  if (/[=<>\\|@#$^~`]|:\/\/|=>|\.\w{2,4}$/.test(bare)) return false;
  return /^\p{Lu}/u.test(bare) || /[.!?…:]$/.test(bare);
}

function loadTypescript() {
  const from = path.join(root, "desktop/package.json");
  try {
    return createRequire(from)("typescript");
  } catch {
    throw Error(
      "typescript is not installed for desktop: run `pnpm install` first",
    );
  }
}

/** Production sources: no tests, stories, fixtures, declarations or the catalogs themselves. */
export function sourceFiles(directory) {
  return readdirSync(directory, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => path.join(entry.parentPath, entry.name))
    .filter((file) => /\.(ts|tsx)$/.test(file))
    .filter((file) => !/\.(test|stories)\.|\.d\.ts$/.test(file))
    .filter((file) => {
      const relative = path.relative(directory, file).split(path.sep);
      return !relative.some((part) => part === "testing" || part === "locales");
    })
    .sort();
}

/**
 * The strings an expression can produce by itself: a literal, a template, the branches of `?:`,
 * `??`, `||`, `&&`. A call (`t("key")`), a name or a member is not one: what it gives is not
 * written here.
 */
function stringLeaves(ts, node) {
  if (ts.isParenthesizedExpression(node))
    return stringLeaves(ts, node.expression);
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node))
    return [{ node, text: node.text }];
  if (ts.isTemplateExpression(node))
    return [
      {
        node,
        text: [
          node.head.text,
          ...node.templateSpans.map((s) => s.literal.text),
        ].join(" "),
      },
    ];
  if (ts.isConditionalExpression(node))
    return [
      ...stringLeaves(ts, node.whenTrue),
      ...stringLeaves(ts, node.whenFalse),
    ];
  if (
    ts.isBinaryExpression(node) &&
    [
      ts.SyntaxKind.QuestionQuestionToken,
      ts.SyntaxKind.BarBarToken,
      ts.SyntaxKind.AmpersandAmpersandToken,
    ].includes(node.operatorToken.kind)
  )
    return [...stringLeaves(ts, node.left), ...stringLeaves(ts, node.right)];
  return [];
}

/** @returns {{file: string, line: number, column: number, kind: string, text: string}[]} */
export function findHardcodedText(files, ts = loadTypescript()) {
  const found = [];
  for (const file of files) {
    const code = readFileSync(file, "utf8");
    const lines = code.split("\n");
    const source = ts.createSourceFile(
      file,
      code,
      ts.ScriptTarget.ES2022,
      true,
      file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
    );
    const seen = new Set();
    const report = (position, kind, text) => {
      const { line, character } =
        source.getLineAndCharacterOfPosition(position);
      const key = `${line}:${character}`;
      if (seen.has(key)) return;
      seen.add(key);
      // The line itself or the one above it.
      if (IGNORE.test(lines[line] ?? "") || IGNORE.test(lines[line - 1] ?? ""))
        return;
      found.push({
        file,
        line: line + 1,
        column: character + 1,
        kind,
        text: text.trim().replace(/\s+/g, " ").slice(0, 60),
      });
    };
    const visit = (node) => {
      if (node.kind === ts.SyntaxKind.JsxText) {
        if (LETTER.test(node.text))
          report(node.pos + node.text.search(/\S/), "text in JSX", node.text);
      } else if (ts.isJsxAttribute(node)) {
        const name = node.name.getText(source);
        const value = node.initializer;
        const leaves =
          !TEXT_PROPS.has(name) || !value
            ? []
            : ts.isStringLiteral(value)
              ? [{ node: value, text: value.text }]
              : ts.isJsxExpression(value) && value.expression
                ? stringLeaves(ts, value.expression)
                : [];
        for (const leaf of leaves)
          if (LETTER.test(leaf.text))
            report(leaf.node.getStart(source), `text in «${name}»`, leaf.text);
      } else if (
        ts.isJsxExpression(node) &&
        node.expression &&
        (ts.isJsxElement(node.parent) || ts.isJsxFragment(node.parent))
      ) {
        for (const leaf of stringLeaves(ts, node.expression))
          if (LETTER.test(leaf.text))
            report(leaf.node.getStart(source), "text in JSX", leaf.text);
      }
      if (
        (ts.isStringLiteral(node) ||
          ts.isNoSubstitutionTemplateLiteral(node) ||
          ts.isTemplateHead(node) ||
          ts.isTemplateMiddle(node) ||
          ts.isTemplateTail(node)) &&
        NON_LATIN.test(node.text)
      )
        report(node.getStart(source), "non-Latin text", node.text);
      // A Latin sentence in a string nothing above caught; a template is read as a whole.
      const whole = ts.isTemplateExpression(node)
        ? [node.head.text, ...node.templateSpans.map((s) => s.literal.text)].join(
            "{}",
          )
        : ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)
          ? node.text
          : null;
      if (whole !== null && looksLikeSentence(whole))
        report(node.getStart(source), "sentence", whole);
      ts.forEachChild(node, visit);
    };
    visit(source);
  }
  return found;
}

/** The line without its NSIS comment (`;` or `#` outside quotes, at the start or after a space). */
function withoutNsisComment(line) {
  let quoted = false;
  for (let k = 0; k < line.length; k++) {
    if (line.startsWith('$\\"', k)) k += 2;
    else if (line[k] === '"') quoted = !quoted;
    else if (
      !quoted &&
      (line[k] === ";" || line[k] === "#") &&
      (k === 0 || /\s/.test(line[k - 1]))
    )
      return line.slice(0, k);
  }
  return line;
}

/** What NSIS puts in a text from outside: `$(name)`, `${DEFINE}`, `$VARIABLE`, `$\n`. */
const NSIS_PLACES = /\$\([^)]*\)|\$\{[^}]*\}|\$\\.|\$\w+/g;

/** @returns {{file: string, line: number, column: number, kind: string, text: string}[]} */
export function findHardcodedInstaller(files) {
  const found = [];
  for (const file of files) {
    const lines = readFileSync(file, "utf8").split("\n");
    lines.forEach((raw, index) => {
      if (IGNORE.test(raw) || IGNORE.test(lines[index - 1] ?? "")) return;
      const code = withoutNsisComment(raw);
      let kind = NON_LATIN.test(code) ? "non-Latin text" : null;
      if (!kind && /^\s*(?:MessageBox|DetailPrint|Abort)\b/i.test(code))
        for (const literal of code.matchAll(/"((?:\$\\"|[^"])*)"/g))
          if (looksLikeSentence(literal[1].replace(NSIS_PLACES, " ")))
            kind = "sentence";
      if (kind)
        found.push({
          file,
          line: index + 1,
          column: code.search(/\S/) + 1,
          kind,
          text: code.trim().replace(/\s+/g, " ").slice(0, 60),
        });
    });
  }
  return found;
}

/** The `.rs` files under a directory, sorted. Tests of the crates live outside `src` and are not here. */
export function rustSourceFiles(directory) {
  return readdirSync(directory, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".rs"))
    .map((entry) => path.join(entry.parentPath, entry.name))
    .sort();
}

/** A catalog key the code asks for: `core.error.invalid`, `shell.tray.open`. */
const CATALOG_KEY = /^(?:core|shell)(?:\.[a-z0-9_]+)+$/;

/** @returns {{file: string, line: number, column: number, kind: string, text: string}[]} */
export function findHardcodedRust(files) {
  const found = [];
  for (const file of files) {
    const code = readFileSync(file, "utf8");
    const lines = code.split("\n");
    for (const literal of rustStrings(code)) {
      // A byte string is data, not a text.
      if (literal.prefix.startsWith("b")) continue;
      const kind = NON_LATIN.test(literal.text)
        ? "non-Latin text"
        : looksLikeSentence(literal.text)
          ? "sentence"
          : null;
      if (!kind) continue;
      if (
        IGNORE.test(lines[literal.line - 1] ?? "") ||
        IGNORE.test(lines[literal.line - 2] ?? "")
      )
        continue;
      found.push({
        file,
        line: literal.line,
        column: literal.column,
        kind,
        text: literal.text.trim().replace(/\s+/g, " ").slice(0, 60),
      });
    }
  }
  return found;
}

/** The catalog keys of the groups Rust reads (`core`, `shell`), from the file of the default language. */
export function rustCatalogKeys(catalogFile = DEFAULT_CATALOG) {
  return Object.keys(
    flattenCatalog(readFileSync(catalogFile, "utf8"), catalogFile),
  ).filter((key) => CATALOG_KEY.test(key));
}

/**
 * What is wrong between the keys the Rust code asks for and the catalog: a key the catalog has
 * not (the person would see the key), and a `core`/`shell` key no code asks for.
 */
export function rustKeyProblems(files, catalogKeys = rustCatalogKeys()) {
  const known = new Set(catalogKeys);
  const used = new Map();
  for (const file of files) {
    for (const literal of rustStrings(readFileSync(file, "utf8"))) {
      if (literal.prefix !== '"' || !CATALOG_KEY.test(literal.text)) continue;
      if (!used.has(literal.text)) used.set(literal.text, []);
      used.get(literal.text).push(`${path.relative(root, file)}:${literal.line}`);
    }
  }
  const found = [];
  for (const [key, places] of used)
    if (!known.has(key))
      found.push(`${places[0]}  the catalog has no key «${key}»`);
  for (const key of known)
    if (!used.has(key))
      found.push(`ru.yml  «${key}» is asked for by no code: remove it`);
  return found;
}

function report(lines, count) {
  for (const line of lines) console.error(line);
  return count;
}

function main(argv) {
  const frontend = argv.length > 0;
  const directory = path.resolve(argv[0] ?? defaultSource);
  const files = sourceFiles(directory);
  const found = findHardcodedText(files);
  const rustFiles = frontend ? [] : RUST_SOURCES.flatMap(rustSourceFiles);
  const rust = frontend ? [] : findHardcodedRust(rustFiles);
  const installer = frontend ? [] : findHardcodedInstaller(INSTALLER_SOURCES);
  const keys = frontend ? [] : rustKeyProblems(rustFiles);
  const all = [...found, ...rust, ...installer];
  const failures = report(
    all.map(
      (f) =>
        `${path.relative(root, f.file)}:${f.line}:${f.column}  ${f.kind}: «${f.text}»`,
    ),
    all.length,
  );
  const wrongKeys = report(keys, keys.length);
  if (failures + wrongKeys > 0) {
    if (failures > 0)
      console.error(
        `\n${failures} hardcoded text(s). Put each in desktop/src/locales/ru.yml and ask for it with t("key") (Rust: t("core.…") / tr!("shell.…"); installer: $(name) of the group installer); a text that must stay carries "i18n-ignore: <why>".`,
      );
    if (wrongKeys > 0)
      console.error(`\n${wrongKeys} key(s) of the catalog and the Rust code differ.`);
    return 1;
  }
  console.log(
    frontend
      ? `No hardcoded text in ${files.length} source files.`
      : `No hardcoded text in ${files.length} frontend, ${rustFiles.length} Rust and ${INSTALLER_SOURCES.length} installer source files; the keys of the catalog and the Rust code agree.`,
  );
  return 0;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    process.exit(main(process.argv.slice(2)));
  } catch (error) {
    console.error(error.message);
    process.exit(2);
  }
}
