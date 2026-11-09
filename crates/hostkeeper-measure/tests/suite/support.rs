//! A fake clock, a simulated fleet behind the coordinator port, and an operator who acts on the
//! fleet when the runner waits for them. Simulations, not measurements: every time below is
//! chosen by the test.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use hostkeeper_measure::clock::Clock;
use hostkeeper_measure::coordinator::{Answer, CallError, Coordinator};
use hostkeeper_measure::params::Params;
use hostkeeper_measure::plan::Plan;
use hostkeeper_measure::record::Record;
use hostkeeper_measure::runner::{self, Io, Names, RunError, Summary};
use hostkeeper_testkit::Scratch;
use serde_json::{Value, json};

/// The unix milliseconds the fake clock starts at.
pub const T0_MS: u64 = 1_800_000_000_000;

/// A clock that moves only when the runner sleeps.
#[derive(Debug)]
pub struct FakeClock {
    now: Cell<u64>,
}

impl Default for FakeClock {
    /// A minute in, so every simulated host has beaten once.
    fn default() -> Self {
        Self {
            now: Cell::new(60_000),
        }
    }
}

impl FakeClock {
    pub fn now(&self) -> u64 {
        self.now.get()
    }
}

impl Clock for FakeClock {
    fn stamp_ms(&self) -> u64 {
        T0_MS + self.now.get()
    }
    fn mono_ms(&self) -> u64 {
        self.now.get()
    }
    fn sleep_ms(&self, ms: u64) {
        self.now.set(self.now.get() + ms);
    }
}

/// A service of a simulated host, as the coordinator would list it.
#[derive(Clone, Debug, Default)]
pub struct Svc {
    pub installed: String,
    pub holds_slot: bool,
    pub health: String,
    pub last_target: Option<String>,
    pub last_outcome: Option<String>,
    pub updating: Option<String>,
}

impl Svc {
    pub fn at(version: &str) -> Self {
        Self {
            installed: version.to_owned(),
            health: "HEALTH_HEALTHY".to_owned(),
            last_target: Some(version.to_owned()),
            last_outcome: Some("OUTCOME_SUCCEEDED".to_owned()),
            ..Self::default()
        }
    }
}

/// A simulated host: it beats every `period_ms` from `offset_ms`, while the coordinator is up.
#[derive(Clone, Debug)]
pub struct SimHost {
    pub node: String,
    pub offset_ms: u64,
    pub period_ms: u64,
    pub services: BTreeMap<String, Svc>,
    /// From when (fake ms) the host exists and beats.
    pub from_ms: u64,
    pub first_heartbeat_ms: Option<u64>,
    pub first_serving_ms: Option<u64>,
    /// Heartbeats off the period: a daemon running an order reports its order and its verdict
    /// at once.
    pub extra_beats: Vec<u64>,
}

impl SimHost {
    pub fn new(node: &str, offset_ms: u64, services: &[(&str, &str)]) -> Self {
        Self {
            node: node.to_owned(),
            offset_ms,
            period_ms: 15_000,
            services: services
                .iter()
                .map(|(s, v)| ((*s).to_owned(), Svc::at(v)))
                .collect(),
            from_ms: 0,
            first_heartbeat_ms: None,
            first_serving_ms: None,
            extra_beats: Vec::new(),
        }
    }
}

/// An audit entry of the simulated coordinator.
#[derive(Clone, Debug)]
pub struct Row {
    pub seq: u64,
    pub action: String,
    pub node: Option<String>,
    pub at_ms: u64,
    pub by_name: String,
}

/// A change the timeline makes to the world at a time.
pub type Change = Box<dyn FnOnce(&mut World)>;

/// What the simulated coordinator knows.
#[derive(Default)]
pub struct World {
    pub hosts: Vec<SimHost>,
    pub audit: Vec<Row>,
    /// Windows (fake ms, start inclusive, end exclusive) in which the coordinator is down.
    pub down: Vec<(u64, u64)>,
    /// The Macs of the MDM gate: id and enrolled-since (unix seconds).
    pub devices: Vec<(String, Option<u64>)>,
    /// How a set-desired rolls out: the first order after the set, the time between two
    /// orders, and how long an update takes (ms).
    pub first_order_ms: u64,
    pub order_gap_ms: u64,
    pub update_ms: u64,
    /// How a force runs: when it starts and how long it takes (ms).
    pub force_start_ms: u64,
    pub force_ms: u64,
    /// How many sets and forces the coordinator was given; each update takes a few seconds
    /// more or less by it, so the values of a run differ.
    pub orders: u64,
}

impl World {
    pub fn audit_at(&mut self, now: u64, action: &str, node: Option<&str>, by: &str) {
        let seq = self.audit.len() as u64 + 1;
        self.audit.push(Row {
            seq,
            action: action.to_owned(),
            node: node.map(str::to_owned),
            at_ms: now,
            by_name: by.to_owned(),
        });
    }

    pub fn host(&mut self, node: &str) -> &mut SimHost {
        self.hosts
            .iter_mut()
            .find(|h| h.node == node)
            .expect("a simulated host")
    }

    fn up(&self, at: u64) -> bool {
        !self.down.iter().any(|(a, b)| (*a..*b).contains(&at))
    }

    /// The host's last heartbeat the coordinator received by `now`, in fake ms.
    fn last_beat(&self, h: &SimHost, now: u64) -> Option<u64> {
        let mut beat = h.from_ms.max(h.offset_ms);
        let mut last = None;
        while beat <= now {
            if self.up(beat) {
                last = Some(beat);
            }
            beat += h.period_ms;
        }
        let extra = h
            .extra_beats
            .iter()
            .copied()
            .filter(|b| *b <= now && *b >= h.from_ms && self.up(*b))
            .max();
        last.max(extra)
    }

    /// The coordinator's time of the last start before `now` (0 for none).
    fn started(&self, now: u64) -> u64 {
        self.down
            .iter()
            .filter(|(_, b)| *b <= now)
            .map(|(_, b)| *b)
            .max()
            .unwrap_or(0)
    }

    fn host_json(&self, h: &SimHost, now: u64) -> Option<Value> {
        let beat = self.last_beat(h, now)?;
        // A coordinator holds a host in its fleet view from the first heartbeat after it started.
        if beat < self.started(now) {
            return None;
        }
        let secs = |ms: u64| ((T0_MS + ms) / 1000).to_string();
        let services: Vec<Value> = h
            .services
            .iter()
            .map(|(name, s)| {
                json!({
                    "service": name,
                    "installedVersion": s.installed,
                    "holdsSlot": s.holds_slot,
                    "health": s.health,
                    "lastUpdate": {"target": s.last_target, "outcome": s.last_outcome},
                })
            })
            .collect();
        let beats: Vec<Value> = h
            .services
            .iter()
            .map(|(name, s)| json!({"name": name, "updating": s.updating}))
            .collect();
        let mut host = json!({
            "nodeId": h.node,
            "live": now - beat < 45_000,
            "lastHeartbeatUnix": secs(beat),
            "services": services,
            "heartbeat": {"services": beats},
        });
        if let Some(ms) = h.first_heartbeat_ms {
            host["firstHeartbeatUnix"] = secs(ms).into();
        }
        if let Some(ms) = h.first_serving_ms {
            host["firstServingUnix"] = secs(ms).into();
        }
        Some(host)
    }
}

/// The simulated coordinator: a world, a timeline of changes, and every call it was given.
pub struct Fleet {
    pub clock: Rc<FakeClock>,
    pub world: World,
    pub timeline: Vec<(u64, Change)>,
    pub calls: Vec<Vec<String>>,
    pub requests: Vec<Option<Value>>,
    /// Answers given once, in place of the world's, to the first call whose arguments start
    /// with the words of the key.
    pub once: Vec<(String, Answer)>,
    /// Hosts per page of `hosts list` and `mdm list-devices`.
    pub page: usize,
}

pub fn ok(value: Value) -> Answer {
    Answer {
        code: 0,
        stdout: format!("{value}\n"),
        stderr: String::new(),
    }
}

pub fn refused(code: i32, reason: &str) -> Answer {
    Answer {
        code,
        stdout: String::new(),
        stderr: format!("refused: {reason}\n"),
    }
}

fn down() -> Answer {
    Answer {
        code: 1,
        stdout: String::new(),
        stderr: "curl: (7) Failed to connect\n".to_owned(),
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

impl Fleet {
    pub fn new(clock: Rc<FakeClock>, hosts: Vec<SimHost>) -> Self {
        Self {
            clock,
            world: World {
                hosts,
                first_order_ms: 5_000,
                order_gap_ms: 40_000,
                update_ms: 30_000,
                force_start_ms: 2_000,
                force_ms: 6_000,
                ..World::default()
            },
            timeline: Vec::new(),
            calls: Vec::new(),
            requests: Vec::new(),
            once: Vec::new(),
            page: 2,
        }
    }

    /// Schedules `change` at fake time `at`.
    pub fn at(&mut self, at: u64, change: Change) {
        self.timeline.push((at, change));
    }

    fn settle(&mut self) {
        let now = self.clock.now();
        loop {
            let due = self
                .timeline
                .iter()
                .enumerate()
                .filter(|(_, (at, _))| *at <= now)
                .min_by_key(|(i, (at, _))| (*at, *i))
                .map(|(i, _)| i);
            let Some(i) = due else { return };
            let (_, change) = self.timeline.remove(i);
            change(&mut self.world);
        }
    }

    /// Orders `node` to `version` of `service` at `start`, done `took` ms later; `slot` when the
    /// order holds an update slot (an update does, a force does not).
    pub fn schedule_update(
        &mut self,
        node: &str,
        service: &str,
        version: &str,
        start: u64,
        took: u64,
        slot: bool,
    ) {
        let (n, s, v) = (node.to_owned(), service.to_owned(), version.to_owned());
        let (n2, s2, v2) = (n.clone(), s.clone(), v.clone());
        self.at(
            start,
            Box::new(move |w| {
                let host = w.host(&n);
                host.extra_beats.push(start);
                let svc = host.services.entry(s).or_default();
                svc.holds_slot = slot;
                svc.updating = Some(v);
            }),
        );
        self.at(
            start + took,
            Box::new(move |w| {
                let host = w.host(&n2);
                host.extra_beats.push(start + took);
                *host.services.entry(s2).or_default() = Svc::at(&v2);
            }),
        );
    }

    fn answer(&mut self, args: &[String], request: Option<&Value>) -> Answer {
        let now = self.clock.now();
        if !self.world.up(now) {
            return down();
        }
        let words: Vec<&str> = args.iter().map(String::as_str).take(2).collect();
        let page_of = |items: Vec<Value>, key: &str, page: usize, token: Option<&str>| {
            let start: usize = token.and_then(|t| t.parse().ok()).unwrap_or(0);
            let end = (start + page).min(items.len());
            let next = if end < items.len() {
                end.to_string()
            } else {
                String::new()
            };
            ok(json!({ key: items[start..end].to_vec(), "nextPageToken": next }))
        };
        match words.as_slice() {
            ["hosts", "list"] => {
                let hosts: Vec<Value> = self
                    .world
                    .hosts
                    .iter()
                    .filter(|h| h.from_ms <= now)
                    .filter_map(|h| self.world.host_json(h, now))
                    .collect();
                page_of(hosts, "hosts", self.page, flag(args, "--page-token"))
            }
            ["hosts", "get"] => {
                let node = flag(args, "--node-id").unwrap_or_default();
                let host = self
                    .world
                    .hosts
                    .iter()
                    .filter(|h| h.from_ms <= now)
                    .find(|h| h.node == node)
                    .and_then(|h| self.world.host_json(h, now));
                match host {
                    Some(host) => ok(json!({"host": host})),
                    None => refused(2, "unknown_host"),
                }
            }
            ["audit", "list"] => {
                let since: u64 = flag(args, "--since-seq")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let action = flag(args, "--action");
                let mut rows: Vec<&Row> = self
                    .world
                    .audit
                    .iter()
                    .filter(|r| r.seq > since && action.is_none_or(|a| a == r.action))
                    .collect();
                if flag(args, "--newest") == Some("true") {
                    let keep = rows.len().saturating_sub(1);
                    rows.drain(..keep);
                }
                let next = rows.last().map_or(since, |r| r.seq);
                let entries: Vec<Value> = rows
                    .iter()
                    .map(|r| {
                        json!({"seq": r.seq.to_string(), "action": r.action, "nodeId": r.node,
                               "atUnix": ((T0_MS + r.at_ms) / 1000).to_string(), "byName": r.by_name})
                    })
                    .collect();
                ok(json!({"entries": entries, "nextSeq": next.to_string()}))
            }
            ["releases", "set-desired"] => {
                let request = request.cloned().unwrap_or(Value::Null);
                let service = request["service"].as_str().unwrap_or_default().to_owned();
                let version = request["desired"]["version"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                self.world.audit_at(now, "set_desired", None, "operator");
                let mut nodes: Vec<String> = self
                    .world
                    .hosts
                    .iter()
                    .filter(|h| h.services.contains_key(&service))
                    .map(|h| h.node.clone())
                    .collect();
                nodes.sort();
                let (first, gap, took) = (
                    self.world.first_order_ms,
                    self.world.order_gap_ms,
                    self.world.update_ms,
                );
                for (i, node) in nodes.iter().enumerate() {
                    let i = i as u64;
                    let took = took + 2_000 * ((self.world.orders + i) % 3);
                    self.schedule_update(
                        node,
                        &service,
                        &version,
                        now + first + gap * i,
                        took,
                        true,
                    );
                }
                self.world.orders += 1;
                ok(json!({"desired": {"version": version}, "record": {"generation": "1"}}))
            }
            ["releases", "force"] => {
                let service = flag(args, "--service").unwrap_or_default().to_owned();
                let version = flag(args, "--version").unwrap_or_default().to_owned();
                self.world.audit_at(now, "force", None, "operator");
                let nodes: Vec<String> = request
                    .and_then(|r| r["hosts"]["nodeIds"].as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let (start, took) = (
                    self.world.force_start_ms,
                    self.world.force_ms + 1_000 * (self.world.orders % 4),
                );
                self.world.orders += 1;
                for node in &nodes {
                    self.schedule_update(node, &service, &version, now + start, took, false);
                }
                ok(json!({"targeted": nodes.len()}))
            }
            ["mdm", "list-devices"] => {
                let devices: Vec<Value> = self
                    .world
                    .devices
                    .iter()
                    .map(|(id, since)| json!({"enrollmentId": id, "enrolledSinceUnix": since.map(|s| s.to_string())}))
                    .collect();
                page_of(devices, "devices", self.page, flag(args, "--page-token"))
            }
            _ => refused(2, "invalid_request"),
        }
    }
}

/// The fleet as the runner's coordinator.
#[derive(Clone)]
pub struct Shared(pub Rc<RefCell<Fleet>>);

impl Coordinator for Shared {
    fn call(&mut self, args: &[String], request: Option<&Value>) -> Result<Answer, CallError> {
        let mut fleet = self.0.borrow_mut();
        fleet.settle();
        fleet.calls.push(args.to_vec());
        fleet.requests.push(request.cloned());
        let joined = args.join(" ");
        if let Some(i) = fleet
            .once
            .iter()
            .position(|(k, _)| joined.starts_with(k.as_str()))
        {
            return Ok(fleet.once.remove(i).1);
        }
        Ok(fleet.answer(args, request))
    }
}

/// What the operator does to the fleet at a time.
pub type Act = Box<dyn FnMut(&mut Fleet, u64)>;

/// The operator: each line the runner reads makes the next scripted change to the fleet, at the
/// time the runner reads it; with no change left, standard input has ended.
pub struct Operator {
    pub fleet: Rc<RefCell<Fleet>>,
    pub acts: VecDeque<Act>,
    buf: Vec<u8>,
}

impl Operator {
    pub fn new(fleet: Rc<RefCell<Fleet>>) -> Self {
        Self {
            fleet,
            acts: VecDeque::new(),
            buf: Vec::new(),
        }
    }

    pub fn then(mut self, act: impl FnMut(&mut Fleet, u64) + 'static) -> Self {
        self.acts.push_back(Box::new(act));
        self
    }
}

impl Read for Operator {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = {
            let buf = self.fill_buf()?;
            let n = buf.len().min(out.len());
            out[..n].copy_from_slice(&buf[..n]);
            n
        };
        self.consume(n);
        Ok(n)
    }
}

impl BufRead for Operator {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.buf.is_empty()
            && let Some(mut act) = self.acts.pop_front()
        {
            let mut fleet = self.fleet.borrow_mut();
            let now = fleet.clock.now();
            act(&mut fleet, now);
            self.buf.push(b'\n');
        }
        Ok(&self.buf)
    }
    fn consume(&mut self, n: usize) {
        self.buf.drain(..n);
    }
}

/// A run's outcome with what it wrote.
pub struct Ran {
    pub result: Result<Summary, RunError>,
    pub records: Vec<Record>,
    pub console: String,
    pub names: Names,
}

/// Runs `plan` against the fleet with the operator.
pub fn run(
    plan: &Plan,
    params: &Params,
    fleet: &Rc<RefCell<Fleet>>,
    operator: &mut Operator,
) -> Ran {
    let clock = Rc::clone(&fleet.borrow().clock);
    let mut coordinator = Shared(Rc::clone(fleet));
    let mut console = Vec::new();
    let mut records = Vec::new();
    let mut names = Names::default();
    let mut sink = |r: &Record| {
        records.push(r.clone());
        Ok(())
    };
    let result = runner::run(
        plan,
        params,
        Io {
            coordinator: &mut coordinator,
            clock: &*clock,
            operator,
            console: &mut console,
            records: &mut sink,
            names: &mut names,
        },
    );
    Ran {
        result,
        records,
        console: String::from_utf8(console).expect("the console is text"),
        names,
    }
}

/// A scratch directory under the test target's temporary directory.
pub fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// The crate's directory.
pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A shipped plan, read and checked.
pub fn shipped(id: &str) -> Plan {
    let text = std::fs::read_to_string(crate_dir().join("plans").join(format!("{id}.toml")))
        .expect("a shipped plan");
    Plan::parse(&text).expect("a shipped plan checks")
}

/// A plan from text, which must check.
pub fn plan(text: &str) -> Plan {
    Plan::parse(text).unwrap_or_else(|e| panic!("the test's plan does not check: {e}"))
}

/// Parameters from `name=value` items.
pub fn params(items: &[&str]) -> Params {
    Params::parse(&items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).expect("parameters")
}
