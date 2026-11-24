//! The texts a person reads that the core and the shell make: journal lines, refusals, the tray menu,
//! the titles of the dialogs. They are not written in the code: they live in the catalogs of
//! the interface, one file per language (`desktop/src/locales/<language>.yml`), and
//! `tooling/locales-sync.mjs` turns the groups `language`, `core` and `shell` of every file into
//! `locales.json`, which is built into the program. The core knows no YAML.
//!
//! The language is the setting `Settings.language`: [`set_language`] is called when the settings are
//! read and saved. A text written to the disk (a journal line) keeps the language it was made in.
//!
//! A text is asked for by its key: [`t`] for a text without a value, [`tr!`] with `{name}` values and
//! the form of a number (`count`). A key that is missing in the current language is read from the
//! default language, then shown as itself; `tooling/locales-sync.mjs` and the tests of this file
//! keep that from ever reaching a person.
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// The language of the first release: the default setting and the one every other is checked against.
pub const DEFAULT_LANGUAGE: &str = "ru";

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Entry {
    Plain(String),
    /// The forms of a number: `one`, `few`, `many`, `other` (the categories of CLDR).
    Plural(HashMap<String, String>),
}

/// The catalogs of all languages, in the order of their codes.
#[derive(Debug)]
pub struct Catalogs {
    languages: Vec<(String, HashMap<String, Entry>)>,
}

impl Catalogs {
    /// Reads `{"ru": {"core.key": "text", "core.number": {"one": "…", "other": "…"}}, …}`.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        let by_code: std::collections::BTreeMap<String, HashMap<String, Entry>> =
            serde_json::from_str(json)?;
        Ok(Self {
            languages: by_code.into_iter().collect(),
        })
    }
    pub fn codes(&self) -> impl Iterator<Item = &str> {
        self.languages.iter().map(|(code, _)| code.as_str())
    }
    pub fn index(&self, code: &str) -> Option<usize> {
        self.languages.iter().position(|(c, _)| c == code)
    }
    fn entry(&self, language: usize, key: &str) -> Option<&Entry> {
        self.languages
            .get(language)
            .and_then(|(_, texts)| texts.get(key))
    }
    /// The text of a key in a language, else in the default one; `None` if neither has it.
    fn find(&self, language: usize, key: &str) -> Option<(&Entry, &str)> {
        let default = self.index(DEFAULT_LANGUAGE);
        self.entry(language, key)
            .map(|e| (e, self.languages[language].0.as_str()))
            .or_else(|| default.and_then(|d| self.entry(d, key).map(|e| (e, DEFAULT_LANGUAGE))))
    }
    /// The text of a key as a string; a plural gives its `other` form.
    pub fn text(&self, language: usize, key: &'static str) -> &str {
        match self.find(language, key) {
            Some((Entry::Plain(text), _)) => text,
            Some((Entry::Plural(forms), _)) => forms.get("other").map_or(key, String::as_str),
            None => key,
        }
    }
    /// The text of a key with its `{name}` filled; a message with forms takes the one of `count`.
    pub fn render(
        &self,
        language: usize,
        key: &'static str,
        params: &[(&str, &dyn Display)],
    ) -> String {
        let Some((entry, code)) = self.find(language, key) else {
            return key.to_string();
        };
        let template = match entry {
            Entry::Plain(text) => text.as_str(),
            Entry::Plural(forms) => {
                let count = params
                    .iter()
                    .find(|(name, _)| *name == "count")
                    .and_then(|(_, value)| value.to_string().parse::<u64>().ok());
                let form = count.and_then(|n| plural_form(code, n)).unwrap_or("other");
                forms
                    .get(form)
                    .or_else(|| forms.get("other"))
                    .map_or(key, String::as_str)
            }
        };
        fill(template, params)
    }
    /// The keys of the language, for the tests that compare languages.
    pub fn keys(&self, code: &str) -> Vec<&str> {
        self.index(code)
            .map(|i| self.languages[i].1.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }
    /// Every text of a key in a language, whole (all forms of a plural).
    pub fn forms(&self, code: &str, key: &str) -> Vec<&str> {
        match self.index(code).and_then(|i| self.entry(i, key)) {
            Some(Entry::Plain(text)) => vec![text],
            Some(Entry::Plural(forms)) => forms.values().map(String::as_str).collect(),
            None => vec![],
        }
    }
}

/// `{name}` replaced by the value of the name; a name nobody gave stays as it is.
fn fill(template: &str, params: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close)
                if !after[..close].is_empty()
                    && after[..close]
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_') =>
            {
                let name = &after[..close];
                match params.iter().find(|(n, _)| *n == name) {
                    Some((_, value)) => out.push_str(&value.to_string()),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The form of a whole number in a language (the rules of CLDR); `None` for a language without rules,
/// which reads `other`. A test refuses a language of the catalogs that has no rule here.
pub fn plural_form(language: &str, n: u64) -> Option<&'static str> {
    match language.split('-').next().unwrap_or(language) {
        "ru" | "uk" | "be" => Some(if n % 10 == 1 && n % 100 != 11 {
            "one"
        } else if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) {
            "few"
        } else {
            "many"
        }),
        "fr" => Some(if n <= 1 { "one" } else { "other" }),
        "en" | "de" | "nl" | "sv" | "it" | "es" | "pt" => {
            Some(if n == 1 { "one" } else { "other" })
        }
        // No forms of number: Chinese, Japanese, Korean, Vietnamese, Thai.
        "zh" | "ja" | "ko" | "vi" | "th" => Some("other"),
        _ => None,
    }
}

fn catalogs() -> &'static Catalogs {
    static CATALOGS: OnceLock<Catalogs> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        Catalogs::parse(include_str!("locales.json"))
            .expect("locales.json is generated by tooling/locales-sync.mjs")
    })
}

static CURRENT: AtomicUsize = AtomicUsize::new(usize::MAX);

fn current() -> usize {
    match CURRENT.load(Ordering::Relaxed) {
        usize::MAX => catalogs().index(DEFAULT_LANGUAGE).unwrap_or(0),
        index => index,
    }
}

/// The codes of the languages that have a catalog: what `Settings.language` may be.
pub fn languages() -> impl Iterator<Item = &'static str> {
    catalogs().codes()
}

pub fn is_language(code: &str) -> bool {
    catalogs().index(code).is_some()
}

/// Sets the language the texts are made in; a code without a catalog changes nothing.
pub fn set_language(code: &str) -> bool {
    match catalogs().index(code) {
        Some(index) => {
            CURRENT.store(index, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// The language the texts are made in.
pub fn language() -> &'static str {
    catalogs()
        .codes()
        .nth(current())
        .unwrap_or(DEFAULT_LANGUAGE)
}

/// The text of a key in the current language.
pub fn t(key: &'static str) -> &'static str {
    catalogs().text(current(), key)
}

/// The text of a key with values (see [`tr!`]).
pub fn render(key: &'static str, params: &[(&str, &dyn Display)]) -> String {
    catalogs().render(current(), key, params)
}

/// `tr!("core.key", name = value, count = n)`: the text of the key with `{name}` replaced.
#[macro_export]
macro_rules! tr {
    ($key:expr $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::text::render($key, &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),*])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two() -> Catalogs {
        Catalogs::parse(
            r#"{
              "ru": {"a": "один {x}", "n": {"one": "{count} сервер", "few": "{count} сервера", "many": "{count} серверов", "other": "{count} сервера"}, "only_ru": "только русский"},
              "en": {"a": "one {x}", "n": {"one": "{count} server", "other": "{count} servers"}}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn a_key_is_read_in_the_language_and_falls_back_to_the_default_then_to_itself() {
        let c = two();
        let (en, ru) = (c.index("en").unwrap(), c.index("ru").unwrap());
        assert_eq!(c.text(ru, "a"), "один {x}");
        assert_eq!(c.text(en, "a"), "one {x}");
        assert_eq!(c.text(en, "only_ru"), "только русский");
        assert_eq!(c.text(en, "nowhere"), "nowhere");
    }

    #[test]
    fn values_are_filled_and_a_plural_takes_the_form_of_the_count() {
        let c = two();
        let (en, ru) = (c.index("en").unwrap(), c.index("ru").unwrap());
        assert_eq!(c.render(ru, "a", &[("x", &"раз")]), "один раз");
        assert_eq!(c.render(ru, "a", &[]), "один {x}");
        for (n, text) in [
            (1, "1 сервер"),
            (2, "2 сервера"),
            (5, "5 серверов"),
            (11, "11 серверов"),
            (21, "21 сервер"),
            (112, "112 серверов"),
            (0, "0 серверов"),
        ] {
            assert_eq!(c.render(ru, "n", &[("count", &n)]), text);
        }
        assert_eq!(c.render(en, "n", &[("count", &1)]), "1 server");
        assert_eq!(c.render(en, "n", &[("count", &2)]), "2 servers");
        // Without a count the plural reads as `other`.
        assert_eq!(c.text(en, "n"), "{count} servers");
    }

    #[test]
    fn french_zero_and_one_render_the_singular_form() {
        let c =
            Catalogs::parse(r#"{"fr":{"n":{"one":"{count} serveur","other":"{count} serveurs"}}}"#)
                .unwrap();
        let fr = c.index("fr").unwrap();
        for (n, expected) in [(0, "0 serveur"), (1, "1 serveur"), (2, "2 serveurs")] {
            assert_eq!(c.render(fr, "n", &[("count", &n)]), expected);
            assert_eq!(plural_form("fr-CA", n), plural_form("fr", n));
        }
        assert_eq!(plural_form("en", 0), Some("other"));
    }

    #[test]
    fn a_value_is_not_read_as_a_placeholder_and_a_stray_brace_stays() {
        assert_eq!(fill("a {x} b", &[("x", &"{x}")]), "a {x} b");
        assert_eq!(fill("{", &[]), "{");
        assert_eq!(fill("{ } {x", &[("x", &1)]), "{ } {x");
        assert_eq!(fill("{x}{x}", &[("x", &7)]), "77");
    }

    #[test]
    fn the_macro_fills_named_values_of_any_display_type() {
        let c = two();
        let ru = c.index("ru").unwrap();
        let rendered = c.render(ru, "a", &[("x", &3u8 as &dyn Display)]);
        assert_eq!(rendered, "один 3");
    }

    #[test]
    fn the_built_in_catalogs_have_the_default_language_and_a_rule_of_number_for_each_language() {
        let c = catalogs();
        assert!(c.index(DEFAULT_LANGUAGE).is_some());
        for code in c.codes() {
            assert!(plural_form(code, 1).is_some(), "{code}: no rule of number");
        }
    }

    #[test]
    fn every_language_has_the_keys_and_the_values_of_the_default_one() {
        let c = catalogs();
        let mut reference = c.keys(DEFAULT_LANGUAGE);
        reference.sort_unstable();
        let names = |text: &str| -> Vec<String> {
            let mut found = vec![];
            let mut rest = text;
            while let Some(open) = rest.find('{') {
                let after = &rest[open + 1..];
                match after.find('}') {
                    Some(close) => {
                        found.push(after[..close].to_string());
                        rest = &after[close + 1..];
                    }
                    None => break,
                }
            }
            found.sort();
            found.dedup();
            found
        };
        for code in c.codes() {
            let mut keys = c.keys(code);
            keys.sort_unstable();
            assert_eq!(
                keys, reference,
                "{code}: the keys differ from {DEFAULT_LANGUAGE}"
            );
            for key in &keys {
                let wanted: Vec<String> = c
                    .forms(DEFAULT_LANGUAGE, key)
                    .iter()
                    .flat_map(|t| names(t))
                    .collect();
                for form in c.forms(code, key) {
                    for name in names(form) {
                        assert!(
                            wanted.contains(&name),
                            "{code}: {key} has {{{name}}} that {DEFAULT_LANGUAGE} has not"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_language_is_set_by_a_code_with_a_catalog_only() {
        assert!(set_language(DEFAULT_LANGUAGE));
        assert!(!set_language("xx"));
        assert_eq!(language(), DEFAULT_LANGUAGE);
        assert!(is_language(DEFAULT_LANGUAGE));
        assert!(!is_language(""));
    }
}
