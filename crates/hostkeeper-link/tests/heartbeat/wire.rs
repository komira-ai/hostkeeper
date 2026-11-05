//! The heartbeat as the JSON route's wire carries it: the limits both sides keep, the bytes
//! kbf's agent sent for the same facts, every word, the largest heartbeat, and the facts the
//! wire has no word for. Each heartbeat goes through the real link and its TLS, and the body is
//! the one the in-process server read.

use std::path::Path;
use std::sync::Arc;

use hostkeeper_link::testing::{RecordingLog, verdict};
use hostkeeper_link::{
    ANNOUNCED, CoordinatorLink, DeferredReport, Health, Heartbeat, LinkError, Liveness,
    MAX_HEARTBEAT_BYTES, Observed, OrderResult, Outcome, Platform, Reason, Reply, ServiceFacts,
    Unsendable,
};
use hostkeeper_wire::wire;
use serde_json::{Value, json};

use crate::tls::{Close, Farm, PipeTransport, Pki, Request, http, link, serve};
use crate::{service, update, version};

/// The recorded fixtures of the coordinator import: kbf's coordinator at `v0.0.64`.
const SEQUENCES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../hostkeeper-coordinator/tests/fixtures/recorded/sequences"
);

/// The body of the step `step` of the recorded sequence `file`.
fn recorded_body(file: &str, step: usize) -> String {
    let text = std::fs::read_to_string(Path::new(SEQUENCES).join(file)).expect("the fixture");
    let sequence: Value = serde_json::from_str(&text).expect("JSON");
    sequence["steps"][step]["body"]
        .as_str()
        .expect("a body")
        .to_owned()
}

/// A service at `at`, healthy, with nothing else to say but what the observer saw.
fn facts(name: &str, at: &str) -> ServiceFacts {
    ServiceFacts {
        name: service(name),
        version: version(at),
        health: Health::Healthy,
        updating: None,
        last_update: None,
        deferred: None,
        health_detail: None,
        observed: Some(Observed {
            liveness: Liveness::Running,
            version: Some(version(at)),
        }),
    }
}

fn beat(platform: Platform, agent: &str, services: Vec<ServiceFacts>) -> Heartbeat {
    Heartbeat {
        platform,
        agent_version: version(agent),
        features: ANNOUNCED.to_vec(),
        services,
    }
}

/// A verdict of `outcome` for the update of `name` to `to`.
fn result(name: &str, to: &str, outcome: Outcome) -> OrderResult {
    verdict(&update("o-1", 1, name, to), outcome)
}

fn reason(text: &str) -> Reason {
    Reason::new(text).expect("a reason")
}

/// What the coordinator read when the link sent `beat`, which it answered with nothing.
fn sent(name: &str, beat: &Heartbeat) -> Request {
    let farm = Farm::new();
    let pki = Pki::with_bundle(name, &farm.ca.pem);
    let transport = Arc::new(PipeTransport::default());
    let log = Arc::new(RecordingLog::default());
    let link = link(&farm, &pki, &transport, &log);
    let server = serve(&transport, farm.server(), Close::Notify, |_| {
        http(200, r#"{"reply":"nothing"}"#)
    });
    let reply = link.heartbeat(&farm.host.identity(), beat);
    assert_eq!(reply, Ok(Reply::default()));
    server.join().expect("the server").expect("a request")
}

/// Catches a limit changed on one side alone: the wire's service limit is the protobuf's, which
/// is the loop's, and the wire's byte bound is the loop's.
#[test]
fn the_wire_service_limit_equals_the_proto_limit() {
    assert_eq!(
        wire::MAX_SERVICES,
        hostkeeper_proto::MAX_SERVICES_PER_HEARTBEAT
    );
    assert_eq!(
        hostkeeper_link::beat::MAX_SERVICES_PER_HEARTBEAT,
        wire::MAX_SERVICES
    );
    assert_eq!(wire::MAX_HEARTBEAT_BYTES, MAX_HEARTBEAT_BYTES);
}

/// Catches a drift of the wire shape: for the facts of the recorded heartbeat of kbf's agent
/// that lists a service (sequence 11, host `d2`), the link sends the recorded bytes, with only
/// the additive `agent_features` and `agent` after them; and the recorded heartbeat of a service
/// with a halted verdict (sequence 10, host `d3`) reads as the same wire heartbeat as the link's
/// without those two. Also the request itself: one POST to the route, its length, and
/// `Connection: close`.
#[test]
fn heartbeat_bytes_equal_the_recorded_kbf_shape_for_the_same_facts() {
    let recorded = recorded_body(
        "11-service-order-only-to-an-updater-that-sent-services.json",
        3,
    );
    let agent = "0.0.19+g123456abcdef-1";
    let request = sent(
        "wire-kbf-bytes",
        &beat(Platform::LinuxX86_64, agent, vec![facts("other", "1.0")]),
    );
    let mut expected = recorded.trim_end_matches('}').to_owned();
    expected.push_str(&format!(
        r#","agent_features":["deferred-v1","halt-v1","heartbeat-v2","forced-v1"],"agent":{{"name":"hostkeeper","version":"{agent}"}}}}"#
    ));
    assert!(recorded.ends_with("}]}"), "{recorded}");
    assert_eq!(
        String::from_utf8(request.body.clone()).expect("text"),
        expected
    );
    assert_eq!(
        request.head,
        format!(
            "POST /v1/heartbeat HTTP/1.1\r\nHost: coordinator.test:8443\r\n\
             Accept: application/json\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close",
            expected.len()
        )
    );

    let recorded = recorded_body("10-halt-generations-of-two-services.json", 8);
    let mut halted = result("other", "3.0", Outcome::Failed);
    halted.phase = Some(hostkeeper_hook::Phase::Halt);
    halted.reason = Some(reason("disk_tainted"));
    halted.detail = Some("not on the wire".to_owned());
    let mut other = facts("other", "1.0");
    other.last_update = Some(halted);
    let request = sent(
        "wire-kbf-halted",
        &beat(Platform::LinuxX86_64, "2.0", vec![other]),
    );
    let mut ours: wire::Heartbeat = serde_json::from_slice(&request.body).expect("a heartbeat");
    assert_eq!(
        ours.agent_features,
        ["deferred-v1", "halt-v1", "heartbeat-v2", "forced-v1"]
    );
    assert_eq!(ours.agent_name(), "hostkeeper");
    ours.agent_features.clear();
    ours.agent = None;
    let theirs: wire::Heartbeat = serde_json::from_str(&recorded).expect("a heartbeat");
    assert_eq!(ours, theirs);
}

/// Catches a word of the loop mapped to another word of the wire, a fact the wire has no field
/// for sent anyway, and a deferred verdict sent as an outcome: every platform, health, phase,
/// class and outcome, a running update, a deferral, and the facts that stay home (the observer's
/// detail, the health detail, the order id and try, the forced flag, the verdict's detail).
#[test]
fn every_word_of_a_heartbeat_is_the_wires() {
    use hostkeeper_hook::{Class, Phase};

    let mut services = Vec::new();
    let mut succeeded = facts("s-a", "1.0");
    succeeded.updating = Some(version("2.0"));
    succeeded.health_detail = Some("stays home".to_owned());
    let mut done = result("s-a", "1.0", Outcome::Succeeded);
    done.forced = true;
    done.detail = Some("stays home".to_owned());
    succeeded.last_update = Some(done);
    services.push(succeeded);
    let failures = [
        (
            "s-b",
            Health::Unhealthy,
            Phase::BeforeCommit,
            Some(Class::Release),
        ),
        (
            "s-c",
            Health::Absent,
            Phase::GateRelease,
            Some(Class::Environment),
        ),
        ("s-d", Health::Healthy, Phase::GateEnvironment, None),
        ("s-e", Health::Healthy, Phase::AfterCommit, None),
        ("s-f", Health::Healthy, Phase::Halt, None),
    ];
    for (name, health, phase, class) in failures {
        let mut failed = result(name, "2.0", Outcome::Failed);
        failed.phase = Some(phase);
        failed.class = class;
        failed.reason = Some(reason("probe_failed"));
        let mut entry = facts(name, "1.0");
        entry.health = health;
        entry.last_update = Some(failed);
        services.push(entry);
    }
    let mut deferred = facts("s-g", "1.0");
    let mut later = result(
        "s-g",
        "2.0",
        Outcome::Deferred {
            retry_after_secs: 60,
        },
    );
    later.reason = Some(reason("drain_bound"));
    deferred.last_update = Some(later);
    deferred.deferred = Some(DeferredReport {
        target: version("2.0"),
        reason: reason("drain_bound"),
        retry_after_secs: 60,
    });
    services.push(deferred);

    let request = sent(
        "wire-words",
        &beat(Platform::DarwinArm64, "0.9.0", services),
    );
    let body: Value = serde_json::from_slice(&request.body).expect("JSON");
    let failed = |name: &str, health: &str, phase: &str, class: Option<&str>| {
        let mut last = json!({
            "target": "2.0", "outcome": "failed", "phase": phase, "reason": "probe_failed",
        });
        if let Some(class) = class {
            last["class"] = json!(class);
        }
        json!({
            "name": name, "version": "1.0", "health": health, "last_update": last,
            "updating": null,
        })
    };
    assert_eq!(
        body,
        json!({
            "platform": "darwin-arm64",
            "version": "0.9.0",
            "health": "healthy",
            "last_update": null,
            "updating": null,
            "services": [
                {
                    "name": "s-a", "version": "1.0", "health": "healthy",
                    "last_update": { "target": "1.0", "outcome": "succeeded" },
                    "updating": "2.0",
                },
                failed("s-b", "unhealthy", "before_commit", Some("release")),
                failed("s-c", "absent", "gate_release", Some("environment")),
                failed("s-d", "healthy", "gate_environment", None),
                failed("s-e", "healthy", "after_commit", None),
                failed("s-f", "healthy", "halt", None),
                {
                    "name": "s-g", "version": "1.0", "health": "healthy",
                    "last_update": null, "updating": null,
                    "deferred": { "target": "2.0", "reason": "drain_bound", "retry_after_secs": 60 },
                },
            ],
            "agent_features": ["deferred-v1", "halt-v1", "heartbeat-v2", "forced-v1"],
            "agent": { "name": "hostkeeper", "version": "0.9.0" },
        })
    );

    for (platform, word) in [
        (Platform::LinuxX86_64, "linux-x86_64"),
        (Platform::LinuxArm64, "linux-arm64"),
    ] {
        let request = sent(
            &format!("wire-{word}"),
            &beat(platform, "0.9.0", Vec::new()),
        );
        let body: Value = serde_json::from_slice(&request.body).expect("JSON");
        assert_eq!(body["platform"], word);
        assert_eq!(body["services"], json!([]), "sent even when empty");
    }
}

/// Catches a bound the link could cross: the largest heartbeat the wire's grammars allow (16
/// services, every name, version and reason at its longest, every optional fact there, the
/// longest words) is under the 16 KiB the coordinator reads, and the coordinator's reader keeps
/// every service of it.
#[test]
fn the_largest_heartbeat_the_json_route_carries_is_under_its_bound() {
    let longest = "1".repeat(128);
    let token = "r".repeat(64);
    let services: Vec<ServiceFacts> = (0..wire::MAX_SERVICES)
        .map(|n| {
            let name = format!("{n:02}{}", "a".repeat(62));
            let mut done = result(&name, &longest, Outcome::Succeeded);
            done.phase = Some(hostkeeper_hook::Phase::GateEnvironment);
            done.class = Some(hostkeeper_hook::Class::Environment);
            done.reason = Some(reason(&token));
            ServiceFacts {
                name: service(&name),
                version: version(&longest),
                health: Health::Unhealthy,
                updating: Some(version(&longest)),
                last_update: Some(done),
                deferred: Some(DeferredReport {
                    target: version(&longest),
                    reason: reason(&token),
                    retry_after_secs: u64::MAX,
                }),
                health_detail: None,
                observed: None,
            }
        })
        .collect();
    let request = sent(
        "wire-largest",
        &beat(Platform::LinuxX86_64, &longest, services),
    );
    assert!(
        request.body.len() <= wire::MAX_HEARTBEAT_BYTES,
        "{}",
        request.body.len()
    );
    let read = wire::read_heartbeat(&request.body, None).expect("the coordinator reads it");
    assert_eq!(read.services_dropped, None);
    assert_eq!(read.extras_dropped, None);
    assert_eq!(read.beat.services.map(|list| list.len()), Some(16));
}

/// Catches a heartbeat sent without a fact the wire cannot carry, or a refusal that does not
/// name it: a service name or a version outside the wire's grammars, wherever it is (the agent,
/// a service, its update, its verdict, its deferral), is refused by name and nothing is dialed.
#[test]
fn a_heartbeat_the_json_route_cannot_carry_is_refused_by_name() {
    let farm = Farm::new();
    let pki = Pki::with_bundle("wire-unsendable", &farm.ca.pem);
    let transport = Arc::new(PipeTransport::default());
    let log = Arc::new(RecordingLog::default());
    let link = link(&farm, &pki, &transport, &log);
    let refused = |beat: Heartbeat| link.heartbeat(&farm.host.identity(), &beat);

    let named = Unsendable::ServiceName("Web_1".to_owned());
    assert_eq!(
        refused(beat(
            Platform::LinuxX86_64,
            "1.0",
            vec![facts("Web_1", "1.0")]
        )),
        Err(LinkError::Unsendable(named.clone()))
    );
    assert_eq!(
        LinkError::Unsendable(named).to_string(),
        "the heartbeat is not sent: the service name \"Web_1\" is not one the JSON heartbeat \
         carries: 1 to 64 bytes of a-z, 0-9 and -"
    );
    let versioned = Unsendable::Version("v2".to_owned());
    assert_eq!(
        LinkError::Unsendable(versioned.clone()).to_string(),
        "the heartbeat is not sent: the version \"v2\" is not one the JSON heartbeat carries: a \
         digit, then letters, digits and . + ~ -, at most 128 bytes"
    );

    let mut running = facts("web", "1.0");
    running.updating = Some(version("v2"));
    let mut verdict_of = facts("web", "1.0");
    verdict_of.last_update = Some(result("web", "v2", Outcome::Failed));
    let mut deferral = facts("web", "1.0");
    deferral.deferred = Some(DeferredReport {
        target: version("v2"),
        reason: reason("drain_bound"),
        retry_after_secs: 1,
    });
    for bad in [
        beat(Platform::LinuxX86_64, "v2", Vec::new()),
        beat(Platform::LinuxX86_64, "1.0", vec![facts("web", "v2")]),
        beat(Platform::LinuxX86_64, "1.0", vec![running]),
        beat(Platform::LinuxX86_64, "1.0", vec![verdict_of]),
        beat(Platform::LinuxX86_64, "1.0", vec![deferral]),
    ] {
        assert_eq!(refused(bad), Err(LinkError::Unsendable(versioned.clone())));
    }
    assert_eq!(transport.dialed(), Vec::<(String, u16)>::new());
    assert_eq!(log.lines(), Vec::<String>::new());
}
