//! The state files across the two coordinators: what kbf's coordinator wrote is read here, and
//! what this one writes is still read by kbf's, which refuses a key it does not know and a
//! schema newer than its own.
//!
//! The fixtures are kbf v0.0.64's bytes (`fixtures/recorded`). `strict-readers.json` lists, for
//! every struct kbf reads from these files (the desired-state file, the ops file and the halts
//! file beside it, and the report in them), the keys it allows, the keys it requires, the keys
//! that take null, the schema it reads, whether it skips a key it does not know (`lenient`: the
//! halts file, its records and the report; every other struct refuses one) and the type of
//! every value. The mirror of those readers (`support::readers`; its own checks are in
//! `state_readers.rs`) is held over the files this coordinator writes, so a file that kbf would
//! refuse, or would read with a record of the halts file passed over, fails here.

mod support;

use std::fmt::Write as _;

use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::ops::{AuditEntry, FailureAck, OpsError, OpsStore, Write};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use support::beats::{Daemon, grpc_beat, runs};
use support::readers::{Readers, parse};
use support::rig::{Rig, Spec};
use support::targets::{SET_DESIRED, grpc, list, repository_entry, set};
use support::{Flags, ci_token, recorded, recorded_bytes, scratch};

/// The desired-state file the rig's coordinator has written, checked against kbf's reader of
/// `file`: `desired-single` for the single state, `desired-map` for the map.
fn check_desired(rig: &Rig, readers: &Readers, file: &str) {
    readers.check_desired_in(rig.dir.path(), file);
}

/// The ops file and the halts file the rig's coordinator has written, checked against kbf's
/// readers, down into the records of both; the halts file names the digest of the ops file it
/// goes with.
fn check_ops(rig: &Rig, readers: &Readers) {
    readers.check_ops_in(rig.dir.path());
}

fn bytes(rig: &Rig, file: &str) -> Vec<u8> {
    std::fs::read(rig.dir.path().join(file)).unwrap_or_else(|why| panic!("{file}: {why}"))
}

const HALT_BEAT: &str = concat!(
    r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","#,
    r#""last_update":{"target":"2.0","outcome":"failed","phase":"halt","reason":"disk_tainted"}}"#
);

/// A second host's report of a halt, with a class and a reason of its own.
const SECOND_HALT_BEAT: &str = concat!(
    r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","#,
    r#""last_update":{"target":"2.0","outcome":"failed","phase":"halt","class":"environment","#,
    r#""reason":"state_unreadable"}}"#
);

/// The six daemons of the scenarios of a rollback.
const DAEMONS: [&str; 6] = ["d1", "d2", "d3", "d4", "d5", "d6"];

/// The rollout file of those scenarios: kbf's service as one of daemons that the coordinator rolls
/// back by itself, as the test of kbf's rollback has it.
const AUTOMATIC: &str =
    "[[service]]\nname = \"kbf\"\nrollback = \"automatic\"\nroles = [\"daemon\"]\n";

/// The desired state of those scenarios a set refuses.
const TWO: &str = r#"{"version":"2.0","min_up":{"storage":0,"frontend":0}}"#;

/// A service that runs on hosts of a role, `gpu`, which the desired-state file format does not
/// name (it carries limits for four roles).
const RENDER: &str = "[[service]]\nname = \"render\"\nroles = [\"gpu\"]\n";

/// kbf's desired-state files (the single state and the map) are read with the recorded flags and
/// written back to the same bytes.
fn kbf_desired_files_are_read_and_rewritten_to_the_same_bytes() {
    let flags = Flags::recorded();
    let legacy = flags.legacy_service.as_deref();
    for (fixture, names) in [
        ("desired-single.json", vec!["kbf"]),
        ("desired-map.json", vec!["kbf", "tierstore"]),
    ] {
        let dir = scratch("state-kbf-desired");
        let path = dir.path().join("desired.json");
        std::fs::write(&path, recorded_bytes(fixture)).expect("written");
        let store = DesiredStore::open(&path, legacy).expect("kbf's file is read");
        let held: Vec<String> = store.all().into_keys().collect();
        assert_eq!(held, names, "{fixture}");
        let kbf = store.get("kbf").expect("kbf's state");
        assert_eq!(kbf.version.to_string(), "2.0");
        assert_eq!(kbf.max_updating.get("daemon"), Some(1));
        assert_eq!(kbf.max_updating.get("mac-daemon"), Some(2));
        assert_eq!(kbf.min_up.get("storage"), Some(1));
        assert_eq!(kbf.min_up.get("frontend"), Some(1));
        if let Some(tier) = store.get("tierstore") {
            assert_eq!(tier.version.to_string(), "7.1");
            assert_eq!(tier.min_up.get("storage"), Some(4));
            assert_eq!(tier.min_up.get("frontend"), Some(0));
        }
        store.set("kbf", kbf).expect("rewritten");
        assert_eq!(
            std::fs::read(&path).expect("read"),
            recorded_bytes(fixture),
            "{fixture} is written back as kbf wrote it"
        );
    }
}

/// kbf's ops file and the halts file beside it are read: the exclusion, the label, the halt with
/// its report and the audit ring; a change made here rewrites both within kbf's readers.
async fn kbf_ops_files_are_read_and_rewritten_within_kbfs_readers(readers: &Readers) {
    let mut rig = Rig::recorded("state-kbf-ops");
    for (fixture, file) in [
        ("desired-single.json", "desired.json"),
        ("ops.json", "desired.json.ops"),
        ("ops.json.halts", "desired.json.ops.halts"),
    ] {
        std::fs::write(rig.dir.path().join(file), recorded_bytes(fixture)).expect("written");
    }
    let store = OpsStore::open(&rig.dir.path().join("desired.json.ops")).expect("kbf's files");
    let state = store.state();
    assert_eq!(state.excluded.len(), 1);
    assert_eq!(
        (
            state.excluded[0].service.as_str(),
            state.excluded[0].node_id.as_str(),
            state.excluded[0].reason.as_str()
        ),
        ("kbf", "s2", "bad disk")
    );
    assert_eq!(state.hosts["s1"].labels["rack"], "r1");
    let halt = state.services["kbf"].halt.clone().expect("the halt");
    assert_eq!(
        (
            halt.node_id.as_str(),
            halt.check.as_str(),
            halt.target.to_string().as_str()
        ),
        ("s1", "disk_tainted", "2.0")
    );
    assert_eq!(
        serde_json::to_value(halt.report.expect("the report of the halts file")).expect("json"),
        json!({"target": "2.0", "outcome": "failed", "phase": "halt", "reason": "disk_tainted"})
    );
    let actions: Vec<&str> = state.audit.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, ["exclude", "set_labels", "halt"]);

    rig.restart();
    rig.at(1_100);
    let (code, body) = rig.convergence(None).await;
    assert_eq!(code, 200, "{body}");
    assert!(
        body.contains(r#""state":"halted""#),
        "kbf's halt stands: {body}"
    );
    let (code, body) = rig
        .operator(r#"{"action":"include","service":"kbf","node_id":"s2"}"#)
        .await;
    assert_eq!(code, 200, "{body}");
    check_ops(&rig, readers);
    let ops = parse(&rig.dir.path().join("desired.json.ops"));
    assert_eq!(ops["excluded"], json!([]));
    assert_eq!(ops["services"]["kbf"]["halt"]["node_id"], "s1");
    let halts = parse(&rig.dir.path().join("desired.json.ops.halts"));
    assert_eq!(
        halts["services"]["kbf"]["halt"]["report"]["reason"], "disk_tainted",
        "the halt keeps its report in the halts file"
    );
}

/// What this coordinator writes, step by step, is within kbf's readers; and for the states and
/// the changes kbf's fixtures record it is kbf's bytes.
async fn what_this_coordinator_writes_is_within_kbfs_readers(readers: &Readers) {
    let rig = Rig::new(
        "state-written",
        Spec::recorded().with_oidc_services(&["kbf", "tierstore"]),
    );
    rig.at(1_100);
    assert!(!rig.dir.path().join("desired.json").exists());
    let (code, body) = rig.desired(support::STATE).await;
    assert_eq!(code, 200, "{body}");
    check_desired(&rig, readers, "desired-single");
    assert_eq!(
        bytes(&rig, "desired.json"),
        recorded_bytes("desired-single.json")
    );
    assert!(
        !rig.dir.path().join("desired.json.ops").exists(),
        "a farm that changes nothing keeps no ops file"
    );

    let tier = r#"{"service":"tierstore","version":"7.1","min_up":{"storage":4}}"#;
    let (code, body) = rig.desired(tier).await;
    assert_eq!(code, 200, "{body}");
    check_desired(&rig, readers, "desired-map");
    assert_eq!(
        bytes(&rig, "desired.json"),
        recorded_bytes("desired-map.json")
    );

    let exclude = r#"{"action":"exclude","service":"kbf","node_id":"s2","reason":"bad disk"}"#;
    let labels = r#"{"action":"set_labels","node_id":"s1","labels":{"rack":"r1"}}"#;
    for body in [exclude, labels] {
        let (code, answer) = rig.operator(body).await;
        assert_eq!(code, 200, "{answer}");
        check_ops(&rig, readers);
    }
    let (code, _) = rig.heartbeat("s1", "storage", HALT_BEAT).await;
    assert_eq!(code, 200);
    check_ops(&rig, readers);
    assert_eq!(bytes(&rig, "desired.json.ops"), recorded_bytes("ops.json"));
    assert_eq!(
        bytes(&rig, "desired.json.ops.halts"),
        recorded_bytes("ops.json.halts"),
        "the halt and its report are written as kbf wrote them"
    );

    let clear = r#"{"action":"clear_halt","service":"kbf","reason":"disk replaced"}"#;
    let include = r#"{"action":"include","service":"kbf","node_id":"s2"}"#;
    let unlabel = r#"{"action":"set_labels","node_id":"s1","labels":{}}"#;
    for body in [clear, include, unlabel] {
        let (code, answer) = rig.operator(body).await;
        assert_eq!(code, 200, "{answer}");
        check_ops(&rig, readers);
    }
    let ops = parse(&rig.dir.path().join("desired.json.ops"));
    assert_eq!(ops["services"]["kbf"]["halt"], Value::Null);
    // The clear's generation is kept in the halts file, as kbf keeps it, so the ops file
    // stays one that kbf's strict reader takes.
    assert_eq!(ops["services"]["kbf"]["halt_generation"], Value::Null);
    let halts = parse(&rig.dir.path().join("desired.json.ops.halts"));
    assert_eq!(halts["services"]["kbf"]["halt_generation"], 1);
    assert_eq!(ops["audit"].as_array().map(Vec::len), Some(6));
}

/// Who made each change of the ops file's ring and what it was, oldest first.
fn audited(ops: &Value) -> Vec<(&str, &str)> {
    ops["audit"]
        .as_array()
        .expect("the ring")
        .iter()
        .map(|entry| {
            (
                entry["by"].as_str().expect("who"),
                entry["action"].as_str().expect("what"),
            )
        })
        .collect()
}

/// The heartbeat of a daemon that announces `target-v1`, on 1.0.
const NATIVE_BEAT: &str = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","agent_features":["target-v1"]}"#;

/// The heartbeat of a daemon on 1.0 whose last update, to 2.0, failed as `report` says.
fn failed_beat(report: &Value) -> String {
    json!({
        "platform": "linux-x86_64",
        "version": "1.0",
        "health": "healthy",
        "agent_features": ["target-v1"],
        "last_update": report
    })
    .to_string()
}

/// CI sets kbf at `version` as a release that lists one `apt` entry for `linux-x86_64`, over gRPC:
/// the hosts that list `apt` ([`hear_the_daemons`]) are picked it, and ordered it.
async fn set_release(rig: &Rig, version: &str) {
    let targets = std::collections::BTreeMap::from([(
        "linux-x86_64".to_owned(),
        list(vec![repository_entry("kbf", "1.6.0", 'c')]),
    )]);
    let answered = grpc(
        rig,
        SET_DESIRED,
        Some(&ci_token()),
        &set("kbf", version, targets),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
}

/// Each of the six daemons says over gRPC that it lists `apt` and runs kbf from it, as a daemon
/// that announces `target-v1` does; the heartbeats over JSON that follow leave that as it was.
async fn hear_the_daemons(rig: &Rig) {
    for host in DAEMONS {
        let daemon = Daemon::new(
            host,
            hostkeeper_proto::hostkeeper::agent::v1::Platform::LinuxX8664,
            &["apt"],
        )
        .running(runs("kbf", "1.0", Some("apt")));
        grpc_beat(rig, &daemon).await;
    }
}

/// One heartbeat of each of the six daemons at `at`: those named in `failed` report the failure
/// they are paired with, the others are healthy on 1.0.
async fn round(rig: &Rig, at: u64, failed: &[(&str, &Value)]) {
    rig.at(at);
    for host in DAEMONS {
        let body = match failed.iter().find(|(name, _)| *name == host) {
            Some((_, report)) => failed_beat(report),
            None => NATIVE_BEAT.to_owned(),
        };
        let (code, answer) = rig.heartbeat(host, "daemon", &body).await;
        assert_eq!(code, 200, "{host}: {answer}");
    }
}

/// A rig whose service the coordinator has rolled back, as the test of kbf's rollback has it:
/// 1.0 is wanted and converges on six daemons, so it is recorded as the last good version; CI
/// sets 2.0; one daemon fails it after the commit and a second at the release gate, which is
/// enough hosts to mark 2.0 bad and set 1.0 again.
async fn rolled_back(name: &str) -> Rig {
    let rig = Rig::new(name, Spec::recorded().with_services(AUTOMATIC));
    hear_the_daemons(&rig).await;
    set_release(&rig, "1.0").await;
    round(&rig, 2_000, &[]).await;
    set_release(&rig, "2.0").await;
    let after_commit =
        json!({"target": "2.0", "outcome": "failed", "phase": "after_commit", "class": "release"});
    let at_the_gate = json!({"target": "2.0", "outcome": "failed", "phase": "gate_release"});
    round(&rig, 2_015, &[("d1", &after_commit)]).await;
    round(&rig, 2_030, &[("d1", &after_commit), ("d2", &at_the_gate)]).await;
    rig
}

/// An automatic rollback writes the last good version and the bad one into the ops file, and
/// sets the desired state back: the files are within kbf's readers.
async fn an_automatic_rollback_writes_its_marks_within_kbfs_readers(readers: &Readers) {
    let rig = rolled_back("state-rollback").await;
    check_ops(&rig, readers);
    check_desired(&rig, readers, "desired-single");
    let ops = parse(&rig.dir.path().join("desired.json.ops"));
    let marks = &ops["services"]["kbf"];
    assert_eq!(marks["last_good"], "1.0");
    assert_eq!(marks["bad"], json!(["2.0"]));
    // CI's sets, with their releases, are audited as they are made.
    assert_eq!(
        audited(&ops),
        [
            ("ci", "set_desired"),
            ("coordinator", "last_good"),
            ("ci", "set_desired"),
            ("coordinator", "mark_bad")
        ]
    );
    assert_eq!(
        parse(&rig.dir.path().join("desired.json"))["version"],
        "1.0",
        "the desired state is the last good version again"
    );
}

/// CI setting by hand the version the coordinator marked bad is refused (`bad_version`) and
/// changes no file; the acks an override writes (one for each host that failed, with the whole
/// report) put in the ops file beside the marks are within kbf's readers.
async fn a_refused_set_of_a_bad_version_and_failure_acks_are_within_kbfs_readers(
    readers: &Readers,
) {
    let rig = rolled_back("state-failure-ack").await;
    let ops_path = rig.dir.path().join("desired.json.ops");
    let before = (bytes(&rig, "desired.json"), bytes(&rig, "desired.json.ops"));
    rig.at(2_040);
    let (code, body) = rig.desired(TWO).await;
    assert_eq!((code, body.as_str()), (409, r#"{"error":"bad_version"}"#));
    assert_eq!(
        (bytes(&rig, "desired.json"), bytes(&rig, "desired.json.ops")),
        before,
        "a refused set changes no file"
    );
    let report = |text: &str| -> hostkeeper_wire::wire::UpdateResult {
        serde_json::from_str(text).expect("a report")
    };
    let acks = [
        (
            "d1",
            report(
                r#"{"target":"2.0","outcome":"failed","phase":"after_commit","class":"release"}"#,
            ),
        ),
        (
            "d2",
            report(r#"{"target":"2.0","outcome":"failed","phase":"gate_release"}"#),
        ),
    ];
    OpsStore::open(&ops_path)
        .expect("the ops file")
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "unmark_bad"),
            |state| {
                let marks = state.services.entry("kbf".to_owned()).or_default();
                marks.bad.clear();
                marks.failure_acked = acks
                    .iter()
                    .map(|(node, report)| FailureAck {
                        node_id: (*node).to_owned(),
                        report: report.clone(),
                    })
                    .collect();
            },
        )
        .expect("written");
    check_ops(&rig, readers);
    check_desired(&rig, readers, "desired-single");
    let ops = parse(&ops_path);
    assert_eq!(
        ops["services"]["kbf"]["failure_acked"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
}

/// Limits for a role the file format does not name (it carries `max_updating` for four roles
/// and `min_up` for two) are kept for the service that has the role, after the format's own keys,
/// and dropped, and named in the answer, for a service that has not: the state file is within
/// kbf's readers after each, and the custom role is in it only under the service that has it.
async fn limits_for_a_custom_role_are_written_within_kbfs_readers(readers: &Readers) {
    let services = format!("{}\n{RENDER}", recorded("kbf.services.toml"));
    let spec = Spec::recorded()
        .with_services(&services)
        .with_oidc_services(&["kbf", "render"]);
    let rig = Rig::new("state-custom-role", spec);

    let render =
        r#"{"service":"render","version":"3.0","max_updating":{"gpu":3},"min_up":{"gpu":2}}"#;
    let (code, body) = rig.desired(render).await;
    assert_eq!(code, 200, "{body}");
    assert!(!body.contains("dropped_roles"), "{body}");
    check_desired(&rig, readers, "desired-map");
    let file = parse(&rig.dir.path().join("desired.json"));
    assert_eq!(
        file["services"]["render"]["max_updating"],
        json!({"frontend": 1, "storage": 1, "daemon": 2, "mac-daemon": 2, "gpu": 3})
    );
    assert_eq!(
        file["services"]["render"]["min_up"],
        json!({"storage": 0, "frontend": 0, "gpu": 2})
    );

    let kbf = r#"{"version":"2.0","max_updating":{"daemon":1,"gpu":4},"min_up":{"storage":1,"frontend":1,"gpu":1}}"#;
    let (code, body) = rig.desired(kbf).await;
    assert_eq!(code, 200, "{body}");
    assert!(body.ends_with(r#","dropped_roles":["gpu"]}"#), "{body}");
    check_desired(&rig, readers, "desired-map");
    let file = parse(&rig.dir.path().join("desired.json"));
    assert_eq!(
        file["services"]["kbf"]["max_updating"],
        json!({"frontend": 1, "storage": 1, "daemon": 1, "mac-daemon": 2})
    );
    assert_eq!(
        file["services"]["kbf"]["min_up"],
        json!({"storage": 1, "frontend": 1})
    );
    assert_eq!(file["services"]["render"]["max_updating"]["gpu"], 3);
    assert_eq!(file["services"]["render"]["min_up"]["gpu"], 2);
}

/// A second host reports a halt while one stands, and an operator clears it: the halts file keeps
/// the report of the standing halt and, apart from it, the report seen while it stood; after the
/// clear, the two acknowledged and the generation. Both files are within kbf's readers each time.
async fn a_second_halt_report_and_the_clear_are_written_within_kbfs_readers(readers: &Readers) {
    let rig = Rig::recorded("state-halt-seen");
    rig.at(1_100);
    let (code, body) = rig.desired(support::STATE).await;
    assert_eq!(code, 200, "{body}");
    for (host, beat) in [("s1", HALT_BEAT), ("s3", SECOND_HALT_BEAT)] {
        let (code, answer) = rig.heartbeat(host, "storage", beat).await;
        assert_eq!(code, 200, "{host}: {answer}");
        check_ops(&rig, readers);
    }
    let ops = parse(&rig.dir.path().join("desired.json.ops"));
    assert!(
        ops["services"]["kbf"].get("halt_seen").is_none(),
        "the report seen while the halt stands is in the halts file only"
    );
    let halts = parse(&rig.dir.path().join("desired.json.ops.halts"));
    let kbf = &halts["services"]["kbf"];
    assert_eq!(kbf["halt"]["node_id"], "s1");
    assert_eq!(kbf["halt_seen"].as_array().map(Vec::len), Some(1));
    assert_eq!(kbf["halt_seen"][0]["node_id"], "s3");
    assert_eq!(kbf["halt_seen"][0]["report"]["reason"], "state_unreadable");

    let clear = r#"{"action":"clear_halt","service":"kbf","reason":"disks replaced"}"#;
    let (code, answer) = rig.operator(clear).await;
    assert_eq!(code, 200, "{answer}");
    check_ops(&rig, readers);
    let halts = parse(&rig.dir.path().join("desired.json.ops.halts"));
    let kbf = &halts["services"]["kbf"];
    let acked: Vec<&str> = kbf["halt_acked"]
        .as_array()
        .expect("the acknowledgements")
        .iter()
        .map(|ack| ack["node_id"].as_str().expect("a host"))
        .collect();
    assert_eq!(acked, ["s1", "s3"], "the clear acknowledges both reports");
    assert_eq!(kbf["halt_generation"], 1);
    assert!(kbf.get("halt").is_none() && kbf.get("halt_seen").is_none());
}

/// The halts file's `rollouts` key: where each service's update plan stands.
fn rollouts_of(rig: &Rig) -> Value {
    parse(&rig.dir.path().join("desired.json.ops.halts"))
        .get("rollouts")
        .cloned()
        .unwrap_or(Value::Null)
}

/// An operator pauses a service's update plan, and the plan of another enters its second stage
/// with hosts left to order: each leaves a position under `rollouts` in the halts file, a key
/// kbf's lenient reader of that file skips, and both files are within kbf's readers each time.
async fn an_update_plan_position_is_written_within_kbfs_readers(readers: &Readers) {
    let rig = Rig::new(
        "state-plan-pause",
        Spec::recorded().with_services(AUTOMATIC),
    );
    assert!(
        !rig.dir.path().join("desired.json.ops.halts").exists(),
        "a farm that has not paused a plan keeps no halts file"
    );
    let pause = hostkeeper_proto::hostkeeper::operator::v1::PauseRequest {
        service: "kbf".to_owned(),
        reason: "waiting for the maintainers".to_owned(),
    };
    let by =
        hostkeeper_coordinator::operator::Caller::Operator("operator".parse().expect("a name"));
    rig.coordinator.rollout_pause(&by, pause).expect("paused");
    check_ops(&rig, readers);
    assert_eq!(
        rollouts_of(&rig)["kbf"]["paused"]["reason"],
        "waiting for the maintainers"
    );
    let ops = parse(&rig.dir.path().join("desired.json.ops"));
    assert!(
        ops.get("rollouts").is_none() && ops["services"]["kbf"].get("rollouts").is_none(),
        "the position is in the halts file only"
    );

    // Six daemons on the default plan are three stages, 1, 3 and 6 hosts: d1 goes alone and,
    // once it runs 2.0, the plan enters the second stage with hosts left to order.
    let rig = Rig::new(
        "state-plan-stage",
        Spec::recorded().with_services(AUTOMATIC),
    );
    hear_the_daemons(&rig).await;
    set_release(&rig, "1.0").await;
    round(&rig, 2_000, &[]).await;
    set_release(&rig, "2.0").await;
    round(&rig, 2_015, &[]).await;
    let on_two = NATIVE_BEAT.replace("\"version\":\"1.0\"", "\"version\":\"2.0\"");
    for at in (2_030..2_090).step_by(15) {
        rig.at(at);
        for host in DAEMONS {
            let body = if host == "d1" { &on_two } else { NATIVE_BEAT };
            let (code, answer) = rig.heartbeat(host, "daemon", body).await;
            assert_eq!(code, 200, "{host}: {answer}");
        }
        if rollouts_of(&rig) != Value::Null {
            break;
        }
    }
    check_ops(&rig, readers);
    let position = &rollouts_of(&rig)["kbf"];
    assert_eq!(position["version"], "2.0");
    assert_eq!(position["done"], json!(["d1"]));
}

/// An ops file of a newer schema is refused at open and never rewritten: its bytes are the same
/// afterwards and nothing else is made beside it.
fn a_newer_ops_file_is_refused_and_left_alone() {
    let dir = scratch("state-newer");
    let path = dir.path().join("ops.json");
    std::fs::write(&path, recorded_bytes("ops-schema2.json")).expect("written");
    match OpsStore::open(&path) {
        Err(OpsError::Newer { found: 2, .. }) => {}
        Err(other) => panic!("not the refusal of a newer schema: {other}"),
        Ok(_) => panic!("a newer schema was read"),
    }
    assert_eq!(
        std::fs::read(&path).expect("read"),
        recorded_bytes("ops-schema2.json")
    );
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .expect("a directory")
        .map(|e| e.expect("an entry").file_name())
        .collect();
    assert_eq!(files, ["ops.json"], "nothing was made beside it");
}

/// Proves: the state files are compatible both ways. A desired-state file (single and map) and
/// an ops file with its halts file, as kbf's coordinator wrote them, are read by this one with
/// the recorded flags, and a desired-state file written back is the same bytes. What this one
/// writes parses as JSON and, checked by a mirror of kbf's readers (`strict-readers.json`: the
/// keys each allows, requires, skips and takes null for, the schema and the type of every value,
/// down into the records of the halts file and the report in them), is a file kbf reads whole:
/// the desired state in both forms, and with limits for a role the format does not name (kept
/// for the service that has the role, dropped for one that has not); the ops file and the halts
/// file after an exclusion, a label, a halt, a second host's halt report, a clear, an include
/// and a removal of labels, after an operator pauses a plan and after a plan enters its second
/// stage (the position under `rollouts` in the halts file), after an automatic rollback (the last good version and the bad
/// one) and after CI sets the bad version by hand (the failure acks). For the changes kbf's
/// fixtures record it is kbf's bytes. An ops file of schema 2 is refused at open, and its bytes
/// are unchanged afterwards.
/// Catches: a change of what this coordinator writes to a state file, in a step of any of those
/// scenarios, that kbf's coordinator would refuse or read with a record of the halts file lost.
#[tokio::test]
async fn state_files_written_by_kbf_are_read_and_the_reverse_holds_by_schema_rules() {
    let readers = Readers::recorded();
    assert!(readers.from.contains("v0.0.64"), "{}", readers.from);
    assert_eq!(
        readers.files.keys().map(String::as_str).collect::<Vec<_>>(),
        ["desired-map", "desired-single", "halts", "ops"]
    );
    kbf_desired_files_are_read_and_rewritten_to_the_same_bytes();
    kbf_ops_files_are_read_and_rewritten_within_kbfs_readers(&readers).await;
    what_this_coordinator_writes_is_within_kbfs_readers(&readers).await;
    an_automatic_rollback_writes_its_marks_within_kbfs_readers(&readers).await;
    a_refused_set_of_a_bad_version_and_failure_acks_are_within_kbfs_readers(&readers).await;
    limits_for_a_custom_role_are_written_within_kbfs_readers(&readers).await;
    a_second_halt_report_and_the_clear_are_written_within_kbfs_readers(&readers).await;
    an_update_plan_position_is_written_within_kbfs_readers(&readers).await;
    a_newer_ops_file_is_refused_and_left_alone();
}

/// Catches: a `check_ops` that reads the root keys and the digest of the halts file and not the
/// records in it. kbf's halts file with an outcome kbf does not know in the report of its
/// standing halt, its root keys and its digest as they were, is refused, naming where.
#[test]
#[should_panic(expected = "halts.services.kbf.halt.report.outcome")]
fn the_check_of_the_halts_file_reads_the_records_in_it() {
    let rig = Rig::recorded("state-bad-halts");
    let mut halts: Value =
        serde_json::from_slice(&recorded_bytes("ops.json.halts")).expect("kbf's halts file");
    halts["services"]["kbf"]["halt"]["report"]["outcome"] = json!("exploded");
    let mut text = serde_json::to_string_pretty(&halts).expect("JSON");
    text.push('\n');
    for (file, contents) in [
        ("desired.json.ops", recorded_bytes("ops.json")),
        ("desired.json.ops.halts", text.into_bytes()),
    ] {
        std::fs::write(rig.dir.path().join(file), contents).expect("written");
    }
    check_ops(&rig, &Readers::recorded());
}

/// Catches: a mirror that takes anything. A key kbf's reader does not have, a key it requires
/// that is missing, a schema it does not read, and the same inside an array or a map are each
/// refused, naming where; kbf's own ops file and halts file are taken.
#[test]
fn the_mirror_of_kbfs_readers_refuses_what_they_refuse() {
    let readers = Readers::recorded();
    let kbf: Value = serde_json::from_str(&recorded("ops.json")).expect("kbf's ops file");
    readers.check("OpsState", &kbf, "ops").expect("kbf's file");
    let halts: Value = serde_json::from_str(&recorded("ops.json.halts")).expect("kbf's halts");
    readers
        .check("ReadHalts", &halts, "halts")
        .expect("kbf's halts file");
    let mut cases: Vec<(Value, &str)> = Vec::new();
    for (patch, word) in [
        (json!({"future": true}), "future"),
        (json!({"schema": 2}), "schema 1"),
    ] {
        let mut value = kbf.clone();
        value
            .as_object_mut()
            .expect("object")
            .extend(patch.as_object().expect("object").clone());
        cases.push((value, word));
    }
    let mut no_schema = kbf.clone();
    no_schema.as_object_mut().expect("object").remove("schema");
    cases.push((no_schema, "requires schema"));
    let mut nested = kbf.clone();
    nested["excluded"][0]["why"] = json!("because");
    cases.push((nested, "excluded[0]"));
    let mut deeper = kbf.clone();
    deeper["services"]["kbf"]["halt"]["extra"] = json!(1);
    cases.push((deeper, "services.kbf.halt"));
    let mut lacking = kbf.clone();
    lacking["audit"][1]
        .as_object_mut()
        .expect("object")
        .remove("by");
    cases.push((lacking, "audit[1]"));
    for (value, word) in cases {
        let why = readers
            .check("OpsState", &value, "ops")
            .expect_err("refused");
        assert!(why.contains(word), "{why} lacks {word}");
    }
    let desired: Value = serde_json::from_str(&recorded("desired-map.json")).expect("desired");
    readers
        .check("MapFile", &desired, "desired")
        .expect("kbf's map");
    let mut stray = desired.clone();
    stray["services"]["kbf"]["version2"] = json!("3");
    assert!(readers.check("MapFile", &stray, "desired").is_err());
}

/// Names the readers of the release before that refuse the keys this coordinator adds, and
/// those that skip them. The desired-state file's readers (`desired-single` and `desired-map`)
/// were strict, and refuse `release_digest`, `release_index` and `digests` once CI sends a
/// release: a coordinator rolled back below this one refuses to start rather than run without
/// the digests. With no digest and no index none of the keys is written (`an_old_state_file_still_reads` in the unit tests). The halts
/// file's reader was lenient and skips `origin` on a halt, keeping the record.
///
/// Catches: a new key put in a file whose earlier reader is strict where it was not meant to be,
/// or a halts record that the earlier reader would pass over.
#[tokio::test]
async fn the_new_keys_are_refused_only_by_the_readers_that_were_strict() {
    let readers = Readers::recorded();
    let spec = Spec::recorded().with_oidc_services(&["kbf", "tierstore"]);
    let rig = Rig::new("state-new-keys", spec);
    let digest = "ab".repeat(32);
    let single = format!(
        r#"{{"version":"2.0","min_up":{{"storage":0,"frontend":0}},"release_digest":"{digest}"}}"#
    );
    let (code, body) = rig.desired(&single).await;
    assert_eq!(code, 200, "{body}");
    let state = parse(&rig.dir.path().join("desired.json"));
    assert_eq!(state["digests"]["2.0"], digest.as_str());
    let refused = readers
        .check(readers.reader_of("desired-single"), &state, "desired.json")
        .expect_err("the single state's reader is strict");
    assert!(refused.contains("digests is a key"), "{refused}");

    let map = format!(
        r#"{{"service":"tierstore","version":"2.0","min_up":{{}},"release_digest":"{digest}"}}"#
    );
    let (code, body) = rig.desired(&map).await;
    assert_eq!(code, 200, "{body}");
    let state = parse(&rig.dir.path().join("desired.json"));
    let refused = readers
        .check(readers.reader_of("desired-map"), &state, "desired.json")
        .expect_err("the map's reader is strict");
    assert!(refused.contains("digests is a key"), "{refused}");

    // A version set over a release index writes the record of the index beside the registry,
    // which the map's reader refuses the same way: by the key it does not know.
    let index = format!(
        r#"{{"packages":[{{"bytes":1,"file":"tierstore-3.0-linux-x86_64.tar.zst","platform":"linux-x86_64","sha256":"{digest}"}}],"schema":1,"service":"tierstore","version":"3.0"}}"#
    );
    let mut index_digest = String::new();
    for b in Sha256::digest(index.as_bytes()).iter() {
        write!(index_digest, "{b:02x}").expect("a string takes text");
    }
    let with_index = json!({
        "service": "tierstore", "version": "3.0", "min_up": {},
        "release_index": index, "release_index_digest": index_digest,
    });
    let (code, body) = rig.desired(&with_index.to_string()).await;
    assert_eq!(code, 200, "{body}");
    let state = parse(&rig.dir.path().join("desired.json"));
    let kept = &state["services"]["tierstore"];
    assert_eq!(kept["release_index"]["digest"], index_digest.as_str());
    // The text of the index is not read: its digest is kept, and no package.
    assert_eq!(kept["release_index"]["packages"], json!({}));
    assert_eq!(kept["digests"]["3.0"]["digest"], index_digest.as_str());
    let mut entry = kept.clone();
    entry.as_object_mut().expect("a state").remove("digests");
    let only = json!({"services": {"tierstore": entry}});
    let refused = readers
        .check(readers.reader_of("desired-map"), &only, "desired.json")
        .expect_err("the map's reader is strict");
    assert!(refused.contains("release_index is a key"), "{refused}");

    let mut halts: Value =
        serde_json::from_slice(&recorded_bytes("ops.json.halts")).expect("kbf's halts file");
    halts["services"]["kbf"]["halt"]["origin"] = json!("updater");
    readers
        .check(readers.reader_of("halts"), &halts, "halts")
        .expect("the halts file's reader is lenient");
}
