//! One rollout live at a time across services, through a restart of a coordinator that has
//! enrollment: what an enrolled host reported of its update is kept with its enrollment, and a
//! restarted coordinator reads it back as the slot the host holds, so a host that was updating
//! and stays silent through the quiet start still holds the slot against another service's
//! update. Every time is explicit; nothing sleeps.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{HostRunning, RunningService};
use hostkeeper_enrollment::names::HostId;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, ServiceBeat, ServiceReply};
use serde_json::{Value, json};

use crate::decide::{Gate, Why};
use crate::enrollment::tests::support::{HostKey, NOW, Rig as EnrollmentRig, enroll_request};
use crate::marks_support::operator;
use crate::membership_tests::{V2, who};
use crate::services::Services;
use crate::testutil::{OPERATOR, Rig, beat, kbf_def, version};

/// `other`, which bakes for two beats, and `second`, both on daemons that announce them.
const PLAIN: &str = "[[service]]\nname = \"other\"\nbake_beats = 2\nroles = [\"daemon\"]\n\
    [[service]]\nname = \"second\"\nroles = [\"daemon\"]\n";

/// A coordinator on the files of `rig` over `enrollment`, started now.
fn start(rig: &Rig, enrollment: Arc<crate::enrollment::Enrollment>) -> crate::service::Coordinator {
    let list = format!(
        "{}services = [\"kbf\", \"other\", \"second\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    Rig::start_with_allow_list(&rig.dir, &rig.clock, &list)
        .with_services(Services::parse(PLAIN).expect("services").with(kbf_def()))
        .with_enrollment(enrollment)
}

/// A coordinator past its quiet start over `PLAIN`, `other` and `second` desired at 3.0, over
/// an enrollment that has approved the hosts `d1` and `d2` as daemons.
async fn rig(name: &str) -> (Rig, EnrollmentRig) {
    let enrollment = EnrollmentRig::new(&format!("{name}-enrollment"));
    let mut rig = Rig::new(name);
    rig.coordinator = start(&rig, Arc::clone(&enrollment.enrollment));
    for service in ["other", "second"] {
        rig.set(&format!(
            r#"{{"service":"{service}","version":"3.0","min_up":{{"storage":0,"frontend":0}}}}"#
        ))
        .await;
    }
    rig.at(2_000);
    for (machine, host) in [("m-1", "d1"), ("m-2", "d2")] {
        let key = HostKey::new();
        enrollment
            .enrollment
            .enroll(enroll_request(&key, machine))
            .expect("posted");
        enrollment.clock.set(NOW + 1_000);
        rig.coordinator
            .enrollment_approve(
                &operator(),
                proto::ApproveRequest {
                    request_id: key.key_hash().to_hex(),
                    key_sha256: key.key_hash().to_hex(),
                    host_id: host.to_owned(),
                    role: "daemon".to_owned(),
                    reason: "racked".to_owned(),
                    ..proto::ApproveRequest::default()
                },
            )
            .expect("approved");
    }
    (rig, enrollment)
}

/// A restart: a coordinator on the same state files and the same enrollment files, started at
/// `at`.
fn restart(rig: &mut Rig, enrollment: &EnrollmentRig, at: u64) {
    rig.at(at);
    rig.coordinator = start(rig, enrollment.restart());
}

/// One service entry of a heartbeat: `name` at `running`, healthy, with the fields of `extra`.
fn entry(name: &str, running: &str, extra: &Value) -> Value {
    let mut one = json!({"name": name, "version": running, "health": "healthy"});
    for (key, value) in extra.as_object().expect("an object") {
        one[key] = value.clone();
    }
    one
}

fn beat_of(entries: &[Value]) -> Heartbeat {
    let mut b = beat("1.0");
    b.agent_features = vec![V2.to_owned()];
    b.services = Some(
        serde_json::from_value::<Vec<ServiceBeat>>(Value::Array(entries.to_vec())).expect("beats"),
    );
    b
}

/// A heartbeat of the enrolled `host` at `at`, by both clocks.
fn send(
    rig: &Rig,
    enrollment: &EnrollmentRig,
    at: u64,
    host: &str,
    entries: &[Value],
) -> ServiceReply {
    rig.at(at);
    enrollment.clock.set(NOW + at);
    rig.coordinator
        .heartbeat_native(who(host), beat_of(entries))
        .expect("a heartbeat")
        .reply
}

fn order(service: &str) -> ServiceReply {
    ServiceReply::Update {
        service: Some(service.to_owned()),
        version: version("3.0"),
    }
}

fn other_at(running: &str) -> Value {
    entry("other", running, &json!({}))
}

fn other_updating() -> Value {
    entry("other", "1.0", &json!({"updating": "3.0"}))
}

fn second_behind() -> Value {
    entry("second", "1.0", &json!({}))
}

/// The hold of `host` under `service` in the status.
fn hold(rig: &Rig, service: &str, host: &str) -> Option<Why> {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    status.per_service[service]
        .hosts
        .iter()
        .find(|h| h.host.node_id == host)
        .and_then(|h| h.hold)
}

/// What the enrollment kept of the services `host` runs: each one's name and the version it was
/// updating to.
fn kept(enrollment: &EnrollmentRig, host: &str) -> Vec<(String, Option<String>)> {
    let id = HostId::parse(host).expect("a host id");
    let running: Option<HostRunning> = enrollment
        .enrollment
        .store()
        .enrolled_host(&id)
        .expect("read")
        .and_then(|enrolled| enrolled.running);
    running
        .map(|running| running.services)
        .unwrap_or_default()
        .into_iter()
        .map(
            |RunningService {
                 service, updating, ..
             }| (service, updating),
        )
        .collect()
}

fn abandon(rig: &Rig, host: &str) {
    rig.coordinator
        .exclude(
            &operator(),
            proto::ExcludeRequest {
                service: "other".to_owned(),
                node_id: host.to_owned(),
                reason: "host is gone".to_owned(),
                abandon: true,
            },
        )
        .expect("abandoned");
}

/// Proves: `d1` reported `other` updating to 3.0 and the coordinator restarts; `d1` says
/// nothing through the quiet start and after it, and `d2`'s `second` is told nothing, held by
/// `rollout_elsewhere`, at 50 s and at 65 s after `d1`'s last beat. Once `d1` reports `other` done
/// at 3.0 and its bake of two beats is done, `d2` is ordered `second`. Catches a restarted
/// coordinator that knows no host until it beats, so a host updating before the restart and
/// silent through the quiet start holds no slot and a second update is ordered in the farm.
#[tokio::test]
async fn a_host_updating_before_a_restart_and_silent_through_the_quiet_start_still_holds_the_slot()
{
    let (mut rig, enrollment) = rig("one-live-restart-enrolled-silent").await;
    assert_eq!(
        send(&rig, &enrollment, 2_000, "d1", &[other_at("1.0")]),
        order("other")
    );
    send(&rig, &enrollment, 2_010, "d1", &[other_updating()]);
    assert_eq!(
        kept(&enrollment, "d1"),
        [("other".to_owned(), Some("3.0".to_owned()))]
    );
    restart(&mut rig, &enrollment, 2_030);
    assert_eq!(
        send(&rig, &enrollment, 2_060, "d2", &[second_behind()]),
        ServiceReply::Nothing,
        "50 s after d1's last word"
    );
    assert_eq!(
        hold(&rig, "second", "d2"),
        Some(Why::Gate(Gate::RolloutElsewhere))
    );
    assert_eq!(
        send(&rig, &enrollment, 2_075, "d2", &[second_behind()]),
        ServiceReply::Nothing,
        "65 s after"
    );
    send(&rig, &enrollment, 2_110, "d1", &[other_at("3.0")]);
    assert_eq!(
        send(&rig, &enrollment, 2_115, "d2", &[second_behind()]),
        ServiceReply::Nothing,
        "d1 is baking"
    );
    send(&rig, &enrollment, 2_125, "d1", &[other_at("3.0")]);
    assert_eq!(
        send(&rig, &enrollment, 2_130, "d2", &[second_behind()]),
        order("second")
    );
}

/// Proves: `d1` updated `other` and an operator excluded it with `abandon` before the
/// restart; after the restart `d2` is ordered `second` past the quiet start, though `d1` never
/// beats again, and while `d1` still updated it held `d2` back. Catches an abandoned host whose
/// kept record still says it is updating, so a restart holds a slot of a host that is gone.
#[tokio::test]
async fn an_abandoned_host_holds_no_slot_after_a_restart() {
    let (mut rig, enrollment) = rig("one-live-restart-enrolled-abandoned").await;
    assert_eq!(
        send(&rig, &enrollment, 2_000, "d1", &[other_at("1.0")]),
        order("other")
    );
    send(&rig, &enrollment, 2_010, "d1", &[other_updating()]);
    assert_eq!(
        send(&rig, &enrollment, 2_012, "d2", &[second_behind()]),
        ServiceReply::Nothing,
        "held while d1 updates"
    );
    abandon(&rig, "d1");
    assert_eq!(
        kept(&enrollment, "d1"),
        [("other".to_owned(), None)],
        "the record forgets the update"
    );
    restart(&mut rig, &enrollment, 2_030);
    assert_eq!(
        send(&rig, &enrollment, 2_060, "d2", &[second_behind()]),
        order("second")
    );
}

/// Proves: `d1` reported `other` updating and then done at 3.0 before the restart; after it,
/// `d1` silent, `d2` is ordered `second` past the quiet start. Catches the kept record keeping
/// `updating` after the host stops updating, so a restart holds a slot of a finished update.
#[tokio::test]
async fn a_host_that_finished_its_update_holds_nothing_after_a_restart() {
    let (mut rig, enrollment) = rig("one-live-restart-enrolled-finished").await;
    assert_eq!(
        send(&rig, &enrollment, 2_000, "d1", &[other_at("1.0")]),
        order("other")
    );
    send(&rig, &enrollment, 2_010, "d1", &[other_updating()]);
    send(&rig, &enrollment, 2_015, "d1", &[other_at("3.0")]);
    assert_eq!(
        kept(&enrollment, "d1"),
        [("other".to_owned(), None)],
        "the record says it is done"
    );
    restart(&mut rig, &enrollment, 2_030);
    assert_eq!(
        send(&rig, &enrollment, 2_060, "d2", &[second_behind()]),
        order("second")
    );
}
