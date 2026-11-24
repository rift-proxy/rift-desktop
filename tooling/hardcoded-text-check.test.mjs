import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import {
  findHardcodedInstaller,
  findHardcodedRust,
  findHardcodedText,
  looksLikeSentence,
  rustKeyProblems,
  rustSourceFiles,
  sourceFiles,
} from "./hardcoded-text-check.mjs";

function check(name, code) {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-hardcoded-"));
  try {
    const file = path.join(directory, name);
    writeFileSync(file, code);
    return findHardcodedText([file]).map((f) => `${f.line}:${f.kind}`);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("letters in the text of a JSX element are found, in any language", () => {
  assert.deepEqual(check("a.tsx", "const a = <p>Save</p>;\n"), [
    "1:text in JSX",
  ]);
  assert.deepEqual(check("a.tsx", "const a = <p>\n  Сохранить\n</p>;\n"), [
    "2:text in JSX",
  ]);
  // Not text: spacing, numbers, symbols, a call, a name.
  assert.deepEqual(
    check(
      "a.tsx",
      'const a = <p> {n} · 5 % — {t("a.b")} <b>{name}</b></p>;\n',
    ),
    [],
  );
});

test("a string given to a prop that shows text is found; other props are not", () => {
  assert.deepEqual(
    check("a.tsx", 'const a = <Field label="Port" placeholder={"x y"} />;\n'),
    ["1:text in «label»", "1:text in «placeholder»"],
  );
  assert.deepEqual(
    check(
      "a.tsx",
      'const a = <Field className="muted small" type="number" label={t("a.b")} value={x} />;\n',
    ),
    [],
  );
});

test("a string an expression puts into the page is found through ?:, ??, && and a template", () => {
  assert.deepEqual(
    check(
      "a.tsx",
      "const a = <p>{ok ? `Rift ${v}` : \"Rift\"}</p>;\nconst b = <X title={v ?? 'None'} />;\n",
    ),
    ["1:text in JSX", "1:text in JSX", "2:text in «title»"],
  );
  assert.deepEqual(
    check("a.tsx", 'const a = <p>{ok ? t("a") : t("b")}</p>;\n'),
    [],
  );
});

test("Cyrillic or CJK in a string or a template is found wherever it is", () => {
  assert.deepEqual(
    check(
      "a.ts",
      'export const a = "Не удалось";\nexport const b = `${n} сек`;\nexport const c = "保存";\n',
    ),
    ["1:non-Latin text", "2:non-Latin text", "3:non-Latin text"],
  );
  // One Latin word is not told from a key or a code value.
  assert.deepEqual(check("a.ts", 'export const a = "online";\n'), []);
});

test("comments are not read", () => {
  assert.deepEqual(
    check(
      "a.ts",
      "// Русский комментарий\n/** «Сохранить» */\nexport const a = 1;\n",
    ),
    [],
  );
});

test("`i18n-ignore:` with a reason lets a text stay, on its line or the line above", () => {
  assert.deepEqual(
    check(
      "a.ts",
      '// i18n-ignore: the name of a language in that language\nexport const a = "Русский";\nexport const b = "Русский"; // i18n-ignore: same\n',
    ),
    [],
  );
  assert.deepEqual(
    check("a.ts", '// i18n-ignore:\nexport const a = "Русский";\n'),
    ["2:non-Latin text"],
  );
  assert.deepEqual(
    check(
      "a.tsx",
      "const a = (\n  <p>\n    {/* i18n-ignore: a product name */}\n    Rift\n  </p>\n);\n",
    ),
    [],
  );
});

test("a text kept in the code is reported once with its place", () => {
  const found = [];
  const directory = mkdtempSync(path.join(tmpdir(), "rift-hardcoded-"));
  try {
    const file = path.join(directory, "a.tsx");
    writeFileSync(file, 'const a = <Field label="Порт" />;\n');
    found.push(...findHardcodedText([file]));
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
  assert.equal(found.length, 1);
  assert.equal(found[0].line, 1);
  assert.equal(found[0].column, 24);
  assert.equal(found[0].text, "Порт");
});

test("only production sources are read: no tests, stories, fixtures, declarations or catalogs", () => {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-hardcoded-"));
  try {
    for (const name of [
      "App.tsx",
      "format.ts",
      "App.test.tsx",
      "App.stories.tsx",
      "locales/yml.d.ts",
      "locales/ru.yml",
      "testing/mock.ts",
      "components/ui.tsx",
      "notes.md",
    ]) {
      mkdirSync(path.dirname(path.join(directory, name)), { recursive: true });
      writeFileSync(path.join(directory, name), "export {};\n");
    }
    assert.deepEqual(
      sourceFiles(directory).map((f) => path.relative(directory, f)),
      ["App.tsx", path.join("components", "ui.tsx"), "format.ts"],
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("what reads as a sentence: two words, a capital or a closing mark, no code", () => {
  for (const text of [
    "Loading failed",
    "try again.",
    "Failed to load {name}",
    "${n} files left.",
    "Retry in:",
    "I agree.",
    "A failure.",
    "I agree",
  ])
    assert.equal(looksLikeSentence(text), true, text);
  for (const text of [
    "online",
    "muted small",
    "react-dom/client",
    "application/json; charset=utf-8",
    "https://example.com is up",
    "a = b + c",
    "Software\\Microsoft\\Windows",
    "{name}",
    "settings.autostart.label",
    "config.json",
    "",
    "Failed.",
    "A {name}",
  ])
    assert.equal(looksLikeSentence(text), false, text);
});

test("a Latin sentence in a plain string or a template is found; a key and a code value are not", () => {
  assert.deepEqual(
    check(
      "a.ts",
      'export const a = "Loading failed.";\nexport const b = `Cannot open ${name}`;\nexport const c = "settings.autostart.label";\nexport const d = "muted small";\n',
    ),
    ["1:sentence", "2:sentence"],
  );
  assert.deepEqual(
    check(
      "a.ts",
      '// i18n-ignore: for the developer\nthrow new Error("Provider missing");\n',
    ),
    [],
  );
});

test("two-word sentences include one-letter words without expanding one-word or dynamic checks", () => {
  assert.deepEqual(
    check(
      "a.ts",
      [
        'export const a = "I agree.";',
        'export const b = "A failure.";',
        'export const one = "Failed.";',
        'export const dynamic = "Failed" + ": " + reason;',
      ].join("\n"),
    ),
    ["1:sentence", "2:sentence"],
  );
});

test("a string given to `text`, `error`, `submitError`, `hint` is found like a label", () => {
  assert.deepEqual(
    check(
      "a.tsx",
      'const a = <Box text="Empty" error={"Bad"} submitError="Oops" hint={ok ? "A" : "B"} />;\n',
    ),
    [
      "1:text in «text»",
      "1:text in «error»",
      "1:text in «submitError»",
      "1:text in «hint»",
      "1:text in «hint»",
    ],
  );
});

function rust(code) {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-hardcoded-rs-"));
  try {
    const file = path.join(directory, "a.rs");
    writeFileSync(file, code);
    return findHardcodedRust([file]).map((f) => `${f.line}:${f.kind}`);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("Rust: Cyrillic, CJK and a Latin sentence in a literal are found", () => {
  assert.deepEqual(
    rust(
      'fn f() {\n    let a = "Не удалось";\n    let b = "保存";\n    let c = "Cannot open the file.\\n";\n    let d = r#"Plain sentence here"#;\n}\n',
    ),
    ["2:non-Latin text", "3:non-Latin text", "4:sentence", "5:sentence"],
  );
});

test("Rust: escaped Unicode and English sentences are checked as their runtime text", () => {
  assert.deepEqual(
    rust(
      [
        String.raw`const A: &str = "\u{41F}\u{440}";`,
        String.raw`const B: &str = "\u{4FDD}\u{5B58}";`,
        'const C: &str = "\\u{1F_600}\\u{4FDD}";',
        String.raw`const D: &str = "\u{49}\x20agree.";`,
        String.raw`const E: &str = "\\u{41F}";`,
        String.raw`const F: &str = r"\u{41F}";`,
        'const G: &[u8] = b"I agree.";',
        'const H: &str = "Failed.";',
      ].join("\n"),
    ),
    ["1:non-Latin text", "2:non-Latin text", "3:non-Latin text", "4:sentence"],
  );
});

test("Rust: a raw delimiter beyond ten hashes cannot hide text between embedded quotes", () => {
  for (const count of [11, 32, 255]) {
    const hashes = "#".repeat(count);
    assert.deepEqual(
      rust([
        `const A: &str = r${hashes}"prefix" Привет "suffix"${hashes};`,
        `const B: &[u8] = br${hashes}"prefix" I agree. "suffix"${hashes};`,
        'const C: &str = "A failure.";',
      ].join("\n")),
      ["1:non-Latin text", "3:sentence"],
    );
  }
});

test("Rust: keys, codes, paths, patterns, bytes and format places are not texts", () => {
  assert.deepEqual(
    rust(
      [
        "fn f() {",
        '    let a = t("core.error.invalid");',
        '    let b = "Software\\\\Microsoft\\\\Windows";',
        '    let c = r"^\\d+ items$";',
        '    let d = b"Hello world";',
        '    let e = format!("{name}");',
        '    let f = "sing-box.exe";',
        "}",
      ].join("\n"),
    ),
    [],
  );
});

test("Rust: comments and test code are not read; `i18n-ignore:` lets a literal stay", () => {
  assert.deepEqual(
    rust(
      [
        "// Комментарий",
        '/// "Doc sentence here."',
        "fn f() {",
        "    // i18n-ignore: what the child prints",
        '    let a = "Только так";',
        '    let b = "Только так"; // i18n-ignore: same',
        "}",
        "#[cfg(test)]",
        "mod tests {",
        '    fn g() { let c = "Не читается"; }',
        "}",
        "#[cfg(all(test, unix))]",
        'fn h() { let d = "Unix test sentence."; }',
        "#[cfg(not(test))]",
        'fn k() { let e = "Production sentence."; }',
      ].join("\n"),
    ),
    ["15:sentence"],
  );
});

test("Rust: attribute comments containing brackets do not leak test-only literals", () => {
  assert.deepEqual(
    rust([
      "#[cfg(/* ] not(test) /* nested ] */ */ test)]",
      'fn hidden() { let a = "Только тест"; }',
      "#[cfg(// ] not(test)",
      "test)]",
      'fn also_hidden() { let a = "Только тест"; }',
      "#[cfg(not(/* test ] */ test))]",
      'fn production() { let a = "Production sentence."; }',
    ].join("\n")),
    ["7:sentence"],
  );
});

test("Rust: nested test cfg attributes cannot reset an outer test-only boundary", () => {
  assert.deepEqual(
    rust([
      "#[cfg(test)]",
      "mod tests {",
      "    #[cfg(test)]",
      '    const INNER: &str = "Nested test sentence.";',
      '    fn after_constant() { let a = "Outer test sentence."; }',
      "    #[cfg(test)]",
      '    fn nested() { let a = "Nested test sentence."; }',
      '    fn after_function() { let a = "Outer test sentence."; }',
      "}",
      'fn production() { let a = "Production sentence."; }',
    ].join("\n")),
    ["10:sentence"],
  );
});

test("Rust: module-scoped inner cfg test does not hide later production code", () => {
  assert.deepEqual(
    rust([
      "mod production {",
      "    mod tests {",
      "        #![cfg(test)]",
      '        const HIDDEN: &str = "Test sentence.";',
      '        fn nested() { let a = "Test sentence."; }',
      "    }",
      '    fn inside() { let a = "Inside production."; }',
      "}",
      'fn outside() { let a = "Outside production."; }',
    ].join("\n")),
    ["7:sentence", "9:sentence"],
  );
});

test("Rust: only .rs files are read", () => {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-hardcoded-rs-"));
  try {
    for (const name of ["lib.rs", "elevation/mod.rs", "locales.json", "notes.md"]) {
      mkdirSync(path.dirname(path.join(directory, name)), { recursive: true });
      writeFileSync(path.join(directory, name), "");
    }
    assert.deepEqual(
      rustSourceFiles(directory).map((f) => path.relative(directory, f)),
      [path.join("elevation", "mod.rs"), "lib.rs"],
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("Rust keys: one the catalog has not, and one nothing asks for, are found; tests do not count", () => {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-hardcoded-rs-"));
  try {
    const file = path.join(directory, "a.rs");
    writeFileSync(
      file,
      [
        "fn f() {",
        String.raw`    t("core.\u{75}sed");`,
        '    tr!("shell.tray.open", name = 1);',
        '    t("core.missing");',
        '    let not_a_key = "core";',
        "}",
        "#[cfg(test)]",
        'mod tests { fn g() { t("core.only_in_tests"); } }',
      ].join("\n"),
    );
    const problems = rustKeyProblems(
      [file],
      ["core.used", "shell.tray.open", "core.only_in_tests", "core.dead"],
    );
    assert.equal(problems.length, 3);
    assert.match(problems[0], /a\.rs:4 {2}the catalog has no key «core\.missing»/);
    assert.deepEqual(
      problems.slice(1).sort(),
      [
        "ru.yml  «core.dead» is asked for by no code: remove it",
        "ru.yml  «core.only_in_tests» is asked for by no code: remove it",
      ],
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

function installer(code) {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-hardcoded-nsis-"));
  try {
    const file = path.join(directory, "installer.nsi");
    writeFileSync(file, code);
    return findHardcodedInstaller([file]).map((f) => `${f.line}:${f.kind}`);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("Installer: a text written in a command is found; `$(name)`, codes and comments are not", () => {
  assert.deepEqual(
    installer(
      [
        'MessageBox MB_OK "Установка завершена"',
        'MessageBox MB_OK "Installation finished."',
        'DetailPrint "Installed to $INSTDIR now."',
        'Abort "Something went wrong"',
        'MessageBox MB_ICONSTOP "$(webview2InstallError)" IDOK done',
        'DetailPrint "$(installing): $INSTDIR"',
        'WriteRegStr SHCTX "Software\\Classes\\rift" "URL Protocol" ""',
        '!define UNINSTKEY "Software\\Microsoft\\Windows"',
        'StrCpy $R1 "$R1 _?=$4" ; append the uninstall directory',
        '; MessageBox MB_OK "Installation finished."',
        '# Установка завершена',
        '${If} $0 == "success"',
      ].join("\n"),
    ),
    ["1:non-Latin text", "2:sentence", "3:sentence", "4:sentence"],
  );
});

test("Installer: `i18n-ignore:` lets a text stay, on its line or the line above", () => {
  assert.deepEqual(
    installer(
      '; i18n-ignore: a debug line\nDetailPrint "Debug output here"\nDetailPrint "Debug output here" ; i18n-ignore: same\n',
    ),
    [],
  );
});
