//! What the gate's tests share: a fake NanoHUB (an in-process HTTP server on `127.0.0.1`, port
//! 0), the webhook events NanoHUB 0.2.0 posts, a clock the test sets, and a gate over a scratch
//! directory that a test can restart.
//!
//! The events are built with the fields NanoMDM v0.9.0's `service/webhook/service.go` sets for
//! each topic; `recorded` reads the two events NanoMDM's own tests recorded
//! (`tests/fixtures/nanohub/README.md`).

#![allow(
    dead_code,
    reason = "each test binary uses a part of the shared support, and a different part"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use hostkeeper_enrollment::fs::{Fs, RealFs};
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_mdm_gate::{
    ApiKey, ApiUrl, DeviceRecord, DeviceStore, EnrollmentId, FileDeviceStore, Gate, NanoHub,
    Random, StoreError,
};
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use plist::{Dictionary, Value};
use serde_json::json;
use tokio::net::TcpListener;

pub mod baseline;
pub mod enrollment;

/// The UDID (and so the enrollment id) and the serial of NanoMDM's recorded events.
pub const UDID: &str = "66ADE930-5FDF-5EC4-8429-15640684C489";
pub const SERIAL: &str = "C02MT66KFLHH";

/// A second device.
pub const OTHER_UDID: &str = "0D4F1C2E-7B3A-4C55-9E21-5A6B7C8D9E0F";

/// The API key the fake NanoHUB takes.
pub const KEY: &str = "a-nanohub-api-key-for-the-tests";

/// A scratch directory under the integration tests' own temporary directory.
pub fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// The time, as a test sets it.
#[derive(Clone, Debug, Default)]
pub struct Clock(pub Arc<AtomicU64>);

impl Clock {
    pub fn at(now: u64) -> Self {
        Self(Arc::new(AtomicU64::new(now)))
    }

    pub fn set(&self, now: u64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl UnixClock for Clock {
    fn now_unix(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Random bytes that count: each draw fills its buffer with the next number, so every command
/// UUID differs.
pub fn counting() -> Random {
    let next = Arc::new(AtomicU8::new(1));
    Box::new(move |buffer: &mut [u8]| {
        buffer.fill(next.fetch_add(1, Ordering::SeqCst));
        true
    })
}

/// Random bytes that count from `first`: a gate opened after another one in a test draws UUIDs
/// the other did not.
pub fn counting_from(first: u8) -> Random {
    let next = Arc::new(AtomicU8::new(first));
    Box::new(move |buffer: &mut [u8]| {
        buffer.fill(next.fetch_add(1, Ordering::SeqCst));
        true
    })
}

/// Random bytes that count, as many draws as the budget says (`-1`: any number), then none.
pub fn budgeted() -> (Random, Arc<std::sync::atomic::AtomicI64>) {
    let budget = Arc::new(std::sync::atomic::AtomicI64::new(-1));
    let left = Arc::clone(&budget);
    let next = Arc::new(AtomicU8::new(1));
    let random: Random = Box::new(move |buffer: &mut [u8]| {
        let n = left.load(Ordering::SeqCst);
        if n == 0 {
            return false;
        }
        if n > 0 {
            left.store(n - 1, Ordering::SeqCst);
        }
        buffer.fill(next.fetch_add(1, Ordering::SeqCst));
        true
    });
    (random, budget)
}

/// Random bytes that cannot be had.
pub fn no_random() -> Random {
    Box::new(|_: &mut [u8]| false)
}

/// One request the fake NanoHUB heard.
#[derive(Clone, Debug)]
pub struct Heard {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

impl Heard {
    /// The command in the body: its `RequestType` and its `CommandUUID`.
    pub fn command(&self) -> (String, String) {
        let root = Value::from_reader(std::io::Cursor::new(&self.body))
            .expect("a command plist")
            .into_dictionary()
            .expect("a dictionary");
        let uuid = root["CommandUUID"].as_string().expect("a UUID").to_owned();
        let kind = root["Command"].as_dictionary().expect("a Command")["RequestType"]
            .as_string()
            .expect("a RequestType")
            .to_owned();
        (kind, uuid)
    }
}

/// What the fake NanoHUB answers an enqueue.
#[derive(Clone, Debug)]
pub enum Answer {
    /// `200` and NanoMDM's result for one queued command.
    Queued,
    /// This status and body.
    Status(u16, String),
    /// Nothing, ever.
    Hang,
}

/// NanoHUB's API, in process: it checks the basic authentication NanoHUB checks, records each
/// request and answers as the test says.
pub struct FakeNanoHub {
    pub addr: std::net::SocketAddr,
    heard: Arc<Mutex<Vec<Heard>>>,
    answer: Arc<Mutex<Answer>>,
    /// Told after each request is recorded.
    told: Arc<tokio::sync::Notify>,
}

impl FakeNanoHub {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let addr = listener.local_addr().expect("its address");
        let heard = Arc::new(Mutex::new(Vec::new()));
        let answer = Arc::new(Mutex::new(Answer::Queued));
        let told = Arc::new(tokio::sync::Notify::new());
        let (h, a, t) = (Arc::clone(&heard), Arc::clone(&answer), Arc::clone(&told));
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (h, a, t) = (Arc::clone(&h), Arc::clone(&a), Arc::clone(&t));
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        let (h, a, t) = (Arc::clone(&h), Arc::clone(&a), Arc::clone(&t));
                        async move {
                            Ok::<_, std::convert::Infallible>(serve(request, &h, &a, &t).await)
                        }
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self {
            addr,
            heard,
            answer,
            told,
        }
    }

    /// Resolves once at least `n` requests were heard: an explicit progress event, no clock.
    pub async fn heard_at_least(&self, n: usize) {
        while self.heard().len() < n {
            self.told.notified().await;
        }
    }

    pub fn url(&self) -> ApiUrl {
        ApiUrl::parse(&format!("http://{}", self.addr)).expect("a loopback URL")
    }

    pub fn client(&self) -> NanoHub {
        NanoHub::new(self.url(), key())
    }

    pub fn heard(&self) -> Vec<Heard> {
        self.heard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The commands heard, as (type, UUID).
    pub fn commands(&self) -> Vec<(String, String)> {
        self.heard().iter().map(Heard::command).collect()
    }

    pub fn answer(&self, answer: Answer) {
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = answer;
    }
}

async fn serve(
    request: Request<Incoming>,
    heard: &Mutex<Vec<Heard>>,
    answer: &Mutex<Answer>,
    told: &tokio::sync::Notify,
) -> Response<Full<Bytes>> {
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let (authorization, content_type) = (header("authorization"), header("content-type"));
    let (method, path) = (
        request.method().to_string(),
        request.uri().path().to_owned(),
    );
    let body = request
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .unwrap_or_default();
    heard
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Heard {
            method,
            path,
            authorization: authorization.clone(),
            content_type,
            body,
        });
    told.notify_one();
    let wanted = format!("Basic {}", STANDARD.encode(format!("nanohub:{KEY}")));
    if authorization.as_deref() != Some(wanted.as_str()) {
        return reply(401, "Unauthorized\n");
    }
    let answer = answer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    match answer {
        Answer::Queued => reply(200, &json!({"status": {UDID: {}}}).to_string()),
        Answer::Status(status, body) => reply(status, &body),
        Answer::Hang => std::future::pending().await,
    }
}

fn reply(status: u16, body: &str) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body.to_owned())));
    *response.status_mut() = StatusCode::from_u16(status).expect("a status");
    response
}

/// The API key the fake takes.
pub fn key() -> ApiKey {
    let dir = scratch("key");
    let path = dir.path().join("api.key");
    write_mode(&path, KEY.as_bytes(), 0o600);
    ApiKey::read(&path).expect("the key")
}

/// Writes `bytes` to `path` with the mode `mode`.
pub fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, bytes).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// The bytes of a property list.
pub fn plist(dict: Dictionary) -> Vec<u8> {
    let mut bytes = Vec::new();
    Value::Dictionary(dict)
        .to_writer_xml(&mut bytes)
        .expect("a plist");
    bytes
}

/// A dictionary of string and boolean values.
pub fn dict(entries: &[(&str, Value)]) -> Dictionary {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn ids(id: &str, kind: &str) -> serde_json::Value {
    json!({"id": id, "type": kind})
}

/// A check-in event of `topic` (`Authenticate`, `TokenUpdate` or `CheckOut`) of the device
/// channel `id`, whose plist holds `keys` besides `MessageType` and `UDID`.
pub fn checkin(topic: &str, id: &str, keys: &[(&str, Value)], tally: Option<u64>) -> Vec<u8> {
    checkin_of(topic, id, "Device", keys, tally)
}

/// A check-in event of a channel of the type `kind`.
pub fn checkin_of(
    topic: &str,
    id: &str,
    kind: &str,
    keys: &[(&str, Value)],
    tally: Option<u64>,
) -> Vec<u8> {
    let mut payload = dict(keys);
    payload.insert("MessageType".into(), Value::from(topic));
    payload.insert("UDID".into(), Value::from(id));
    let mut event = json!({
        "topic": format!("mdm.{topic}"),
        "created_at": "2026-11-06T12:00:00Z",
        "checkin_event": {
            "ids": ids(id, kind),
            "udid": id,
            "raw_payload": STANDARD.encode(plist(payload)),
        },
    });
    if let Some(tally) = tally {
        event["checkin_event"]["token_update_tally"] = json!(tally);
    }
    serde_json::to_vec(&event).expect("json")
}

pub fn authenticate(id: &str, serial: Option<&str>) -> Vec<u8> {
    let mut keys = vec![("Topic", Value::from("com.apple.mgmt.External.test"))];
    if let Some(serial) = serial {
        keys.push(("SerialNumber", Value::from(serial)));
    }
    checkin("Authenticate", id, &keys, None)
}

pub fn token_update(id: &str, tally: u64) -> Vec<u8> {
    let token = Value::Data(vec![1, 2, 3]);
    checkin("TokenUpdate", id, &[("Token", token)], Some(tally))
}

pub fn check_out(id: &str) -> Vec<u8> {
    checkin("CheckOut", id, &[], None)
}

/// A command result event (`mdm.Connect`) of the device `id`: `status` for the command `uuid`,
/// whose plist holds `keys` besides `CommandUUID`, `Status` and `UDID`.
pub fn result(id: &str, uuid: Option<&str>, status: &str, keys: &[(&str, Value)]) -> Vec<u8> {
    let mut payload = dict(keys);
    if let Some(uuid) = uuid {
        payload.insert("CommandUUID".into(), Value::from(uuid));
    }
    payload.insert("Status".into(), Value::from(status));
    payload.insert("UDID".into(), Value::from(id));
    let mut acknowledge = json!({
        "ids": ids(id, "Device"),
        "udid": id,
        "status": status,
        "raw_payload": STANDARD.encode(plist(payload)),
    });
    if let Some(uuid) = uuid {
        acknowledge["command_uuid"] = json!(uuid);
    }
    serde_json::to_vec(&json!({
        "topic": "mdm.Connect",
        "created_at": "2026-11-06T12:00:00Z",
        "acknowledge_event": acknowledge,
    }))
    .expect("json")
}

/// An `Idle` of the device `id`: it fetched its queue and found it empty.
pub fn idle(id: &str) -> Vec<u8> {
    result(id, None, "Idle", &[])
}

/// The `DeviceInformation` answer of the device `id` to the command `uuid`.
pub fn device_information(
    id: &str,
    uuid: &str,
    serial: Option<&str>,
    supervised: Option<bool>,
) -> Vec<u8> {
    let mut answers = vec![("UDID", Value::from(id))];
    if let Some(serial) = serial {
        answers.push(("SerialNumber", Value::from(serial)));
    }
    if let Some(supervised) = supervised {
        answers.push(("IsSupervised", Value::Boolean(supervised)));
    }
    let answers = Value::Dictionary(dict(&answers));
    result(
        id,
        Some(uuid),
        "Acknowledged",
        &[
            ("RequestType", Value::from("DeviceInformation")),
            ("QueryResponses", answers),
        ],
    )
}

/// The `SecurityInfo` answer of the device `id` to the command `uuid`.
pub fn security_info(id: &str, uuid: &str, dep: bool, approved: bool) -> Vec<u8> {
    let status = dict(&[
        ("EnrolledViaDEP", Value::Boolean(dep)),
        ("UserApprovedEnrollment", Value::Boolean(approved)),
        ("IsUserEnrollment", Value::Boolean(false)),
    ]);
    let info = dict(&[("ManagementStatus", Value::Dictionary(status))]);
    result(
        id,
        Some(uuid),
        "Acknowledged",
        &[
            ("RequestType", Value::from("SecurityInfo")),
            ("SecurityInfo", Value::Dictionary(info)),
        ],
    )
}

/// The bytes of a recorded event (`tests/fixtures/nanohub/<name>.json`).
pub fn recorded(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nanohub")
        .join(format!("{name}.json"));
    std::fs::read(path).expect("a recorded event")
}

/// The recorded `DeviceInformation` result, as the answer to the command `uuid`: its
/// `command_uuid`, and the `CommandUUID` in its plist, replaced. Nothing else changes: it
/// answers `HostName` and `UDID`, and no serial and no supervision.
pub fn recorded_device_information(uuid: &str) -> Vec<u8> {
    const RECORDED_UUID: &str = "76eda240-5488-4989-8339-f2ae160113c4";
    let mut event: serde_json::Value =
        serde_json::from_slice(&recorded("DeviceInformation.1")).expect("json");
    let raw = event["acknowledge_event"]["raw_payload"]
        .as_str()
        .expect("a payload")
        .to_owned();
    let payload = String::from_utf8(STANDARD.decode(raw).expect("base64")).expect("utf-8");
    assert!(payload.contains(RECORDED_UUID));
    let payload = payload.replace(RECORDED_UUID, uuid);
    event["acknowledge_event"]["raw_payload"] = json!(STANDARD.encode(payload));
    event["acknowledge_event"]["command_uuid"] = json!(uuid);
    serde_json::to_vec(&event).expect("json")
}

/// A gate over a scratch directory and a fake NanoHUB, which a test can restart.
pub struct Rig {
    pub scratch: Scratch,
    pub hub: FakeNanoHub,
    pub clock: Clock,
    pub gate: Gate,
}

impl Rig {
    pub async fn new(name: &str) -> Self {
        Self::with_random(name, counting()).await
    }

    /// A rig whose gate draws its command UUIDs from `random`.
    pub async fn with_random(name: &str, random: Random) -> Self {
        let scratch = scratch(name);
        let hub = FakeNanoHub::start().await;
        let clock = Clock::at(1_000);
        let fs: Arc<dyn Fs> = Arc::new(RealFs);
        let gate = open_on(fs, &dir_of(&scratch), &hub, &clock, random).expect("the gate opens");
        Self {
            scratch,
            hub,
            clock,
            gate,
        }
    }

    pub fn dir(&self) -> PathBuf {
        dir_of(&self.scratch)
    }

    /// A new gate over the same directory, as after a restart of the coordinator.
    pub fn restart(&mut self) -> Result<(), StoreError> {
        self.gate = open(&self.dir(), &self.hub, &self.clock)?;
        Ok(())
    }

    pub async fn post(&self, body: &[u8]) {
        self.gate.webhook(body).await.expect("the event is taken");
    }

    /// The record of the device `id`.
    pub fn record(&self, id: &str) -> DeviceRecord {
        self.gate
            .records()
            .into_iter()
            .find(|record| record.enrollment_id.as_str() == id)
            .expect("a record of the device")
    }

    /// The UUID of the unanswered command `kind` of the device `id`.
    pub fn sent(&self, id: &str, kind: &str) -> String {
        self.record(id)
            .sent
            .iter()
            .find(|sent| sent.request_type == kind)
            .map(|sent| sent.uuid.as_str().to_owned())
            .expect("an unanswered command of that type")
    }

    /// Takes the device `id` to enrolled, answering `DeviceInformation` with `serial` and
    /// `supervised`, and `SecurityInfo`.
    pub async fn enroll(&self, id: &str, serial: &str, supervised: bool) {
        self.post(&authenticate(id, Some(serial))).await;
        self.post(&token_update(id, 1)).await;
        let uuid = self.sent(id, "DeviceInformation");
        self.post(&device_information(
            id,
            &uuid,
            Some(serial),
            Some(supervised),
        ))
        .await;
        let uuid = self.sent(id, "SecurityInfo");
        self.post(&security_info(id, &uuid, true, true)).await;
    }
}

pub fn dir_of(scratch: &Scratch) -> PathBuf {
    scratch.path().join("mdm-devices")
}

/// The gate over the records in `dir`.
pub fn open(dir: &Path, hub: &FakeNanoHub, clock: &Clock) -> Result<Gate, StoreError> {
    let fs: Arc<dyn Fs> = Arc::new(RealFs);
    open_on(fs, dir, hub, clock, counting())
}

/// The gate over the records in `dir` of the file system `fs`, drawing UUIDs from `random`.
pub fn open_on(
    fs: Arc<dyn Fs>,
    dir: &Path,
    hub: &FakeNanoHub,
    clock: &Clock,
    random: Random,
) -> Result<Gate, StoreError> {
    let store: Box<dyn DeviceStore> = Box::new(FileDeviceStore::open(fs, dir)?);
    Gate::open(store, hub.client(), Arc::new(clock.clone()), random)
}

/// `event` (a result event) with the `command_uuid` of its event object replaced by `uuid`,
/// and its plist left as it was.
pub fn retarget(event: &[u8], uuid: &str) -> Vec<u8> {
    let mut event: serde_json::Value = serde_json::from_slice(event).expect("json");
    event["acknowledge_event"]["command_uuid"] = json!(uuid);
    serde_json::to_vec(&event).expect("json")
}

/// A `Fs` over the real one that fails every write from the `n`-th on (counted from 1; 0 fails
/// none) and shows the file `hidden` as missing when it is read.
#[derive(Default)]
pub struct TestFs {
    writes: AtomicU64,
    fail_from: AtomicU64,
    fail_only: AtomicU64,
    hidden: Mutex<Option<String>>,
}

impl TestFs {
    /// How many writes were asked for so far.
    pub fn writes(&self) -> u64 {
        self.writes.load(Ordering::SeqCst)
    }

    pub fn fail_writes_from(&self, n: u64) {
        self.fail_from.store(n, Ordering::SeqCst);
    }

    /// Fails the `n`-th write only.
    pub fn fail_write(&self, n: u64) {
        self.fail_only.store(n, Ordering::SeqCst);
    }

    pub fn hide(&self, name: &str) {
        *self.hidden.lock().unwrap_or_else(PoisonError::into_inner) = Some(name.to_owned());
    }
}

impl Fs for TestFs {
    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        let hidden = self
            .hidden
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if hidden.is_some_and(|name| path.file_name().is_some_and(|f| f == name.as_str())) {
            return Ok(None);
        }
        RealFs.read(path)
    }

    fn read_private(
        &self,
        path: &Path,
    ) -> std::io::Result<Option<(Vec<u8>, hostkeeper_enrollment::fs::FileStat)>> {
        RealFs.read_private(path)
    }

    fn write_new(&self, path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
        let n = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
        let from = self.fail_from.load(Ordering::SeqCst);
        if (from != 0 && n >= from) || n == self.fail_only.load(Ordering::SeqCst) {
            return Err(std::io::Error::other(format!("write {n} refused")));
        }
        RealFs.write_new(path, bytes, mode)
    }

    fn sync_file(&self, path: &Path) -> std::io::Result<()> {
        RealFs.sync_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        RealFs.rename(from, to)
    }

    fn remove(&self, path: &Path) -> std::io::Result<()> {
        RealFs.remove(path)
    }

    fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
        RealFs.sync_dir(dir)
    }

    fn create_dir(&self, dir: &Path) -> std::io::Result<()> {
        RealFs.create_dir(dir)
    }

    fn list(&self, dir: &Path) -> std::io::Result<Vec<String>> {
        RealFs.list(dir)
    }

    fn lock(&self, path: &Path) -> std::io::Result<hostkeeper_enrollment::fs::FileLock> {
        RealFs.lock(path)
    }
}

/// The enrollment id `id`.
pub fn id(id: &str) -> EnrollmentId {
    EnrollmentId::parse(id).expect("an enrollment id")
}

/// One log event: its `level`, its `message` and its other fields by name.
pub type LogEvent = std::collections::BTreeMap<String, String>;

/// The events a test's subscriber heard.
#[derive(Clone, Default)]
pub struct Logs(Arc<Mutex<Vec<LogEvent>>>);

impl Logs {
    pub fn events(&self) -> Vec<LogEvent> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The `level` and `message` of every event.
    pub fn lines(&self) -> Vec<(String, String)> {
        self.events()
            .into_iter()
            .map(|event| (event["level"].clone(), event["message"].clone()))
            .collect()
    }
}

struct Fields<'a>(&'a mut LogEvent);

impl tracing::field::Visit for Fields<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }
}

impl tracing::Subscriber for Logs {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }

    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = LogEvent::new();
        fields.insert("level".to_owned(), event.metadata().level().to_string());
        event.record(&mut Fields(&mut fields));
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(fields);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// Starts recording this thread's log events (a current-thread test runtime runs every task on
/// it); they stop when the guard drops.
pub fn capture_logs() -> (tracing::subscriber::DefaultGuard, Logs) {
    let logs = Logs::default();
    (tracing::subscriber::set_default(logs.clone()), logs)
}
