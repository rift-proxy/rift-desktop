import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import {
  configProblems,
  forRust,
  installerConfig,
  installerProblems,
  main,
  nsisFile,
  outputs,
  problems,
  readCatalogs,
  usedByInstaller,
} from "./locales-sync.mjs";

const meta = (nsis, extra = "") =>
  `language:\n  name: "N"\n  flag: "🇷🇺"\n  tag: "xx"\ninstaller:\n  nsis_language: "${nsis}"\n${extra}`;
const catalogs = (files) => {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-locales-"));
  try {
    for (const [name, text] of Object.entries(files))
      writeFileSync(path.join(directory, `${name}.yml`), text);
    return readCatalogs(directory);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
};

test("every file of the folder is a language, named by the file", () => {
  const c = catalogs({
    ru: meta("Russian"),
    "zh-CN": meta("SimpChinese"),
    notes: "a: b\n",
  });
  assert.deepEqual(Object.keys(c), ["notes", "ru", "zh-CN"]);
});

test("a language with a key, a text or a name the default has not is refused", () => {
  const ru = `${meta("Russian", "  older: 'x {a}'\n")}core:\n  first: "{n} раз"\n`;
  const ok = catalogs({ ru, en: ru.replace("Russian", "English") });
  assert.deepEqual(problems(ok), []);
  const en = catalogs({
    ru,
    en: `${meta("English", "  older: 'x {a} {b}'\n")}core:\n  second: "x"\n`,
  });
  assert.deepEqual(problems(en), [
    "en: «core.first» is missing",
    "en: «installer.older» has {b} that ru has not",
    "en: «core.second» is not in ru",
  ]);
  assert.deepEqual(problems(catalogs({ en: meta("English") })), [
    "no catalog of the default language «ru»",
  ]);
  assert.deepEqual(
    problems(catalogs({ ru: "language:\n  name: N\n" })).sort(),
    [
      "ru: «installer.nsis_language» is missing",
      "ru: «language.flag» is missing",
      "ru: «language.tag» is missing",
    ],
  );
});

test("a translation that drops a name of the default text is refused, plural forms count together", () => {
  const ru = `${meta("Russian")}core:\n  plain: "{n} раз"\n  servers:\n    one: "{n} сервер"\n    other: "{n} серверов"\n`;
  const lost = catalogs({
    ru,
    en: `${meta("English")}core:\n  plain: "once"\n  servers:\n    one: "server"\n    other: "{n} servers"\n`,
  });
  // `servers.one` may leave out {n}: the message as a whole still has it.
  assert.deepEqual(problems(lost), ["en: «core.plain» is missing {n}"]);
  const allLost = catalogs({
    ru,
    en: `${meta("English")}core:\n  plain: "{n} time"\n  servers:\n    one: "server"\n    other: "servers"\n`,
  });
  assert.deepEqual(problems(allLost), ["en: «core.servers» is missing {n}"]);
});

test("the NSIS language of a catalog is a file name of NSIS, written once", () => {
  const ru = meta("Russian");
  assert.deepEqual(problems(catalogs({ ru, en: meta("English") })), []);
  assert.deepEqual(problems(catalogs({ ru, uk: meta("Russian") })), [
    "uk: «installer.nsis_language» is «Russian», as in ru",
  ]);
  // `${LANG_RUSSIAN}` and a file name on a case-insensitive disk are one language for both spellings.
  for (const same of ["russian", "RUSSIAN"])
    assert.deepEqual(problems(catalogs({ ru, uk: meta(same) })), [
      `uk: «installer.nsis_language» is «${same}», as in ru`,
    ]);
  for (const bad of ["../Russian", "Rus/sian", "Rus ian", "1Russian"])
    assert.deepEqual(
      problems(catalogs({ ru, xx: meta(JSON.stringify(bad).slice(1, -1)) })),
      [
        `xx: «installer.nsis_language» is «${bad}», not the name of an NSIS language`,
      ],
    );
});

test("the form of Tauri for the product name is refused in the installer group", () => {
  const c = catalogs({
    ru: meta(
      "Russian",
      '  bad: "{{product_name}} запущен"\n  good: "${PRODUCTNAME} запущен"\n',
    ),
  });
  assert.deepEqual(problems(c), [
    "ru: «installer.bad» has {{…}}, write the NSIS form (${PRODUCTNAME}) instead",
  ]);
});

test("an installer text with plural forms is refused, NSIS has one string per name", () => {
  const c = catalogs({
    ru: meta(
      "Russian",
      '  files:\n    one: "{n} файл"\n    other: "{n} файлов"\n  plain: "Готово"\n',
    ),
  });
  assert.deepEqual(problems(c), [
    "ru: «installer.files» has plural forms, an installer text is one string",
  ]);
});

test("tauri.conf.json is compared without the order its keys are written in", () => {
  const one = catalogs({ ru: meta("Russian") });
  const { customLanguageFiles, ...rest } = installerConfig(one);
  const shuffled = {
    displayLanguageSelector: rest.displayLanguageSelector,
    customLanguageFiles,
    languages: rest.languages,
  };
  assert.deepEqual(
    configProblems(one, { bundle: { windows: { nsis: shuffled } } }),
    [],
  );
  const two = catalogs({ ru: meta("Russian"), en: meta("English") });
  const files = installerConfig(two).customLanguageFiles;
  const reversed = Object.fromEntries(Object.entries(files).reverse());
  const nsis = { ...installerConfig(two), customLanguageFiles: reversed };
  assert.deepEqual(configProblems(two, { bundle: { windows: { nsis } } }), []);
});

test("a catalog without its NSIS language is reported, not a stack trace, by check and by write", () => {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-main-"));
  const printed = [];
  const original = console.error;
  console.error = (line) => printed.push(line);
  try {
    writeFileSync(
      path.join(directory, "ru.yml"),
      'language:\n  name: "N"\n  flag: "x"\n  tag: "xx"\n',
    );
    for (const mode of ["check", "write"]) {
      printed.length = 0;
      assert.equal(main([mode], directory), 1);
      assert.ok(
        printed.includes("ru: «installer.nsis_language» is missing"),
        mode,
      );
    }
  } finally {
    console.error = original;
    rmSync(directory, { recursive: true, force: true });
  }
});

test("the core gets the language, core and shell groups and nothing of the page or the installer", () => {
  const c = catalogs({
    ru: `${meta("Russian")}core:\n  a: "1"\nshell:\n  b: "2"\nsettings:\n  c: "3"\n`,
  });
  assert.deepEqual(Object.keys(forRust(c.ru)), [
    "language.name",
    "language.flag",
    "language.tag",
    "core.a",
    "shell.b",
  ]);
});

test("the installer text becomes LangStrings with NSIS escapes and the language of NSIS", () => {
  const c = catalogs({
    ru: meta(
      "Russian",
      '  add_or_reinstall: "Добавить"\n  app_running_ok_kill: "${PRODUCTNAME}!$\\\\nOK"\n  quote: \'Он сказал "да"\'\n',
    ),
  });
  assert.equal(
    nsisFile(c.ru),
    "; Generated by tooling/locales-sync.mjs from the group `installer` of the catalog.\n" +
      "; Do not edit: change desktop/src/locales/*.yml and run `node tooling/locales-sync.mjs write`.\n" +
      'LangString addOrReinstall ${LANG_RUSSIAN} "Добавить"\n' +
      'LangString appRunningOkKill ${LANG_RUSSIAN} "${PRODUCTNAME}!$\\nOK"\n' +
      'LangString quote ${LANG_RUSSIAN} "Он сказал $\\"да$\\""\n',
  );
});

test("the files made are the Rust catalogs and one installer file per language", () => {
  const c = catalogs({ ru: meta("Russian"), en: meta("English") });
  const names = [...outputs(c).keys()].map((f) => path.basename(f));
  assert.deepEqual(names, ["locales.json", "English.nsh", "Russian.nsh"]);
});

test("tauri.conf.json must list the installer languages of the files", () => {
  const one = catalogs({ ru: meta("Russian") });
  assert.deepEqual(installerConfig(one), {
    languages: ["Russian"],
    customLanguageFiles: { Russian: "windows/lang/Russian.nsh" },
    displayLanguageSelector: false,
  });
  const nsis = installerConfig(one);
  assert.deepEqual(configProblems(one, { bundle: { windows: { nsis } } }), []);
  assert.equal(
    configProblems(one, {
      bundle: {
        windows: { nsis: { ...nsis, languages: ["Russian", "English"] } },
      },
    }).length,
    1,
  );
  // With two languages the installer asks which one.
  const two = catalogs({ ru: meta("Russian"), en: meta("English") });
  assert.equal(installerConfig(two).displayLanguageSelector, true);
  assert.equal(
    configProblems(two, { bundle: { windows: { nsis } } }).length,
    3,
  );
});

test("a LangString the installer template asks for must be written in every language", () => {
  const c = catalogs({ ru: meta("Russian", '  older: "x"\n') });
  assert.deepEqual(installerProblems(c, new Set(["older"])), []);
  assert.deepEqual(installerProblems(c, new Set(["older", "unknown"])), [
    "ru: the installer asks for $(unknown), «installer.unknown» is missing",
  ]);
});

test("the names the template and its hooks ask for are read from them", () => {
  const directory = mkdtempSync(path.join(tmpdir(), "rift-nsis-"));
  try {
    mkdirSync(directory, { recursive: true });
    writeFileSync(
      path.join(directory, "installer.nsi"),
      'DetailPrint "$(installingWebview2)"\nMessageBox MB_OK "$(^Name)"\n',
    );
    writeFileSync(
      path.join(directory, "installer-hooks.nsh"),
      'DetailPrint "$(webview2DownloadError)"\n',
    );
    assert.deepEqual([...usedByInstaller(directory)].sort(), [
      "installingWebview2",
      "webview2DownloadError",
    ]);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
