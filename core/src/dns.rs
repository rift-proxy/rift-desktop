//! The names sing-box dials itself: servers with a domain address and every site that leaves
//! directly. sing-box asks this module over a loopback TCP port, and the module asks a chain of
//! resolvers, one after another, each with three seconds to answer.
//!
//! Why not the DNS of sing-box: its `local` resolver on Windows sends its own packets to port 53,
//! and a VPN client's "no DNS outside the tunnel" filter drops exactly those (the system DNS
//! client is exempt, so everything else on the machine resolves). A resolver on port 443 passes.
//! sing-box takes one server for the names it dials and skips its DNS rules for them, so the
//! chain cannot live in its config.
//!
//! The chain: Google, Cloudflare, the machine's own resolver, Yandex. A resolver that did not
//! answer is left out for [`COOLDOWN`], so a blocked one costs one question, not every one; when
//! all of them are left out they are all asked again. Only an answer with the code NOERROR or
//! NXDOMAIN ends the chain: a name that does not exist is an answer, a failure is not.
//!
//! Names are never logged or kept: the statistics are counts and the kind of the last miss.
use async_trait::async_trait;
use serde::Serialize;
use std::{
    net::IpAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex as AsyncMutex, Semaphore},
    task::{JoinHandle, JoinSet},
    time::{timeout, Instant},
};

/// How long each resolver of the chain has to answer.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a resolver that did not answer is left out of the questions that follow.
pub const COOLDOWN: Duration = Duration::from_secs(30);
/// Bootstrap destinations of the HTTPS chain; TUN must leave these reachable directly.
pub const DOH_IPS: [&str; 6] = [
    "8.8.8.8",
    "8.8.4.4",
    "1.1.1.1",
    "1.0.0.1",
    "77.88.8.8",
    "77.88.8.1",
];
/// Longest message the bridge reads from sing-box; a question is a few dozen bytes.
const MAX_MESSAGE: usize = 4096;
/// Longest answer taken from a resolver over HTTPS.
const MAX_ANSWER: usize = 65535;
/// Questions in flight at once, over all connections.
const MAX_IN_FLIGHT: usize = 64;
/// Idle local clients must not consume an unbounded number of sockets or tasks.
const MAX_CONNECTIONS: usize = 128;
/// How long a connection of sing-box may be silent before the bridge closes it.
const IDLE: Duration = Duration::from_secs(15);
/// How long an answer of the machine's own resolver may be kept.
const SYSTEM_TTL: u32 = 30;
const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;
const CLASS_IN: u16 = 1;
const RCODE_NOERROR: u8 = 0;
const RCODE_FORMERR: u8 = 1;
const RCODE_SERVFAIL: u8 = 2;
const RCODE_NXDOMAIN: u8 = 3;

/// The one question of a DNS query: the name, as the dotted text, and the type asked for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Question {
    pub id: u16,
    pub name: String,
    pub qtype: u16,
    /// Where the question ends in the message: the end of the header plus the question.
    end: usize,
}

/// Reads the query of a client: a standard query with exactly one question, a name without
/// compression and the class IN. Anything else is not a question this module answers.
pub fn parse_question(message: &[u8]) -> Option<Question> {
    if message.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([message[0], message[1]]);
    let flags = u16::from_be_bytes([message[2], message[3]]);
    let questions = u16::from_be_bytes([message[4], message[5]]);
    // A query (QR 0) with the standard opcode (0) and one question.
    if flags & 0xF800 != 0 || questions != 1 {
        return None;
    }
    let mut at = 12;
    let mut name = String::new();
    loop {
        let length = usize::from(*message.get(at)?);
        at += 1;
        if length == 0 {
            break;
        }
        // 0b01 and 0b10 are reserved, 0b11 is a compression pointer: neither is in a question.
        if length > 63 {
            return None;
        }
        let label = message.get(at..at + length)?;
        at += length;
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(&String::from_utf8_lossy(label));
        if name.len() > 253 {
            return None;
        }
    }
    let tail = message.get(at..at + 4)?;
    let qtype = u16::from_be_bytes([tail[0], tail[1]]);
    let class = u16::from_be_bytes([tail[2], tail[3]]);
    (class == CLASS_IN).then(|| Question {
        id,
        name,
        qtype,
        end: at + 4,
    })
}

/// A response to `query` that carries no answer and the code `rcode`.
pub fn empty_response(query: &[u8], rcode: u8) -> Vec<u8> {
    let mut response = vec![0u8; 12];
    if query.len() >= 2 {
        response[..2].copy_from_slice(&query[..2]);
    }
    // QR, the RD of the query (when there is a flags field), RA, the code.
    response[2] = 0x80 | query.get(2).map_or(0, |flags| flags & 0x01);
    response[3] = 0x80 | (rcode & 0x0F);
    // The question is copied when there is one, so that the client can match it.
    if let Some(question) = parse_question(query) {
        response[5] = 1;
        response.extend_from_slice(&query[12..question.end]);
    }
    response
}

/// The response with the addresses of `question`; `query` is the message it answers.
fn address_response(query: &[u8], question: &Question, addresses: &[IpAddr]) -> Vec<u8> {
    let mut response = empty_response(query, RCODE_NOERROR);
    let count = u16::try_from(addresses.len()).unwrap_or(u16::MAX);
    response[6..8].copy_from_slice(&count.to_be_bytes());
    for address in addresses {
        // The name is a pointer to the one in the question.
        response.extend_from_slice(&[0xC0, 0x0C]);
        response.extend_from_slice(&question.qtype.to_be_bytes());
        response.extend_from_slice(&CLASS_IN.to_be_bytes());
        response.extend_from_slice(&SYSTEM_TTL.to_be_bytes());
        match address {
            IpAddr::V4(v4) => {
                response.extend_from_slice(&4u16.to_be_bytes());
                response.extend_from_slice(&v4.octets());
            }
            IpAddr::V6(v6) => {
                response.extend_from_slice(&16u16.to_be_bytes());
                response.extend_from_slice(&v6.octets());
            }
        }
    }
    response
}

/// The response to `query` that lists `addresses` as the answer (the kind the question asks for
/// is the caller's to choose); none when `query` is not a question this module reads.
pub fn answer_addresses(query: &[u8], addresses: &[IpAddr]) -> Option<Vec<u8>> {
    let question = parse_question(query)?;
    Some(address_response(query, &question, addresses))
}

/// Why a resolver did not give an answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Miss {
    /// No answer in [`STEP_TIMEOUT`].
    Timeout,
    /// The connection or the exchange failed.
    Unreachable,
    /// An answer with a failing code (SERVFAIL, REFUSED...).
    Refused,
    /// The resolver cannot answer this kind of question.
    Unsupported,
    /// The answer is not a DNS message of this question.
    Malformed,
}
impl Miss {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Unreachable => "unreachable",
            Self::Refused => "refused",
            Self::Unsupported => "unsupported",
            Self::Malformed => "malformed",
        }
    }
}

/// One resolver of the chain.
#[async_trait]
pub trait Upstream: Send + Sync {
    fn name(&self) -> &'static str;
    /// The raw answer to the raw query.
    async fn ask(&self, query: &[u8]) -> Result<Vec<u8>, Miss>;
}

/// A public resolver reached over HTTPS (RFC 8484) at fixed addresses: the address is the host of
/// the URL and in the certificate of the resolver, so its name is never looked up and a machine
/// that cannot resolve names still reaches it. Its addresses race within the shared budget.
pub struct Doh {
    name: &'static str,
    client: reqwest::Client,
    urls: Vec<String>,
}
impl Doh {
    pub fn new(name: &'static str, addresses: &[&str]) -> Option<Self> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(STEP_TIMEOUT)
            .timeout(STEP_TIMEOUT)
            .build()
            .ok()?;
        let urls = addresses
            .iter()
            .map(|address| format!("https://{address}/dns-query"))
            .collect();
        Some(Self { name, client, urls })
    }
    /// The resolver at `url`, asked with `client`.
    pub fn at(name: &'static str, client: reqwest::Client, url: String) -> Self {
        Self {
            name,
            client,
            urls: vec![url],
        }
    }
    async fn ask_at(&self, url: &str, query: &[u8]) -> Result<Vec<u8>, Miss> {
        let miss = |error: reqwest::Error| {
            if error.is_timeout() {
                Miss::Timeout
            } else {
                Miss::Unreachable
            }
        };
        let mut response = self
            .client
            .post(url)
            .header("content-type", "application/dns-message")
            .header("accept", "application/dns-message")
            .body(query.to_vec())
            .send()
            .await
            .map_err(miss)?;
        if !response.status().is_success() {
            return Err(Miss::Refused);
        }
        let mut answer = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(miss)? {
            if answer.len() + chunk.len() > MAX_ANSWER {
                return Err(Miss::Malformed);
            }
            answer.extend_from_slice(&chunk);
        }
        Ok(answer)
    }
}
#[async_trait]
impl Upstream for Doh {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn ask(&self, query: &[u8]) -> Result<Vec<u8>, Miss> {
        // Both addresses share the resolver's three-second budget. A silent address must
        // not use all of it before the alternative even gets a chance to answer.
        let attempts = self
            .urls
            .iter()
            .map(|url| Box::pin(self.ask_at(url, query)));
        futures::future::select_ok(attempts)
            .await
            .map(|(answer, _)| answer)
    }
}

/// The resolver of the machine, through the system call every program of the machine uses. It
/// answers questions for addresses (A and AAAA), which is all that dialing needs.
pub struct System;
#[async_trait]
impl Upstream for System {
    fn name(&self) -> &'static str {
        "local"
    }
    async fn ask(&self, query: &[u8]) -> Result<Vec<u8>, Miss> {
        let question = parse_question(query).ok_or(Miss::Malformed)?;
        let wanted_v6 = match question.qtype {
            TYPE_A => false,
            TYPE_AAAA => true,
            _ => return Err(Miss::Unsupported),
        };
        // Host names only: nothing else is worth handing to the system call.
        if question.name.is_empty()
            || !question
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return Err(Miss::Unsupported);
        }
        let found = match tokio::net::lookup_host((question.name.as_str(), 0)).await {
            Ok(found) => found,
            Err(error) => return system_failure(query, &error),
        };
        let mut addresses: Vec<IpAddr> = Vec::new();
        for address in found.map(|a| a.ip()) {
            if address.is_ipv6() == wanted_v6 && !addresses.contains(&address) {
                addresses.push(address);
            }
        }
        // A name with no address of the kind asked for is an answer too: no data.
        Ok(address_response(query, &question, &addresses))
    }
}

fn system_failure(query: &[u8], error: &std::io::Error) -> Result<Vec<u8>, Miss> {
    if system_name_not_found(error) {
        Ok(empty_response(query, RCODE_NXDOMAIN))
    } else {
        Err(Miss::Unreachable)
    }
}

fn system_name_not_found(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::NotFound {
        return true;
    }
    // std::net preserves getaddrinfo's EAI_NONAME / WSAHOST_NOT_FOUND as the raw error.
    #[cfg(target_os = "linux")]
    if error.raw_os_error() == Some(-2) {
        return true;
    }
    #[cfg(windows)]
    if error.raw_os_error() == Some(11001) {
        return true;
    }
    false
}

/// What the bridge knows about one resolver; counts and the kind of the last miss, no names.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResolverStats {
    pub name: &'static str,
    pub answered: u64,
    pub missed: u64,
    pub last_miss: Option<&'static str>,
    /// How long the last answer took.
    pub last_answer_ms: Option<u64>,
    /// Left out of the questions for now because it did not answer.
    pub left_out: bool,
}

struct Step {
    upstream: Box<dyn Upstream>,
    left_out_until: Mutex<Option<Instant>>,
    answered: AtomicU64,
    missed: AtomicU64,
    last_miss: Mutex<Option<Miss>>,
    last_answer_ms: AtomicU64,
}
impl Step {
    fn new(upstream: Box<dyn Upstream>) -> Self {
        Self {
            upstream,
            left_out_until: Mutex::new(None),
            answered: AtomicU64::new(0),
            missed: AtomicU64::new(0),
            last_miss: Mutex::new(None),
            last_answer_ms: AtomicU64::new(0),
        }
    }
    fn left_out(&self, now: Instant) -> bool {
        self.left_out_until
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|until| until > now)
    }
}

/// The resolvers in the order they are asked.
pub struct Chain {
    steps: Vec<Step>,
}
impl Chain {
    pub fn new(upstreams: Vec<Box<dyn Upstream>>) -> Self {
        Self {
            steps: upstreams.into_iter().map(Step::new).collect(),
        }
    }
    /// How many resolvers there are.
    pub fn len(&self) -> usize {
        self.steps.len()
    }
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
    /// Google, Cloudflare, the machine's own resolver, Yandex.
    pub fn public() -> Self {
        let mut upstreams: Vec<Box<dyn Upstream>> = Vec::new();
        if let Some(google) = Doh::new("google", &DOH_IPS[..2]) {
            upstreams.push(Box::new(google));
        }
        if let Some(cloudflare) = Doh::new("cloudflare", &DOH_IPS[2..4]) {
            upstreams.push(Box::new(cloudflare));
        }
        upstreams.push(Box::new(System));
        if let Some(yandex) = Doh::new("yandex", &DOH_IPS[4..]) {
            upstreams.push(Box::new(yandex));
        }
        Self::new(upstreams)
    }

    /// The answer to `query`: the first resolver that gives one. The resolvers that did not
    /// answer lately are asked last, and only if the others did not answer either. A message with
    /// no answer from any of them is a response with the code SERVFAIL.
    pub async fn resolve(&self, query: &[u8]) -> Vec<u8> {
        // Client errors must not reach upstreams or affect their health/cooldown.
        if parse_question(query).is_none() {
            return empty_response(query, RCODE_FORMERR);
        }
        let now = Instant::now();
        let (live, left_out): (Vec<&Step>, Vec<&Step>) =
            self.steps.iter().partition(|step| !step.left_out(now));
        for step in live.into_iter().chain(left_out) {
            let started = Instant::now();
            let asked = timeout(STEP_TIMEOUT, step.upstream.ask(query))
                .await
                .unwrap_or(Err(Miss::Timeout));
            match asked.and_then(|answer| accept(query, answer)) {
                Ok(answer) => {
                    step.answered.fetch_add(1, Ordering::Relaxed);
                    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    step.last_answer_ms.store(ms.max(1), Ordering::Relaxed);
                    *step
                        .left_out_until
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = None;
                    return answer;
                }
                // A resolver that cannot answer this kind of question is not a broken one.
                Err(Miss::Unsupported) => {}
                Err(miss) => {
                    step.missed.fetch_add(1, Ordering::Relaxed);
                    *step.last_miss.lock().unwrap_or_else(|e| e.into_inner()) = Some(miss);
                    *step
                        .left_out_until
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + COOLDOWN);
                }
            }
        }
        empty_response(query, RCODE_SERVFAIL)
    }

    pub fn stats(&self) -> Vec<ResolverStats> {
        let now = Instant::now();
        self.steps
            .iter()
            .map(|step| ResolverStats {
                name: step.upstream.name(),
                answered: step.answered.load(Ordering::Relaxed),
                missed: step.missed.load(Ordering::Relaxed),
                last_miss: step
                    .last_miss
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .map(Miss::as_str),
                last_answer_ms: Some(step.last_answer_ms.load(Ordering::Relaxed))
                    .filter(|ms| *ms > 0),
                left_out: step.left_out(now),
            })
            .collect()
    }
}

/// An answer that ends the chain: a DNS message with the id of the query, NOERROR or NXDOMAIN.
/// The id is set to the one of the query whatever the resolver echoed.
pub fn accept(query: &[u8], mut answer: Vec<u8>) -> Result<Vec<u8>, Miss> {
    if answer.len() < 12 || query.len() < 2 || answer[2] & 0x80 == 0 {
        return Err(Miss::Malformed);
    }
    match answer[3] & 0x0F {
        RCODE_NOERROR | RCODE_NXDOMAIN => {
            answer[..2].copy_from_slice(&query[..2]);
            Ok(answer)
        }
        _ => Err(Miss::Refused),
    }
}

/// The loopback port sing-box asks for names, and the chain behind it. Dropped, it closes.
pub struct DnsBridge {
    port: u16,
    chain: Arc<Chain>,
    task: JoinHandle<()>,
}
impl DnsBridge {
    /// Listens on a free port of the loopback interface and answers from `chain`.
    pub async fn start(chain: Chain) -> std::io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let chain = Arc::new(chain);
        let task = tokio::spawn(serve(listener, chain.clone()));
        Ok(Self { port, chain, task })
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn stats(&self) -> Vec<ResolverStats> {
        self.chain.stats()
    }
}
impl Drop for DnsBridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(listener: TcpListener, chain: Arc<Chain>) {
    let incoming = futures::stream::unfold(listener, |listener| async {
        let next = listener.accept().await.map(|(stream, _)| stream);
        Some((next, listener))
    });
    serve_connections(Box::pin(incoming), chain).await;
}

async fn serve_connections(
    mut incoming: impl futures::Stream<Item = std::io::Result<TcpStream>> + Unpin,
    chain: Arc<Chain>,
) {
    use futures::StreamExt;
    let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    // Dropping this set also aborts every accepted connection and its pending questions.
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = incoming.next() => match accepted {
                Some(Ok(stream)) => {
                    if let Ok(permit) = connections.clone().try_acquire_owned() {
                        let chain = chain.clone();
                        let in_flight = in_flight.clone();
                        tasks.spawn(async move {
                            let _permit = permit;
                            connection(stream, chain, in_flight).await;
                        });
                    }
                }
                // Resource exhaustion and aborted handshakes are transient; do not retire
                // the bridge or spin while the machine is short of descriptors.
                Some(Err(_)) => tokio::time::sleep(Duration::from_millis(25)).await,
                None => return,
            }
        }
    }
}

/// DNS over TCP (RFC 7766): every message is a 16-bit length and the message.
async fn connection(stream: TcpStream, chain: Arc<Chain>, in_flight: Arc<Semaphore>) {
    let (mut reader, writer) = stream.into_split();
    let writer = Arc::new(AsyncMutex::new(writer));
    let mut questions = JoinSet::new();
    loop {
        let mut length = [0u8; 2];
        let read = {
            let read = timeout(IDLE, reader.read_exact(&mut length));
            tokio::pin!(read);
            loop {
                tokio::select! {
                    read = &mut read => break read,
                    done = questions.join_next(), if !questions.is_empty() => {
                        if !matches!(done, Some(Ok(true))) { return; }
                    }
                }
            }
        };
        match read {
            Ok(Ok(_)) => {}
            _ => return,
        }
        let length = usize::from(u16::from_be_bytes(length));
        if length == 0 || length > MAX_MESSAGE {
            return;
        }
        let Ok(Ok(permit)) = timeout(IDLE, in_flight.clone().acquire_owned()).await else {
            return;
        };
        let mut query = vec![0u8; length];
        if !matches!(
            timeout(IDLE, reader.read_exact(&mut query)).await,
            Ok(Ok(_))
        ) {
            return;
        }
        let chain = chain.clone();
        let writer = writer.clone();
        questions.spawn(async move {
            let _permit = permit;
            let answer = chain.resolve(&query).await;
            // Serialize whole response frames, not the DNS lookups. Both waiting for the
            // writer and writing have a deadline, including clients that never read.
            write_answer(&writer, &answer).await
        });
    }
}

async fn write_answer<W: tokio::io::AsyncWrite + Unpin>(
    writer: &AsyncMutex<W>,
    answer: &[u8],
) -> bool {
    let Ok(size) = u16::try_from(answer.len()) else {
        return false;
    };
    matches!(
        timeout(IDLE, async {
            let mut writer = writer.lock().await;
            writer.write_all(&size.to_be_bytes()).await?;
            writer.write_all(answer).await
        })
        .await,
        Ok(Ok(()))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut message = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            message.push(label.len() as u8);
            message.extend_from_slice(label.as_bytes());
        }
        message.push(0);
        message.extend_from_slice(&qtype.to_be_bytes());
        message.extend_from_slice(&1u16.to_be_bytes());
        message
    }

    #[test]
    fn a_question_is_read_with_its_name_and_type() {
        let question = parse_question(&query("ya.ru", TYPE_A)).unwrap();
        assert_eq!(question.id, 0x1234);
        assert_eq!(question.name, "ya.ru");
        assert_eq!(question.qtype, TYPE_A);
    }

    #[test]
    fn what_is_not_a_plain_question_is_refused() {
        assert!(parse_question(&[]).is_none());
        assert!(parse_question(&[0; 11]).is_none());
        // A response, not a query.
        let mut response = query("ya.ru", TYPE_A);
        response[2] |= 0x80;
        assert!(parse_question(&response).is_none());
        // Two questions.
        let mut two = query("ya.ru", TYPE_A);
        two[5] = 2;
        assert!(parse_question(&two).is_none());
        // A compression pointer in the name.
        let mut pointer = query("ya.ru", TYPE_A);
        pointer[12] = 0xC0;
        assert!(parse_question(&pointer).is_none());
        // A name that runs past the end.
        let mut cut = query("ya.ru", TYPE_A);
        cut.truncate(16);
        assert!(parse_question(&cut).is_none());
        // Another class than IN.
        let mut class = query("ya.ru", TYPE_A);
        let last = class.len() - 1;
        class[last] = 3;
        assert!(parse_question(&class).is_none());
    }

    #[test]
    fn an_empty_response_keeps_the_id_the_question_and_sets_the_code() {
        let asked = query("ya.ru", TYPE_A);
        let response = empty_response(&asked, RCODE_SERVFAIL);
        assert_eq!(&response[..2], &asked[..2]);
        assert_eq!(response[2] & 0x80, 0x80, "a response");
        assert_eq!(response[2] & 0x01, 0x01, "recursion desired is echoed");
        assert_eq!(response[3] & 0x0F, RCODE_SERVFAIL);
        assert_eq!(&response[12..], &asked[12..]);
    }

    #[test]
    fn an_address_response_lists_the_addresses_of_the_kind_asked() {
        let asked = query("ya.ru", TYPE_A);
        let question = parse_question(&asked).unwrap();
        let response = address_response(&asked, &question, &["77.88.44.242".parse().unwrap()]);
        assert_eq!(&response[6..8], &[0, 1], "one answer");
        // The answer: a pointer to the name, A, IN, the time to live, four bytes, the address.
        let answer = &response[asked.len()..];
        assert_eq!(&answer[..4], &[0xC0, 0x0C, 0, 1]);
        assert_eq!(&answer[10..12], &[0, 4]);
        assert_eq!(&answer[12..], &[77, 88, 44, 242]);
    }

    struct Fixed {
        name: &'static str,
        answer: Result<Vec<u8>, Miss>,
        asked: Arc<AtomicU64>,
        delay: Duration,
    }
    #[async_trait]
    impl Upstream for Fixed {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn ask(&self, _query: &[u8]) -> Result<Vec<u8>, Miss> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.answer.clone()
        }
    }
    /// An answer to `query` with the code `rcode` and a recognisable tail.
    fn answer_with(rcode: u8, tail: u8) -> Vec<u8> {
        let mut answer = empty_response(&query("ya.ru", TYPE_A), rcode);
        answer.push(tail);
        answer
    }
    fn fixed(
        name: &'static str,
        answer: Result<Vec<u8>, Miss>,
        delay: Duration,
    ) -> (Box<dyn Upstream>, Arc<AtomicU64>) {
        let asked = Arc::new(AtomicU64::new(0));
        let upstream = Fixed {
            name,
            answer,
            asked: asked.clone(),
            delay,
        };
        (Box::new(upstream), asked)
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_resolver_that_answers_ends_the_chain() {
        let (first, first_asked) = fixed("first", Ok(answer_with(0, 1)), Duration::ZERO);
        let (second, second_asked) = fixed("second", Ok(answer_with(0, 2)), Duration::ZERO);
        let chain = Chain::new(vec![first, second]);
        let answer = chain.resolve(&query("ya.ru", TYPE_A)).await;
        assert_eq!(answer.last(), Some(&1));
        assert_eq!(first_asked.load(Ordering::SeqCst), 1);
        assert_eq!(second_asked.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_resolver_that_is_silent_for_three_seconds_gives_way_to_the_next() {
        let (silent, _) = fixed(
            "silent",
            Ok(answer_with(0, 1)),
            STEP_TIMEOUT + Duration::from_secs(1),
        );
        let (next, _) = fixed("next", Ok(answer_with(0, 2)), Duration::ZERO);
        let chain = Chain::new(vec![silent, next]);
        let started = Instant::now();
        let answer = chain.resolve(&query("ya.ru", TYPE_A)).await;
        assert_eq!(answer.last(), Some(&2));
        assert_eq!(started.elapsed(), STEP_TIMEOUT, "three seconds, no more");
        let stats = chain.stats();
        assert_eq!(stats[0].last_miss, Some("timeout"));
        assert!(stats[0].left_out);
        assert_eq!(stats[1].answered, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_code_moves_on_but_a_name_that_does_not_exist_is_an_answer() {
        let (broken, _) = fixed("broken", Ok(answer_with(RCODE_SERVFAIL, 1)), Duration::ZERO);
        let (missing, _) = fixed(
            "missing",
            Ok(answer_with(RCODE_NXDOMAIN, 2)),
            Duration::ZERO,
        );
        let (never, never_asked) = fixed("never", Ok(answer_with(0, 3)), Duration::ZERO);
        let chain = Chain::new(vec![broken, missing, never]);
        let answer = chain.resolve(&query("ya.ru", TYPE_A)).await;
        assert_eq!(answer.last(), Some(&2));
        assert_eq!(answer[3] & 0x0F, RCODE_NXDOMAIN);
        assert_eq!(never_asked.load(Ordering::SeqCst), 0);
        assert_eq!(chain.stats()[0].last_miss, Some("refused"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_resolver_that_did_not_answer_is_left_out_until_the_cooldown_ends() {
        let (down, down_asked) = fixed("down", Err(Miss::Unreachable), Duration::ZERO);
        let (up, _) = fixed("up", Ok(answer_with(0, 2)), Duration::ZERO);
        let chain = Chain::new(vec![down, up]);
        let asked = query("ya.ru", TYPE_A);
        chain.resolve(&asked).await;
        chain.resolve(&asked).await;
        assert_eq!(
            down_asked.load(Ordering::SeqCst),
            1,
            "not asked again at once"
        );
        tokio::time::advance(COOLDOWN + Duration::from_secs(1)).await;
        chain.resolve(&asked).await;
        assert_eq!(
            down_asked.load(Ordering::SeqCst),
            2,
            "asked again after the cooldown"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn when_every_resolver_is_left_out_they_are_all_asked_again() {
        let (first, first_asked) = fixed("first", Err(Miss::Unreachable), Duration::ZERO);
        let (second, second_asked) = fixed("second", Err(Miss::Timeout), Duration::ZERO);
        let chain = Chain::new(vec![first, second]);
        let asked = query("ya.ru", TYPE_A);
        let failed = chain.resolve(&asked).await;
        assert_eq!(failed[3] & 0x0F, RCODE_SERVFAIL, "no answer from anyone");
        assert_eq!(&failed[..2], &asked[..2]);
        chain.resolve(&asked).await;
        assert_eq!(first_asked.load(Ordering::SeqCst), 2);
        assert_eq!(second_asked.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_resolver_that_cannot_answer_a_kind_of_question_is_not_counted_as_broken() {
        let (limited, _) = fixed("limited", Err(Miss::Unsupported), Duration::ZERO);
        let (next, _) = fixed("next", Ok(answer_with(0, 2)), Duration::ZERO);
        let chain = Chain::new(vec![limited, next]);
        chain.resolve(&query("ya.ru", 65)).await;
        let stats = chain.stats();
        assert_eq!(stats[0].missed, 0);
        assert!(!stats[0].left_out);
    }

    #[tokio::test(start_paused = true)]
    async fn the_id_of_the_query_is_put_on_the_answer() {
        let mut other = answer_with(0, 1);
        other[0] = 0xAB;
        other[1] = 0xCD;
        let (first, _) = fixed("first", Ok(other), Duration::ZERO);
        let chain = Chain::new(vec![first]);
        let answer = chain.resolve(&query("ya.ru", TYPE_A)).await;
        assert_eq!(&answer[..2], &[0x12, 0x34]);
    }

    #[tokio::test]
    async fn the_machine_resolver_answers_addresses_and_nothing_else() {
        let a = System.ask(&query("localhost", TYPE_A)).await.unwrap();
        assert_eq!(&a[6..8], &[0, 1], "localhost has one IPv4 address");
        assert_eq!(&a[a.len() - 4..], &[127, 0, 0, 1]);
        assert_eq!(
            System.ask(&query("localhost", 65)).await.unwrap_err(),
            Miss::Unsupported
        );
        assert_eq!(
            System.ask(&query("a b", TYPE_A)).await.unwrap_err(),
            Miss::Unsupported,
            "a name that is not a host name is not handed to the system"
        );
        assert_eq!(System.ask(&[1, 2, 3]).await.unwrap_err(), Miss::Malformed);
    }

    #[tokio::test]
    async fn the_bridge_answers_over_tcp_and_closes_with_its_handle() {
        let (first, _) = fixed("first", Ok(answer_with(0, 7)), Duration::ZERO);
        let bridge = DnsBridge::start(Chain::new(vec![first])).await.unwrap();
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, bridge.port()))
            .await
            .unwrap();
        let asked = query("ya.ru", TYPE_A);
        for _ in 0..2 {
            // Two questions on one connection.
            stream
                .write_all(&(asked.len() as u16).to_be_bytes())
                .await
                .unwrap();
            stream.write_all(&asked).await.unwrap();
            let mut length = [0u8; 2];
            stream.read_exact(&mut length).await.unwrap();
            let mut answer = vec![0u8; usize::from(u16::from_be_bytes(length))];
            stream.read_exact(&mut answer).await.unwrap();
            assert_eq!(answer.last(), Some(&7));
        }
        assert_eq!(bridge.stats()[0].answered, 2);
        let port = bridge.port();
        drop(bridge);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .is_err(),
            "the port is closed with the bridge"
        );
    }

    #[tokio::test]
    async fn the_bridge_closes_a_connection_that_asks_for_too_much() {
        let (first, _) = fixed("first", Ok(answer_with(0, 7)), Duration::ZERO);
        let bridge = DnsBridge::start(Chain::new(vec![first])).await.unwrap();
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, bridge.port()))
            .await
            .unwrap();
        stream
            .write_all(&((MAX_MESSAGE + 1) as u16).to_be_bytes())
            .await
            .unwrap();
        let mut rest = Vec::new();
        let read = timeout(Duration::from_secs(5), stream.read_to_end(&mut rest))
            .await
            .expect("the bridge closes the connection");
        assert!(read.is_ok());
        assert!(rest.is_empty());
    }

    /// A resolver over HTTP, for the test: answers `status` with `body` to every request.
    async fn http_resolver(status: u16, body: Vec<u8>) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let url = format!("http://{}/dns-query", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    // The whole request is read, the headers and then the body they announce:
                    // closing with bytes unread would reset the connection under the client.
                    let mut seen = Vec::new();
                    let mut chunk = [0u8; 1024];
                    let (head_end, length) = loop {
                        let Ok(n) = stream.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        seen.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&seen).to_lowercase();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let length = text
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length: "))
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            break (end + 4, length);
                        }
                    };
                    while seen.len() < head_end + length {
                        match stream.read(&mut chunk).await {
                            Ok(n) if n > 0 => seen.extend_from_slice(&chunk[..n]),
                            _ => return,
                        }
                    }
                    let head = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/dns-message\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                });
            }
        });
        (url, task)
    }

    #[tokio::test]
    async fn a_resolver_over_https_is_asked_with_a_post_and_its_answer_is_taken() {
        let (url, _server) = http_resolver(200, answer_with(0, 9)).await;
        let doh = Doh::at("test", reqwest::Client::new(), url);
        let answer = doh.ask(&query("ya.ru", TYPE_A)).await.unwrap();
        assert_eq!(answer.last(), Some(&9));
    }

    #[tokio::test]
    async fn a_resolver_over_https_that_refuses_or_is_not_there_is_a_miss() {
        let (url, _server) = http_resolver(503, Vec::new()).await;
        let doh = Doh::at("test", reqwest::Client::new(), url);
        assert_eq!(
            doh.ask(&query("ya.ru", TYPE_A)).await.unwrap_err(),
            Miss::Refused
        );
        // Nothing listens here: the port of a listener that is gone.
        let gone = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let url = format!("http://{}/dns-query", gone.local_addr().unwrap());
        drop(gone);
        let doh = Doh::at("test", reqwest::Client::new(), url);
        assert_eq!(
            doh.ask(&query("ya.ru", TYPE_A)).await.unwrap_err(),
            Miss::Unreachable
        );
    }

    #[tokio::test]
    async fn pipelined_questions_are_resolved_together_and_drop_closes_existing_clients() {
        let (upstream, asked) = fixed("test", Ok(answer_with(0, 7)), Duration::from_millis(150));
        let bridge = DnsBridge::start(Chain::new(vec![upstream])).await.unwrap();
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, bridge.port()))
            .await
            .unwrap();
        for id in 0..3u16 {
            let mut q = query("ya.ru", TYPE_A);
            q[..2].copy_from_slice(&id.to_be_bytes());
            stream
                .write_all(&(q.len() as u16).to_be_bytes())
                .await
                .unwrap();
            stream.write_all(&q).await.unwrap();
        }
        timeout(Duration::from_millis(400), async {
            let mut ids = Vec::new();
            for _ in 0..3 {
                let n = stream.read_u16().await.unwrap();
                let mut answer = vec![0; n as usize];
                stream.read_exact(&mut answer).await.unwrap();
                ids.push(u16::from_be_bytes([answer[0], answer[1]]));
            }
            ids.sort_unstable();
            assert_eq!(ids, [0, 1, 2]);
        })
        .await
        .expect("three concurrent answers, not 450 ms of serial work");
        assert_eq!(asked.load(Ordering::SeqCst), 3);
        drop(bridge);
        let mut byte = [0];
        assert_eq!(
            timeout(Duration::from_secs(1), stream.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_truncated_query_never_reaches_a_resolver() {
        let (upstream, asked) = fixed("test", Ok(answer_with(0, 7)), Duration::ZERO);
        let bridge = DnsBridge::start(Chain::new(vec![upstream])).await.unwrap();
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, bridge.port()))
            .await
            .unwrap();
        let q = query("ya.ru", TYPE_A);
        stream.write_u16((q.len() + 20) as u16).await.unwrap();
        stream.write_all(&q[..10]).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut response = Vec::new();
        timeout(Duration::from_secs(1), stream.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.is_empty());
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        assert_eq!(bridge.stats()[0].missed, 0);
    }

    #[tokio::test]
    async fn malformed_questions_do_not_reach_upstreams_or_change_their_health() {
        let (upstream, asked) = fixed("test", Ok(answer_with(0, 7)), Duration::ZERO);
        let bridge = DnsBridge::start(Chain::new(vec![upstream])).await.unwrap();
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, bridge.port()))
            .await
            .unwrap();
        let q = query("ya.ru", TYPE_A);
        let mut response = q.clone();
        response[2] |= 0x80;
        let mut compressed = q.clone();
        compressed[12] = 0xc0;
        let mut other_class = q.clone();
        *other_class.last_mut().unwrap() = 3;
        for malformed in [vec![0; 12], response, compressed, other_class] {
            // The direct public entry point and a complete TCP frame have the same guard.
            let direct = bridge.chain.resolve(&malformed).await;
            assert_eq!(direct[3] & 0x0f, 1, "FORMERR before any upstream");
            stream.write_u16(malformed.len() as u16).await.unwrap();
            stream.write_all(&malformed).await.unwrap();
            let answer = timeout(Duration::from_secs(1), async {
                let size = stream.read_u16().await.unwrap();
                let mut answer = vec![0; usize::from(size)];
                stream.read_exact(&mut answer).await.unwrap();
                answer
            })
            .await
            .unwrap();
            assert_eq!(answer, direct);
            assert_eq!(&answer[..2], &malformed[..2]);
            assert_eq!(asked.load(Ordering::SeqCst), 0);
            let stats = bridge.stats();
            assert_eq!(stats[0].answered, 0);
            assert_eq!(stats[0].missed, 0);
            assert_eq!(stats[0].last_miss, None);
            assert_eq!(stats[0].last_answer_ms, None);
            assert!(!stats[0].left_out);
        }
        stream.write_u16(q.len() as u16).await.unwrap();
        stream.write_all(&q).await.unwrap();
        let size = stream.read_u16().await.unwrap();
        let mut answer = vec![0; usize::from(size)];
        stream.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer.last(), Some(&7));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(bridge.stats()[0].answered, 1);
    }

    #[tokio::test]
    async fn idle_clients_and_waiting_questions_have_deadlines() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let (upstream, asked) = fixed("test", Ok(answer_with(0, 7)), Duration::ZERO);
        let permits = Arc::new(Semaphore::new(0));
        let task = tokio::spawn(connection(
            server,
            Arc::new(Chain::new(vec![upstream])),
            permits,
        ));
        let q = query("ya.ru", TYPE_A);
        client.write_u16(q.len() as u16).await.unwrap();
        client.write_all(&q).await.unwrap();
        // Waiting for admission is bounded too; no upstream runs without a permit.
        timeout(IDLE + Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 0);

        let (server, _) = tokio::join!(listener.accept(), async {
            TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap()
        });
        let (server, _) = server.unwrap();
        let task = tokio::spawn(connection(
            server,
            Arc::new(Chain::new(Vec::new())),
            Arc::new(Semaphore::new(1)),
        ));
        timeout(IDLE + Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn oversized_https_answers_are_refused() {
        let (url, server) = http_resolver(200, vec![0; MAX_ANSWER + 1]).await;
        let resolver = Doh::at("test", reqwest::Client::new(), url);
        assert_eq!(
            resolver.ask(&query("ya.ru", TYPE_A)).await,
            Err(Miss::Malformed)
        );
        server.abort();
    }

    #[test]
    fn a_missing_name_is_distinguished_from_a_transport_failure() {
        assert!(system_name_not_found(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
        assert!(!system_name_not_found(&std::io::Error::from(
            std::io::ErrorKind::TimedOut
        )));
        #[cfg(target_os = "linux")]
        assert!(system_name_not_found(&std::io::Error::from_raw_os_error(
            -2
        )));
        #[cfg(windows)]
        assert!(system_name_not_found(&std::io::Error::from_raw_os_error(
            11001
        )));
    }

    #[tokio::test]
    async fn a_silent_first_https_address_does_not_starve_the_second() {
        let silent = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let (url, server) = http_resolver(200, answer_with(0, 9)).await;
        let resolver = Doh {
            name: "test",
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(STEP_TIMEOUT)
                .build()
                .unwrap(),
            urls: vec![
                format!("http://{}/dns-query", silent.local_addr().unwrap()),
                url,
            ],
        };
        let answer = timeout(
            Duration::from_secs(1),
            resolver.ask(&query("ya.ru", TYPE_A)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(answer.last(), Some(&9));
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_does_not_read_cannot_hold_a_question_forever() {
        let (writer, _reader) = tokio::io::duplex(1);
        let writer = AsyncMutex::new(writer);
        let started = Instant::now();
        assert!(!write_answer(&writer, &[0; 512]).await);
        assert_eq!(started.elapsed(), IDLE);
    }

    #[tokio::test]
    async fn the_question_limit_is_global_even_with_one_pipelined_connection() {
        let (upstream, asked) = fixed("test", Ok(answer_with(0, 7)), Duration::from_secs(2));
        let bridge = DnsBridge::start(Chain::new(vec![upstream])).await.unwrap();
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, bridge.port()))
            .await
            .unwrap();
        let q = query("ya.ru", TYPE_A);
        for _ in 0..MAX_IN_FLIGHT + 1 {
            stream.write_u16(q.len() as u16).await.unwrap();
            stream.write_all(&q).await.unwrap();
        }
        timeout(Duration::from_secs(1), async {
            while asked.load(Ordering::SeqCst) < MAX_IN_FLIGHT as u64 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(asked.load(Ordering::SeqCst), MAX_IN_FLIGHT as u64);
    }

    #[tokio::test]
    async fn the_https_client_can_speak_http2() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let url = format!("http://{}/dns-query", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut preface = [0; 24];
            stream.read_exact(&mut preface).await.unwrap();
            assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .http2_prior_knowledge()
            .build()
            .unwrap();
        let _ = Doh::at("test", client, url)
            .ask(&query("ya.ru", TYPE_A))
            .await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_accept_error_is_retried_and_the_next_client_gets_an_answer() {
        use futures::StreamExt;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let incoming = futures::stream::iter(vec![
            Err(std::io::Error::from(std::io::ErrorKind::ConnectionAborted)),
            Ok(server),
        ])
        .chain(futures::stream::pending());
        let (upstream, _) = fixed("test", Ok(answer_with(0, 7)), Duration::ZERO);
        let task = tokio::spawn(serve_connections(
            incoming,
            Arc::new(Chain::new(vec![upstream])),
        ));
        let q = query("ya.ru", TYPE_A);
        client.write_u16(q.len() as u16).await.unwrap();
        client.write_all(&q).await.unwrap();
        let size = timeout(Duration::from_secs(1), client.read_u16())
            .await
            .unwrap()
            .unwrap();
        let mut answer = vec![0; size as usize];
        client.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer.last(), Some(&7));
        task.abort();
    }

    #[tokio::test]
    async fn excess_idle_connections_are_closed_without_stopping_the_bridge() {
        let bridge = DnsBridge::start(Chain::new(Vec::new())).await.unwrap();
        let mut clients = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            let mut client = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, bridge.port()))
                .await
                .unwrap();
            // One byte keeps the client admitted, without completing a frame or a question.
            client.write_all(&[0]).await.unwrap();
            clients.push(client);
        }
        let mut excess = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, bridge.port()))
            .await
            .unwrap();
        let mut byte = [0];
        assert_eq!(
            timeout(Duration::from_secs(2), excess.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert_eq!(clients.len(), MAX_CONNECTIONS);
    }

    #[test]
    fn committed_positive_fuzz_seeds_reach_the_question_parser() {
        for bytes in [
            &include_bytes!("../../fuzz/seeds/dns_query/query-a.bin")[..],
            &include_bytes!("../../fuzz/seeds/dns_query/query-aaaa.bin")[..],
        ] {
            assert!(
                parse_question(bytes).is_some(),
                "positive seeds contain wire bytes"
            );
        }
        let pair = include_bytes!("../../fuzz/seeds/dns_query/query-and-answer.bin");
        let (query, answer) = pair[1..].split_at(pair[0] as usize);
        assert!(parse_question(query).is_some());
        assert!(
            accept(query, answer.to_vec()).is_ok(),
            "the pair seed reaches the accepted-answer branch"
        );
    }

    #[tokio::test]
    async fn a_system_negative_answer_keeps_the_local_resolver_in_the_chain() {
        let q = query("missing.invalid", TYPE_A);
        #[cfg(windows)]
        let missing = std::io::Error::from_raw_os_error(11001);
        #[cfg(target_os = "linux")]
        let missing = std::io::Error::from_raw_os_error(-2);
        #[cfg(not(any(windows, target_os = "linux")))]
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        let (local, _) = fixed("local", system_failure(&q, &missing), Duration::ZERO);
        let chain = Chain::new(vec![local]);
        let answer = chain.resolve(&q).await;
        assert_eq!(answer[3] & 15, RCODE_NXDOMAIN);
        assert_eq!(chain.stats()[0].answered, 1);
        assert_eq!(chain.stats()[0].missed, 0);
        assert!(!chain.stats()[0].left_out);
        assert_eq!(
            system_failure(&q, &std::io::Error::from(std::io::ErrorKind::TimedOut)),
            Err(Miss::Unreachable)
        );
    }

    #[test]
    fn the_public_chain_is_google_cloudflare_the_machine_then_yandex() {
        let names: Vec<_> = Chain::public().stats().iter().map(|s| s.name).collect();
        assert_eq!(names, ["google", "cloudflare", "local", "yandex"]);
    }
}
