type Entry = string | Record<string, string>;
type Catalog = Record<string, Entry>;
export type Locale = string;
type Params = Record<string, string | number>;

/**
 * Every language is a file of `locales/`: its name is the code of the language. The list of languages
 * is the list of the files, so a new language is a new file and nothing in the code.
 */
export const CATALOGS: Readonly<Record<Locale, Catalog>> = Object.fromEntries(
  Object.entries(
    import.meta.glob<Catalog>("./locales/*.yml", {
      eager: true,
      import: "default",
    }),
  )
    .map(([path, catalog]) => [
      /([^/]+)\.yml$/.exec(path)?.[1] ?? path,
      catalog,
    ])
    .sort(([a], [b]) => String(a).localeCompare(String(b))),
);
/** The language of the first release: the default setting, and the one every other is checked against. */
export const DEFAULT_LOCALE: Locale = "ru";

const own = (code: Locale, key: string): string => {
  const entry = CATALOGS[code]?.[key];
  return typeof entry === "string" ? entry : "";
};

/**
 * The languages the interface can be set to. The name is the language's own and is not
 * translated: whoever cannot read the current language must still find theirs.
 */
export const LANGUAGES: readonly {
  code: Locale;
  flag: string;
  name: string;
}[] = Object.keys(CATALOGS).map((code) => ({
  code,
  flag: own(code, "language.flag"),
  name: own(code, "language.name"),
}));

let current: Locale = DEFAULT_LOCALE;
let plurals = new Intl.PluralRules(own(current, "language.tag"));

/** Switches the language; a code without a file is ignored, the language stays. */
export function setLocale(locale: Locale): void {
  if (!Object.hasOwn(CATALOGS, locale)) return;
  current = locale;
  plurals = new Intl.PluralRules(own(locale, "language.tag"));
}
/** Whether the current language has a message under the key. */
export const has = (key: string): boolean =>
  Object.hasOwn(CATALOGS[current], key);
/** The message `prefix.code` when the current language has one, else `null`. */
export const tFor = (prefix: string, code: string): string | null =>
  has(`${prefix}.${code}`) ? t(`${prefix}.${code}`) : null;
export const localeTag = (): string => own(current, "language.tag");

/**
 * The text of a message in the current language. `{name}` is replaced by `params.name`; a message
 * with plural forms takes the form of `params.count`. An unknown key shows as itself, which a test
 * (`i18n.test.ts`) keeps from ever reaching a screen.
 */
export function t(key: string, params: Params = {}): string {
  if (!has(key)) return key;
  const entry = CATALOGS[current][key];
  const template =
    typeof entry === "string"
      ? entry
      : (entry[plurals.select(Number(params.count))] ?? entry.other);
  return template.replace(/\{(\w+)\}/g, (whole, name: string) =>
    name in params ? String(params[name]) : whole,
  );
}
