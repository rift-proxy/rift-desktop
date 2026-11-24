//! What the user hands to Rift as text: a pasted clipboard, the content of a QR code or a
//! `rift://` link from a browser. All of it is untrusted; nothing is added from it before the
//! user has seen a preview and confirmed.
//!
//! The text may hold prose around the link, several links, or a `rift://add?url=…` /
//! `rift://add?uri=…` wrapper. [`classify`] finds what to import; [`preview_input`] checks it
//! the way the real import does.
use crate::text::t;
use crate::{model::ServerView, parser::parse_uri, subscription::validate_url, Error, Result};
use regex::Regex;
use serde::Serialize;
use std::{collections::HashSet, sync::LazyLock};

/// Most server links taken from one text.
pub const MAX_LINKS: usize = 100;
/// Longest text looked at; a paste can be anything the clipboard holds.
pub const MAX_INPUT_BYTES: usize = 256 * 1024;
/// Longest value of a `rift://` parameter.
const MAX_PARAMETER_BYTES: usize = 8192;

/// What the text asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Input {
    /// One subscription: its `https://` link.
    Subscription(String),
    /// One or more servers: their links, in the order they were found, without repeats.
    Servers(Vec<String>),
}

/// Safe to show: a subscription link the user pasted, or the servers behind the links with
/// the link of each (to be sent back on confirmation). Never a stored URL.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputPreview {
    Subscription {
        url: String,
    },
    Servers {
        items: Vec<ServerPreview>,
        /// Links in the text that no supported server could be read from.
        rejected: usize,
    },
}
#[derive(Clone, Debug, Serialize)]
pub struct ServerPreview {
    pub uri: String,
    pub server: ServerView,
}

const SERVER_SCHEMES: [&str; 4] = ["vless", "vmess", "trojan", "ss"];

/// A scheme we handle, at the start of the text or after something that is not part of a word,
/// up to the next space, quote or bracket.
static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:^|[^A-Za-z0-9+.\-])((?:vless|vmess|trojan|ss|https|rift)://[^\s<>"'`]+)"#)
        .expect("a constant pattern")
});

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}

/// The link at the start of `token`: lowercase scheme, without sentence punctuation after it.
fn tidy(token: &str) -> String {
    let token = token.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '>']);
    match token.split_once("://") {
        Some((scheme, rest)) => format!("{}://{rest}", scheme.to_ascii_lowercase()),
        None => token.to_owned(),
    }
}

/// The link inside `rift://add?url=<https link>` or `rift://add?uri=<server link>`.
fn unwrap_deep_link(link: &str) -> Result<String> {
    let bad = || invalid(t("core.link.bad_rift_link"));
    let parsed = url::Url::parse(link).map_err(|_| bad())?;
    if parsed.scheme() != "rift"
        || !parsed
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("add"))
        || !matches!(parsed.path(), "" | "/")
        || parsed.username() != ""
        || parsed.password().is_some()
    {
        return Err(bad());
    }
    let mut pairs = parsed.query_pairs();
    let (Some((key, value)), None) = (pairs.next(), pairs.next()) else {
        return Err(bad());
    };
    if value.len() > MAX_PARAMETER_BYTES {
        return Err(bad());
    }
    let value = value.trim().to_owned();
    let scheme = value
        .split_once("://")
        .map(|(scheme, _)| scheme.to_ascii_lowercase())
        .unwrap_or_default();
    match key.as_ref() {
        "url" if scheme == "https" => Ok(value),
        "uri" if SERVER_SCHEMES.contains(&scheme.as_str()) => Ok(value),
        _ => Err(bad()),
    }
}

/// Finds what to import in `text`. Server links win over a web link in the same text (a link
/// may carry `https://` inside its name); without servers the first `https://` link is a
/// subscription.
pub fn classify(text: &str) -> Result<Input> {
    if text.len() > MAX_INPUT_BYTES {
        return Err(invalid(t("core.link.text_too_large")));
    }
    let mut servers = Vec::new();
    let mut seen = HashSet::new();
    let mut subscription = None;
    let mut wrapped_error = None;
    for capture in TOKEN.captures_iter(text) {
        let mut link = tidy(&capture[1]);
        if link.starts_with("rift://") {
            match unwrap_deep_link(&link) {
                Ok(inner) => link = inner,
                Err(error) => {
                    wrapped_error.get_or_insert(error);
                    continue;
                }
            }
        }
        let scheme = link.split_once("://").map_or("", |(scheme, _)| scheme);
        if scheme == "https" {
            subscription.get_or_insert(link);
        } else if seen.insert(link.clone()) {
            servers.push(link);
            if servers.len() > MAX_LINKS {
                return Err(invalid(t("core.link.too_many_links")));
            }
        }
    }
    if !servers.is_empty() {
        return Ok(Input::Servers(servers));
    }
    if let Some(url) = subscription {
        return Ok(Input::Subscription(url));
    }
    Err(wrapped_error.unwrap_or_else(|| invalid(t("core.link.no_link"))))
}

/// [`classify`] and the checks of the real import: a subscription link must be a valid HTTPS
/// link; every server link is read, and the ones that cannot be are counted.
pub fn preview_input(text: &str) -> Result<InputPreview> {
    match classify(text)? {
        Input::Subscription(url) => {
            validate_url(&url)?;
            Ok(InputPreview::Subscription { url })
        }
        Input::Servers(links) => {
            let total = links.len();
            let items: Vec<ServerPreview> = links
                .into_iter()
                .filter_map(|uri| {
                    parse_uri(&uri, None).ok().map(|server| ServerPreview {
                        server: server.summary(),
                        uri,
                    })
                })
                .collect();
            if items.is_empty() {
                return Err(invalid(t("core.link.no_supported_servers")));
            }
            Ok(InputPreview::Servers {
                rejected: total - items.len(),
                items,
            })
        }
    }
}
