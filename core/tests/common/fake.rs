//! A coordinator over fakes: a runtime that only flips a flag, a probe whose answers the test
//! sets by port, and no subscriptions. Time is the test's (`start_paused`).
use async_trait::async_trait;
use rift_core::{
    application::{Application, Snapshot},
    config,
    jitter::Jitter,
    model::{Server, Store},
    parser::parse_uri,
    runtime::Runtime,
    selection::Probe,
    subscription::SubscriptionSource,
    Error, Result,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Notify;

pub const SECRET: &str = "12345678901234567890123456789012";

pub struct FakeRuntime(Arc<AtomicBool>);
#[async_trait]
impl Runtime for FakeRuntime {
    async fn validate(&mut self, store: &Store, secret: &str) -> Result<()> {
        config::generate(store, secret).map(|_| ())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        self.0.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
pub struct NoFeed;
#[async_trait]
impl SubscriptionSource for NoFeed {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        Err(Error::Network)
    }
}

/// Answers through each port (`None` = no answer); the log says who was asked.
#[derive(Default)]
pub struct Net {
    pub proxy: Mutex<HashMap<u16, Option<u64>>>,
    /// Answers given once, in order, before the standing answer of the port.
    pub once: Mutex<HashMap<u16, VecDeque<Option<u64>>>>,
    pub asked: Mutex<Vec<u16>>,
    pub selects: Mutex<Vec<String>>,
    pub gate: Mutex<bool>,
    pub entered: Notify,
    pub release: Notify,
}
impl Net {
    pub fn answer(&self, port: u16, ms: Option<u64>) {
        self.proxy.lock().unwrap().insert(port, ms);
    }
    /// The next questions through `port` get these answers, one each; after them the standing answer.
    pub fn answer_next(&self, port: u16, answers: &[Option<u64>]) {
        self.once
            .lock()
            .unwrap()
            .entry(port)
            .or_default()
            .extend(answers.iter().copied());
    }
    pub fn asked(&self) -> Vec<u16> {
        self.asked.lock().unwrap().clone()
    }
    pub fn count(&self, port: u16) -> usize {
        self.asked().iter().filter(|p| **p == port).count()
    }
}
#[async_trait]
impl Probe for Net {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(10)
    }
    async fn proxy(&self, s: &Server) -> Result<u64> {
        self.asked.lock().unwrap().push(s.port);
        let gated = std::mem::take(&mut *self.gate.lock().unwrap());
        if gated {
            self.entered.notify_one();
            self.release.notified().await;
        }
        let once = self
            .once
            .lock()
            .unwrap()
            .get_mut(&s.port)
            .and_then(VecDeque::pop_front);
        once.unwrap_or_else(|| self.proxy.lock().unwrap().get(&s.port).copied().flatten())
            .ok_or(Error::Network)
    }
    async fn select(&self, id: &str) -> Result<()> {
        self.selects.lock().unwrap().push(id.into());
        Ok(())
    }
}

pub struct H {
    pub app: Arc<Application>,
    pub net: Arc<Net>,
    pub ids: Vec<String>,
    pub _dir: tempfile::TempDir,
}
/// Servers 1 and 2 (ports), the first one chosen; both answer unless `net` says otherwise.
pub fn start(auto: bool, jitter: Jitter) -> H {
    start_on(auto, jitter, |_| {})
}
/// `start`, with the store changed by `edit` first (an API port, say).
pub fn start_on(auto: bool, jitter: Jitter, edit: impl FnOnce(&mut Store)) -> H {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::default();
    store.settings.auto_select = auto;
    let net = Net::default();
    for (port, ms) in [(1, 40), (2, 100)] {
        store
            .servers
            .push(parse_uri(&format!("trojan://p@example.com:{port}"), None).unwrap());
        net.answer(port, Some(ms));
    }
    let ids: Vec<String> = store.servers.iter().map(|s| s.id.clone()).collect();
    store.selected = Some(ids[0].clone());
    edit(&mut store);
    let net = Arc::new(net);
    let app = Application::with_jitter(
        store,
        dir.path().into(),
        Box::new(FakeRuntime(Arc::new(AtomicBool::new(false)))),
        net.clone(),
        SECRET.into(),
        Arc::new(NoFeed),
        jitter,
    );
    app.start();
    H {
        app,
        net,
        ids,
        _dir: dir,
    }
}
/// Servers on ports `1..=n`, each on a host of its own (one reserve per machine), answering through
/// the proxy as `answers` say (`None`: silent); the first is selected. Auto Select as `auto`.
pub fn start_hosts(auto: bool, jitter: Jitter, answers: &[Option<u64>]) -> H {
    start_hosts_on(auto, jitter, answers, Arc::new(NoFeed), |_| {})
}
/// `start_hosts` with a source of subscriptions and a change of the store before it starts.
pub fn start_hosts_on(
    auto: bool,
    jitter: Jitter,
    answers: &[Option<u64>],
    feed: Arc<dyn SubscriptionSource>,
    edit: impl FnOnce(&mut Store),
) -> H {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::default();
    store.settings.auto_select = auto;
    let net = Net::default();
    for (i, ms) in answers.iter().enumerate() {
        let port = u16::try_from(i + 1).unwrap();
        store
            .servers
            .push(parse_uri(&format!("trojan://p@h{port}.example.com:{port}"), None).unwrap());
        net.answer(port, *ms);
    }
    let ids: Vec<String> = store.servers.iter().map(|s| s.id.clone()).collect();
    store.selected = ids.first().cloned();
    edit(&mut store);
    let net = Arc::new(net);
    let app = Application::with_jitter(
        store,
        dir.path().into(),
        Box::new(FakeRuntime(Arc::new(AtomicBool::new(false)))),
        net.clone(),
        SECRET.into(),
        feed,
        jitter,
    );
    app.start();
    H {
        app,
        net,
        ids,
        _dir: dir,
    }
}
pub async fn pass(seconds: u64) {
    tokio::time::sleep(Duration::from_secs(seconds)).await;
}
pub async fn until(app: &Application, what: &str, predicate: impl Fn(&Snapshot) -> bool) {
    let mut rx = app.snapshots.subscribe();
    tokio::time::timeout(Duration::from_secs(120), rx.wait_for(|s| predicate(s)))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}: {:?}", app.snapshot()))
        .unwrap();
}
