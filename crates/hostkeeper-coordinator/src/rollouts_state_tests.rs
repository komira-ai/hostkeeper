//! Where an update plan stands, on disk and across releases and restarts
//! (`docs/design/rollout-update-plans.md` section 3): the position is kept in the halts file and
//! never in the ops file; the release before reads both files this release writes, and this
//! release reads the files that release wrote; a record it cannot read is passed over and
//! counted; a coordinator that restarts mid-plan resumes at the stage it had entered; and the
//! coordinator writes a position only when the plan enters a stage with something left to order,
//! never while it is halted, bad, paused or quiet, trying again at the next heartbeat when a
//! write fails. Every time is the rig's clock; nothing sleeps.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_testkit::Scratch;
use hostkeeper_wire::wire::ServiceReply;
use serde_json::{Value, json};

use crate::desired::StoreError;
use crate::holds_calls_tests::PreviousOps;
use crate::marks_support::{
    DB, NEW_DAEMON, WEB, ask, beat_of, operator, restart, rig, runs, set, update,
};
use crate::ops::{AuditEntry, Halt, OpsError, OpsState, OpsStore, ServiceOps, Write};
use crate::ops_file::{digest, halts_path, joined};
use crate::rollback_tests::status;
use crate::rollouts::{Pause, Rollout};
use crate::services::Services;
use crate::testutil::{Log, OPERATOR, Rig, scratch, version};

/// The role of every host of these tests.
pub(crate) const DAEMON: &str = "daemon";

/// Eight hosts, in node-id order.
const HOSTS: [&str; 8] = ["d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8"];

// ---- the files --------------------------------------------------------------------------

/// A rollout position for 2.0 that has entered three stages with `d1` recorded done.
fn mid_plan() -> Rollout {
    Rollout {
        version: Some(version("2.0")),
        entered: 3,
        done: ["d1".to_owned()].into(),
        paused: None,
    }
}

/// A pause by `operator`.
fn pause(reason: &str) -> Pause {
    Pause {
        by: "operator".to_owned(),
        reason: reason.to_owned(),
    }
}

/// A store over a new file in `dir`, and the file's path.
fn store_in(dir: &Scratch) -> (OpsStore, PathBuf) {
    let path = dir.path().join("ops.json");
    (OpsStore::open(&path).expect("a store"), path)
}

/// Makes `edit` to the state of `store` as an operator's change, written before memory.
fn change(store: &OpsStore, action: &str, edit: impl FnOnce(&mut OpsState)) {
    store
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", action),
            edit,
        )
        .expect("written");
}

/// The JSON of the file at `path`.
fn json_of(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("the file")).expect("JSON")
}

/// The top-level keys of the JSON object `value`, sorted.
fn keys_of(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect()
}

/// Proves: a position the store holds is written to the halts file under the top-level key
/// `rollouts`, by service name, each record with only the fields that hold something (a pause
/// alone is `{"paused": ...}`, a bare version is `{"version": ...}`); it is not written to the
/// ops file, whose top-level keys stay those of a farm that never planned; and a restart (the
/// same path opened again) reads back an equal state.
///
/// Catches: a position written into the ops file, which the release before refuses, a record
/// that spells its empty fields, or one the next start does not read back.
#[test]
fn a_position_is_written_to_the_halts_file_only_and_read_back() {
    let dir = scratch("rollouts-state-halts-only");
    let (store, path) = store_in(&dir);
    change(&store, "rollout_advance", |state| {
        let mut full = mid_plan();
        full.paused = Some(pause("hold on"));
        state.rollouts.insert("web".to_owned(), full);
        state.rollouts.insert(
            "db".to_owned(),
            Rollout {
                paused: Some(pause("window closed")),
                ..Rollout::default()
            },
        );
        state.rollouts.insert(
            "api".to_owned(),
            Rollout {
                version: Some(version("1.5")),
                ..Rollout::default()
            },
        );
    });

    let ops = json_of(&path);
    assert_eq!(
        keys_of(&ops),
        ["audit", "excluded", "hosts", "schema", "services"]
    );
    assert!(
        !fs::read_to_string(&path)
            .expect("the ops file")
            .contains("rollouts")
    );

    let ops_text = fs::read(&path).expect("the ops file");
    assert_eq!(
        json_of(&halts_path(&path)),
        json!({
            "services": {},
            "ops": digest(&ops_text),
            "rollouts": {
                "api": {"version": "1.5"},
                "db": {"paused": {"by": "operator", "reason": "window closed"}},
                "web": {
                    "version": "2.0",
                    "entered": 3,
                    "done": ["d1"],
                    "paused": {"by": "operator", "reason": "hold on"}
                }
            }
        })
    );

    let restarted = OpsStore::open(&path).expect("a restart reads");
    assert_eq!(restarted.state(), store.state());
    assert_eq!(restarted.state().rollouts.len(), 3);
}

/// Proves: a farm with no position and no halt gets no halts file however many changes it
/// makes; once a position is kept the file exists, and taking the last position away rewrites it
/// without the `rollouts` key, which a restart reads as no position.
///
/// Catches: an empty halts file written for a farm that never planned, or a taken-away position
/// that stays in the file and comes back at the next start.
#[test]
fn no_position_and_no_halt_writes_no_halts_file() {
    let dir = scratch("rollouts-state-no-file");
    let (store, path) = store_in(&dir);
    change(&store, "hold", |_| {});
    change(&store, "hold", |_| {});
    assert!(path.exists());
    assert!(!halts_path(&path).exists());

    change(&store, "rollout_pause", |state| {
        state.rollouts.insert(
            "web".to_owned(),
            Rollout {
                paused: Some(pause("look first")),
                ..Rollout::default()
            },
        );
    });
    assert!(halts_path(&path).exists());
    assert_eq!(
        OpsStore::open(&path)
            .expect("a restart")
            .state()
            .rollouts
            .len(),
        1
    );

    change(&store, "rollout_resume", |state| {
        state.rollouts.remove("web");
    });
    let halts = json_of(&halts_path(&path));
    assert_eq!(keys_of(&halts), ["ops", "services"]);
    assert!(
        OpsStore::open(&path)
            .expect("a restart")
            .state()
            .rollouts
            .is_empty()
    );
}

// ---- the release before and the release after -------------------------------------------

/// The halts file reader of the release before this one: it reads `services` and `ops` and skips
/// every key it does not know, `rollouts` among them (no `deny_unknown_fields`).
mod previous {
    use std::collections::BTreeMap;

    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Debug, Deserialize)]
    pub struct ReadService {
        #[serde(default)]
        pub halt: Option<Value>,
        #[serde(default)]
        pub halt_acked: Vec<Value>,
        #[serde(default)]
        pub halt_seen: Vec<Value>,
        #[serde(default)]
        pub halt_generation: u64,
    }

    #[derive(Debug, Deserialize)]
    pub struct ReadHalts {
        #[serde(default)]
        pub services: BTreeMap<String, ReadService>,
        #[serde(default)]
        pub ops: Option<String>,
    }
}

/// A halt of `web` raised by the host `s1` on 2.0.
fn halted() -> ServiceOps {
    ServiceOps {
        halt: Some(Halt {
            node_id: "s1".to_owned(),
            check: "tainted".to_owned(),
            target: version("2.0"),
            report: None,
            origin: crate::ops::HaltOrigin::Operator,
        }),
        halt_generation: 2,
        ..ServiceOps::default()
    }
}

/// Proves (the release before on the data of this one): with a position kept, a halt standing and
/// a clear counted, the ops file parses with the release before's strict reader, and the halts
/// file parses with its lenient one, which skips `rollouts`, still sees the halt and the
/// generation, and finds the digest of the ops file it goes with: the release before loses the
/// position and nothing else.
///
/// Catches: a position that reaches the ops file or makes the halts file unreadable to the
/// release before (B8).
#[test]
fn the_release_before_reads_the_files_a_position_is_written_in() {
    let dir = scratch("rollouts-state-previous-reads");
    let (store, path) = store_in(&dir);
    change(&store, "halt", |state| {
        state.services.insert("web".to_owned(), halted());
        state.rollouts.insert("web".to_owned(), mid_plan());
    });

    let ops_text = fs::read_to_string(&path).expect("the ops file");
    let ops = PreviousOps::read(&ops_text).expect("the release before starts on the ops file");
    drop(ops);

    let halts: previous::ReadHalts =
        serde_json::from_slice(&fs::read(halts_path(&path)).expect("the halts file"))
            .expect("the release before reads the halts file");
    assert_eq!(halts.ops, Some(digest(ops_text.as_bytes())));
    let service = &halts.services["web"];
    assert_eq!(service.halt.as_ref().expect("a halt")["node_id"], "s1");
    assert_eq!(service.halt_generation, 2);
    assert!(service.halt_acked.is_empty() && service.halt_seen.is_empty());
    assert!(
        json_of(&halts_path(&path))
            .get("rollouts")
            .is_some_and(|rollouts| rollouts["web"]["entered"] == 3),
        "the file does hold the position the release before skipped"
    );
}

/// Proves (this release on the data of the one before): the ops file and halts file the release
/// before wrote, which have no `rollouts` key, are read with no position, the clear generation
/// they hold intact; and a position kept afterwards sits beside that generation and both come back
/// at the next start.
///
/// Catches: a halts file without `rollouts` refused or read with a position made up, or a
/// position that costs the file its other contents (B8).
#[test]
fn a_halts_file_without_positions_reads_with_none() {
    let dir = scratch("rollouts-state-previous-wrote");
    let path = dir.path().join("ops.json");
    let ops_text = r#"{"schema":1,"hosts":{},"excluded":[],"services":{"web":{"halt":null,"halt_acked":[],"last_good":"1.0","bad":[]}},"audit":[]}"#;
    fs::write(&path, ops_text).expect("the ops file");
    let halts = json!({
        "services": {"web": {"halt_generation": 2}},
        "ops": digest(ops_text.as_bytes())
    });
    fs::write(halts_path(&path), halts.to_string()).expect("the halts file");

    let store = OpsStore::open(&path).expect("this release starts on the files");
    let state = store.state();
    assert!(state.rollouts.is_empty());
    assert_eq!(state.services["web"].halt_generation, 2);
    assert_eq!(state.services["web"].last_good, Some(version("1.0")));

    change(&store, "rollout_advance", |state| {
        state.rollouts.insert("web".to_owned(), mid_plan());
    });
    let after = OpsStore::open(&path).expect("a restart").state();
    assert_eq!(after.rollouts["web"], mid_plan());
    assert_eq!(after.services["web"].halt_generation, 2);
}

/// Proves: of the records under `rollouts`, the one whose `entered` is not a number and the one
/// whose `done` is not a list are passed over and counted (two), the record this release reads
/// is kept, and the rest of the file (a clear generation) is read as it is, both through
/// [`joined`] and when the store opens.
///
/// Catches: one unreadable record that makes the whole halts file refuse the start, that is
/// passed over without being counted, or that takes the readable records with it.
#[test]
fn a_record_this_release_cannot_read_is_passed_over_and_counted() {
    let dir = scratch("rollouts-state-skipped");
    let path = dir.path().join("ops.json");
    let halts = json!({
        "services": {"web": {"halt_generation": 3}},
        "rollouts": {
            "web": {"version": "2.0", "entered": 3, "done": ["d1"]},
            "db": {"version": "2.0", "entered": "two"},
            "api": {"version": "2.0", "entered": 2, "done": "d1"}
        }
    });
    fs::write(halts_path(&path), halts.to_string()).expect("the halts file");

    let (state, skipped) = joined(&path, OpsState::default()).expect("read");
    assert_eq!(skipped, 2);
    assert_eq!(
        state
            .rollouts
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["web"]
    );
    assert_eq!(state.rollouts["web"], mid_plan());
    assert_eq!(state.services["web"].halt_generation, 3);

    let opened = OpsStore::open(&path)
        .expect("the start is not refused")
        .state();
    assert_eq!(opened.rollouts, state.rollouts);
    assert_eq!(opened.services["web"].halt_generation, 3);
}

// ---- the record -------------------------------------------------------------------------

/// The plan of `app` in a rollout file with the `extra` lines in its table.
fn plan_of(extra: &str) -> crate::stages::Plan {
    Services::parse(&format!(
        "[[service]]\nname = \"app\"\nroles = [\"farm\"]\n{extra}\n"
    ))
    .expect("the services")
    .get("app")
    .expect("app")
    .plan
    .clone()
}

/// Proves: a position that holds nothing serializes to `{}`, is empty, and reads back from `{}`;
/// a position holding any one field is not empty, and each serializes under its own name with
/// the done hosts in node-id order.
///
/// Catches: a default position that is written (a key per service that never planned), or a field
/// that stops being left out when it holds nothing.
#[test]
fn a_default_position_is_empty_and_written_as_nothing() {
    let empty = Rollout::default();
    assert!(empty.is_empty());
    assert_eq!(serde_json::to_value(&empty).expect("JSON"), json!({}));
    assert_eq!(
        serde_json::from_value::<Rollout>(json!({})).expect("read"),
        empty
    );

    let each = [
        Rollout {
            version: Some(version("2.0")),
            ..Rollout::default()
        },
        Rollout {
            entered: 1,
            ..Rollout::default()
        },
        Rollout {
            done: ["d1".to_owned()].into(),
            ..Rollout::default()
        },
        Rollout {
            paused: Some(pause("why")),
            ..Rollout::default()
        },
    ];
    for one in &each {
        assert!(!one.is_empty(), "{one:?}");
        assert_eq!(
            &serde_json::from_value::<Rollout>(serde_json::to_value(one).expect("JSON"))
                .expect("read"),
            one
        );
    }
    let full = Rollout {
        done: ["d2".to_owned(), "d1".to_owned()].into(),
        ..mid_plan()
    };
    assert_eq!(
        serde_json::to_value(&full).expect("JSON"),
        json!({"version": "2.0", "entered": 3, "done": ["d1", "d2"]})
    );
}

/// Proves: a position for the desired version gives its own count of entered stages, and one for
/// another version (or for none) gives the count of a plan with none kept: 1 when the first
/// stage enters by itself, 0 when it waits for an operator.
///
/// Catches: a position for an old version that keeps a new version's plan at its old stage, or a
/// manual first stage entered without the operator.
#[test]
fn a_position_counts_only_for_its_own_version() {
    let auto = plan_of("");
    let manual = plan_of("stages = [{ percent = 10, advance = \"manual\" }, 100]");
    let own = Rollout {
        version: Some(version("2.0")),
        entered: 3,
        ..Rollout::default()
    };
    assert_eq!(own.entered_for(&auto, &version("2.0")), 3);
    assert_eq!(own.entered_for(&manual, &version("2.0")), 3);
    assert_eq!(own.entered_for(&auto, &version("3.0")), 1);
    assert_eq!(own.entered_for(&manual, &version("3.0")), 0);

    let none = Rollout::default();
    assert_eq!(none.entered_for(&auto, &version("2.0")), 1);
    assert_eq!(none.entered_for(&manual, &version("2.0")), 0);

    let zero = Rollout {
        version: Some(version("2.0")),
        ..Rollout::default()
    };
    assert_eq!(zero.entered_for(&manual, &version("2.0")), 0);
    assert_eq!(zero.entered_for(&auto, &version("2.0")), 0);
}

// ---- a farm of eight --------------------------------------------------------------------

/// What a fake host runs.
#[derive(Clone, Debug)]
pub(crate) enum Run {
    /// This version, healthy.
    At(String),
    /// This version, updating to the second.
    Updating(String, String),
}

/// A farm of daemons that run `web`, driven by the heartbeats the test sends at the rig's clock.
pub(crate) struct Farm {
    pub(crate) rig: Rig,
    pub(crate) hosts: BTreeMap<String, Run>,
    pub(crate) at: u64,
}

impl Farm {
    /// A farm over the rig `test` with the `extra` lines of `web`'s definition, each host in
    /// `hosts` having beaten once at the version beside it. No version of `web` is desired.
    pub(crate) fn new(test: &str, extra: &str, hosts: &[(&str, &str)]) -> Self {
        Self::over(rig(test, extra), hosts)
    }

    /// The farm of `hosts` over `rig`.
    pub(crate) fn over(rig: Rig, hosts: &[(&str, &str)]) -> Self {
        let mut farm = Self {
            rig,
            hosts: hosts
                .iter()
                .map(|(host, at)| ((*host).to_owned(), Run::At((*at).to_owned())))
                .collect(),
            at: 2_000,
        };
        farm.rig.at(farm.at);
        for (host, _) in hosts {
            assert_eq!(farm.beat(host), ServiceReply::Nothing);
        }
        farm
    }

    /// Eight hosts at 1.0, but for `up` of them (the first) at 2.0.
    pub(crate) fn eight(test: &str, extra: &str, up: usize) -> Self {
        let hosts: Vec<(&str, &str)> = HOSTS
            .iter()
            .enumerate()
            .map(|(i, host)| (*host, if i < up { "2.0" } else { "1.0" }))
            .collect();
        Self::new(test, extra, &hosts)
    }

    /// `host` beats as it runs now, and acts on the answer: an ordered host updates, its next beat
    /// says so, and the one after runs the new version.
    pub(crate) fn beat(&mut self, host: &str) -> ServiceReply {
        let run = self.hosts[host].clone();
        let (running, updating) = match &run {
            Run::At(v) => (v.as_str(), None),
            Run::Updating(from, to) => (from.as_str(), Some(to.as_str())),
        };
        let mut one = runs(WEB, running);
        one.updating = updating.map(version);
        let reply = ask(
            &self.rig,
            host,
            DAEMON,
            beat_of(NEW_DAEMON, vec![one, runs(DB, "1.0")]),
        );
        let next = match (run, &reply) {
            (Run::At(from), ServiceReply::Update { version: to, .. }) => {
                Run::Updating(from, to.to_string())
            }
            (Run::Updating(_, to), _) => Run::At(to),
            (run, _) => run,
        };
        self.hosts.insert(host.to_owned(), next);
        reply
    }

    /// Ten seconds on, every host beats in node-id order; the hosts that were ordered `web` 2.0.
    pub(crate) fn round(&mut self) -> Vec<String> {
        self.at += 10;
        self.rig.at(self.at);
        let hosts: Vec<String> = self.hosts.keys().cloned().collect();
        hosts
            .into_iter()
            .filter(|host| self.beat(host) == update(WEB, "2.0"))
            .collect()
    }

    /// Rounds until `until` holds, at most `most`.
    pub(crate) fn rounds_until(&mut self, most: usize, until: impl Fn(&Self) -> bool) {
        for _ in 0..most {
            if until(self) {
                return;
            }
            self.round();
        }
        assert!(until(self), "not within {most} rounds");
    }

    /// A coordinator that starts now on the same files, over the same hosts.
    pub(crate) fn restart(&mut self, extra: &str) {
        restart(&mut self.rig, extra);
    }

    /// Every host that runs `version` and does not update.
    pub(crate) fn running(&self, version: &str) -> Vec<&str> {
        self.hosts
            .iter()
            .filter(|(_, run)| matches!(run, Run::At(v) if v == version))
            .map(|(host, _)| host.as_str())
            .collect()
    }

    pub(crate) fn ops_path(&self) -> PathBuf {
        self.rig.dir.path().join("desired.json.ops")
    }

    /// The `rollouts` key of the halts file; none when there is no file or no such key.
    pub(crate) fn kept(&self) -> Option<Value> {
        let text = fs::read(halts_path(&self.ops_path())).ok()?;
        let file: Value = serde_json::from_slice(&text).expect("JSON");
        file.get("rollouts").cloned()
    }

    /// The entries `rollout_advance` in the audit ring.
    pub(crate) fn advances(&self) -> Vec<AuditEntry> {
        self.rig
            .coordinator
            .ops()
            .state()
            .audit
            .into_iter()
            .filter(|entry| entry.action == "rollout_advance")
            .collect()
    }

    /// The plan of `web` as the status answers it.
    pub(crate) fn plan(&self) -> proto::RolloutPlan {
        status(&self.rig).per_service[WEB]
            .plan
            .clone()
            .expect("a plan")
    }

    /// The state of each host in the plan of `web`.
    pub(crate) fn states(&self) -> BTreeMap<String, proto::PlanHostState> {
        self.plan()
            .stages
            .iter()
            .flat_map(|stage| &stage.members)
            .map(|m| (m.node_id.clone(), m.state()))
            .collect()
    }

    /// Whether the farm has written no position: no `rollouts` in the halts file and no
    /// `rollout_advance` in the ring.
    pub(crate) fn wrote_nothing(&self) -> bool {
        self.kept().is_none() && self.advances().is_empty()
    }
}

/// The first stage of `web` on eight hosts with the default plan and `bake_beats = 2` is rolled
/// until `d1` is done and the coordinator has entered the second stage: `d1` updated and baked,
/// `d2` and `d3` (the two a daemon role may update at once) ordered by the second stage, and the
/// farm still in the beat of `d2` and `d3` updating.
pub(crate) async fn in_second_stage(test: &str) -> Farm {
    let mut farm = Farm::eight(test, "bake_beats = 2", 0);
    set(&farm.rig, WEB, "2.0", 2).await;
    assert_eq!(farm.round(), ["d1"]);
    assert!(
        farm.wrote_nothing(),
        "stage 1 of 3 is entered without a word"
    );
    farm.rounds_until(6, |farm| !farm.advances().is_empty());
    assert_eq!(farm.advances().len(), 1);
    farm
}

// ---- when the coordinator writes a position ---------------------------------------------

/// Proves: when every host already runs the desired version the plan enters all its stages
/// without a word: after two rounds there is no halts file, no `rollout_advance` in the audit
/// ring, no position in memory, and no host is ordered.
///
/// Catches: a position written for a rollout with nothing to order, which would leave a halts
/// file (and a line in the audit log) on every farm at every release that is already everywhere.
#[tokio::test]
async fn a_plan_with_nothing_to_order_writes_no_position() {
    let mut farm = Farm::eight("rollouts-state-nothing-to-order", "", 8);
    set(&farm.rig, WEB, "2.0", 2).await;
    assert_eq!(farm.round(), Vec::<String>::new());
    assert_eq!(farm.round(), Vec::<String>::new());
    assert!(!halts_path(&farm.ops_path()).exists());
    assert!(farm.advances().is_empty());
    assert!(farm.rig.coordinator.ops().state().rollouts.is_empty());
}

/// Proves: when four of eight hosts already run 2.0 and the plan's first two stages (1 and 4
/// hosts) are filled by them, the coordinator enters both and the third (8 hosts) in one write: a
/// single `rollout_advance` by `coordinator` naming "stage 3 of 3 (8 hosts)", the position on
/// disk 2.0 with all four stages of the rollout file entered and the four hosts done, and the
/// hosts left are ordered under it; a later heartbeat writes nothing more.
///
/// Catches: one write per stage entered, a stage entered and not audited, an audit entry under
/// another name than the coordinator's, or hosts at the version not recorded done.
#[tokio::test]
async fn entering_stages_with_hosts_left_to_order_is_one_write_and_one_audit_entry() {
    let mut farm = Farm::eight("rollouts-state-enter-many", "", 4);
    set(&farm.rig, WEB, "2.0", 2).await;
    assert!(farm.wrote_nothing());

    assert_eq!(farm.round(), ["d5", "d6"]);
    let advances = farm.advances();
    assert_eq!(advances.len(), 1);
    assert_eq!(advances[0].by, "coordinator");
    assert_eq!(advances[0].service.as_deref(), Some(WEB));
    assert_eq!(
        advances[0].reason.as_deref(),
        Some("web 2.0: stage 3 of 3 (8 hosts)")
    );
    assert_eq!(
        farm.kept(),
        Some(json!({"web": {
            "version": "2.0",
            "entered": 4,
            "done": ["d1", "d2", "d3", "d4"]
        }}))
    );
    farm.round();
    assert_eq!(farm.advances().len(), 1);
}

/// Proves: a plan that has not gone past its first stage keeps nothing, however many beats pass
/// (the first host updating and baking); the position is written at the beat that finds the
/// first stage done, as "stage 2 of 3 (4 hosts)", with `d1` recorded done, and the hosts of the
/// second stage are ordered at that very heartbeat, before the position is read back.
///
/// Catches: a position written for the first stage (a file for every rollout that is one host),
/// or a second stage whose orders wait for the next beat after its write.
#[tokio::test]
async fn the_first_stage_writes_nothing_and_the_second_is_written_when_found_done() {
    let mut farm = Farm::eight("rollouts-state-second-stage", "bake_beats = 2", 0);
    set(&farm.rig, WEB, "2.0", 2).await;
    assert_eq!(farm.round(), ["d1"]);
    for _ in 0..2 {
        assert_eq!(farm.round(), Vec::<String>::new());
        assert!(farm.wrote_nothing());
    }
    // d1 beats at 2.0 for the second time: the stage is done at this heartbeat.
    assert_eq!(farm.round(), ["d2", "d3"]);
    let advances = farm.advances();
    assert_eq!(advances.len(), 1);
    assert_eq!(advances[0].by, "coordinator");
    assert_eq!(
        advances[0].reason.as_deref(),
        Some("web 2.0: stage 2 of 3 (4 hosts)")
    );
    assert_eq!(
        farm.kept(),
        Some(json!({"web": {"version": "2.0", "entered": 3, "done": ["d1"]}}))
    );
}

/// Proves: with four of eight hosts at 2.0 and a position to write, nothing is written while the
/// service is halted, or while 2.0 is marked bad, or while the plan is paused (the pause alone
/// is kept), or while the coordinator is within its quiet start; and each position is written,
/// once, at the first heartbeat after the condition ends.
///
/// Catches: a plan that records a stage it may not order (a halted, bad, paused or quiet
/// service), or one that forgets the stage once the condition ends.
#[tokio::test]
async fn nothing_is_written_while_halted_bad_paused_or_quiet() {
    let written = json!({"web": {
        "version": "2.0",
        "entered": 4,
        "done": ["d1", "d2", "d3", "d4"]
    }});

    // Halted.
    let mut farm = Farm::eight("rollouts-state-halted", "", 4);
    set(&farm.rig, WEB, "2.0", 2).await;
    farm.rig
        .coordinator
        .halt(
            &operator(),
            proto::HaltRequest {
                service: WEB.to_owned(),
                reason: "look first".to_owned(),
            },
        )
        .expect("halted");
    assert_eq!(farm.round(), Vec::<String>::new());
    assert!(farm.wrote_nothing());
    farm.rig
        .coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"clear_halt","service":"web","reason":"looked"}"#,
        )
        .expect("cleared");
    farm.round();
    assert_eq!(farm.kept(), Some(written.clone()));
    assert_eq!(farm.advances().len(), 1);

    // Bad.
    let mut farm = Farm::eight("rollouts-state-bad", "", 4);
    set(&farm.rig, WEB, "2.0", 2).await;
    farm.rig
        .coordinator
        .mark_bad(
            &operator(),
            proto::MarkBadRequest {
                service: WEB.to_owned(),
                version: "2.0".to_owned(),
                reason: "known bad".to_owned(),
            },
        )
        .expect("marked");
    assert_eq!(farm.round(), Vec::<String>::new());
    assert!(farm.wrote_nothing());
    farm.rig
        .coordinator
        .unmark_bad(
            &operator(),
            proto::UnmarkBadRequest {
                service: WEB.to_owned(),
                version: "2.0".to_owned(),
                reason: "it is fine".to_owned(),
            },
        )
        .expect("unmarked");
    farm.round();
    assert_eq!(farm.kept(), Some(written.clone()));
    assert_eq!(farm.advances().len(), 1);

    // Paused.
    let mut farm = Farm::eight("rollouts-state-paused", "", 4);
    set(&farm.rig, WEB, "2.0", 2).await;
    farm.rig
        .coordinator
        .rollout_pause(
            &operator(),
            proto::PauseRequest {
                service: WEB.to_owned(),
                reason: "look first".to_owned(),
            },
        )
        .expect("paused");
    assert_eq!(farm.round(), Vec::<String>::new());
    assert_eq!(
        farm.kept(),
        Some(json!({"web": {"paused": {"by": "operator", "reason": "look first"}}}))
    );
    assert!(farm.advances().is_empty());
    farm.rig
        .coordinator
        .rollout_resume(
            &operator(),
            proto::ResumeRequest {
                service: WEB.to_owned(),
                reason: "looked".to_owned(),
            },
        )
        .expect("resumed");
    farm.round();
    assert_eq!(farm.kept(), Some(written.clone()));
    assert_eq!(farm.advances().len(), 1);

    // Quiet.
    let mut farm = Farm::eight("rollouts-state-quiet", "", 4);
    set(&farm.rig, WEB, "2.0", 2).await;
    farm.restart("");
    farm.at += 5;
    assert_eq!(farm.round(), Vec::<String>::new());
    assert!(farm.wrote_nothing());
    farm.at += 20;
    assert_eq!(farm.round(), ["d5", "d6"]);
    assert_eq!(farm.kept(), Some(written));
    assert_eq!(farm.advances().len(), 1);
}

// ---- a write that fails -----------------------------------------------------------------

/// An ops store that fails every write while `failing` is set, counting the writes it was asked
/// for while it failed; otherwise it keeps nothing.
fn flaky(failing: &Arc<AtomicBool>, attempts: &Arc<AtomicUsize>) -> OpsStore {
    let (failing, attempts) = (Arc::clone(failing), Arc::clone(attempts));
    OpsStore::with_writer(
        OpsState::default(),
        Box::new(move |_| {
            if failing.load(Ordering::SeqCst) {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(OpsError::Io(StoreError::Io {
                    path: "ops".into(),
                    source: std::io::Error::other("disk full"),
                }))
            } else {
                Ok(())
            }
        }),
    )
}

/// Proves: when the write of the second stage fails the error is logged ("the plan's next stage
/// was not entered", the service named), nothing reaches the state or the audit ring, the
/// heartbeat is still answered by the position the plan reaches by itself (`d2` and `d3` are
/// ordered and take the role's two slots, `d4` is held by them), each following heartbeat tries
/// the write once more (one attempt at `d1`'s beat, a second at `d2`'s), and the first heartbeat
/// after the store takes writes again lands it: one `rollout_advance` "stage 2 of 3 (4 hosts)"
/// and the position 2.0, three stages entered, `d1` done.
///
/// Catches: a stage that waits for its write (the second stage's hosts not ordered while the disk
/// is full), a failed write that is not retried, or one that leaves half a position in memory.
#[tokio::test]
async fn a_failed_write_is_logged_retried_and_does_not_hold_the_next_stage() {
    let failing = Arc::new(AtomicBool::new(false));
    let attempts = Arc::new(AtomicUsize::new(0));
    let mut rig = rig("rollouts-state-flaky", "bake_beats = 2");
    rig.at(1_000);
    rig.coordinator = crate::marks_support::start(&rig.dir, &rig.clock, "bake_beats = 2")
        .with_ops(flaky(&failing, &attempts));
    rig.at(2_000);
    let hosts: Vec<(&str, &str)> = HOSTS.iter().map(|host| (*host, "1.0")).collect();
    let mut farm = Farm::over(rig, &hosts);
    set(&farm.rig, WEB, "2.0", 2).await;
    assert_eq!(farm.round(), ["d1"]);
    let log = Log::default();
    let _guard = log.install();

    failing.store(true, Ordering::SeqCst);
    // d1 beats at 2.0 twice (updating and the first bake beat come first).
    farm.round();
    farm.round();
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        0,
        "stage 1 of 3 writes nothing"
    );
    farm.at += 10;
    farm.rig.at(farm.at);
    assert_eq!(farm.beat("d1"), ServiceReply::Nothing);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(
        log.text().contains("the plan's next stage was not entered"),
        "{}",
        log.text()
    );
    assert!(log.text().contains("service=web"), "{}", log.text());
    assert!(log.text().contains("disk full"), "{}", log.text());

    assert_eq!(farm.beat("d2"), update(WEB, "2.0"));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "tried again at the next heartbeat"
    );
    assert_eq!(farm.beat("d3"), update(WEB, "2.0"));
    assert_eq!(farm.beat("d4"), ServiceReply::Nothing);
    assert!(farm.rig.coordinator.ops().state().rollouts.is_empty());
    assert!(farm.advances().is_empty());

    failing.store(false, Ordering::SeqCst);
    assert_eq!(farm.beat("d5"), ServiceReply::Nothing);
    assert_eq!(
        farm.rig.coordinator.ops().state().rollouts["web"],
        Rollout {
            version: Some(version("2.0")),
            entered: 3,
            done: ["d1".to_owned()].into(),
            paused: None,
        }
    );
    let advances = farm.advances();
    assert_eq!(advances.len(), 1);
    assert_eq!(advances[0].by, "coordinator");
    assert_eq!(
        advances[0].reason.as_deref(),
        Some("web 2.0: stage 2 of 3 (4 hosts)")
    );
}
