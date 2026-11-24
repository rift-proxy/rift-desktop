//! How much of the journal is kept: the newest entries of the last day, at most 5000 of them
//! and at most 100 KiB of `journal.json`. Whatever is older or over a limit is cut off the start;
//! there is no archive.
use crate::{
    application::{Category, Entry},
    model::Store,
    persistence, Result,
};
use aho_corasick::{
    automaton::{Automaton, StateID},
    nfa::contiguous::NFA,
    Anchored,
};
use regex::{Captures, Regex};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::HashMap,
    io::{self, Read},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, OnceLock},
};

/// Most entries kept.
pub const MAX_ENTRIES: usize = 5000;
/// Most bytes of the serialized journal (the `journal.json` file): 100 KiB.
pub const MAX_BYTES: usize = 100 * 1024;
/// Oldest age of an entry kept: one day, in seconds.
pub const MAX_AGE_SECS: u64 = 24 * 60 * 60;
/// Panic events wait outside the live journal until its sole writer can recover them.
pub const MAX_CRASH_RECORDS: usize = 16;
/// Includes worst-case JSON escaping of a bounded message and detail.
pub const MAX_CRASH_BYTES: usize = 128 * 1024;
const CRASH_DIRECTORY: &str = "crash-journal";

/// Maximum cleaned technical detail stored for one event.
pub const MAX_DETAIL_BYTES: usize = 16 * 1024;
/// Maximum cleaned short message stored for one event.
pub const MAX_MESSAGE_BYTES: usize = 2 * 1024;
/// Limit work on arbitrary host/frontend text before redaction.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_SECRET_VALUES: usize = 32_768;
const MAX_SECRET_BYTES: usize = 8 * 1024 * 1024;
const HIDDEN: &str = "[redacted]";

/// Known private strings for the current local Store. Intentionally not `Debug`.
/// Exceeding its memory limits fails closed instead of leaving omitted secrets visible.
#[derive(Clone)]
pub struct Secrets {
    values: Vec<String>,
    bytes: usize,
    overflow: bool,
    matcher: OnceLock<Option<Arc<SecretMatcher>>>,
}

impl Default for Secrets {
    fn default() -> Self {
        let mut secrets = Self {
            values: Vec::new(),
            bytes: 0,
            overflow: false,
            matcher: OnceLock::new(),
        };
        for key in [
            "HOME",
            "USERPROFILE",
            "USERNAME",
            "USER",
            "COMPUTERNAME",
            "HOSTNAME",
        ] {
            if let Ok(value) = std::env::var(key) {
                secrets.extend([value]);
            }
        }
        secrets
    }
}

impl Secrets {
    /// Include Store credentials, subscription addresses, and the installation path.
    pub fn from_store(store: &Store, directory: &Path) -> Self {
        let mut secrets = Self::default();
        secrets.extend([directory.to_string_lossy().into_owned()]);
        for server in &store.servers {
            secrets.extend([server.host.clone(), server.name.clone()]);
            secrets.collect(&server.outbound);
        }
        for subscription in &store.subscriptions {
            secrets.extend([subscription.url.clone(), subscription.name.clone()]);
            if let Ok(url) = reqwest::Url::parse(&subscription.url) {
                secrets.extend(url.query_pairs().map(|(_, value)| value.into_owned()));
                secrets.extend([url.username().to_owned()]);
                if let Some(segments) = url.path_segments() {
                    for segment in segments.filter(|segment| segment.len() >= 8) {
                        secrets.extend([
                            segment.to_owned(),
                            percent_encoding::percent_decode_str(segment)
                                .decode_utf8_lossy()
                                .into_owned(),
                        ]);
                    }
                }
                if let Some(password) = url.password() {
                    secrets.extend([password.to_owned()]);
                }
            }
        }
        secrets
    }

    /// Add process-local credentials, such as the runtime proxy API secret.
    pub fn extend(&mut self, values: impl IntoIterator<Item = String>) {
        for value in values {
            if value.is_empty() {
                continue;
            }
            let position = match self.values.binary_search_by(|before| {
                value
                    .len()
                    .cmp(&before.len())
                    .then_with(|| before.cmp(&value))
            }) {
                Ok(_) => continue,
                Err(position) => position,
            };
            if self.values.len() >= MAX_SECRET_VALUES
                || self.bytes.saturating_add(value.len()) > MAX_SECRET_BYTES
                || value.len() > MAX_INPUT_BYTES
            {
                self.overflow = true;
                continue;
            }
            self.bytes += value.len();
            self.matcher.take();
            self.values.insert(position, value);
        }
    }

    fn matcher(&self) -> Option<&SecretMatcher> {
        self.matcher
            .get_or_init(|| SecretMatcher::new(&self.values).map(Arc::new))
            .as_deref()
    }

    fn collect(&mut self, value: &Value) {
        match value {
            Value::String(value) => {
                self.extend([value.clone()]);
                // Quoted JSON diagnostics can spell credentials using escapes.
                if let Ok(escaped) = serde_json::to_string(value) {
                    self.extend([escaped[1..escaped.len() - 1].to_owned()]);
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.collect(value);
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if ![
                        "type",
                        "network",
                        "flow",
                        "security",
                        "method",
                        "fingerprint",
                        "tag",
                    ]
                    .contains(&key.as_str())
                    {
                        self.collect(value);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Cache output summaries as well as the automaton. Enumerating every overlapping
/// pattern at search time would reintroduce secrets × input work for dense suffixes.
struct SecretMatcher {
    automaton: NFA,
    start: StateID,
    matches: HashMap<StateID, SecretMatch>,
}

#[derive(Default)]
struct SecretMatch {
    longest: usize,
    unrestricted: usize,
    // At one endpoint there is at most one distinct suffix per Unicode length.
    short: [usize; 4],
}

impl SecretMatcher {
    fn new(values: &[String]) -> Option<Self> {
        let automaton = NFA::builder().prefilter(false).build(values).ok()?;
        let start = automaton.start_state(Anchored::No).ok()?;
        let characters: Vec<_> = values
            .iter()
            .map(|value| value.chars().take(5).count())
            .collect();
        let mut matches = HashMap::new();
        // Every reachable trie state is a prefix of a credential. Visit those
        // prefixes once at construction, not once per clean call.
        for value in values {
            let mut state = start;
            for byte in value.bytes() {
                state = automaton.next_state(Anchored::No, state, byte);
                if automaton.is_match(state) {
                    matches.entry(state).or_insert_with(|| {
                        let mut summary = SecretMatch::default();
                        for index in 0..automaton.match_len(state) {
                            let pattern = automaton.match_pattern(state, index);
                            let len = automaton.pattern_len(pattern);
                            summary.longest = summary.longest.max(len);
                            let count = characters[pattern.as_usize()];
                            if count <= 4 {
                                summary.short[count - 1] = len;
                            } else {
                                summary.unrestricted = summary.unrestricted.max(len);
                            }
                        }
                        summary
                    });
                }
            }
        }
        Some(Self {
            automaton,
            start,
            matches,
        })
    }

    fn matches<'a>(&'a self, text: &'a str) -> impl Iterator<Item = (usize, &'a SecretMatch)> + 'a {
        let mut state = self.start;
        text.bytes().enumerate().filter_map(move |(at, byte)| {
            state = self.automaton.next_state(Anchored::No, state, byte);
            self.matches.get(&state).map(|summary| (at + 1, summary))
        })
    }

    fn input_prefix<'a>(&self, text: &'a str) -> &'a str {
        let input = bounded(text, MAX_INPUT_BYTES);
        let longest = self.automaton.max_pattern_len();
        if input.len() == text.len() || longest == 0 {
            return input;
        }
        // Only this window can contain a credential crossing the work boundary.
        // The longest match at each endpoint also has the earliest start.
        let mut start = input.len().saturating_sub(longest - 1);
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let end = bounded(text, input.len().saturating_add(longest)).len();
        let mut cutoff = input.len();
        for (at, summary) in self.matches(&text[start..end]) {
            let end = start + at;
            if end > input.len() {
                cutoff = cutoff.min(end - summary.longest);
            }
        }
        &text[..cutoff]
    }

    fn replace<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let mut coverage: Option<Vec<(i32, bool)>> = None;
        for (end, summary) in self.matches(text) {
            let mut cover = |len: usize, seed: bool| {
                if len != 0 {
                    let coverage = coverage.get_or_insert_with(|| vec![(0, false); text.len() + 1]);
                    coverage[end - len].0 += 1;
                    coverage[end - len].1 |= seed;
                    coverage[end].0 -= 1;
                }
            };
            cover(summary.unrestricted, true);
            let after_word = word(text[end..].chars().next());
            for len in summary.short {
                if len != 0 {
                    cover(
                        len,
                        !after_word && !word(text[..end - len].chars().next_back()),
                    );
                }
            }
        }
        let Some(coverage) = coverage else {
            return Cow::Borrowed(text);
        };
        // Keep original Unicode/punctuation boundaries for isolated short
        // credentials. Conservatively absorb connected short matches once any
        // match is redacted: replacing a neighbor with a marker can make a
        // previously blocked boundary valid (for example "-a" in "-a-a").
        // This also merges all overlaps without enumerating long suffix matches.
        let mut out = None;
        let mut depth = 0;
        let mut copied = 0;
        let mut component_start = 0;
        let mut seeded = false;
        for (at, (delta, seed)) in coverage.into_iter().enumerate() {
            if depth == 0 && delta > 0 {
                component_start = at;
                seeded = false;
            }
            depth += delta;
            seeded |= seed;
            if depth == 0 && seeded {
                let out = out.get_or_insert_with(|| String::with_capacity(text.len()));
                out.push_str(&text[copied..component_start]);
                out.push_str(HIDDEN);
                copied = at;
                seeded = false;
            }
        }
        match out {
            Some(mut out) => {
                out.push_str(&text[copied..]);
                Cow::Owned(out)
            }
            None => Cow::Borrowed(text),
        }
    }
}

// Only shipped asset positions lose their own origin. Query/credentials and all other
// URLs still go through the generic redactor; the filename is not a privacy exemption.
static PACKAGED_ASSET_POSITION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\b(?:https?://tauri\.localhost|tauri://localhost)/(assets/[A-Za-z0-9_-]+\.js:\d+:\d+)([)\s]|$)"#,
    )
    .unwrap()
});
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b[a-z][a-z0-9+.-]*://[^\s<>"']*"#).unwrap());
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?ix)
        (["']?\b(?:
            (?:[a-z0-9]+[_-])*
            (?:password|passwd|pass|pwd|token|secret|key|uuid|pbk|sid|auth|authorization|login|username)
            | (?-i:[A-Za-z][A-Za-z0-9]*(?:Password|Passwd|Token|Secret|Key|Uuid|UUID|Auth|Authorization|Login|Username))
        )["']?\s*[=:]\s*)
        (?:"(?:\\.|[^"\\])*"|'[^']*'|[^\s,;&}\]]+)
        "#,
    )
    .unwrap()
});
static BEARER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bbearer\s+\S+").unwrap());
static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)")
        .unwrap()
});
static BODY_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?im)\b(?:(?:raw[_ ]?)?(?:subscription[_ ]?|response[_ ]?)?body)\s*[:=]\s*[^\r\n]*",
    )
    .unwrap()
});
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+").unwrap());
static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[0-9a-f]{8}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{12}\b").unwrap()
});
static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\w:.\[\]%]+").unwrap());
static DOMAIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+([a-z]{2,63})\b").unwrap()
});
static OPAQUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/_=-]{24,}").unwrap());
static USER_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:[a-z]:[\\/]|\\\\[^\\\s]+\\|/(?:home|users|root|tmp|var|etc|opt|mnt|media|run)/)[^\s"'<>]*"#).unwrap()
});
const FILE_EXTENSIONS: [&str; 19] = [
    "json", "rs", "ts", "tsx", "js", "mjs", "exe", "dll", "md", "toml", "yaml", "yml", "txt",
    "log", "mmdb", "html", "css", "png", "zst",
];
// Fixed Xray/parser path components, not provider-selected hosts.
const SCHEMA_FIELDS: &[&str] = &[
    "outbounds",
    "settings",
    "vnext",
    "servers",
    "users",
    "id",
    "password",
    "method",
    "address",
    "port",
    "streamSettings",
    "realitySettings",
    "tlsSettings",
    "publicKey",
    "shortId",
    "flow",
    "network",
    "security",
    "subscription",
    "rows",
    "origin",
    "file",
    "line",
    "first_attempt",
    "failure_code",
    "cause",
    "via",
    "selection",
    "kind",
    "checked",
    "reachable",
    "working",
    "rejected_by",
    "accepted_parameters",
    "rejected_parameters",
    "rejected_transports",
    "ignored_parameters",
    "fm",
    "mode",
    "extra",
    "total",
    "accepted",
    "rejected",
    "body_kind",
    "body_size",
    "content_type",
    "redirects",
    "filtered",
];

fn bounded(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Clean a short host/frontend message, bounded to [`MAX_MESSAGE_BYTES`].
pub fn clean(text: &str, secrets: &Secrets) -> String {
    clean_bounded(text, secrets, MAX_MESSAGE_BYTES)
}

/// Apply privacy rules before truncation. Raw JSON objects and arrays are discarded
/// rather than persisting a possible subscription body.
fn clean_bounded(text: &str, secrets: &Secrets, max_bytes: usize) -> String {
    if secrets.overflow {
        return HIDDEN.to_owned();
    }
    let Some(matcher) = secrets.matcher() else {
        // A matcher that cannot represent the complete credential set fails closed.
        return HIDDEN.to_owned();
    };
    let input = matcher.input_prefix(text);
    let out = PRIVATE_KEY.replace_all(input, "[private key]");
    let out = BODY_LINE.replace_all(&out, "body: [body]");
    let out = matcher.replace(&out);
    let out = PACKAGED_ASSET_POSITION.replace_all(&out, "${1}${2}");
    let out = URL.replace_all(&out, "[url]");
    let out = USER_PATH.replace_all(&out, "[path]");
    let out = EMAIL.replace_all(&out, "[email]");
    // i18n-ignore: HTTP authentication scheme plus a redaction marker, not human prose
    let out = BEARER.replace_all(&out, "Bearer [redacted]");
    let out = ASSIGNMENT.replace_all(&out, "${1}[redacted]");
    let out = UUID.replace_all(&out, "[uuid]");
    let out = TOKEN.replace_all(&out, |captures: &Captures| ip_token(&captures[0]));
    let out = DOMAIN.replace_all(&out, |captures: &Captures| {
        if FILE_EXTENSIONS.contains(&captures[1].to_ascii_lowercase().as_str())
            || captures[0]
                .split('.')
                .all(|part| SCHEMA_FIELDS.contains(&part))
        {
            captures[0].to_owned()
        } else {
            "[host]".to_owned()
        }
    });
    let out = OPAQUE.replace_all(&out, |captures: &Captures| {
        let token = &captures[0];
        if token.bytes().any(|byte| byte.is_ascii_digit())
            || (token.bytes().any(|byte| byte.is_ascii_uppercase())
                && token.bytes().any(|byte| byte.is_ascii_lowercase()))
        {
            HIDDEN.to_owned()
        } else {
            token.to_owned()
        }
    });
    let out = redact_structured(&out);
    let invalid_control = |c: char| c.is_control() && !matches!(c, '\n' | '\t');
    let prefix = bounded(&out, max_bytes);
    if !prefix.chars().any(invalid_control) {
        return prefix.to_owned();
    }
    let mut cleaned = String::with_capacity(max_bytes.min(out.len()));
    for c in out.chars().filter(|c| !invalid_control(*c)) {
        if cleaned.len() + c.len_utf8() > max_bytes {
            break;
        }
        cleaned.push(c);
    }
    cleaned
}

/// Clean a short human-readable journal message.
pub fn clean_message(message: &str, secrets: &Secrets) -> String {
    clean(message, secrets)
}

/// Clean optional details, omitting an empty result.
pub fn clean_details(details: Option<&str>, secrets: &Secrets) -> Option<String> {
    details
        .map(|details| clean_bounded(details, secrets, MAX_DETAIL_BYTES))
        .filter(|details| !details.trim().is_empty())
}

/// Clean untrusted string leaves without changing numeric/boolean observations or keys.
/// Do not apply this to a serialized typed report: generated fingerprints resemble credentials,
/// and fixed enum labels may coincide with known passwords. Clean free text before assembly.
pub fn clean_value(value: &mut Value, secrets: &Secrets) {
    match value {
        Value::String(text) => *text = clean_bounded(text, secrets, MAX_DETAIL_BYTES),
        Value::Array(values) => {
            for value in values {
                clean_value(value, secrets);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                clean_value(value, secrets);
            }
        }
        _ => {}
    }
}

fn ip_token(token: &str) -> String {
    let core = token.trim_end_matches(['.', ':']);
    let bare = core.trim_start_matches('[').trim_end_matches(']');
    let is_ip = core.parse::<SocketAddr>().is_ok()
        || bare.parse::<IpAddr>().is_ok()
        || bare
            .split('%')
            .next()
            .is_some_and(|part| part.parse::<IpAddr>().is_ok());
    if is_ip {
        format!("[ip]{}", &token[core.len()..])
    } else {
        token.to_owned()
    }
}

fn redact_structured(text: &str) -> Cow<'_, str> {
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, character)) = chars.next() {
        let path_index = text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | ']'));
        let structured = character == '{'
            || (character == '['
                && !path_index
                && text[at + character.len_utf8()..]
                    .trim_start()
                    .chars()
                    .next()
                    .is_some_and(|c| matches!(c, '{' | '"' | '0'..='9' | '-')));
        if !structured {
            continue;
        }
        let out = out.get_or_insert_with(|| String::with_capacity(text.len()));
        out.push_str(&text[copied..at]);
        out.push_str("[body]");
        let mut end = at + character.len_utf8();
        let mut depth = 1usize;
        let mut quoted = false;
        let mut escaped = false;
        for (index, c) in chars.by_ref() {
            end = index + c.len_utf8();
            if quoted {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    quoted = false;
                }
            } else {
                match c {
                    '"' => quoted = true,
                    '{' | '[' => depth += 1,
                    '}' | ']' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        copied = end;
    }
    match out {
        Some(mut out) => {
            out.push_str(&text[copied..]);
            Cow::Owned(out)
        }
        None => Cow::Borrowed(text),
    }
}

/// Append a startup failure before an Application exists. The caller owns the
/// install-local directory; never call concurrently with a live Application writer.
pub fn record_external(directory: &Path, message: &str, detail: Option<&str>) -> Result<()> {
    let store = persistence::load(&directory.join("state.json")).unwrap_or_default();
    let secrets = Secrets::from_store(&store, directory);
    let path = directory.join("journal.json");
    let mut entries = match std::fs::File::open(&path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
            if bytes.len() > MAX_BYTES {
                Vec::new()
            } else {
                serde_json::from_slice::<Vec<Entry>>(&bytes).unwrap_or_default()
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    for entry in &mut entries {
        entry.message = clean_message(&entry.message, &secrets);
        entry.details = clean_details(entry.details.as_deref(), &secrets);
    }
    let now = crate::application::timestamp();
    entries.push(Entry {
        at: now,
        level: "error".to_owned(),
        message: clean_message(message, &secrets),
        details: clean_details(detail, &secrets),
        crash_id: None,
        failure_code: None,
        category: Category::App,
    });
    trim(&mut entries, now);
    persistence::atomic_write(&path, &serde_json::to_vec(&entries)?)
}

/// Persist one already-cleaned panic entry without taking Application locks or replacing
/// journal.json. Unique files prevent concurrent/reentrant hooks from overwriting each other.
pub(crate) fn record_crash(directory: &Path, entry: &Entry) -> Result<()> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = format!("{nanos:039}-{}.json", uuid::Uuid::new_v4().simple());
    let bytes = serde_json::to_vec(entry)?;
    if bytes.len() > MAX_CRASH_BYTES {
        // i18n-ignore: internal size invariant, never exposed by the panic hook
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "crash entry exceeds local limit",
        )
        .into());
    }
    persistence::atomic_write(&directory.join(CRASH_DIRECTORY).join(name), &bytes)?;
    crash_paths(directory)?;
    Ok(())
}

pub(crate) fn has_pending_crashes(directory: &Path) -> Result<bool> {
    Ok(!crash_paths(directory)?.is_empty())
}

/// Called by the live journal writer after recovery, while it owns the journal gate.
pub(crate) fn clear(directory: &Path) -> Result<()> {
    // Clearing the persisted IDs while acknowledgement is pending would replay a cleared panic.
    // A new concurrent panic is also left recoverable instead of being deleted without a commit.
    if has_pending_crashes(directory)? {
        return Err(crate::Error::failure(
            crate::failure::FailureCode::Persistence,
            crate::text::t("core.application.journal_pending_crashes"),
        ));
    }
    persistence::atomic_write(&directory.join("journal.json"), b"[]")?;
    Ok(())
}

/// Retain only the newest bounded set; filenames contain no user-controlled data.
/// The working selection never exceeds MAX_CRASH_RECORDS + 1 paths.
fn crash_paths(directory: &Path) -> Result<Vec<PathBuf>> {
    let listing = match std::fs::read_dir(directory.join(CRASH_DIRECTORY)) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for file in listing {
        let file = file?;
        let name = file.file_name();
        let valid = name.to_str().is_some_and(|name| {
            name.strip_suffix(".json")
                .and_then(|name| name.split_once('-'))
                .is_some_and(|(time, id)| {
                    time.len() == 39
                        && time.bytes().all(|byte| byte.is_ascii_digit())
                        && id.len() == 32
                        && id.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        });
        if !valid {
            continue;
        }
        match file.file_type() {
            Ok(kind) if kind.is_file() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        }
        paths.push(file.path());
        paths.sort_unstable();
        if paths.len() > MAX_CRASH_RECORDS {
            match std::fs::remove_file(paths.remove(0)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(paths)
}

/// Called only by the live journal's serialized writer (or before it exists).
/// Publish journal.json atomically before acknowledging crash files. A failed write leaves
/// them recoverable. Generated crash identities make acknowledgement retries idempotent,
/// without coalescing distinct panics with identical text in the same second.
pub(crate) fn persist_with_crashes(
    directory: &Path,
    entries: &mut Vec<Entry>,
    secrets: &Secrets,
) -> Result<()> {
    let paths = crash_paths(directory)?;
    for path in &paths {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take((MAX_CRASH_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_CRASH_BYTES {
            continue;
        }
        if let Ok(mut entry) = serde_json::from_slice::<Entry>(&bytes) {
            if entry.crash_id.is_none() {
                continue;
            }
            entry.message = clean_message(&entry.message, secrets);
            entry.details = clean_details(entry.details.as_deref(), secrets);
            if !entries
                .iter()
                .any(|existing| existing.crash_id == entry.crash_id)
            {
                entries.push(entry);
            }
        }
    }
    if !paths.is_empty() {
        entries.sort_by_key(|entry| entry.at);
    }
    trim(entries, crate::application::timestamp());
    persistence::atomic_write(
        &directory.join("journal.json"),
        &serde_json::to_vec(entries)?,
    )?;
    for path in paths {
        // A deletion failure cannot undo the durable journal. A later pass deduplicates it.
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// Counts the bytes written; keeps none.
struct Count(usize);
impl io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Bytes of one entry in the file.
fn entry_bytes(entry: &Entry) -> usize {
    let mut count = Count(0);
    serde_json::to_writer(&mut count, entry).map_or(0, |()| count.0)
}

/// Cuts the journal down to the limits at moment `now` (Unix seconds).
///
/// An entry older than a day is dropped wherever it stands; an entry exactly a day old stays,
/// and so does one stamped after `now` (the clock went back). Then the oldest entries go until
/// there are at most [`MAX_ENTRIES`] and the file `[a,b,…]` is at most [`MAX_BYTES`]. The newest
/// entry always stays: a single entry over the size limit is not worth losing the last event for.
pub fn trim(journal: &mut Vec<Entry>, now: u64) {
    trim_to(journal, now, MAX_ENTRIES, MAX_BYTES);
}
/// [`trim`] with its limits given. An entry takes at least 48 bytes of the file, so 5000 of them
/// are over 240 KB: the size limit always binds first and the count limit is only reachable here.
fn trim_to(journal: &mut Vec<Entry>, now: u64, max_entries: usize, max_bytes: usize) {
    journal.retain(|entry| entry.at.saturating_add(MAX_AGE_SECS) >= now);
    let excess = journal.len().saturating_sub(max_entries);
    journal.drain(..excess);
    // The file is `[` + the entries + a comma between each two + `]`.
    let sizes: Vec<usize> = journal.iter().map(entry_bytes).collect();
    let mut total = 2 + sizes.iter().sum::<usize>() + sizes.len().saturating_sub(1);
    let mut cut = 0;
    while total > max_bytes && cut + 1 < sizes.len() {
        total -= sizes[cut] + 1;
        cut += 1;
    }
    journal.drain(..cut);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::Category;

    fn entry(at: u64, message: &str) -> Entry {
        Entry {
            at,
            level: "info".into(),
            message: message.into(),
            details: None,
            crash_id: None,
            failure_code: None,
            category: Category::App,
        }
    }
    fn file_bytes(journal: &[Entry]) -> usize {
        serde_json::to_vec(journal).unwrap().len()
    }

    #[test]
    fn counts_the_bytes_of_the_file_exactly() {
        let journal = vec![entry(1, "a"), entry(2, &"b".repeat(300)), entry(3, "")];
        let sizes: usize = journal.iter().map(entry_bytes).sum();
        assert_eq!(file_bytes(&journal), 2 + sizes + 2);
        assert_eq!(file_bytes(&[]), 2);
    }

    #[test]
    fn the_size_limit_is_inclusive() {
        // Two entries whose file is exactly the limit both stay; one byte more and the older goes.
        let base = entry_bytes(&entry(1, ""));
        let room = MAX_BYTES - 4 - 2 * base;
        let mut journal = vec![entry(1, "a"), entry(2, &"x".repeat(room))];
        assert_eq!(file_bytes(&journal), MAX_BYTES);
        trim(&mut journal, 3);
        assert_eq!(journal.len(), 2);

        journal[1].message.push('x');
        assert_eq!(file_bytes(&journal), MAX_BYTES + 1);
        trim(&mut journal, 3);
        assert_eq!(journal.len(), 1);
        assert_eq!(journal[0].at, 2);
        assert!(file_bytes(&journal) <= MAX_BYTES);
    }

    #[test]
    fn the_entry_limit_is_inclusive() {
        let mut journal: Vec<Entry> = (0..5u64).map(|at| entry(at, "")).collect();
        trim_to(&mut journal, 5, 5, usize::MAX);
        assert_eq!(journal.len(), 5);
        journal.push(entry(5, ""));
        trim_to(&mut journal, 5, 5, usize::MAX);
        assert_eq!(journal.len(), 5);
        assert_eq!(journal[0].at, 1);
    }

    #[test]
    fn the_size_limit_binds_long_before_the_entry_limit() {
        let mut journal: Vec<Entry> = (0..=MAX_ENTRIES as u64).map(|at| entry(at, "")).collect();
        trim(&mut journal, MAX_ENTRIES as u64);
        assert!(journal.len() < MAX_ENTRIES / 2, "{}", journal.len());
        assert!(file_bytes(&journal) <= MAX_BYTES);
        assert_eq!(journal.last().unwrap().at, MAX_ENTRIES as u64);
    }

    #[test]
    fn an_entry_exactly_a_day_old_stays_and_one_second_older_goes() {
        let now = 10 * MAX_AGE_SECS;
        let mut journal = vec![
            entry(now - MAX_AGE_SECS - 1, "gone"),
            entry(now - MAX_AGE_SECS, "stays"),
            entry(now, "now"),
        ];
        trim(&mut journal, now);
        let kept: Vec<&str> = journal.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(kept, ["stays", "now"]);
    }

    #[test]
    fn an_entry_from_the_future_stays() {
        let mut journal = vec![entry(1_000, "later than the clock")];
        trim(&mut journal, 10);
        assert_eq!(journal.len(), 1);
    }

    #[test]
    fn the_newest_entry_stays_even_when_it_alone_is_too_big() {
        let mut journal = vec![entry(1, "old"), entry(2, &"x".repeat(MAX_BYTES * 2))];
        trim(&mut journal, 3);
        assert_eq!(journal.len(), 1);
        assert_eq!(journal[0].at, 2);
    }

    #[test]
    fn an_empty_journal_stays_empty() {
        let mut journal = Vec::new();
        trim(&mut journal, 0);
        assert!(journal.is_empty());
    }

    #[test]
    fn a_clock_at_the_start_of_time_does_not_overflow() {
        let mut journal = vec![entry(u64::MAX, "x"), entry(0, "y")];
        trim(&mut journal, 0);
        assert_eq!(journal.len(), 2);
    }

    fn no_secrets() -> Secrets {
        Secrets {
            values: Vec::new(),
            bytes: 0,
            overflow: false,
            matcher: OnceLock::new(),
        }
    }

    #[test]
    fn details_keep_stage_cause_and_parser_path() {
        let raw = "stage: parse\nfield_path: outbounds[0].settings.vnext[0].users[0].id\nJSON at line 4 column 12: missing field";
        let cleaned = clean(raw, &no_secrets());
        assert!(cleaned.contains("stage: parse"));
        assert!(cleaned.contains("outbounds[0].settings.vnext[0].users[0].id"));
        assert!(cleaned.contains("line 4 column 12"));
    }

    #[test]
    fn structured_bodies_do_not_consume_following_diagnostic_context() {
        let object = serde_json::json!({
            "value": "brace } and quote \"",
            "nested": [{"payload": "firstpayload"}]
        });
        let raw = format!(
            "stage=parse {object} cause=invalid [{{\"payload\":\"secondpayload\"}}] line=4"
        );
        let cleaned = clean(&raw, &no_secrets());
        for context in ["stage=parse", "cause=invalid", "line=4"] {
            assert!(cleaned.contains(context), "{cleaned}");
        }
        for payload in ["firstpayload", "secondpayload"] {
            assert!(!cleaned.contains(payload), "{cleaned}");
        }
    }

    #[test]
    fn controls_are_removed_without_losing_multiline_context() {
        let cleaned = clean("stage=parse\0\r\n\tline=4", &no_secrets());
        assert!(cleaned.contains("stage=parse\n\tline=4"), "{cleaned:?}");
        assert!(!cleaned.contains(['\0', '\r']));
    }

    #[test]
    fn details_remove_addresses_quoted_credentials_and_bodies() {
        let raw = r#"https://provider.invalid/feed?token=abc vless://id@host:443
password="two words" signing_key='secret-value' Authorization: Bearer token
C:\Users\runneradmin\AppData\Rift /home/runner/rift
body: {"outbounds":[{"password":"raw-body-secret"}]}
peer 203.0.113.5:443"#;
        let cleaned = clean(raw, &no_secrets());
        for secret in [
            "provider",
            "id@host",
            "two words",
            "secret-value",
            "Bearer token",
            "runneradmin",
            "runner",
            "raw-body-secret",
            "203.0.113.5",
        ] {
            assert!(!cleaned.contains(secret), "{secret}: {cleaned}");
        }
        assert!(cleaned.contains("[body]"));
    }

    #[test]
    fn compound_credential_assignments_are_cleaned_without_store_knowledge() {
        for key in [
            "access_token",
            "client_secret",
            "api_key",
            "oauth_refresh_token",
            "refresh-token",
            "accessToken",
            "clientSecret",
            "apiKey",
        ] {
            let text = format!("stage=fetch {key}=\"abc123\" cause=refused");
            let cleaned = clean(&text, &no_secrets());
            assert!(!cleaned.contains("abc123"), "{key}: {cleaned}");
            assert!(cleaned.contains("stage=fetch"), "{cleaned}");
            assert!(cleaned.contains("cause=refused"), "{cleaned}");
        }
    }

    #[test]
    fn short_known_credentials_are_token_bounded() {
        let mut secrets = no_secrets();
        secrets.extend(["x".into(), "ws".into(), "key".into()]);
        let cleaned = clean("x ws key proxy websocket monkey field_path", &secrets);
        assert_eq!(
            cleaned,
            "[redacted] [redacted] [redacted] proxy websocket monkey field_path"
        );
    }

    #[test]
    fn short_credentials_use_unicode_and_punctuation_neighbor_boundaries() {
        let mut secrets = no_secrets();
        secrets.extend(["é", "密", "!+", "a-b", "é中🙂я"].map(str::to_owned));
        for (raw, expected) in [
            ("(é)", "([redacted])"),
            ("éx xé _é é_ 中é é中", "éx xé _é é_ 中é é中"),
            ("(密)", "([redacted])"),
            ("密a a密", "密a a密"),
            ("(!+)", "([redacted])"),
            ("a!+ !+a _!+ !+_", "a!+ !+a _!+ !+_"),
            ("(a-b)", "([redacted])"),
            ("xa-b a-bx a-b_", "xa-b a-bx a-b_"),
            ("(é中🙂я)", "([redacted])"),
            ("xé中🙂я é中🙂яx", "xé中🙂я é中🙂яx"),
        ] {
            assert_eq!(clean(raw, &secrets), expected, "{raw}");
        }
        let mut longer = no_secrets();
        longer.extend(["é中🙂яa".into()]);
        assert_eq!(clean("xé中🙂яay", &longer), "x[redacted]y");
    }

    #[test]
    fn short_credentials_adjacent_to_redaction_cannot_become_newly_visible() {
        let mut secrets = no_secrets();
        secrets.extend(["-a", "-я", "private-value"].map(str::to_owned));
        for raw in ["-a-a", "-я-я", "private-value-a", "private-value-я"] {
            assert_eq!(clean(raw, &secrets), HIDDEN, "{raw}");
        }
        for raw in ["я-a-a", "x-я-я", "_-a-a", "-aя", "-я_"] {
            assert_eq!(clean(raw, &secrets), raw, "{raw}");
        }
        // Stricter than sequential replacement: a seeded redaction absorbs
        // every connected short match, including chains with blocked neighbors.
        let mut chain = no_secrets();
        chain.extend(["private-value", "я"].map(str::to_owned));
        assert_eq!(clean("private-valueяя", &chain), HIDDEN);
    }

    #[test]
    fn overlapping_credentials_hide_the_union_instead_of_leaving_suffixes() {
        let mut secrets = no_secrets();
        secrets.extend(["abcde", "defghi", "cdefg", "fghi"].map(str::to_owned));
        assert_eq!(
            clean("stage=parse abcdefghi cause=refused", &secrets),
            "stage=parse [redacted] cause=refused"
        );
    }

    #[test]
    fn dense_overlaps_have_one_cached_summary_per_endpoint() {
        let mut secrets = no_secrets();
        secrets.extend((5..=256).map(|len| "a".repeat(len)));
        let raw = "a".repeat(MAX_INPUT_BYTES);
        let matcher = secrets.matcher().unwrap();
        assert_eq!(matcher.matches(&raw).count(), raw.len() - 4);
        assert_eq!(matcher.replace(&raw), HIDDEN);
        assert_eq!(clean(&raw, &secrets), HIDDEN);
    }

    #[test]
    fn maximum_secret_count_absent_input_reuses_matcher_without_copying() {
        let mut secrets = no_secrets();
        secrets.extend((0..MAX_SECRET_VALUES).map(|index| format!("credential-{index:05}-absent")));
        assert_eq!(secrets.values.len(), MAX_SECRET_VALUES);
        assert!(!secrets.overflow);
        let raw = format!("{}context", "context ".repeat(MAX_INPUT_BYTES / 8));
        let input = &raw[..MAX_INPUT_BYTES];
        let matcher = secrets.matcher().unwrap();
        assert_eq!(matcher.input_prefix(&raw), input);
        assert!(matches!(matcher.replace(input), Cow::Borrowed(_)));
        let cached = Arc::clone(secrets.matcher.get().unwrap().as_ref().unwrap());
        for _ in 0..3 {
            assert_eq!(
                clean_details(Some(&raw), &secrets).unwrap(),
                raw[..MAX_DETAIL_BYTES]
            );
            assert!(Arc::ptr_eq(
                &cached,
                secrets.matcher.get().unwrap().as_ref().unwrap()
            ));
        }
        let last = format!("credential-{:05}-absent", MAX_SECRET_VALUES - 1);
        assert_eq!(clean(&last, &secrets), HIDDEN);
        secrets.extend(["one-more-credential".into()]);
        assert!(secrets.overflow);
        assert_eq!(clean("possibly sensitive text", &secrets), HIDDEN);
    }

    #[test]
    fn credential_changes_invalidate_only_the_changed_matcher_cache() {
        let mut secrets = no_secrets();
        secrets.extend(["first-private-value".into()]);
        assert!(secrets.matcher.get().is_none());
        assert_eq!(clean("first-private-value", &secrets), HIDDEN);
        let cached = Arc::clone(secrets.matcher.get().unwrap().as_ref().unwrap());
        let unchanged = secrets.clone();
        secrets.extend(["first-private-value".into(), String::new()]);
        assert!(Arc::ptr_eq(
            &cached,
            secrets.matcher.get().unwrap().as_ref().unwrap()
        ));
        secrets.extend(["second-private-value".into()]);
        assert!(secrets.matcher.get().is_none());
        assert_eq!(
            clean("first-private-value second-private-value", &secrets),
            "[redacted] [redacted]"
        );
        assert!(!Arc::ptr_eq(
            &cached,
            secrets.matcher.get().unwrap().as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            &cached,
            unchanged.matcher.get().unwrap().as_ref().unwrap()
        ));
        assert_eq!(
            clean("second-private-value", &unchanged),
            "second-private-value"
        );
    }

    #[test]
    fn cutoff_hides_short_crossing_credentials_even_without_word_boundaries() {
        let mut secrets = no_secrets();
        secrets.extend(["я!".into()]);
        let raw = format!("{}xя!", "q".repeat(MAX_INPUT_BYTES - 3));
        let prefix = secrets.matcher().unwrap().input_prefix(&raw);
        assert_eq!(prefix.len(), MAX_INPUT_BYTES - 2);
        assert!(prefix.ends_with('x'));
        assert!(!prefix.contains('я'));
    }

    #[test]
    fn overlapping_credentials_at_the_work_limit_hide_the_earliest_prefix() {
        let raw = format!(
            "{}{}{}",
            "z ".repeat(50),
            "q ".repeat(50),
            "m ".repeat(33_000)
        );
        let mut secrets = no_secrets();
        secrets.extend([raw[100..65_600].into(), raw[200..65_710].into()]);
        let cleaned = clean_details(Some(&raw), &secrets).unwrap();
        assert!(
            !cleaned.contains('q'),
            "a credential prefix survived: {cleaned}"
        );
        assert!(cleaned.contains("z "), "unrelated context was lost");
    }

    #[test]
    fn known_credentials_and_unicode_crossing_the_work_boundary_are_safe() {
        let mut secrets = no_secrets();
        secrets.extend(["秘密credential".into()]);
        let raw = format!("{}秘密credential", "я".repeat(MAX_INPUT_BYTES / 2 - 1));
        let cleaned = clean(&raw, &secrets);
        assert!(cleaned.len() <= MAX_DETAIL_BYTES);
        assert!(!cleaned.contains("秘密"));
        assert!(
            clean_details(Some(&"я".repeat(MAX_DETAIL_BYTES)), &no_secrets())
                .unwrap()
                .len()
                <= MAX_DETAIL_BYTES
        );
        assert!(
            clean_message(&"я".repeat(MAX_MESSAGE_BYTES), &no_secrets()).len() <= MAX_MESSAGE_BYTES
        );
    }

    #[test]
    fn export_cleaning_preserves_observations_and_detail_limit() {
        let mut secrets = no_secrets();
        secrets.extend(["export-secret".into()]);
        let mut value = serde_json::json!({
            "http_status": 403, "previous_servers_preserved": true,
            "details": format!("{} export-secret", "a ".repeat(2000))
        });
        clean_value(&mut value, &secrets);
        assert_eq!(value["http_status"], 403);
        assert_eq!(value["previous_servers_preserved"], true);
        let details = value["details"].as_str().unwrap();
        assert!(details.len() > MAX_MESSAGE_BYTES);
        assert!(!details.contains("export-secret"));
    }

    #[test]
    fn free_text_never_exempts_strings_that_resemble_export_identifiers() {
        let fingerprint = "0123456789abcdef".repeat(4);
        let credential = uuid::Uuid::new_v4().to_string();
        let mut value = serde_json::json!({
            "id": fingerprint,
            "details": format!("uuid: {credential} opaque credential: {fingerprint}")
        });
        clean_value(&mut value, &no_secrets());
        let text = value.to_string();
        assert!(!text.contains(&fingerprint));
        assert!(!text.contains(&credential));
    }

    #[test]
    fn secret_capacity_overflow_fails_closed() {
        let mut secrets = no_secrets();
        secrets.extend(["s".repeat(MAX_INPUT_BYTES + 1)]);
        assert_eq!(clean("potentially sensitive text", &secrets), HIDDEN);
    }

    #[test]
    fn startup_append_cleans_and_preserves_existing_entries() {
        let directory = tempfile::tempdir().unwrap();
        let previous = vec![entry(crate::application::timestamp(), "previous")];
        persistence::atomic_write(
            &directory.path().join("journal.json"),
            &serde_json::to_vec(&previous).unwrap(),
        )
        .unwrap();
        record_external(
            directory.path(),
            "startup failed",
            Some("Authorization: Bearer startup-secret"),
        )
        .unwrap();
        let bytes = std::fs::read(directory.path().join("journal.json")).unwrap();
        let entries: Vec<Entry> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "previous");
        assert!(!entries[1]
            .details
            .as_deref()
            .unwrap()
            .contains("startup-secret"));
        assert!(bytes.len() <= MAX_BYTES);
    }

    #[test]
    fn crash_acknowledgement_retries_deduplicate_ids_not_equal_event_text() {
        let directory = tempfile::tempdir().unwrap();
        let mut first = entry(crate::application::timestamp(), "identical panic");
        first.crash_id = Some(uuid::Uuid::new_v4());
        let mut second = first.clone();
        second.crash_id = Some(uuid::Uuid::new_v4());
        record_crash(directory.path(), &first).unwrap();
        record_crash(directory.path(), &second).unwrap();
        let pending: Vec<_> = crash_paths(directory.path())
            .unwrap()
            .into_iter()
            .map(|path| {
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        let mut entries = Vec::new();
        persist_with_crashes(directory.path(), &mut entries, &no_secrets()).unwrap();
        assert_eq!(entries.len(), 2);
        // The persisted identity also survives a restart between commit and acknowledgement.
        entries =
            serde_json::from_slice(&std::fs::read(directory.path().join("journal.json")).unwrap())
                .unwrap();
        // Simulate failed acknowledgement after the unified journal was committed.
        for (path, bytes) in pending {
            persistence::atomic_write(&path, &bytes).unwrap();
        }
        let mut third = first;
        third.crash_id = Some(uuid::Uuid::new_v4());
        record_crash(directory.path(), &third).unwrap();
        persist_with_crashes(directory.path(), &mut entries, &no_secrets()).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().any(|entry| entry.crash_id == third.crash_id));
        assert!(!has_pending_crashes(directory.path()).unwrap());
    }

    #[test]
    fn clearing_cannot_discard_deduplication_for_unacknowledged_crashes() {
        let directory = tempfile::tempdir().unwrap();
        let mut panic = entry(crate::application::timestamp(), "pending panic");
        panic.crash_id = Some(uuid::Uuid::new_v4());
        record_crash(directory.path(), &panic).unwrap();
        let mut entries = Vec::new();
        persist_with_crashes(directory.path(), &mut entries, &no_secrets()).unwrap();
        let saved = std::fs::read(directory.path().join("journal.json")).unwrap();
        // The crash has been committed but its acknowledgement was not durable.
        record_crash(directory.path(), &panic).unwrap();
        assert!(clear(directory.path()).is_err());
        assert_eq!(
            std::fs::read(directory.path().join("journal.json")).unwrap(),
            saved
        );
        let mut journal: Vec<Entry> =
            serde_json::from_slice(&std::fs::read(directory.path().join("journal.json")).unwrap())
                .unwrap();
        journal.push(entry(crate::application::timestamp(), "next event"));
        persist_with_crashes(directory.path(), &mut journal, &no_secrets()).unwrap();
        assert_eq!(
            journal
                .iter()
                .filter(|entry| entry.crash_id == panic.crash_id)
                .count(),
            1
        );
        clear(directory.path()).unwrap();
        let mut next = vec![entry(crate::application::timestamp(), "after clear")];
        persist_with_crashes(directory.path(), &mut next, &no_secrets()).unwrap();
        let journal: Vec<Entry> =
            serde_json::from_slice(&std::fs::read(directory.path().join("journal.json")).unwrap())
                .unwrap();
        assert!(journal.iter().all(|entry| entry.crash_id.is_none()));
        assert!(journal.iter().any(|entry| entry.message == "after clear"));
    }

    #[test]
    fn crash_spool_is_bounded_and_old_records_expire_in_the_unified_journal() {
        let directory = tempfile::tempdir().unwrap();
        for n in 0..MAX_CRASH_RECORDS + 4 {
            let mut event = entry(crate::application::timestamp(), &format!("event {n}"));
            event.crash_id = Some(uuid::Uuid::new_v4());
            record_crash(directory.path(), &event).unwrap();
        }
        let paths = crash_paths(directory.path()).unwrap();
        assert_eq!(paths.len(), MAX_CRASH_RECORDS);
        assert!(paths
            .iter()
            .all(|path| std::fs::metadata(path).unwrap().len() <= MAX_CRASH_BYTES as u64));
        let mut entries = Vec::new();
        persist_with_crashes(directory.path(), &mut entries, &no_secrets()).unwrap();
        assert_eq!(entries.len(), MAX_CRASH_RECORDS);
        assert!(serde_json::to_vec(&entries).unwrap().len() <= MAX_BYTES);
        let mut old = entry(
            crate::application::timestamp() - MAX_AGE_SECS - 1,
            "expired panic",
        );
        old.crash_id = Some(uuid::Uuid::new_v4());
        record_crash(directory.path(), &old).unwrap();
        persist_with_crashes(directory.path(), &mut entries, &no_secrets()).unwrap();
        assert!(!entries.iter().any(|entry| entry.crash_id == old.crash_id));
        assert!(!has_pending_crashes(directory.path()).unwrap());
    }
}
