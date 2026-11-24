use crate::{
    failure::FailureCode,
    jitter::Jitter,
    model::Server,
    pool::{self, Ranked},
    Error, Result,
};
use async_trait::async_trait;
use futures::{stream, StreamExt};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait Probe: Send + Sync {
    async fn endpoint(&self, server: &Server) -> Result<u64>;
    async fn proxy(&self, server: &Server) -> Result<u64>;
    async fn select(&self, id: &str) -> Result<()>;
    /// Makes the rules a provider sent with the selected server the ones in force: `mode` is the
    /// Clash mode they act in ([`Server::routes_mode`]). A probe with no sing-box behind it has
    /// nothing to switch.
    async fn routes(&self, _mode: &str) -> Result<()> {
        Ok(())
    }
    /// Whether the machine reaches the network at all, asked outside the servers: a cycle on a
    /// machine without one would find every server dead and say nothing about any of them. A probe
    /// that cannot tell says yes.
    async fn network(&self) -> Network {
        Network::Up
    }
}
/// What the machine said to the question whether there is a network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Network {
    Up,
    /// The machine is in the network but the sidecar could not get out by itself: it will not
    /// reach the servers either. Says what it saw.
    SidecarDeaf(String),
    /// Nothing answered, whichever way it was asked. Says what each way saw.
    Down(String),
}
impl Network {
    pub fn is_up(&self) -> bool {
        !matches!(self, Network::Down(_))
    }
}
/// The two kinds of check: the fast one asks the pool, the full one every server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cycle {
    Fast,
    Full,
}
/// Why a check runs; the journal tells it, the fast check's verdicts and the events of the
/// application are what set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The timer of the schedule.
    Scheduled,
    /// A connection was made.
    Connect,
    /// The user asked to check now.
    CheckNow,
    /// Auto Select was turned on.
    AutoSelectOn,
    /// The list of servers changed by hand or by an import, or a restart began a new selector.
    ListChanged,
    /// A subscription brought other addresses.
    Subscription,
    /// A server of the pool got worse.
    PoolDegraded,
    /// A server of the pool did not answer.
    PoolLost,
    /// The pool has fewer servers than it should.
    PoolIncomplete,
    /// The list holds no more servers than the pool does: a fast check is a full one.
    SmallList,
    /// The server in use did not answer the fast check.
    CurrentLost,
    /// The light check of the server in use found the connection lost.
    Failover,
    /// The network came back.
    NetworkBack,
}
impl Reason {
    const ALL: [Reason; 13] = [
        Reason::Scheduled,
        Reason::Connect,
        Reason::CheckNow,
        Reason::AutoSelectOn,
        Reason::ListChanged,
        Reason::Subscription,
        Reason::PoolDegraded,
        Reason::PoolLost,
        Reason::PoolIncomplete,
        Reason::SmallList,
        Reason::CurrentLost,
        Reason::Failover,
        Reason::NetworkBack,
    ];
    /// A number for the reason, never zero: a request is passed between tasks as one byte.
    pub fn code(self) -> u8 {
        Self::ALL
            .iter()
            .position(|reason| *reason == self)
            .map_or(0, |i| u8::try_from(i + 1).unwrap_or(0))
    }
    pub fn from_code(code: u8) -> Option<Self> {
        usize::from(code)
            .checked_sub(1)
            .and_then(|i| Self::ALL.get(i).copied())
    }
}
#[derive(Clone, Debug)]
pub struct Measurement {
    pub latency_ms: Option<u64>,
    pub failure_code: Option<FailureCode>,
}
impl Measurement {
    pub(crate) fn from_result(result: Result<u64>, fallback: FailureCode) -> Self {
        match result {
            Ok(latency_ms) => Self {
                latency_ms: Some(latency_ms),
                failure_code: None,
            },
            Err(error) => Self {
                latency_ms: None,
                failure_code: Some(error.failure_code().unwrap_or(fallback)),
            },
        }
    }
}
#[derive(Clone, Debug)]
pub enum Progress {
    Endpoint(String, Measurement),
    /// The server in use, asked first: its answer decides how much else is touched.
    Current(String, Measurement),
    Proxy(String, Measurement),
    /// The middle of the samples taken of one server: the numbers the choice was made on. The
    /// distance is there when it was sampled as well.
    Median {
        id: String,
        proxy_ms: u64,
        endpoint_ms: Option<u64>,
    },
    SelectionFailed(String, FailureCode),
    Selected(String, Instant),
}
/// What a full check decided about the server in use: the journal tells it, without a name or an
/// address. Delays are the middle of three answers (one answer where nothing was compared).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Nothing was in use; the best server was taken.
    Chose { ms: u64 },
    /// The server in use is the best of those that answered.
    Best { ms: u64 },
    /// Another is faster, but not by both margins: the server in use stays.
    Kept { ms: u64, best_ms: u64 },
    /// Another is faster by both margins and takes its place.
    Faster { from_ms: u64, to_ms: u64 },
    /// The server in use did not answer; another takes its place.
    Lost { to_ms: u64 },
    /// The server in use answered, but not reliably; a steady one takes its place.
    Unreliable { to_ms: u64 },
    /// The pool is not complete yet: the best server is taken, whichever was in use.
    Pool { from_ms: Option<u64>, to_ms: u64 },
}
#[derive(Debug)]
pub struct Outcome {
    pub selected: String,
    pub last_switch: Option<Instant>,
    /// Why the server in use stays or gives way.
    pub decision: Decision,
    /// The best servers by the middle of their answers, fastest first, one per host, at most
    /// `Options::pool_size`: what the fast checks that follow keep watching.
    pub pool: Vec<Ranked>,
    /// Every server that answered reliably, fastest first: where the pool is made up again from
    /// when one of its servers is kept out of Auto Select.
    pub ranking: Vec<Ranked>,
}
/// Samples taken after the first one, so that the choice rests on the middle of three.
const EXTRA_SAMPLES: usize = 2;
/// While servers are compared at a measured pace, the server in use is asked this often whether it
/// still answers: a comparison of many servers takes minutes, and the connection may be lost in
/// the meantime.
const LIFELINE_EVERY: Duration = Duration::from_secs(15);
/// How long a switch under way may go on after the cycle was cancelled.
const SWITCH_GRACE: Duration = Duration::from_secs(2);
/// What a cycle is allowed to do besides probing.
#[derive(Clone, Debug)]
pub struct Options {
    pub concurrency: usize,
    /// A working server is left only for one faster by at least this share of its delay...
    pub threshold_percent: u64,
    /// ...and by at least this many milliseconds. Two servers of the same quality do not
    /// trade places every cycle.
    pub threshold_ms: u64,
    /// The pauses between servers and their order. Without a source that gives pauses the servers
    /// are measured at once, in the order they came in.
    pub jitter: Jitter,
    /// Servers that did not answer lately: they are measured only when the server in use does not
    /// work and nobody else is left to try. The server in use is never held back.
    pub held_back: HashSet<String>,
    /// How many of the best servers the pool holds.
    pub pool_size: usize,
    /// The pool was not complete before this check (Auto Select was just turned on, or fewer
    /// servers work than the pool holds): the best server is taken whichever is in use, the others
    /// stay in reserve. A complete pool leaves a server in use alone unless it is clearly beaten.
    pub pool_incomplete: bool,
    /// The servers of the pool, best first. Asked before all the others when nothing in use works:
    /// the first that answers takes the place at once.
    pub prefer: Vec<String>,
    /// Servers not measured in this check, the one in use excepted: they were found dead a moment ago.
    pub skip: HashSet<String>,
    /// The caller has just found the server in use dead: it is not asked again.
    pub current_lost: bool,
    /// Servers that are asked only because they are in use (the user keeps them out of Auto
    /// Select): never part of the pool.
    pub keep_out_of_pool: HashSet<String>,
}
impl Options {
    pub const DEFAULT_THRESHOLD_PERCENT: u64 = 30;
    pub const DEFAULT_THRESHOLD_MS: u64 = 50;
    pub fn with_concurrency(concurrency: usize) -> Self {
        Self {
            concurrency,
            threshold_percent: Self::DEFAULT_THRESHOLD_PERCENT,
            threshold_ms: Self::DEFAULT_THRESHOLD_MS,
            jitter: Jitter::none(),
            held_back: HashSet::new(),
            pool_size: crate::model::Settings::default().pool_size,
            pool_incomplete: false,
            prefer: Vec::new(),
            skip: HashSet::new(),
            current_lost: false,
            keep_out_of_pool: HashSet::new(),
        }
    }
    pub fn from_settings(settings: &crate::model::Settings, jitter: Jitter) -> Self {
        Self {
            concurrency: settings.concurrency,
            threshold_percent: settings.switch_threshold_percent,
            threshold_ms: settings.switch_threshold_ms,
            jitter,
            held_back: HashSet::new(),
            pool_size: settings.pool_size,
            pool_incomplete: false,
            prefer: Vec::new(),
            skip: HashSet::new(),
            current_lost: false,
            keep_out_of_pool: HashSet::new(),
        }
    }
    /// Whether a server answering in `candidate_ms` is faster than one answering in
    /// `current_ms` by both margins. Equal answers never win, whatever the margins.
    pub fn beats(&self, candidate_ms: u64, current_ms: u64) -> bool {
        let gain = current_ms.saturating_sub(candidate_ms);
        gain > 0
            && gain >= self.threshold_ms
            && u128::from(gain) * 100 >= u128::from(current_ms) * u128::from(self.threshold_percent)
    }
}
/// The middle of the answers, when there are enough of them to trust: three give the middle one,
/// two give the slower (a server that failed once is judged by its worse answer), one gives none.
pub(crate) fn median(mut samples: Vec<u64>) -> Option<u64> {
    samples.sort_unstable();
    match samples.len() {
        0 | 1 => None,
        2 => Some(samples[1]),
        n => Some(samples[n / 2]),
    }
}
#[cfg(test)]
mod tests {
    use super::median;

    #[test]
    fn the_middle_of_the_answers_when_there_are_enough_of_them() {
        assert_eq!(median(vec![]), None);
        assert_eq!(median(vec![7]), None);
        // Two answers: the slower, a server that failed once is judged by its worse answer.
        assert_eq!(median(vec![9, 4]), Some(9));
        assert_eq!(median(vec![30, 10, 20]), Some(20));
        // More than three is not asked for today, but the middle stays the middle.
        assert_eq!(median(vec![4, 1, 3, 2]), Some(3));
        assert_eq!(median(vec![5, 1, 4, 2, 3]), Some(3));
    }
}
/// One answer of a server through the proxy; the outer error is a cancelled cycle.
pub(crate) async fn proxy_once<P: Probe + ?Sized>(
    probe: &P,
    server: &Server,
    cancel: &CancellationToken,
) -> Result<Result<u64>> {
    tokio::select! {biased;
        _=cancel.cancelled()=>Err(Error::Cancelled),
        result=probe.proxy(server)=>match result {
            Err(Error::Cancelled)=>Err(Error::Cancelled),
            result=>Ok(result),
        },
    }
}
/// One connection to the server itself; the outer error is a cancelled cycle.
async fn endpoint_once<P: Probe + ?Sized>(
    probe: &P,
    server: &Server,
    cancel: &CancellationToken,
) -> Result<Result<u64>> {
    tokio::select! {biased;
        _=cancel.cancelled()=>Err(Error::Cancelled),
        result=probe.endpoint(server)=>Ok(result),
    }
}
/// The server in use gets one more try when it merely timed out: a missed answer there costs
/// a switch away from a connection that works, while a missed answer of any other server
/// costs nothing.
pub(crate) async fn probe_in_use<P: Probe + ?Sized>(
    probe: &P,
    server: &Server,
    cancel: &CancellationToken,
) -> Result<Measurement> {
    let mut result = proxy_once(probe, server, cancel).await?;
    if matches!(&result, Err(e) if e.failure_code() == Some(FailureCode::ProxyTimeout)) {
        result = proxy_once(probe, server, cancel).await?;
    }
    Ok(Measurement::from_result(
        result,
        FailureCode::ProxyTransport,
    ))
}
/// The pace of a comparison. While a connection works (the server in use answered, or one was
/// taken in its place in this cycle) the servers are measured one after another with a random
/// pause between them, and the server in use is asked now and then; while nothing works there are
/// no pauses, the first server that answers is wanted at once.
struct Pacer<'a, P: ?Sized> {
    probe: &'a P,
    jitter: &'a Jitter,
    cancel: &'a CancellationToken,
    in_use: Option<&'a Server>,
    /// The server in use answered, and still did when it was last asked.
    in_use_ok: AtomicBool,
    /// A server taken in this cycle because nothing else worked.
    stand_in: AtomicBool,
    last_asked: Mutex<Instant>,
}
impl<'a, P: Probe + ?Sized> Pacer<'a, P> {
    fn new(
        probe: &'a P,
        jitter: &'a Jitter,
        cancel: &'a CancellationToken,
        in_use: Option<&'a Server>,
    ) -> Self {
        Self {
            probe,
            jitter,
            cancel,
            in_use,
            in_use_ok: AtomicBool::new(false),
            stand_in: AtomicBool::new(false),
            last_asked: Mutex::new(Instant::now()),
        }
    }
    fn paced(&self) -> bool {
        self.in_use_ok() || self.stand_in.load(Ordering::SeqCst)
    }
    fn in_use_ok(&self) -> bool {
        self.in_use_ok.load(Ordering::SeqCst)
    }
    fn asked_just_now(&self) {
        *self.last_asked.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }
    /// The wait before the measurement of the next of `servers` servers, if a connection works.
    async fn gap(&self, servers: usize) -> Result<()> {
        let pause = self.jitter.pause(servers);
        if pause.is_zero() {
            return Ok(());
        }
        // Asks the server in use again if it is due, and finds out whether a connection works.
        self.keep_watch().await?;
        if !self.paced() {
            return Ok(());
        }
        tokio::select! {biased;
            _=self.cancel.cancelled()=>Err(Error::Cancelled),
            _=tokio::time::sleep(pause)=>Ok(()),
        }
    }
    /// Asks the server in use again when it has not been asked for a while. Two misses in a row
    /// mean the connection is gone: nothing is paced after that, and the first server that answers
    /// takes its place.
    async fn keep_watch(&self) -> Result<()> {
        let Some(server) = self.in_use.filter(|_| self.in_use_ok()) else {
            return Ok(());
        };
        let last = *self.last_asked.lock().unwrap_or_else(|e| e.into_inner());
        if Instant::now() < last + self.jitter.spread(LIFELINE_EVERY) {
            return Ok(());
        }
        let mut answered = probe_in_use(self.probe, server, self.cancel)
            .await?
            .latency_ms
            .is_some();
        if !answered {
            answered = proxy_once(self.probe, server, self.cancel).await?.is_ok();
        }
        self.asked_just_now();
        if !answered {
            self.in_use_ok.store(false, Ordering::SeqCst);
        }
        Ok(())
    }
}
async fn switch_to<P: Probe + ?Sized>(
    probe: &P,
    server: &Server,
    cancel: &CancellationToken,
    emit: &mut (impl FnMut(Progress) + Send),
) -> Result<Instant> {
    // A switch is two calls to the local API, the selector and then its rules. A cancellation does
    // not cut it in the middle, which would leave the traffic on the new server with the rules of
    // the old one: it is begun or it is not. But an API that does not answer does not hold the
    // cancellation for long either.
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let switch = async {
        probe.select(&server.id).await?;
        probe.routes(&server.routes_mode()).await
    };
    tokio::pin!(switch);
    let switched = tokio::select! {
        result = &mut switch => result,
        () = async {
            cancel.cancelled().await;
            tokio::time::sleep(SWITCH_GRACE).await;
        } => return Err(Error::Cancelled),
    };
    if let Err(error) = switched {
        emit(Progress::SelectionFailed(
            server.id.clone(),
            FailureCode::SelectorSwitch,
        ));
        return Err(error.with_code(FailureCode::SelectorSwitch));
    }
    let now = Instant::now();
    emit(Progress::Selected(server.id.clone(), now));
    Ok(now)
}
/// `run_with` with the default thresholds.
pub async fn run<P: Probe + ?Sized>(
    probe: &P,
    servers: &[Server],
    current: Option<&str>,
    concurrency: usize,
    cancel: &CancellationToken,
    emit: impl FnMut(Progress) + Send,
) -> Result<Outcome> {
    run_with(
        probe,
        servers,
        current,
        Options::with_concurrency(concurrency),
        cancel,
        emit,
    )
    .await
}
/// Network-independent algorithm of a full check. Event callback fires immediately after a
/// successful switch.
///
/// The server in use is asked first. Then the rest are compared, and a working server in use is
/// left only for one that is faster by both thresholds; when nothing in use works, the first
/// working server is taken at once and the fastest follows.
pub async fn run_with<P: Probe + ?Sized>(
    probe: &P,
    servers: &[Server],
    current: Option<&str>,
    options: Options,
    cancel: &CancellationToken,
    mut emit: impl FnMut(Progress) + Send,
) -> Result<Outcome> {
    if options.concurrency == 0 {
        // i18n-ignore: a guard against a caller bug, no input reaches it
        return Err(Error::Invalid("Concurrency must be positive".into()));
    }
    // Store keeps source-owned duplicate rows, but a selection cycle probes the canonical
    // outbound once. Preserve input order so the first eligible row is the representative.
    let mut seen = HashSet::new();
    let mut candidates = Vec::with_capacity(servers.len());
    for server in servers {
        if seen.insert(server.id.as_str()) {
            candidates.push(server);
        }
    }
    // Servers the caller has just found dead are not measured again, the one in use excepted.
    candidates.retain(|s| !options.skip.contains(&s.id) || Some(s.id.as_str()) == current);
    // Held-back servers wait, except the one in use.
    let (mut candidates, held): (Vec<&Server>, Vec<&Server>) = candidates
        .into_iter()
        .partition(|s| !options.held_back.contains(&s.id) || Some(s.id.as_str()) == current);
    let mut selected = current.map(str::to_owned);
    let mut last_switch = None;
    let in_use = current.and_then(|id| candidates.iter().copied().find(|s| s.id == id));
    // Whether the server in use was among those asked: if it is not working in the end, it was dead.
    let in_use_asked = in_use.is_some();
    let pacer = Pacer::new(probe, &options.jitter, cancel, in_use);
    let mut in_use_ms = None;
    let mut in_use_endpoint = None;
    if let Some(server) = in_use {
        // A server the caller has just found dead is not asked again: it is dead for this check.
        if !options.current_lost {
            // The distance to the server is measured too, beside the answer through it: the table
            // shows both for the server in use, and a failed distance changes no decision.
            let (endpoint, measurement) = tokio::join!(
                endpoint_once(probe, server, cancel),
                probe_in_use(probe, server, cancel),
            );
            let endpoint = Measurement::from_result(endpoint?, FailureCode::EndpointConnect);
            in_use_endpoint = endpoint.latency_ms;
            emit(Progress::Endpoint(server.id.clone(), endpoint));
            let measurement = measurement?;
            in_use_ms = measurement.latency_ms;
            pacer.in_use_ok.store(in_use_ms.is_some(), Ordering::SeqCst);
            pacer.asked_just_now();
            emit(Progress::Current(server.id.clone(), measurement));
        }
        // Already asked, working or not: neither phase below repeats it.
        candidates.retain(|s| s.id != server.id);
    }
    let mut reachable: Vec<(u64, &Server)> = vec![];
    let mut working: Vec<(u64, &Server)> = vec![];
    // Servers whose answer through the proxy is already known: the scan does not ask them again.
    let mut proxied: HashSet<String> = HashSet::new();
    // Nothing in use works: the pool, the best servers of the last full check, is asked first, and
    // the first of them that answers is taken at once.
    if in_use_ms.is_none() && !options.prefer.is_empty() {
        let order: Vec<&Server> = options
            .prefer
            .iter()
            .filter_map(|id| candidates.iter().copied().find(|s| &s.id == id))
            .collect();
        if !order.is_empty() {
            let (tried, winner) =
                try_first(probe, &order, options.concurrency, cancel, &mut emit).await?;
            let asked: HashSet<String> = tried.iter().map(|t| t.server.id.clone()).collect();
            for attempt in &tried {
                if let Some(ms) = attempt.endpoint.latency_ms {
                    reachable.push((ms, attempt.server));
                }
                if let Some(proxy) = &attempt.proxy {
                    proxied.insert(attempt.server.id.clone());
                    if let Some(ms) = proxy.latency_ms {
                        working.push((ms, attempt.server));
                    }
                }
            }
            if let Some(winner) = winner {
                let server = tried[winner].server;
                pacer.stand_in.store(true, Ordering::SeqCst);
                last_switch = Some(switch_to(probe, server, cancel, &mut emit).await?);
                selected = Some(server.id.clone());
            }
            candidates.retain(|s| !asked.contains(&s.id));
        }
    }
    // Nothing in use works and nobody else is left to try: the servers held back are all there is.
    if in_use_ms.is_none() && candidates.is_empty() && working.is_empty() {
        candidates = held;
    }
    // Not in the order of the subscription, and with a pause before each while a connection
    // works: a burst to every server in the same order at the same pace is easy to recognise.
    options.jitter.shuffle(&mut candidates);
    let total = candidates.len();
    let pacing = !options.jitter.pause(total).is_zero();
    // One turn at a time: the pauses follow each other, whatever the number of servers in flight.
    let turn = tokio::sync::Mutex::new(());
    let mut endpoint_futures = Vec::with_capacity(total);
    for server in candidates {
        let (pacer, turn) = (&pacer, &turn);
        endpoint_futures.push(async move {
            if pacing {
                let _turn = turn.lock().await;
                pacer.gap(total).await?;
            }
            let measurement = Measurement::from_result(
                probe.endpoint(server).await,
                FailureCode::EndpointConnect,
            );
            Ok::<_, Error>((server, measurement))
        });
    }
    let endpoints = stream::iter(endpoint_futures).buffer_unordered(options.concurrency);
    tokio::pin!(endpoints);
    loop {
        tokio::select! { biased;
            _=cancel.cancelled()=>return Err(Error::Cancelled),
            next=endpoints.next()=>match next{
                Some(Ok((s,measurement)))=>{
                    let latency = measurement.latency_ms;
                    emit(Progress::Endpoint(s.id.clone(),measurement));
                    if let Some(ms)=latency{reachable.push((ms,s));}
                },
                Some(Err(error))=>return Err(error),
                None=>break,
            }
        }
    }
    reachable.sort_by(|(a, sa), (b, sb)| a.cmp(b).then_with(|| sa.id.cmp(&sb.id)));
    for (_, s) in &reachable {
        if proxied.contains(&s.id) {
            continue;
        }
        pacer.gap(reachable.len()).await?;
        let measurement = Measurement::from_result(
            proxy_once(probe, s, cancel).await?,
            FailureCode::ProxyTransport,
        );
        let latency = measurement.latency_ms;
        emit(Progress::Proxy(s.id.clone(), measurement));
        if let Some(ms) = latency {
            // A working server in use needs no stand-in; without one the first that answers is
            // taken, and so is the next one if the connection was lost in the meantime.
            if !pacer.paced() {
                pacer.stand_in.store(true, Ordering::SeqCst);
                last_switch = Some(switch_to(probe, s, cancel, &mut emit).await?);
                selected = Some(s.id.clone());
            }
            working.push((ms, s));
        }
    }
    let in_use = in_use.filter(|_| pacer.in_use_ok());
    if let (Some(server), Some(ms)) = (in_use, in_use_ms) {
        working.push((ms, server));
    }
    working.sort_by(|(a, sa), (b, sb)| a.cmp(b).then_with(|| sa.id.cmp(&sb.id)));
    if working.is_empty() {
        return Err(Error::Unavailable);
    }
    // One answer is noise: the fastest few, and the one in use, are measured again and compared
    // by the middle of their answers. Alone there is nothing to compare with.
    let mut steady: Vec<(u64, &Server)> = vec![];
    if working.len() >= 2 {
        let hosts: Vec<String> = working.iter().map(|(_, s)| s.host.clone()).collect();
        let mut again: Vec<(u64, &Server)> = working
            .iter()
            .take(pool::refine_count(&hosts, options.pool_size))
            .copied()
            .collect();
        if let (Some(server), Some(ms)) = (in_use, in_use_ms) {
            if !again.iter().any(|(_, p)| p.id == server.id) {
                again.push((ms, server));
            }
        }
        let again_total = again.len();
        for (first, s) in again {
            pacer.gap(again_total).await?;
            // The distance is sampled the same way, when the first sample found it.
            let first_endpoint = in_use
                .filter(|server| server.id == s.id)
                .and(in_use_endpoint)
                .or_else(|| {
                    reachable
                        .iter()
                        .find(|(_, r)| r.id == s.id)
                        .map(|(ms, _)| *ms)
                });
            let mut endpoint_samples: Vec<u64> = first_endpoint.into_iter().collect();
            if !endpoint_samples.is_empty() {
                for _ in 0..EXTRA_SAMPLES {
                    if let Ok(ms) = endpoint_once(probe, s, cancel).await? {
                        endpoint_samples.push(ms);
                    }
                }
            }
            let mut samples = vec![first];
            for _ in 0..EXTRA_SAMPLES {
                if let Ok(ms) = proxy_once(probe, s, cancel).await? {
                    samples.push(ms);
                }
            }
            if let Some(middle) = median(samples) {
                emit(Progress::Median {
                    id: s.id.clone(),
                    proxy_ms: middle,
                    endpoint_ms: median(endpoint_samples),
                });
                steady.push((middle, s));
            }
        }
        steady.sort_by(|(a, sa), (b, sb)| a.cmp(b).then_with(|| sa.id.cmp(&sb.id)));
    }
    // The pool: the best of the steady servers. A single working server is not compared with
    // anyone, its one answer stands for its middle.
    let ranking: Vec<Ranked> = if working.len() == 1 {
        &working
    } else {
        &steady
    }
    .iter()
    .filter(|(_, s)| !options.keep_out_of_pool.contains(&s.id))
    .map(|(ms, s)| Ranked {
        id: s.id.clone(),
        host: s.host.clone(),
        ms: *ms,
    })
    .collect();
    let pool = pool::build(&ranking, options.pool_size);
    let in_use_steady = in_use.and_then(|server| {
        steady
            .iter()
            .find(|(_, s)| s.id == server.id)
            .map(|(ms, _)| *ms)
    });
    let target: &Server = match (in_use, in_use_steady, steady.first().copied()) {
        // The pool is not complete yet: the best server is taken, whichever is in use.
        (_, _, Some((_, best))) if options.pool_incomplete => best,
        // A steady server in use is left only for a clearly faster one.
        (Some(server), Some(server_ms), Some((best_ms, best))) => {
            if best.id == server.id || !options.beats(best_ms, server_ms) {
                server
            } else {
                best
            }
        }
        // One that answered but not reliably gives way to a steady one.
        (Some(_), None, Some((_, best))) => best,
        (Some(server), _, None) => server,
        // Nothing in use works: the server taken in this check as a stand-in gives way to the best.
        (None, _, Some((_, best))) => best,
        // Too few answers to compare: the fastest single one.
        (None, _, None) => working[0].1,
    };
    // The delay of the target, the middle where there is one.
    let target_ms = steady
        .iter()
        .chain(working.iter())
        .find(|(_, s)| s.id == target.id)
        .map_or(0, |(ms, _)| *ms);
    let stayed = current == Some(target.id.as_str());
    let decision = if stayed {
        match (steady.first().copied(), in_use_steady.or(in_use_ms)) {
            (Some((best_ms, best)), Some(ms)) if best.id != target.id => {
                Decision::Kept { ms, best_ms }
            }
            (_, ms) => Decision::Best {
                ms: ms.unwrap_or(target_ms),
            },
        }
    } else if in_use.is_some() {
        match in_use_steady {
            None => Decision::Unreliable { to_ms: target_ms },
            Some(from_ms) if options.pool_incomplete => Decision::Pool {
                from_ms: Some(from_ms),
                to_ms: target_ms,
            },
            Some(from_ms) => Decision::Faster {
                from_ms,
                to_ms: target_ms,
            },
        }
    } else if in_use_asked {
        Decision::Lost { to_ms: target_ms }
    } else if options.pool_incomplete && current.is_some() {
        Decision::Pool {
            from_ms: None,
            to_ms: target_ms,
        }
    } else {
        Decision::Chose { ms: target_ms }
    };
    if selected.as_deref() != Some(&target.id) {
        last_switch = Some(switch_to(probe, target, cancel, &mut emit).await?);
    }
    Ok(Outcome {
        decision,
        selected: target.id.clone(),
        last_switch,
        pool,
        ranking,
    })
}
/// What asking one server of the pool ahead of the others came to.
struct Tried<'a> {
    server: &'a Server,
    endpoint: Measurement,
    /// Only asked when the distance answered.
    proxy: Option<Measurement>,
}
/// Asks the servers of `order` (best first), at most `concurrency` at once and without pauses:
/// nothing works, and the first that answers through its proxy is wanted at once. Stops at it; the
/// attempts that were still going on are dropped and ask again in the scan. Says what each attempt
/// found when it ends, and returns the attempts that ended and the index of the first that answered.
async fn try_first<'a, P: Probe + ?Sized>(
    probe: &P,
    order: &[&'a Server],
    concurrency: usize,
    cancel: &CancellationToken,
    emit: &mut (impl FnMut(Progress) + Send),
) -> Result<(Vec<Tried<'a>>, Option<usize>)> {
    let mut attempts = Vec::with_capacity(order.len());
    for &server in order {
        attempts.push(async move {
            let endpoint = Measurement::from_result(
                endpoint_once(probe, server, cancel).await?,
                FailureCode::EndpointConnect,
            );
            let proxy = if endpoint.latency_ms.is_some() {
                Some(Measurement::from_result(
                    proxy_once(probe, server, cancel).await?,
                    FailureCode::ProxyTransport,
                ))
            } else {
                None
            };
            Ok::<_, Error>(Tried {
                server,
                endpoint,
                proxy,
            })
        });
    }
    let attempts = stream::iter(attempts).buffer_unordered(concurrency);
    tokio::pin!(attempts);
    let mut tried = vec![];
    loop {
        tokio::select! { biased;
            _=cancel.cancelled()=>return Err(Error::Cancelled),
            next=attempts.next()=>match next {
                Some(Ok(attempt))=>{
                    emit(Progress::Endpoint(attempt.server.id.clone(), attempt.endpoint.clone()));
                    if let Some(proxy)=&attempt.proxy {
                        emit(Progress::Proxy(attempt.server.id.clone(), proxy.clone()));
                    }
                    let answered = attempt.proxy.as_ref().is_some_and(|p| p.latency_ms.is_some());
                    tried.push(attempt);
                    if answered {
                        let winner = tried.len() - 1;
                        return Ok((tried, Some(winner)));
                    }
                },
                Some(Err(error))=>return Err(error),
                None=>return Ok((tried, None)),
            }
        }
    }
}
/// One turn at a time: the pause before the next of `total` servers is waited out holding the turn,
/// so the pauses follow each other whatever the number of servers in flight.
pub(crate) async fn wait_turn(
    turn: &tokio::sync::Mutex<()>,
    jitter: &Jitter,
    total: usize,
    cancel: &CancellationToken,
) -> Result<()> {
    let _turn = turn.lock().await;
    let pause = jitter.pause(total);
    if pause.is_zero() {
        return Ok(());
    }
    tokio::select! {biased;
        _=cancel.cancelled()=>Err(Error::Cancelled),
        _=tokio::time::sleep(pause)=>Ok(()),
    }
}
/// Asks servers that did not answer once more, apart from any selection: the distance of each, and
/// when that answers the answer through the server, once. At most `concurrency` at the same time,
/// each begun after a pause of its own, in a random order. Nothing is switched. What came of each
/// server is told by the events (`Endpoint`, and `Proxy` when the distance answered); a cancelled
/// pass ends with `Cancelled` after the events of the servers that were finished.
pub async fn recheck<P: Probe + ?Sized>(
    probe: &P,
    servers: &[Server],
    concurrency: usize,
    jitter: &Jitter,
    cancel: &CancellationToken,
    mut emit: impl FnMut(Progress) + Send,
) -> Result<()> {
    if concurrency == 0 {
        // i18n-ignore: a guard against a caller bug, no input reaches it
        return Err(Error::Invalid("Concurrency must be positive".into()));
    }
    let mut list: Vec<&Server> = servers.iter().collect();
    jitter.shuffle(&mut list);
    let total = list.len();
    // One turn at a time, as in a comparison: the pauses follow each other.
    let turn = tokio::sync::Mutex::new(());
    let mut asked = Vec::with_capacity(total);
    for server in list {
        let turn = &turn;
        asked.push(async move {
            wait_turn(turn, jitter, total, cancel).await?;
            let endpoint = Measurement::from_result(
                endpoint_once(probe, server, cancel).await?,
                FailureCode::EndpointConnect,
            );
            let proxy = if endpoint.latency_ms.is_some() {
                Some(Measurement::from_result(
                    proxy_once(probe, server, cancel).await?,
                    FailureCode::ProxyTransport,
                ))
            } else {
                None
            };
            Ok::<_, Error>((server, endpoint, proxy))
        });
    }
    let answers = stream::iter(asked).buffer_unordered(concurrency);
    tokio::pin!(answers);
    loop {
        tokio::select! { biased;
            _=cancel.cancelled()=>return Err(Error::Cancelled),
            next=answers.next()=>match next {
                Some(Ok((server, endpoint, proxy)))=>{
                    emit(Progress::Endpoint(server.id.clone(), endpoint));
                    if let Some(proxy)=proxy {
                        emit(Progress::Proxy(server.id.clone(), proxy));
                    }
                },
                Some(Err(error))=>return Err(error),
                None=>return Ok(()),
            }
        }
    }
}
/// A check owed or due: which one, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub cycle: Cycle,
    pub reason: Reason,
}
impl Plan {
    pub fn full(reason: Reason) -> Self {
        Self {
            cycle: Cycle::Full,
            reason,
        }
    }
    pub fn fast(reason: Reason) -> Self {
        Self {
            cycle: Cycle::Fast,
            reason,
        }
    }
}
/// Coordinator scheduling state. Only the owning actor may start/finish a cycle.
///
/// There are two timers. The fast check is due `interval` after the end of any check, the full check
/// `full_interval` after the end of the last full one; each wait is spread by `jitter`. After a
/// switch no automatic check starts before the switch plus the interval, failover excepted.
#[derive(Debug)]
pub struct Schedule {
    pub running: bool,
    pub pending: Option<Plan>,
    fast_deadline: Instant,
    full_deadline: Instant,
    pub last_switch: Option<Instant>,
    /// No automatic cycle starts before this: the last switch plus the interval, spread once
    /// when the switch happened.
    cooldown_until: Option<Instant>,
    interval: Duration,
    full_interval: Duration,
    jitter: Jitter,
    cycle_switched: bool,
    /// Which kind of check is running.
    cycle: Option<Cycle>,
}
impl Schedule {
    pub fn new(interval: Duration, full_interval: Duration, jitter: Jitter) -> Self {
        let now = Instant::now();
        Self {
            running: false,
            pending: None,
            fast_deadline: now,
            full_deadline: now,
            last_switch: None,
            cooldown_until: None,
            interval,
            full_interval,
            jitter,
            cycle_switched: false,
            cycle: None,
        }
    }
    /// Queues a check. The full check wins over the fast one, and of two full checks the one that
    /// was asked for first keeps its reason.
    pub fn request(&mut self, plan: Plan) {
        let keep = match self.pending {
            Some(queued) => queued.cycle == Cycle::Full || plan.cycle == Cycle::Fast,
            None => false,
        };
        if !keep {
            self.pending = Some(plan);
        }
    }
    /// When the next automatic check is due: the sooner of the two timers.
    pub fn deadline(&self) -> Instant {
        self.fast_deadline.min(self.full_deadline)
    }
    /// The intervals changed in the settings: a timer whose interval did not change keeps its time.
    pub fn set_intervals(&mut self, interval: Duration, full_interval: Duration) {
        // Saving any other setting must not push a check away.
        if interval != self.interval {
            self.interval = interval;
            self.cooldown_until = self.last_switch.map(|at| at + self.jitter.spread(interval));
            self.fast_deadline = self
                .cooldown_until
                .unwrap_or_else(|| Instant::now() + self.jitter.spread(interval));
        }
        if full_interval != self.full_interval {
            self.full_interval = full_interval;
            self.full_deadline = Instant::now() + self.jitter.spread(full_interval);
        }
    }
    pub fn begin(&mut self, now: Instant) -> Option<Plan> {
        if self.running || self.cooldown_until.is_some_and(|until| now < until) {
            return None;
        }
        let plan = self.pending.take().or_else(|| {
            if now >= self.full_deadline {
                Some(Plan::full(Reason::Scheduled))
            } else if now >= self.fast_deadline {
                Some(Plan::fast(Reason::Scheduled))
            } else {
                None
            }
        })?;
        self.running = true;
        self.cycle_switched = false;
        self.cycle = Some(plan.cycle);
        Some(plan)
    }
    /// The check that was begun as a fast one is a full one: the worker gave it a full plan (the
    /// list is no longer than the pool, the network came back). Its end restarts the full timer.
    pub fn became_full(&mut self) {
        if let Some(cycle) = self.cycle.as_mut() {
            *cycle = Cycle::Full;
        }
    }
    /// Starts a full check now, whatever the cooldown says: the connection is lost and waiting out
    /// the interval would only keep it that way. It takes the place of any queued check. `None`
    /// while a check runs.
    pub fn begin_urgent(&mut self, reason: Reason) -> Option<Plan> {
        if self.running {
            return None;
        }
        self.pending = None;
        self.running = true;
        self.cycle_switched = false;
        self.cycle = Some(Cycle::Full);
        Some(Plan::full(reason))
    }
    /// The check that was begun did not run, for lack of a network: nothing starts before the next
    /// regular deadline, and a full check that was asked for stays owed.
    pub fn skipped(&mut self, plan: Plan, now: Instant) {
        self.running = false;
        self.cycle = None;
        let next = now + self.jitter.spread(self.interval);
        self.fast_deadline = next;
        if plan.cycle == Cycle::Full {
            self.full_deadline = next;
        }
        self.cooldown_until = Some(self.cooldown_until.map_or(next, |until| until.max(next)));
        if plan.cycle == Cycle::Full && plan.reason != Reason::Scheduled {
            self.request(plan);
        }
    }
    /// Time until a queued or automatic cycle may start. Call only while Auto Select is
    /// enabled or a manual cycle is pending.
    pub fn due_in(&self, now: Instant) -> Duration {
        let cooldown = self.cooldown_until.unwrap_or(now);
        let due = if self.pending.is_some() {
            cooldown
        } else {
            self.deadline().max(cooldown)
        };
        due.saturating_duration_since(now)
    }
    pub fn switched(&mut self, at: Instant) {
        self.last_switch = Some(at);
        self.cooldown_until = Some(at + self.jitter.spread(self.interval));
        self.cycle_switched = true;
    }
    pub fn finish(&mut self, now: Instant) {
        self.running = false;
        let cycle = self.cycle.take();
        let mut fast = now + self.jitter.spread(self.interval);
        if let (true, Some(until)) = (self.cycle_switched, self.cooldown_until) {
            fast = fast.max(until);
        }
        self.fast_deadline = fast;
        // A full check looks at the pool too: the fast check has nothing to add before its turn.
        if cycle == Some(Cycle::Full) {
            self.full_deadline = now + self.jitter.spread(self.full_interval);
        }
    }
    pub fn automatic_due(&self, now: Instant) -> bool {
        !self.running && now >= self.deadline()
    }
}
