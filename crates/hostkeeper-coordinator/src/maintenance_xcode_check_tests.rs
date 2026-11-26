//! A run of `probe` that names an archive of the Xcode store to check
//! ([`crate::maintenance_service`], [`crate::decide_maintenance`]): what a `Schedule` of it
//! refuses and whom it holds, the SHA-256 the run shows in each answer, and what the order of each
//! probe turn carries, over an Xcode store in a directory of the test's own. The builders are in
//! `maintenance_xcode_tests.rs` and `maintenance_calls_tests.rs`.

use std::fs;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MaintenanceRefusal as Refusal;
use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::Heartbeat;

use super::tests::{Farm, MAC, ORDER, entry_of};
use crate::decide_maintenance::tests::{beat, table, tell};
use crate::host_groups_support::SELF;
use crate::maintenance::text_of;
use crate::maintenance_calls_tests::{
    FIRST, NONE, alpha, clean_mac, edit_runs, get, hosts_request, kept, list, member, schedule,
};
use crate::maintenance_runs::{Act, Kind, Step};
use crate::orders::{Order, XcodeOrder};
use crate::xcode_store::Archive;

/// A group of the tests that keeps no Xcode.
const BARE: &str = "bare";

/// A heartbeat of a Mac whose daemon lists `probe` on demand, with nothing due.
fn probe_beat() -> Heartbeat {
    let mut heartbeat = beat(&[], None);
    heartbeat.platform = Platform::DarwinArm64;
    let report = agent::MaintenanceReport {
        mac: Some(clean_mac()),
        on_demand: vec![MaintenanceKind::Probe as i32],
        ..agent::MaintenanceReport::default()
    };
    heartbeat.maintenance = Some(text_of(&report));
    heartbeat
}

/// A confirmed `Schedule` of `probe` alone for the hosts `ids`, naming the archive `sha256`
/// (empty: none), which may take a group's last host.
fn probe_of(ids: &[&str], sha256: &str) -> proto::ScheduleMaintenanceRequest {
    proto::ScheduleMaintenanceRequest {
        kinds: vec![MaintenanceKind::Probe as i32],
        xcode_sha256: sha256.to_owned(),
        take_last_host: true,
        ..hosts_request(ids)
    }
}

/// A farm of the group [`GROUP`] (keeping one Xcode) holding the Mac [`MAC`], heard to list `probe`
/// on demand, and the group [`BARE`] (keeping none) holding the Mac `b1`, heard the same.
fn farm(name: &str) -> Farm {
    let farm = Farm::new(name, 1, None, &[MAC]);
    let policy = proto::RolloutPolicy {
        maintenance: Some(table(1, 0)),
        ..proto::RolloutPolicy::default()
    };
    farm.world
        .put_with(BARE, &[SELF], Some(policy), None)
        .expect("a group that keeps no Xcode");
    member(&farm.world, BARE, "b1");
    tell(&farm.world, MAC, probe_beat());
    tell(&farm.world, "b1", probe_beat());
    farm
}

/// The `maintain` the probe of [`MAC`] is sent at its next heartbeat, with its wire form.
fn probe_order(farm: &Farm) -> (crate::orders::Maintain, agent::Order) {
    farm.maintain(probe_beat()).expect("an order")
}

/// The hosts of the first run the world stores, by id.
fn run_hosts(farm: &Farm) -> Vec<String> {
    farm.world.ops().maintenance_runs[0]
        .hosts
        .iter()
        .map(|host| host.host_id.clone())
        .collect()
}

/// Proves: a `Schedule` that names an archive is `invalid_request` when the text is not 64
/// lower-case hex characters (upper case, too short, too long, not hex), when the kinds are not
/// exactly `probe` (none, another kind, `probe` with another), and when the store does not hold an
/// archive of that SHA-256 (also on a coordinator that has no store); each leaves the state and
/// the audit as they were, and the same request naming the held archive is taken.
///
/// Catches: a run that stores an archive name no daemon can fetch, or one that rides on a run of
/// updates, whose hosts would be ordered an archive to check.
#[test]
fn an_archive_that_is_not_a_held_sha256_of_a_run_of_probe_alone_is_invalid() {
    let farm = farm("mxc-invalid");
    let held = farm.add("27.2", "27B52");
    let before = kept(&farm.world);
    let upper = held.sha256.to_uppercase();
    let with = |sha256: &str, kinds: &[MaintenanceKind]| proto::ScheduleMaintenanceRequest {
        kinds: kinds.iter().map(|kind| *kind as i32).collect(),
        ..probe_of(&[MAC], sha256)
    };
    let probe = [MaintenanceKind::Probe];
    for (name, request) in [
        ("upper case", with(&upper, &probe)),
        ("too short", with(&held.sha256[..63], &probe)),
        ("too long", with(&format!("{}0", held.sha256), &probe)),
        ("not hex", with(&"g".repeat(64), &probe)),
        ("no kinds", with(&held.sha256, &[])),
        (
            "another kind",
            with(&held.sha256, &[MaintenanceKind::Linux]),
        ),
        (
            "with another kind",
            with(
                &held.sha256,
                &[MaintenanceKind::Probe, MaintenanceKind::Linux],
            ),
        ),
        ("not held", with(&"f".repeat(64), &probe)),
    ] {
        assert_eq!(
            schedule(&farm.world, request).err(),
            Some(Refusal::InvalidRequest),
            "{name}"
        );
        assert_eq!(kept(&farm.world), before, "{name}");
    }
    schedule(&farm.world, with(&held.sha256, &probe)).expect("the held archive is taken");
}

/// Proves: a `Schedule` that names an archive on a coordinator that has no Xcode store is
/// `invalid_request`, since the store holds none.
///
/// Catches: a coordinator without a store that stores a run whose probes can never fetch.
#[test]
fn an_archive_is_invalid_on_a_coordinator_without_an_xcode_store() {
    let world = alpha("mxc-no-store", &[("m1", NONE)]);
    let before = kept(&world);
    assert_eq!(
        schedule(&world, probe_of(&["m1"], &"a".repeat(64))).err(),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(kept(&world), before);
}

/// Proves: a `Schedule` that names an archive while the store's directory cannot be read is
/// `store_failed` and stores nothing, and, once the directory is back, is taken.
///
/// Catches: a store fault answered as a missing archive (`invalid_request`, which sends the
/// operator to upload what is already there), or a run stored without the check being possible.
#[test]
fn an_archive_is_store_failed_while_the_store_cannot_be_read() {
    let farm = farm("mxc-unread");
    let held = farm.add("27.2", "27B52");
    let before = kept(&farm.world);
    let away = farm.dir.with_file_name("xcode-away");
    fs::rename(&farm.dir, &away).expect("moved away");
    assert_eq!(
        schedule(&farm.world, probe_of(&[MAC], &held.sha256)).err(),
        Some(Refusal::StoreFailed)
    );
    assert_eq!(kept(&farm.world), before);
    fs::rename(&away, &farm.dir).expect("moved back");
    schedule(&farm.world, probe_of(&[MAC], &held.sha256)).expect("taken");
}

/// Proves: a run that names an archive holds a host of a group that keeps Xcode, waiting with the
/// kind `probe`, and leaves out a host of a group that keeps none, as one that does not list
/// `probe`; with such a host alone it is `nothing_due`, and stores nothing. A run without an
/// archive still holds the host of the group that keeps none.
///
/// Catches: a probe turn ordered to a host the store refuses (its `GET` answers 403), which would
/// drain the host for a fetch that cannot happen.
#[test]
fn a_run_with_an_archive_holds_only_hosts_of_groups_that_keep_xcode() {
    let farm = farm("mxc-hosts");
    let held = farm.add("27.2", "27B52");

    let before = kept(&farm.world);
    assert_eq!(
        schedule(&farm.world, probe_of(&["b1"], &held.sha256)).err(),
        Some(Refusal::NothingDue)
    );
    assert_eq!(kept(&farm.world), before);

    let answer = schedule(&farm.world, probe_of(&[MAC, "b1"], &held.sha256)).expect("scheduled");
    assert_eq!(
        answer
            .hosts
            .iter()
            .map(|host| (host.host_id.as_str(), host.kinds.clone(), host.step()))
            .collect::<Vec<_>>(),
        vec![(
            MAC,
            vec![MaintenanceKind::Probe as i32],
            proto::MaintenanceHostStep::Waiting
        )]
    );
    assert_eq!(run_hosts(&farm), vec![MAC.to_owned()]);
    assert_eq!(
        farm.world.ops().maintenance_runs[0].kinds,
        vec![Kind::Probe]
    );

    let plain = schedule(&farm.world, probe_of(&["b1"], "")).expect("a plain probe");
    assert_eq!(plain.hosts.len(), 1);
    assert_eq!(plain.hosts[0].host_id, "b1");
}

/// Proves: the run's `xcode_sha256` is in the answer of a `Schedule` with `confirm` and, as it
/// would be, without it (which stores nothing), is stored with the run, and is shown by `Get` and
/// by `List`; a run without an archive shows it empty.
///
/// Catches: an archive that the operator cannot read back from the run, so that nothing says what
/// a probe run checked.
#[test]
fn the_run_shows_its_archive_in_every_answer() {
    let farm = farm("mxc-shown");
    let held = farm.add("27.2", "27B52");
    let mut dry = probe_of(&[MAC], &held.sha256);
    dry.confirm = false;
    let before = kept(&farm.world);
    let answer = schedule(&farm.world, dry).expect("the run as it would be");
    assert_eq!(answer.run.expect("a run").xcode_sha256, held.sha256);
    assert_eq!(kept(&farm.world), before);

    let answer = schedule(&farm.world, probe_of(&[MAC], &held.sha256)).expect("scheduled");
    let run = answer.run.expect("a run");
    assert_eq!(run.id, FIRST);
    assert_eq!(run.xcode_sha256, held.sha256);
    assert_eq!(
        farm.world.ops().maintenance_runs[0].xcode_sha256,
        held.sha256
    );
    let got = get(&farm.world, FIRST).expect("got");
    assert_eq!(got.run.expect("a run").xcode_sha256, held.sha256);
    let listed = list(&farm.world, &[], 0).expect("listed");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].xcode_sha256, held.sha256);

    edit_runs(&farm.world, |runs| runs[0].xcode_sha256.clear());
    let plain = get(&farm.world, FIRST).expect("got");
    assert_eq!(plain.run.expect("a run").xcode_sha256, "");
}

/// The entry `archive` makes in an order, and the whole order a check carries.
fn checked(archive: &Archive) -> XcodeOrder {
    XcodeOrder {
        entries: vec![entry_of(archive)],
        remove: Vec::new(),
        default: None,
    }
}

/// Proves: the order of the probe turn of a run that names an archive carries that archive's
/// entry alone (the store's version, build, size and SHA-256), with no version to remove and no
/// default, on the wire as in the order, and the kind `probe` alone; the archive is the one named
/// though the store holds a newer one the group would keep, and the turn is started.
///
/// Catches: an order that names the group's kept releases (so the daemon fetches the wrong
/// archive), or that asks a check to remove or select an Xcode.
#[test]
fn the_order_of_a_probe_turn_carries_the_one_archive_named() {
    let farm = farm("mxc-order");
    let older = farm.add("26.4", "26E50");
    let newer = farm.add("27.2", "27B52");
    schedule(&farm.world, probe_of(&[MAC], &older.sha256)).expect("scheduled");

    let (maintain, wire) = probe_order(&farm);
    assert_eq!(maintain.kinds, vec![agent::MaintenanceKind::Probe]);
    assert_eq!(maintain.action, agent::MaintainAction::Run);
    assert_eq!(maintain.xcode, Some(checked(&older)));
    assert_eq!(wire.order_id, ORDER);
    let Some(agent::order::Verb::Maintain(said)) = wire.verb else {
        panic!("a maintain on the wire")
    };
    assert_eq!(said.xcodes, vec![entry_of(&older)]);
    assert_eq!(said.xcode_remove, Vec::<String>::new());
    assert_eq!(said.xcode_default, "");
    assert_ne!(entry_of(&older), entry_of(&newer));
    assert_eq!(farm.turn(), (vec![Kind::Probe], Step::Staging));
}

/// Proves: the order of the probe turn of a run that names no archive carries no Xcode, though the
/// group keeps Xcode and the store holds releases.
///
/// Catches: a plain probe that is ordered to fetch an archive it was not asked to check.
#[test]
fn a_probe_turn_of_a_run_without_an_archive_is_ordered_none() {
    let farm = farm("mxc-plain");
    farm.add("27.2", "27B52");
    schedule(&farm.world, probe_of(&[MAC], "")).expect("scheduled");
    let (maintain, wire) = probe_order(&farm);
    assert_eq!(maintain.kinds, vec![agent::MaintenanceKind::Probe]);
    assert_eq!(maintain.xcode, None);
    let Some(agent::order::Verb::Maintain(said)) = wire.verb else {
        panic!("a maintain on the wire")
    };
    assert_eq!(said.xcodes, Vec::new());
}

/// Proves: when the archive a run names has left the store by the time its turn starts, the order
/// carries no archive (a plain probe), and the turn is started all the same.
///
/// Catches: an order that names an archive the store no longer holds, which the daemon's fetch
/// would answer `xcode_not_in_store` after draining the Mac.
#[test]
fn an_archive_that_left_the_store_gives_a_plain_probe_order() {
    let farm = farm("mxc-gone");
    let held = farm.add("27.2", "27B52");
    schedule(&farm.world, probe_of(&[MAC], &held.sha256)).expect("scheduled");
    fs::remove_file(farm.store.archive_path(&held.sha256)).expect("removed");

    let (maintain, _) = probe_order(&farm);
    assert_eq!(maintain.kinds, vec![agent::MaintenanceKind::Probe]);
    assert_eq!(maintain.xcode, None);
    assert_eq!(farm.turn(), (vec![Kind::Probe], Step::Staging));
}

/// Proves: while the store's directory cannot be read, a waiting probe turn of such a run is sent
/// no order and stays waiting, and a started turn that has not reported is sent nothing again;
/// once the directory is back each is sent the order with the archive.
///
/// Catches: an order sent with no archive because the store could not be listed, which would run
/// a plain probe under the order id of a check, and report a pass for nothing.
#[test]
fn a_probe_turn_with_an_archive_is_not_sent_while_the_store_cannot_be_read() {
    let farm = farm("mxc-unread-order");
    let held = farm.add("27.2", "27B52");
    schedule(&farm.world, probe_of(&[MAC], &held.sha256)).expect("scheduled");

    let away = farm.dir.with_file_name("xcode-away");
    fs::rename(&farm.dir, &away).expect("moved away");
    assert_eq!(farm.maintain(probe_beat()), None);
    assert_eq!(farm.turn(), (vec![Kind::Probe], Step::Waiting));
    fs::rename(&away, &farm.dir).expect("moved back");
    assert_eq!(probe_order(&farm).0.xcode, Some(checked(&held)));
    assert_eq!(farm.turn(), (vec![Kind::Probe], Step::Staging));

    fs::rename(&farm.dir, &away).expect("moved away");
    assert_eq!(farm.maintain(probe_beat()), None);
    assert_eq!(farm.turn(), (vec![Kind::Probe], Step::Staging));
    fs::rename(&away, &farm.dir).expect("moved back");
    assert_eq!(probe_order(&farm).0.xcode, Some(checked(&held)));
}

/// Proves: an operator's Put back queued on a halted probe turn of such a run is sent while the
/// store cannot be read: action `PUT_BACK`, the try it was queued under, and no archive.
///
/// Catches: a store fault that keeps a drained, halted Mac out of service, since a Put back needs
/// nothing of the store.
#[test]
fn a_put_back_of_a_halted_probe_is_sent_while_the_store_cannot_be_read() {
    let farm = farm("mxc-putback");
    let held = farm.add("27.2", "27B52");
    schedule(&farm.world, probe_of(&[MAC], &held.sha256)).expect("scheduled");
    edit_runs(&farm.world, |runs| {
        let host = &mut runs[0].hosts[0];
        host.step = Step::Halted;
        host.queued = Some(Act::PutBack);
        host.order_try = 2;
    });
    fs::rename(&farm.dir, farm.dir.with_file_name("xcode-away")).expect("moved away");
    let (maintain, wire) = probe_order(&farm);
    assert_eq!(maintain.action, agent::MaintainAction::PutBack);
    assert_eq!(maintain.xcode, None);
    assert_eq!(wire.r#try, 2);
}

/// Proves: a group that stops keeping Xcode after the run was scheduled is ordered a plain probe:
/// the store is not read for it and no archive is named.
///
/// Catches: an order that names an archive to a host the store's `GET` would refuse (403).
#[test]
fn a_group_that_stops_keeping_xcode_is_ordered_a_plain_probe() {
    let farm = farm("mxc-stops");
    let held = farm.add("27.2", "27B52");
    schedule(&farm.world, probe_of(&[MAC], &held.sha256)).expect("scheduled");
    farm.settings(0, None);
    let (maintain, _) = probe_order(&farm);
    assert_eq!(maintain.kinds, vec![agent::MaintenanceKind::Probe]);
    assert_eq!(maintain.xcode, None);
}

/// Proves: a probe turn of a run that names an archive goes to a Mac whose heartbeat is
/// answered with the same order as the order the first answer carried, so a lost answer is not a
/// run that never starts (the resend carries the archive again).
///
/// Catches: a resend that drops the archive of a check.
#[test]
fn a_probe_order_not_yet_reported_is_sent_again_with_its_archive() {
    let farm = farm("mxc-resend");
    let held = farm.add("27.2", "27B52");
    schedule(&farm.world, probe_of(&[MAC], &held.sha256)).expect("scheduled");
    let first = probe_order(&farm);
    let again = tell(&farm.world, MAC, probe_beat());
    let Some(Order::Maintain { maintain, .. }) = again else {
        panic!("sent again")
    };
    assert_eq!(maintain.xcode, Some(checked(&held)));
    assert_eq!(first.0.xcode, maintain.xcode);
}
