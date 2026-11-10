//! Update plans on a real coordinator over scratch files, at a clock the test sets: a release
//! failure that reaches the rollback threshold inside a stage, a manual stage, the holds, forces
//! and aborts of a plan, and one service's rollout at a time across the fleet.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, ServiceBeat, ServiceReply};
use serde_json::{Value, json};

use crate::decide::{Gate, Why};
use crate::marks_support::{
    NEW_DAEMON, ask, ask_typed, beat_of, digest, force_request, hold, hosts_target, operator,
    scope_host, unhold,
};
use crate::orders::Order;
use crate::services::Services;
use crate::testutil::{OPERATOR, Rig};

/// A host's copy of one service, as the fake updater runs it.
#[derive(Clone, Debug)]
struct Sim {
    running: String,
    /// The version it is updating to.
    updating: Option<String>,
    /// What its last update reported, as the JSON of `last_update`.
    last: Option<Value>,
}

/// A coordinator and the daemons `d1`.. that heartbeat it, each listing its services.
struct Farm {
    rig: Rig,
    hosts: BTreeMap<String, BTreeMap<String, Sim>>,
    at: u64,
}

impl Farm {
    /// A coordinator over the rollout file `text`, letting `live` services roll at once, whose
    /// daemons `hosts` each run every service of `services` at 1.0.
    fn new(test: &str, text: &str, live: usize, services: &[&str], hosts: usize) -> Self {
        let mut rig = Rig::new(test);
        let names: Vec<String> = services.iter().map(|s| format!("\"{s}\"")).collect();
        let list = format!(
            "{}services = [{}]\n",
            hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER),
            names.join(", ")
        );
        rig.coordinator = Rig::start_with_allow_list(&rig.dir, &rig.clock, &list)
            .with_services(Services::parse(text).expect("the services"))
            .with_legacy_service(None)
            .with_live_rollouts(live);
        let sims = services
            .iter()
            .map(|s| {
                let sim = Sim {
                    running: "1.0".to_owned(),
                    updating: None,
                    last: None,
                };
                ((*s).to_owned(), sim)
            })
            .collect::<BTreeMap<_, _>>();
        let hosts = (1..=hosts)
            .map(|i| (format!("d{i}"), sims.clone()))
            .collect();
        let mut farm = Self {
            rig,
            hosts,
            at: 2_000,
        };
        // Every daemon beats once before any version is set, as a coordinator past its quiet
        // start has heard them: nothing is ordered yet.
        assert_eq!(farm.round(), Vec::<String>::new());
        farm
    }

    /// CI sets `service` to `version`, every daemon updating at once at most `max`.
    async fn set(&self, service: &str, version: &str, max: u32) {
        let body = json!({
            "service": service,
            "version": version,
            "release_digest": digest(1),
            "max_updating": {"daemon": max},
        });
        self.rig.set(&body.to_string()).await;
    }

    fn beat(&self, host: &str) -> Heartbeat {
        let services = self.hosts[host]
            .iter()
            .map(|(name, sim)| {
                let mut one = json!({"name": name, "version": sim.running, "health": "healthy"});
                if let Some(target) = &sim.updating {
                    one["updating"] = json!(target);
                }
                if let Some(last) = &sim.last {
                    one["last_update"] = last.clone();
                }
                serde_json::from_value::<ServiceBeat>(one).expect("a service beat")
            })
            .collect();
        beat_of(NEW_DAEMON, services)
    }

    /// One heartbeat of `host`: an update it was told the beat before has finished; the
    /// service and version it is told now, if any.
    fn ask(&mut self, host: &str) -> Option<(String, String)> {
        for sim in self.hosts.get_mut(host).expect("a host").values_mut() {
            if let Some(target) = sim.updating.take() {
                sim.running = target;
            }
        }
        match ask(&self.rig, host, "daemon", self.beat(host)) {
            ServiceReply::Update { service, version } => {
                let service = service.expect("a named service");
                let sim = self.sim(host, &service);
                sim.updating = Some(version.as_str().to_owned());
                Some((service, version.as_str().to_owned()))
            }
            ServiceReply::Nothing => None,
        }
    }

    fn sim(&mut self, host: &str, service: &str) -> &mut Sim {
        self.hosts
            .get_mut(host)
            .and_then(|h| h.get_mut(service))
            .expect("a host of the service")
    }

    /// One round fifteen seconds after the last: every daemon beats, by node id; who was told
    /// what.
    fn round(&mut self) -> Vec<String> {
        self.at += 15;
        self.rig.at(self.at);
        let hosts: Vec<String> = self.hosts.keys().cloned().collect();
        hosts
            .into_iter()
            .filter_map(|h| self.ask(&h).map(|(s, v)| format!("{h} {s} {v}")))
            .collect()
    }

    /// The reason the status gives for the host `host` of `service`.
    fn hold(&self, service: &str, host: &str) -> Option<Why> {
        let status = self.rig.coordinator.status(Some(OPERATOR)).expect("status");
        status.per_service[service]
            .hosts
            .iter()
            .find(|h| h.host.node_id == host)
            .and_then(|h| h.hold)
    }

    /// The plan of `service` as `Releases.Status` answers it.
    fn plan(&self, service: &str) -> proto::RolloutPlan {
        let status = self.rig.coordinator.status(Some(OPERATOR)).expect("status");
        let answer = crate::grpc::report::status(&status, service);
        answer.services[0].plan.clone().expect("a plan")
    }

    fn running(&self, service: &str, version: &str) -> Vec<String> {
        self.hosts
            .iter()
            .filter(|(_, s)| s[service].running == version)
            .map(|(h, _)| h.clone())
            .collect()
    }
}

/// `daemon` hosts of `web`, with `extra` lines of its definition.
fn web(extra: &str) -> String {
    format!("[[service]]\nname = \"web\"\nroles = [\"daemon\"]\n{extra}\n")
}

fn wait(plan: &proto::RolloutPlan) -> proto::PlanWait {
    proto::PlanWait::try_from(plan.waiting).expect("a wait")
}

/// Proves: release failures that reach the threshold (the larger of 25 percent of 8 hosts and
/// 2) within the second stage halt the plan, mark the version bad and set the last good version
/// again, and no host of the third stage is ever ordered the bad version; the hosts that ran it
/// are then ordered back, together.
/// Catches: a threshold checked only at the end of a stage, or a plan that goes on to its next
/// stage over a bad version.
#[tokio::test]
async fn a_failure_reaching_the_threshold_in_stage_two_halts_the_plan_and_rolls_back() {
    let text =
        web("rollback = \"automatic\"\nrollback_after_failed = { percent = 25, min_hosts = 2 }");
    let mut farm = Farm::new("plan-threshold", &text, 1, &["web"], 8);
    farm.set("web", "1.0", 8).await;
    farm.round();
    farm.set("web", "2.0", 8).await;
    let mut told = farm.round();
    assert_eq!(told, ["d1 web 2.0"], "the first stage");
    told.extend(farm.round());
    assert_eq!(
        told[1..],
        ["d2 web 2.0", "d3 web 2.0", "d4 web 2.0"],
        "d1 ran 2.0: stage 2"
    );
    // d2 and d3 fail 2.0 after its commit and stay on 1.0; d4 runs 2.0.
    for host in ["d2", "d3"] {
        let sim = farm.sim(host, "web");
        sim.updating = None;
        sim.last = Some(
            json!({"target": "2.0", "outcome": "failed", "phase": "after_commit", "class": "release"}),
        );
    }
    for _ in 0..4 {
        told.extend(farm.round());
    }
    let ops = farm.rig.coordinator.ops().marks("web");
    assert_eq!(
        ops.bad.iter().map(|v| v.as_str()).collect::<Vec<_>>(),
        ["2.0"]
    );
    assert_eq!(
        farm.rig
            .coordinator
            .desired
            .get("web")
            .expect("web")
            .version
            .as_str(),
        "1.0"
    );
    let third_stage: Vec<&String> = told
        .iter()
        .filter(|t| {
            t.ends_with(" 2.0")
                && ["d5", "d6", "d7", "d8"]
                    .iter()
                    .any(|h| t.starts_with(&format!("{h} ")))
        })
        .collect();
    assert!(
        third_stage.is_empty(),
        "no host of stage 3 was told 2.0: {told:?}"
    );
    assert!(
        told.contains(&"d1 web 1.0".to_owned()) && told.contains(&"d4 web 1.0".to_owned()),
        "{told:?}"
    );
    assert_eq!(farm.running("web", "1.0").len(), 8);
}

/// Proves: a manual stage waits, its hosts told `advance_wait` and the plan `manual_advance`,
/// until an operator's Advance enters it; its hosts are then ordered at their next heartbeats.
/// A pause stops new orders while an update already running finishes; a resume goes on.
#[tokio::test]
async fn a_manual_stage_waits_for_advance_and_a_pause_stops_new_orders() {
    let text = web("stages = [25, { percent = 50, advance = \"manual\" }, 100]");
    let mut farm = Farm::new("plan-manual", &text, 1, &["web"], 8);
    farm.set("web", "2.0", 8).await;
    assert_eq!(farm.round(), ["d1 web 2.0", "d2 web 2.0"]);
    farm.round();
    assert_eq!(
        farm.round(),
        Vec::<String>::new(),
        "stage 1 done; stage 2 is manual"
    );
    let plan = farm.plan("web");
    assert_eq!((plan.current, plan.stages.len()), (1, 3));
    assert_eq!(wait(&plan), proto::PlanWait::ManualAdvance);
    assert_eq!(farm.hold("web", "d3"), Some(Why::Gate(Gate::AdvanceWait)));
    let advance = proto::AdvanceRequest {
        service: "web".to_owned(),
        reason: "canaries look fine".to_owned(),
    };
    let done = farm
        .rig
        .coordinator
        .rollout_advance(&operator(), advance.clone())
        .expect("advanced");
    let entry = done.entry.expect("an entry");
    assert_eq!(entry.action, "rollout_advance");
    assert_eq!(entry.by_name.as_deref(), Some("operator"));
    assert_eq!(
        entry.reason.as_deref(),
        Some("web 2.0: stage 2 of 3 (4 hosts): canaries look fine")
    );
    assert_eq!(done.plan.expect("a plan").current, 2);
    assert_eq!(farm.round(), ["d3 web 2.0", "d4 web 2.0"]);
    // Paused while d3 and d4 update: they finish, nobody else goes, and the next stage, which
    // enters by itself, waits for the resume.
    let pause = proto::PauseRequest {
        service: "web".to_owned(),
        reason: "night".to_owned(),
    };
    farm.rig
        .coordinator
        .rollout_pause(&operator(), pause)
        .expect("paused");
    for _ in 0..3 {
        assert_eq!(farm.round(), Vec::<String>::new());
    }
    assert_eq!(farm.running("web", "2.0"), ["d1", "d2", "d3", "d4"]);
    assert_eq!(farm.hold("web", "d5"), Some(Why::Gate(Gate::Paused)));
    assert_eq!(wait(&farm.plan("web")), proto::PlanWait::Paused);
    let resume = proto::ResumeRequest {
        service: "web".to_owned(),
        reason: "morning".to_owned(),
    };
    farm.rig
        .coordinator
        .rollout_resume(&operator(), resume)
        .expect("resumed");
    assert_eq!(
        farm.round(),
        ["d5 web 2.0", "d6 web 2.0", "d7 web 2.0", "d8 web 2.0"]
    );
    assert_eq!(
        farm.rig
            .coordinator
            .rollout_advance(&operator(), advance)
            .map(|_| ()),
        Err(proto::RolloutsRefusal::NotWaiting),
        "every stage is entered"
    );
}

/// Proves: inside a plan, a host held before its stage is not ordered and takes no place of the
/// stage (the next host to ask takes it); a force orders its host whatever the stage, and the
/// plan does not count a forced host; an abort's halt stops the plan where it stands, and once
/// the operator clears it the plan goes on in the same stage, the aborted host left out; an
/// unheld host is eligible again and is ordered when the plan has a place for it. The abort is
/// the operator's `Hosts.Abort`: the host's next typed heartbeat is answered with the order.
#[tokio::test]
async fn holds_forces_and_aborts_work_inside_a_plan() {
    let mut farm = Farm::new("plan-marks", &web(""), 1, &["web"], 8);
    farm.set("web", "2.0", 8).await;
    hold(&farm.rig, "web", scope_host("d3")).expect("held");
    assert_eq!(farm.round(), ["d1 web 2.0"]);
    assert_eq!(farm.plan("web").eligible, 7, "the held host is not counted");
    // d1 runs 2.0 at its next beat; stage 2 of 7 eligible holds 4: three new places, d3 held,
    // so d2, d4 and d5 take them.
    assert_eq!(farm.round(), ["d2 web 2.0", "d4 web 2.0", "d5 web 2.0"]);
    assert_eq!(farm.hold("web", "d3"), Some(Why::Gate(Gate::Held)));
    // A force of 2.0 to d8 orders it at its next heartbeat, past the plan.
    farm.rig
        .coordinator
        .force(
            &operator(),
            force_request("web", "2.0", hosts_target(&["d8"]), false),
        )
        .expect("forced");
    let beat = farm.beat("d8");
    let (_, order) = ask_typed(&farm.rig, "d8", "daemon", beat);
    assert!(matches!(order, Some(Order::Force { .. })), "{order:?}");
    assert_eq!(farm.plan("web").eligible, 6, "nor is the forced host");
    // The operator aborts d2 mid-update through Hosts.Abort: d2's next typed heartbeat is
    // answered with the abort, and it reports the halt that ends its order.
    farm.rig
        .coordinator
        .abort(
            &operator(),
            proto::AbortRequest {
                node_id: "d2".to_owned(),
                service: "web".to_owned(),
                reason: "stuck step".to_owned(),
            },
        )
        .expect("queued");
    let beat = farm.beat("d2");
    let (_, order) = ask_typed(&farm.rig, "d2", "daemon", beat);
    assert!(
        matches!(&order, Some(Order::Abort { service, .. }) if service == "web"),
        "{order:?}"
    );
    farm.sim("d2", "web").updating = None;
    farm.sim("d2", "web").last = Some(
        json!({"target": "2.0", "outcome": "failed", "phase": "halt", "reason": "aborted_by_operator"}),
    );
    farm.round();
    let plan = farm.plan("web");
    assert_eq!((wait(&plan), plan.current), (proto::PlanWait::Halted, 2));
    assert_eq!(farm.hold("web", "d6"), Some(Why::Gate(Gate::Halted)));
    farm.rig
        .coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"clear_halt","service":"web","reason":"aborted on purpose"}"#,
        )
        .expect("cleared");
    let told = farm.round();
    assert_eq!(
        farm.plan("web").current,
        3,
        "d2 left the plan: stage 2 done with d4 and d5"
    );
    assert!(!told.iter().any(|t| t.starts_with("d2 ")), "{told:?}");
    // The operator releases d3: it is eligible again, counted by the plan, and ordered at its
    // next heartbeat since the third stage has a place for it.
    assert_eq!(
        farm.plan("web").eligible,
        5,
        "d3 is held, d8 forced, d2 aborted"
    );
    unhold(&farm.rig, "web", scope_host("d3")).expect("unheld");
    assert_eq!(farm.plan("web").eligible, 6, "d3 counts again");
    assert_eq!(farm.hold("web", "d3"), None);
    let told = farm.round();
    assert!(told.contains(&"d3 web 2.0".to_owned()), "{told:?}");
}

/// Proves: with `--live-rollouts 1` a service whose rollout is not live orders nothing while
/// another's is, between that one's stages too (`rollout_elsewhere`), and goes once it is
/// done; with 2, both roll at once.
/// Catches: the rule of one service's rollout at a time lost between two stages of a plan.
#[tokio::test]
async fn one_service_rolls_at_a_time_unless_the_setting_lets_more() {
    let text = format!(
        "{}\n{}",
        web(""),
        "[[service]]\nname = \"db\"\nroles = [\"daemon\"]\n"
    );
    let mut farm = Farm::new("plan-one-service", &text, 1, &["web", "db"], 4);
    farm.set("web", "2.0", 8).await;
    farm.set("db", "2.0", 8).await;
    let first = farm.round();
    let (rolling, other) = if first[0].contains(" web ") {
        ("web", "db")
    } else {
        ("db", "web")
    };
    assert_eq!(first, [format!("d1 {rolling} 2.0")]);
    farm.round();
    assert_eq!(
        farm.hold(other, "d4"),
        Some(Why::Gate(Gate::RolloutElsewhere))
    );
    let mut log = Vec::new();
    for _ in 0..8 {
        log.extend(farm.round());
    }
    let first_other = log
        .iter()
        .position(|t| t.contains(&format!(" {other} ")))
        .expect("the other rolled");
    assert!(
        log[..first_other]
            .iter()
            .all(|t| t.contains(&format!(" {rolling} "))),
        "{other} waited for every stage of {rolling}: {log:?}"
    );
    assert_eq!(farm.running(rolling, "2.0").len(), 4);
    // Two at once.
    let mut farm = Farm::new("plan-two-services", &text, 2, &["web", "db"], 4);
    farm.set("web", "2.0", 8).await;
    farm.set("db", "2.0", 8).await;
    let told = farm.round();
    assert_eq!(told.len(), 2, "{told:?}");
    assert!(told.iter().any(|t| t.contains(" web ")) && told.iter().any(|t| t.contains(" db ")));
}
