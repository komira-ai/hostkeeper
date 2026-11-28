//! What the tests of the check on a farm Mac share: a fake coordinator that answers
//! `Maintenance.Schedule`, `Maintenance.Get`, `Maintenance.Cancel`, `Hosts.Get` and
//! `Xcode.Remove` from a script of replies per path (on top of the Xcode store of `add_rig`, which
//! answers the rest), whose answers are the real messages of `hostkeeper-proto` written as the
//! coordinator's gateway writes them; every request it answers is recorded by the server, so a
//! test reads it back by equality. And a wait that records the seconds it was asked for, in place
//! of the system's sleep.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use hostkeeper_hk::tailnet_wait::Tailnet;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use serde_json::Value;

use crate::add_rig::{Add, Store};
use crate::rig::{Opts, Ran, Reply, Request, Rig, Seen, Server};
use crate::summary_rig::{gateway_json, refusal};

pub const SCHEDULE: &str = "/hostkeeper.operator.v1.Maintenance/Schedule";
pub const GET: &str = "/hostkeeper.operator.v1.Maintenance/Get";
pub const CANCEL: &str = "/hostkeeper.operator.v1.Maintenance/Cancel";
pub const HOST: &str = "/hostkeeper.operator.v1.Hosts/Get";
pub const REMOVE: &str = "/hostkeeper.operator.v1.Xcode/Remove";

/// The farm Mac of the tests.
pub const MAC: &str = "mac-farm-1";

/// The run the fake coordinator schedules.
pub const RUN: &str = "run-7";

/// What the fake coordinator answers on each path of the farm: the replies in order, the last
/// repeated. A path with no script is answered by the store, which refuses it.
#[derive(Clone, Default)]
pub struct Farm {
    script: BTreeMap<&'static str, Vec<Reply>>,
}

impl Farm {
    pub fn new() -> Farm {
        Farm::default()
    }

    /// The replies of `path`.
    pub fn on(mut self, path: &'static str, replies: Vec<Reply>) -> Farm {
        self.script.insert(path, replies);
        self
    }

    /// The answers of a good check: scheduled as [`RUN`] naming `sha256`, the turn `steps` in
    /// order (the last repeated), the probe `probe`, and `{}` to a cancel and a removal.
    pub fn checking(sha256: &str, steps: Vec<Reply>, probe: Reply) -> Farm {
        Farm::new()
            .on(SCHEDULE, vec![scheduled(Some(sha256))])
            .on(GET, steps)
            .on(HOST, vec![probe])
            .on(CANCEL, vec![ok()])
            .on(REMOVE, vec![ok()])
    }

    /// The server: the farm's paths answered from the script, the rest by `store`.
    pub fn serve(self, store: Store, rig: &Rig) -> Server {
        self.serve_over(store, rig, |_| None)
    }

    /// The server: the farm's paths answered from the script, the paths `fleet` has an answer to
    /// (the Macs the summary reads) by it, the rest by `store`.
    pub fn serve_over(
        self,
        store: Store,
        rig: &Rig,
        fleet: impl Fn(&Request) -> Option<Reply> + Send + 'static,
    ) -> Server {
        let counts: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());
        store.serve_with(rig, move |request| {
            let Some(replies) = self.script.get(request.target.as_str()) else {
                return fleet(request);
            };
            let mut counts = counts.lock().expect("the counts");
            let n = counts.entry(request.target.clone()).or_insert(0);
            let reply = replies[(*n).min(replies.len() - 1)].clone();
            *n += 1;
            Some(reply)
        })
    }
}

/// `{}`: what a cancel and a removal answer.
pub fn ok() -> Reply {
    Reply::new(200, "{}")
}

/// What `Maintenance.Schedule` answers: the run [`RUN`] of the kind probe on [`MAC`], naming
/// `sha256` when it is given (a coordinator that cannot order an archive check names none).
pub fn scheduled(sha256: Option<&str>) -> Reply {
    let answer = proto::ScheduleMaintenanceResponse {
        run: Some(proto::MaintenanceRun {
            id: RUN.to_owned(),
            state: proto::MaintenanceState::Running as i32,
            kinds: vec![agent::MaintenanceKind::Probe as i32],
            host_ids: vec![MAC.to_owned()],
            xcode_sha256: sha256.unwrap_or_default().to_owned(),
            ..proto::MaintenanceRun::default()
        }),
        hosts: vec![turn(proto::MaintenanceHostStep::Waiting, "")],
        entry: None,
    };
    Reply::new(
        200,
        gateway_json("Maintenance", "Schedule", &answer).to_string(),
    )
}

/// [`MAC`]'s turn at `step`, with `reason` as the daemon words a deferral or a halt.
pub fn turn(step: proto::MaintenanceHostStep, reason: &str) -> proto::MaintenanceHost {
    proto::MaintenanceHost {
        host_id: MAC.to_owned(),
        step: step as i32,
        kinds: vec![agent::MaintenanceKind::Probe as i32],
        reason: reason.to_owned(),
        ..proto::MaintenanceHost::default()
    }
}

/// What `Maintenance.Get` answers for [`RUN`] in `state` with [`MAC`]'s turn `turn`.
pub fn got(state: proto::MaintenanceState, turn: proto::MaintenanceHost) -> Reply {
    let answer = proto::GetMaintenanceRunResponse {
        run: Some(proto::MaintenanceRun {
            id: RUN.to_owned(),
            state: state as i32,
            ..proto::MaintenanceRun::default()
        }),
        hosts: vec![turn],
    };
    Reply::new(200, gateway_json("Maintenance", "Get", &answer).to_string())
}

/// The turn of a run that is going on, at `step`.
pub fn at(step: proto::MaintenanceHostStep) -> Reply {
    got(proto::MaintenanceState::Running, turn(step, ""))
}

/// The turn of a run that is done: [`MAC`]'s done.
pub fn done() -> Reply {
    got(
        proto::MaintenanceState::Done,
        turn(proto::MaintenanceHostStep::Done, ""),
    )
}

/// One step of a probe: its name, exit code (none: not by itself), output and whether cut.
pub type Step<'a> = (&'a str, Option<i32>, &'a str, bool);

/// The steps a Mac reports for an archive that passes every check.
pub fn passing() -> Vec<Step<'static>> {
    vec![
        (
            "sw_vers",
            Some(0),
            "ProductName: macOS; ProductVersion: 26.1; BuildVersion: 25B78",
            false,
        ),
        ("uname", Some(0), "Darwin arm64", false),
        (
            "pkgutil",
            Some(0),
            "signed Apple Software, its chain ending at Apple Root CA",
            false,
        ),
        ("xip", Some(0), "", false),
        ("app", None, "Xcode.app", false),
        ("version", None, "27.2 27B52", false),
        ("codesign", Some(0), "", false),
        ("verdict", Some(0), "pass", false),
    ]
}

/// What `Hosts.Get` answers for [`MAC`] when its last probe is of the run `run` with `steps`,
/// ended at `probed_unix`.
pub fn probe_of(run: &str, probed_unix: u64, steps: &[Step<'_>]) -> Reply {
    let probe = agent::MaintenanceProbe {
        run_id: run.to_owned(),
        probed_unix,
        steps: steps
            .iter()
            .map(|(name, exit, output, cut)| agent::MaintenanceProbeStep {
                name: (*name).to_owned(),
                exit_code: *exit,
                output: (*output).to_owned(),
                cut: *cut,
            })
            .collect(),
    };
    let host = proto::Host {
        node_id: MAC.to_owned(),
        maintenance: Some(agent::MaintenanceReport {
            probe: Some(probe),
            ..agent::MaintenanceReport::default()
        }),
        ..proto::Host::default()
    };
    let answer = proto::GetHostResponse { host: Some(host) };
    Reply::new(200, gateway_json("Hosts", "Get", &answer).to_string())
}

/// What `Hosts.Get` answers for a Mac that reports no probe.
pub fn no_probe() -> Reply {
    let host = proto::Host {
        node_id: MAC.to_owned(),
        ..proto::Host::default()
    };
    let answer = proto::GetHostResponse { host: Some(host) };
    Reply::new(200, gateway_json("Hosts", "Get", &answer).to_string())
}

/// The turn of a good check: waiting, then each step the Mac goes through, then done.
pub fn good_turn() -> Vec<Reply> {
    use proto::MaintenanceHostStep as Step;
    vec![
        at(Step::Waiting),
        at(Step::Waiting),
        at(Step::Staging),
        at(Step::Draining),
        at(Step::Applying),
        at(Step::Checking),
        at(Step::PuttingBack),
        done(),
    ]
}

/// The refusal of a call: the gateway's code and reason.
pub fn refused(code: &str, reason: &str) -> Reply {
    refusal(code, reason)
}

/// The requests of `server` to `path`, as the JSON they carried.
pub fn asked(server: &Server, path: &str) -> Vec<Value> {
    server
        .seen()
        .iter()
        .filter(|seen| seen.target == path)
        .map(|seen| serde_json::from_slice(&seen.body).expect("a JSON body"))
        .collect()
}

/// The paths of every request `server` answered, in order.
pub fn paths(server: &Server) -> Vec<String> {
    server.seen().into_iter().map(|seen| seen.target).collect()
}

/// The requests of `server` that are not the Xcode store's own or the Macs summary's.
pub fn farm_calls(server: &Server) -> Vec<Seen> {
    server
        .seen()
        .into_iter()
        .filter(|seen| [SCHEDULE, GET, CANCEL, HOST, REMOVE].contains(&seen.target.as_str()))
        .collect()
}

/// The waits a run asked for.
#[derive(Default)]
pub struct Waits(RefCell<Vec<Duration>>);

impl Waits {
    pub fn new() -> Waits {
        Waits::default()
    }

    /// The seconds of each wait, in order.
    pub fn secs(&self) -> Vec<u64> {
        self.0.borrow().iter().map(Duration::as_secs).collect()
    }
}

/// A test's directory on a system that is not macOS: `Add`, with the programs of the Mac not run.
pub fn linux(name: &str) -> Add {
    let mut add = Add::new(&format!("farm-{name}"));
    add.mac.os = "linux";
    add
}

/// `hk <words>` as [`Add::run`], with the world's wait recorded in `waits` and returning at once.
pub fn run_waiting(add: &Add, waits: &Waits, port: u16, words: &[&str]) -> Ran {
    let record = |wait: Duration| waits.0.borrow_mut().push(wait);
    let tailnet = Tailnet {
        tailscale: PathBuf::from("no-tailscale"),
        limit: Duration::from_secs(1),
        wait: &record,
        now: &|| Duration::ZERO,
    };
    add.run_with(Opts::real().tailnet(&tailnet), port, words)
}

/// The record `hk` kept for the archive `sha256`, as JSON.
pub fn record_of(add: &Add, sha256: &str) -> Value {
    let text = std::fs::read_to_string(add.kept(&format!("{sha256}.json"))).expect("a record");
    serde_json::from_str(&text).expect("a record is JSON")
}
