import { describe, it, expect } from "vitest";
import {
  CATALOGS,
  DEFAULT_LOCALE,
  has,
  LANGUAGES,
  localeTag,
  setLocale,
  t,
  tFor,
} from "./i18n";
import { flattenCatalog, forThePage } from "../yamlCatalog";

describe("t", () => {
  it("returns the text of a key, with the placeholders filled", () => {
    expect(t("format.latency", { value: 42 })).toBe("42 мс");
    expect(t("format.usage_of", { used: "1 МБ", total: "2 МБ" })).toBe(
      "1 МБ из 2 МБ",
    );
  });
  it("shows an unknown key as itself and keeps a placeholder nobody filled", () => {
    expect(t("no.such.key")).toBe("no.such.key");
    expect(t("format.latency")).toBe("{value} мс");
    // Inherited names are not keys.
    expect(has("constructor")).toBe(false);
    expect(t("toString")).toBe("toString");
  });
  it("does not read a placeholder out of a value it was given", () => {
    expect(t("format.latency", { value: "{value}" })).toBe("{value} мс");
  });
  it("finds a text by a prefix and a code, or says there is none", () => {
    expect(tFor("mode", "tun")).toBe("TUN");
    expect(tFor("mode", "nope")).toBeNull();
  });
  it("takes the languages from the files of texts, each with its name and flag", () => {
    expect(LANGUAGES).toEqual(
      Object.keys(CATALOGS).map((code) => ({
        code,
        flag: expect.stringMatching(/^\p{Regional_Indicator}{2}$/u),
        name: expect.stringMatching(/\S/),
      })),
    );
    expect(LANGUAGES.map((l) => l.code)).toContain(DEFAULT_LOCALE);
    expect(LANGUAGES.find((l) => l.code === "ru")).toEqual({
      code: "ru",
      flag: "🇷🇺",
      name: "Русский",
    });
  });
  it("tags its formats with the language and keeps the language for a code it has no file for", () => {
    expect(localeTag()).toBe("ru-RU");
    setLocale("xx");
    expect(localeTag()).toBe("ru-RU");
    setLocale("ru");
    expect(localeTag()).toBe("ru-RU");
  });
  it("gives the page the texts of the interface and none of the core, the shell or the installer", () => {
    expect(
      forThePage({
        "core.a": "x",
        "shell.b": "y",
        "installer.c": "z",
        "d.e": "w",
      }),
    ).toEqual({ "d.e": "w" });
    expect(
      Object.keys(CATALOGS.ru).some((key) =>
        /^(core|shell|installer)\./.test(key),
      ),
    ).toBe(false);
  });
});

describe("flattenCatalog", () => {
  it("flattens groups into dotted keys", () => {
    expect(flattenCatalog("a:\n  b:\n    c: text\n  d: more\n")).toEqual({
      "a.b.c": "text",
      "a.d": "more",
    });
  });
  it("reads a map of plural categories with `other` as one message", () => {
    expect(
      flattenCatalog(
        'n:\n  one: "{count} сервер"\n  other: "{count} серверов"',
      ),
    ).toEqual({ n: { one: "{count} сервер", other: "{count} серверов" } });
  });
  it("refuses what would show as `undefined`", () => {
    expect(() => flattenCatalog("a: 5")).toThrow(/«a» is not text/);
    expect(() => flattenCatalog("a:\n  - x")).toThrow(/is not text/);
    expect(() => flattenCatalog("a:")).toThrow(/is not text/);
    expect(() => flattenCatalog("n:\n  one: x")).toThrow(/no «other»/);
    expect(() => flattenCatalog("n:\n  one: 1\n  other: x")).toThrow(
      /is not a string/,
    );
    expect(() => flattenCatalog('"a.b": x')).toThrow(/the key «a\.b»/);
  });
  it("refuses empty maps at the root and within a message group", () => {
    expect(() => flattenCatalog("{}", "ru.yml")).toThrow(
      /ru\.yml: «\(root\)» is an empty map/,
    );
    expect(() => flattenCatalog("format:\n  latency: {}", "ru.yml")).toThrow(
      /ru\.yml: «format\.latency» is an empty map/,
    );
  });
  it("refuses a key written twice", () => {
    expect(() => flattenCatalog("a: x\na: y")).toThrow();
  });
});
