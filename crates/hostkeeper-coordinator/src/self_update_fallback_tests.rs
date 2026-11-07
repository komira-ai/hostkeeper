//! The coordinator's half of the design's test of a self-update that fell back
//! (`a_fallback_marks_the_version_bad_and_the_coordinator_stops_ordering_it`; the host's half is in
//! `hostkeeper-daemon`): a host whose new hostkeeper binary could not start reports the update of
//! the service `hostkeeper` `Failed` after the commit, of class release, with the reason
//! `self_update_fallback`. The rule that stops the coordinator ordering that version to that host
//! again is the failed-here rule of `decide()` (rule 4, `Skip::FailedHere`); no code of the
//! coordinator changes for it, and this pins it for this service and this verdict.

use hostkeeper_wire::RoleName;
use hostkeeper_wire::wire::{
    Class, Heartbeat, Outcome, Phase, ServiceBeat, ServiceReply, Token, UpdateResult,
};
use serde_json::json;

use crate::identity::Identity;
use crate::services::Services;
use crate::testutil::{OPERATOR, Rig, beat, id, role, version, with_kbf};

/// hostkeeper as a service of the coordinator: an entry of a rollout file after kbf's, rolled to
/// the hosts of the storage role in one stage, one at a time (the desired state's `max_updating`
/// of the storage role is 1, [`TWO`]), rolled back by itself only after two failed hosts.
const HOSTKEEPER_ENTRY: &str = r#"
[[service]]
name = "hostkeeper"
roles = ["storage"]
stages = [100]
bake_beats = 1
rollback = "automatic"
rollback_after_failed = { percent = 25, min_hosts = 2 }
install_when_absent = false

[service.selector]
roles = ["storage"]
"#;

const HOSTS: [&str; 6] = ["s1", "s2", "s3", "s4", "s5", "s6"];
const TWO: &str = r#"{"service":"hostkeeper","version":"2.0","max_updating":{"storage":1}}"#;

/// A rig whose coordinator runs kbf's and hostkeeper's definitions, and whose CI token may set
/// both.
fn rig(test: &str) -> Rig {
    let mut rig = Rig::new(test);
    let list = format!(
        "{}services = [\"kbf\", \"hostkeeper\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    let services: Services = with_kbf(HOSTKEEPER_ENTRY);
    rig.coordinator =
        Rig::start_with_allow_list(&rig.dir, &rig.clock, &list).with_services(services);
    rig
}

fn who(host: &str, role: RoleName) -> Identity {
    Identity {
        host_id: id(host),
        role,
    }
}

/// The heartbeat of a host whose only service is hostkeeper at `1.0`, with `last` as its last
/// update.
fn beat_of(last: Option<UpdateResult>) -> Heartbeat {
    let mut b = beat("1.0");
    b.agent_features = vec!["heartbeat-v2".to_owned()];
    let mut service: ServiceBeat = serde_json::from_value(json!({
        "name": "hostkeeper", "version": "1.0", "health": "healthy"
    }))
    .expect("a service beat");
    service.last_update = last;
    b.services = Some(vec![service]);
    b
}

/// What the daemon of a host reports once the fallback put its previous binary back.
fn fell_back(target: &str) -> UpdateResult {
    UpdateResult {
        target: version(target),
        outcome: Outcome::Failed,
        phase: Some(Phase::AfterCommit),
        class: Some(Class::Release),
        reason: Token::parse("self_update_fallback"),
        order_id: None,
        order_try: None,
    }
}

/// One round at `at`: every host beats, in order, `s1` with `s1_last` as its last update. The
/// answer is the hosts that were told to update hostkeeper to `2.0`.
fn round(rig: &Rig, at: u64, s1_last: Option<&UpdateResult>) -> Vec<String> {
    rig.at(at);
    let mut ordered = Vec::new();
    for host in HOSTS {
        let last = match host {
            "s1" => s1_last.cloned(),
            _ => None,
        };
        let reply = rig
            .coordinator
            .heartbeat(who(host, role("storage")), beat_of(last))
            .expect("answered")
            .reply;
        if let ServiceReply::Update { service, version } = reply {
            assert_eq!(service.as_deref(), Some("hostkeeper"));
            assert_eq!(version, crate::testutil::version("2.0"));
            ordered.push(host.to_owned());
        }
    }
    ordered
}

/// Catches a host whose self-update fell back being ordered the same version again, or the
/// failure of one host holding the version from the others: `s1` is told to update hostkeeper to
/// `2.0`, reports `Failed` after the commit, of class release, `self_update_fallback`, and is not
/// told to update to `2.0` again in any later round, while `2.0` stays desired and another host
/// is told to update to it.
#[tokio::test]
async fn a_fallback_marks_the_version_bad_and_the_coordinator_stops_ordering_it() {
    let rig = rig("hk-self-update-fallback");
    rig.set(TWO).await;
    let first = round(&rig, 2_000, None);
    assert_eq!(first, ["s1"], "one host at a time, the first in order");

    let failed = fell_back("2.0");
    let mut later = Vec::new();
    for at in [2_015, 2_030, 2_045, 2_060] {
        later.extend(round(&rig, at, Some(&failed)));
    }
    assert!(
        !later.iter().any(|host| host == "s1"),
        "s1 was ordered 2.0 again: {later:?}"
    );
    assert!(
        later.iter().any(|host| host == "s2"),
        "no other host was ordered 2.0: {later:?}"
    );
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    assert_eq!(status.services["hostkeeper"].version, version("2.0"));
}
