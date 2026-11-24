//! Fakes for the elevated TUN helper: a runtime that only records what it is told, and
//! a valid TUN store.
use async_trait::async_trait;
use rift_core::{
    failure::FailureCode,
    model::{Mode, Store},
    parser::parse_uri,
    runtime::Runtime,
    Error, Result,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

pub const SECRET: &str = "0123456789abcdef0123456789abcdef";

pub fn tun_store() -> Store {
    let mut store = Store::default();
    store
        .servers
        .push(parse_uri("trojan://secret@example.com:443#Alpha", None).unwrap());
    store.settings.mode = Mode::Tun;
    store
}

/// What the helper did to its runtime.
pub struct Calls {
    /// Who these calls belong to, and the log of all calls of a test, in order (`label:start`).
    pub label: &'static str,
    pub log: Arc<Mutex<Vec<String>>>,
    pub starts: AtomicUsize,
    pub stops: AtomicUsize,
    /// The stores `start` received, as the JSON the runtime would have built its config from.
    pub stores: Mutex<Vec<Store>>,
    pub secrets: Mutex<Vec<String>>,
    /// Whether the pretend sing-box is running.
    pub alive: AtomicBool,
    /// A status query fails without implying that the owned child exited.
    pub status_error: AtomicBool,
    /// `start` fails with this code once.
    pub fail_next_start: Mutex<Option<FailureCode>>,
}
impl Default for Calls {
    fn default() -> Self {
        Self::labeled("runtime", Arc::default())
    }
}
impl Calls {
    pub fn labeled(label: &'static str, log: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            label,
            log,
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            stores: Mutex::new(vec![]),
            secrets: Mutex::new(vec![]),
            alive: AtomicBool::new(false),
            status_error: AtomicBool::new(false),
            fail_next_start: Mutex::new(None),
        }
    }
    fn note(&self, what: &str) {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:{what}", self.label));
    }
}
pub struct FakeRuntime(pub Arc<Calls>);
#[async_trait]
impl Runtime for FakeRuntime {
    async fn validate(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn start(&mut self, store: &Store, secret: &str) -> Result<()> {
        self.0.starts.fetch_add(1, Ordering::SeqCst);
        self.0.note("start");
        self.0.stores.lock().unwrap().push(store.clone());
        self.0.secrets.lock().unwrap().push(secret.into());
        if let Some(code) = self.0.fail_next_start.lock().unwrap().take() {
            self.0.alive.store(false, Ordering::SeqCst);
            return Err(Error::failure(code, "fake"));
        }
        self.0.alive.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        self.0.stops.fetch_add(1, Ordering::SeqCst);
        self.0.note("stop");
        self.0.alive.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        if self.0.status_error.load(Ordering::SeqCst) {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into());
        }
        Ok(self.0.alive.load(Ordering::SeqCst))
    }
}

// ---- the world around a helper: a fake of the Windows prompt and of the channel ----------------

use rift_core::elevation::{
    client::{BoxStream, Elevator, Launched, Listener, Transport},
    helper::{serve, Exit},
    protocol::VERSION,
};
use std::{collections::HashMap, time::Duration};
use tokio::{sync::oneshot, task::JoinHandle};

/// What the pretend Windows prompt does when it is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prompt {
    /// The user agrees: the helper starts and connects.
    Accept,
    Decline,
    /// The helper could not be started.
    Fail,
    /// The user agrees, but no helper ever connects.
    NeverConnects,
    /// The prompt never answers.
    Hangs,
    /// The helper connects and greets with another token.
    WrongToken,
    /// The helper connects and greets in another version.
    WrongVersion,
    /// The helper connects, greets, and then says nothing.
    Silent,
}
pub struct World {
    pub prompt: Mutex<Prompt>,
    /// Prompts shown: UACs asked of the user.
    pub prompts: AtomicUsize,
    pub log: Arc<Mutex<Vec<String>>>,
    /// The runtime inside the helper (the pretend sing-box with administrator rights).
    pub helper_calls: Arc<Calls>,
    /// The plain runtime (a pretend sing-box without rights).
    pub plain_calls: Arc<Calls>,
    slots: Mutex<HashMap<String, oneshot::Sender<BoxStream>>>,
    pub helpers: Mutex<Vec<JoinHandle<Exit>>>,
    next: AtomicUsize,
}
impl World {
    pub fn new(prompt: Prompt) -> Arc<Self> {
        let log = Arc::new(Mutex::new(vec![]));
        Arc::new(Self {
            prompt: Mutex::new(prompt),
            prompts: AtomicUsize::new(0),
            helper_calls: Arc::new(Calls::labeled("helper", log.clone())),
            plain_calls: Arc::new(Calls::labeled("plain", log.clone())),
            log,
            slots: Mutex::new(HashMap::new()),
            helpers: Mutex::new(vec![]),
            next: AtomicUsize::new(0),
        })
    }
    pub fn prompts(&self) -> usize {
        self.prompts.load(Ordering::SeqCst)
    }
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
    pub fn set_prompt(&self, prompt: Prompt) {
        *self.prompt.lock().unwrap() = prompt;
    }
    pub fn elevator(self: &Arc<Self>) -> Arc<dyn Elevator> {
        Arc::new(FakeElevator(self.clone()))
    }
    pub fn transport(self: &Arc<Self>) -> Arc<dyn Transport> {
        Arc::new(FakeTransport(self.clone()))
    }
    /// How the most recent helper ended, once it has.
    pub async fn last_exit(&self) -> Exit {
        let handle = self
            .helpers
            .lock()
            .unwrap()
            .pop()
            .expect("no helper was started");
        tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("the helper did not end")
            .unwrap()
    }
    /// Ends the most recent helper abruptly, as a crash would.
    pub fn crash_last_helper(&self) {
        self.helpers
            .lock()
            .unwrap()
            .last()
            .expect("no helper")
            .abort();
    }
    pub fn helpers_alive(&self) -> usize {
        self.helpers
            .lock()
            .unwrap()
            .iter()
            .filter(|h| !h.is_finished())
            .count()
    }
}
struct FakeListener {
    name: String,
    receiver: Option<oneshot::Receiver<BoxStream>>,
}
#[async_trait]
impl Listener for FakeListener {
    fn name(&self) -> String {
        self.name.clone()
    }
    async fn accept(&mut self) -> std::io::Result<BoxStream> {
        match self.receiver.take() {
            Some(receiver) => receiver
                .await
                .map_err(|_| std::io::Error::other("the channel was dropped")),
            None => std::future::pending().await,
        }
    }
}
struct FakeTransport(Arc<World>);
impl Transport for FakeTransport {
    fn listen(&self) -> Result<Box<dyn Listener>> {
        let name = format!("fake-pipe-{}", self.0.next.fetch_add(1, Ordering::SeqCst));
        let (sender, receiver) = oneshot::channel();
        self.0.slots.lock().unwrap().insert(name.clone(), sender);
        Ok(Box::new(FakeListener {
            name,
            receiver: Some(receiver),
        }))
    }
}
struct FakeElevator(Arc<World>);
#[async_trait]
impl Elevator for FakeElevator {
    async fn launch(&self, pipe: &str, token: &str) -> Launched {
        let world = &self.0;
        world.prompts.fetch_add(1, Ordering::SeqCst);
        world.log.lock().unwrap().push("prompt".into());
        let behaviour = *world.prompt.lock().unwrap();
        match behaviour {
            Prompt::Decline => return Launched::Declined,
            Prompt::Fail => return Launched::Failed,
            Prompt::Hangs => std::future::pending::<()>().await,
            Prompt::NeverConnects => return Launched::Started,
            _ => {}
        }
        let slot = world
            .slots
            .lock()
            .unwrap()
            .remove(pipe)
            .expect("a pipe that was made");
        let (rift_end, helper_end) = tokio::io::duplex(1024 * 1024);
        let _ = slot.send(Box::new(rift_end));
        let calls = world.helper_calls.clone();
        let token = match behaviour {
            Prompt::WrongToken => "another-token".to_owned(),
            _ => token.to_owned(),
        };
        let handle = match behaviour {
            Prompt::WrongVersion => tokio::spawn(async move {
                use rift_core::elevation::protocol::{write_frame, FromHelper};
                let mut end = helper_end;
                let _ = write_frame(
                    &mut end,
                    &FromHelper::Hello {
                        version: VERSION + 1,
                        token,
                    },
                )
                .await;
                std::future::pending::<Exit>().await
            }),
            Prompt::Silent => tokio::spawn(async move {
                use rift_core::elevation::protocol::{write_frame, FromHelper};
                let mut end = helper_end;
                let _ = write_frame(
                    &mut end,
                    &FromHelper::Hello {
                        version: VERSION,
                        token,
                    },
                )
                .await;
                std::future::pending::<Exit>().await
            }),
            _ => tokio::spawn(async move {
                let mut runtime = FakeRuntime(calls);
                serve(helper_end, &token, &mut runtime, Duration::from_millis(20)).await
            }),
        };
        world.helpers.lock().unwrap().push(handle);
        Launched::Started
    }
}
