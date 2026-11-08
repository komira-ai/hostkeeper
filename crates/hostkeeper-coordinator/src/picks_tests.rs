//! The pick's own rules ([`crate::picks`]) where the wire does not reach them: the bounds of the
//! installers a heartbeat over gRPC may list, a list from a daemon without `target-v1`, the
//! release a force of a version set with targets records, how two picked entries compare, and
//! the proof that `target-v1` is acted on. The rest of the pick is proved over the wire in
//! `tests/target_orders.rs`.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_wire::Platform;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{ServiceBeat, ServiceReply};

use crate::marks_support::{
    INSTALLER, WEB, ask, ask_typed, beat_of, force_request, operator, rig, runs, update, who,
};
use crate::orders::{Order, PickedTarget};
use crate::picks::{Heard, MAX_INSTALLERS, TARGET_FEATURE, recorded};
use crate::registry::{IndexKey, IndexRecord, Release as Registered, index_platform};
use crate::release_index_support::{AGENT, beat_on, sha};
use crate::releases::{Release, Targets};
use crate::releases_tests::{digest, entry, hex, list};
use crate::service::ci_calls::Setter;
use crate::testutil::{Rig, ci_token, id};
use crate::write_record::WriteAsk;

const ARCHIVE: &str = "hostkeeper-archive";

/// A heartbeat over gRPC of a daemon announcing `target-v1` that lists `installers` and runs
/// `web` from `installer`.
fn said(installers: &[&str], installer: Option<&str>) -> agent::HeartbeatRequest {
    agent::HeartbeatRequest {
        features: vec![TARGET_FEATURE.to_owned()],
        installers: installers.iter().map(|i| (*i).to_owned()).collect(),
        services: vec![agent::ServiceBeat {
            name: WEB.to_owned(),
            installer: installer.map(str::to_owned),
            ..agent::ServiceBeat::default()
        }],
        ..agent::HeartbeatRequest::default()
    }
}

/// Proves: a list of [`MAX_INSTALLERS`] distinct names of 32 bytes is read, with the installer of
/// each service that names one; one more installer, one listed twice, an empty name, one of 33
/// bytes, one with a capital or a dot, and a service's installer that breaks the same rule are
/// each refused whole.
/// Catches: a list over the bound or with a bad name kept, which the pick would match against.
#[test]
fn a_list_is_read_only_within_its_bounds() {
    let names: Vec<String> = (0..MAX_INSTALLERS)
        .map(|n| format!("{n:02}{}", "a".repeat(30)))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(Heard::of(&said(&refs, Some(refs[0]))).is_some());
    let mut over = refs.clone();
    over.push("tarball");
    let long = "a".repeat(33);
    let refused: [(Vec<&str>, Option<&str>); 6] = [
        (over, None),
        (vec!["deb", "deb"], None),
        (vec![""], None),
        (vec![&long], None),
        (vec!["Deb", "rpm.x"], None),
        (vec!["deb"], Some("Tarball")),
    ];
    for (installers, installer) in refused {
        assert_eq!(
            Heard::of(&said(&installers, installer)),
            None,
            "{installers:?} {installer:?}"
        );
    }
}

/// Proves: the coordinator keeps the list a `target-v1` daemon said over gRPC, and the installer
/// it named for `web`; a later list that breaks a rule leaves the one read before, counted as a
/// dropped fact; a heartbeat from a daemon without `target-v1` is not read at all; and a host
/// released as it retires is forgotten.
/// Catches: a bad list that wipes the good one, or a list read from a daemon that does not take
/// targets.
#[test]
fn only_a_good_list_of_a_target_v1_daemon_is_kept() {
    let rig = Rig::new("picks-hear");
    let d1 = who("d1", "daemon");
    rig.coordinator
        .hear_installers(&d1, &said(&[ARCHIVE, "deb"], Some("deb")));
    rig.coordinator
        .hear_installers(&d1, &said(&["deb", "deb"], None));
    let mut old = said(&["tarball"], None);
    old.features.clear();
    rig.coordinator.hear_installers(&d1, &old);
    let d2 = who("d2", "daemon");
    rig.coordinator.hear_installers(&d2, &old);
    let mut fleet = rig.coordinator.lock_fleet();
    assert_eq!(
        fleet.picks.listed(&id("d1")),
        Some(&[ARCHIVE.to_owned(), "deb".to_owned()][..])
    );
    assert_eq!(fleet.picks.running(&id("d1"), WEB), Some("deb"));
    assert_eq!(fleet.picks.listed(&id("d2")), None);
    // A host released when it retires leaves no list behind.
    assert!(fleet.release_if_final(&id("d1"), std::time::Duration::from_secs(2_000)));
    assert_eq!(fleet.picks.listed(&id("d1")), None);
}

/// Targets whose `linux-x86_64` list holds a `deb` and an archive entry, and whose `macos-arm64`
/// list holds no archive.
fn mixed() -> Targets {
    Targets::from([
        (
            "linux-x86_64".to_owned(),
            list(vec![
                entry(WEB, "deb", 'a', 10),
                entry(WEB, ARCHIVE, 'f', 20),
            ]),
        ),
        (
            "macos-arm64".to_owned(),
            list(vec![entry(WEB, "tarball", 'd', 30)]),
        ),
    ])
}

/// Proves: the release a force of a version set with targets records names the digest of the
/// index set with them, else the SHA-256 of the release's stored form, and as the index's
/// packages the archive entry of each platform that has one, the others left out.
/// Catches: a record whose digest or packages would order an older coordinator's hosts bytes the
/// release does not hold.
#[test]
fn a_force_records_the_release_as_its_index_or_its_digest() {
    let packages = std::collections::BTreeMap::from([(
        IndexKey::of(index_platform(Platform::LinuxX86_64)),
        digest('f'),
    )]);
    let indexed = Release::new(mixed(), Some(digest('9')));
    assert_eq!(
        recorded(&indexed),
        Some(Registered::Index(IndexRecord {
            digest: digest('9'),
            packages: packages.clone(),
        }))
    );
    let plain = Release::new(mixed(), None);
    let named = crate::registry::Digest::parse(&sha(&plain.to_json().to_string())).ok();
    assert_eq!(
        recorded(&plain),
        named.map(|digest| Registered::Index(IndexRecord { digest, packages }))
    );
}

/// Proves: two picked entries are equal when they encode alike, and differ by any field.
/// Catches: an order compared equal to one of another entry.
#[test]
fn picked_entries_compare_by_their_encoding() {
    let deb = PickedTarget::new(entry(WEB, "deb", 'a', 10));
    assert_eq!(deb, PickedTarget::new(entry(WEB, "deb", 'a', 10)));
    assert_ne!(deb, PickedTarget::new(entry(WEB, "deb", 'b', 10)));
    assert_eq!(deb.target().digest, hex('a'));
}

/// Sets `web` at `version` with `targets`, as CI does.
async fn set_targets(rig: &Rig, version: &str, targets: Targets) {
    let verified = rig
        .coordinator
        .ci(Some(&ci_token()))
        .await
        .expect("a token");
    let body = format!(
        r#"{{"service":"web","version":"{version}","min_up":{{"storage":0,"frontend":0}}}}"#
    );
    rig.coordinator
        .set_desired_with(
            &Setter::Ci(&verified),
            body.as_bytes(),
            &WriteAsk::default(),
            Some(targets),
        )
        .await
        .expect("set");
}

/// Targets whose one list, `linux-x86_64`'s, holds a `deb` and then an archive.
fn mixed_x86() -> Targets {
    Targets::from([(
        "linux-x86_64".to_owned(),
        list(vec![
            entry(WEB, "deb", 'a', 10),
            entry(WEB, ARCHIVE, 'f', 20),
        ]),
    )])
}

/// The features of a daemon of `target-v1` that reads order terms and takes installs.
fn target_v1() -> Vec<String> {
    AGENT
        .iter()
        .chain(&[TARGET_FEATURE])
        .map(|f| (*f).to_owned())
        .collect()
}

/// What a host reports of `web`: it runs 1.0, and declined 2.0 for `why`.
fn declined(why: &str) -> ServiceBeat {
    serde_json::from_value(serde_json::json!({
        "name": WEB, "version": "1.0", "health": "healthy",
        "deferred": {"target": "2.0", "reason": why, "retry_after_secs": 0},
    }))
    .expect("a service beat")
}

/// Proves, through the coordinator's own calls: a daemon that reads no order terms and runs
/// `web` from archives is told the version alone, as today; a `target-v1` daemon picked its
/// `deb` is told nothing on the JSON route, and is ordered the `deb` on the typed one at once
/// after it declined it `installer_not_listed` (a deferral that opens no wait); a Mac, whose
/// platform has no list, is out of the plan; and `Releases.Force` orders the two hosts picked an
/// entry and skips the Mac as `no_target`.
/// Catches: a native entry sent where no typed order can carry it, an `installer_not_listed`
/// deferral that holds the host, a host picked nothing that takes a place in the plan, or a force
/// of a version set with targets that misses the pick.
#[tokio::test]
async fn the_gate_the_plan_and_a_force_follow_the_pick() {
    let rig = rig("picks-gate-plan-force", "stages = [100]");
    set_targets(&rig, "2.0", mixed_x86()).await;
    let mac = beat_on(Platform::DarwinArm64, vec![runs(WEB, "1.0")]);
    assert_eq!(ask(&rig, "m1", "daemon", mac), ServiceReply::Nothing);
    let plain = beat_of(INSTALLER, vec![runs(WEB, "1.0")]);
    assert_eq!(ask(&rig, "d1", "daemon", plain), update(WEB, "2.0"));

    rig.coordinator
        .hear_installers(&who("d2", "daemon"), &said(&[ARCHIVE, "deb"], Some("deb")));
    let mut json = beat_on(Platform::LinuxX86_64, vec![runs(WEB, "1.0")]);
    json.agent_features = target_v1();
    assert_eq!(ask(&rig, "d2", "daemon", json), ServiceReply::Nothing);
    let mut deferred = beat_on(
        Platform::LinuxX86_64,
        vec![declined("installer_not_listed")],
    );
    deferred.agent_features = target_v1();
    let (_, order) = ask_typed(&rig, "d2", "daemon", deferred);
    assert!(
        matches!(
            order,
            Some(Order::Update {
                target: Some(_),
                ..
            })
        ),
        "{order:?}"
    );

    let fleet = proto::force_request::Targets::Fleet(proto::FleetTarget {
        confirm_service: WEB.to_owned(),
    });
    let forced = rig
        .coordinator
        .force(&operator(), force_request(WEB, "2.0", fleet, false))
        .expect("forced");
    assert_eq!(forced.targeted, 2);
    let skipped: Vec<(&str, &str)> = forced
        .skipped
        .iter()
        .map(|s| (s.node_id.as_str(), s.reason.as_str()))
        .collect();
    assert_eq!(skipped, [("m1", "no_target")]);
}

/// Catches: `target-v1` advertised and not acted on: the same daemon, running `web` from its
/// `deb`, is ordered the release's archive entry as an old daemon (no target) without the
/// feature, and the `deb` entry with the target once it announces it and lists `deb` over gRPC.
pub(crate) async fn target_v1_is_acted_on() {
    let rig = rig("picks-target-v1", "stages = [100]");
    set_targets(&rig, "2.0", mixed_x86()).await;
    let target = |order: Option<Order>| match order {
        Some(Order::Update { digest, target, .. }) => (digest.as_str().to_owned(), target),
        other => panic!("not an update: {other:?}"),
    };
    let without = beat_on(Platform::LinuxX86_64, vec![runs(WEB, "1.0")]);
    let (_, order) = ask_typed(&rig, "d1", "daemon", without);
    assert_eq!(target(order), (hex('f'), None));

    let d2 = who("d2", "daemon");
    rig.coordinator
        .hear_installers(&d2, &said(&[ARCHIVE, "deb"], Some("deb")));
    let mut with = beat_on(Platform::LinuxX86_64, vec![runs(WEB, "1.0")]);
    with.agent_features = target_v1();
    let (_, order) = ask_typed(&rig, "d2", "daemon", with);
    assert_eq!(
        target(order),
        (
            hex('a'),
            Some(PickedTarget::new(entry(WEB, "deb", 'a', 10)))
        )
    );
}
