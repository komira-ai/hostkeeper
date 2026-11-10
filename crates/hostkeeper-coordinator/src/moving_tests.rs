//! The sequence of a host moved into another group ([`crate::moving`]): the admission by the new
//! group's plans, the uninstalls of the old group's packages and the installs and updates of the
//! new group's, and the end of the move. The tests drive heartbeats of enrolled hosts over a
//! transport that carries typed orders, and say what the host was told as `verb package`.

use hostkeeper_enrollment::enroll::Membership;
use hostkeeper_proto::hostkeeper::operator::v1::{PolicyStage, RolloutPolicy, StageAdvance};
use hostkeeper_wire::wire::{ServiceBeat, ServiceReply};

use crate::decide::{Gate, Why};
use crate::force::{ForceRecord, ForceTargets};
use crate::group_decision_support::{Farm, policy, stages};
use crate::holds::{HoldMark, HoldScope};
use crate::host_groups_support::{REASON, SELF, operator_entry};
use crate::marks_support::{INSTALLER, ask_typed, beat_of, digest, runs};
use crate::ops::{AuditEntry, Exclusion, Halt, HaltOrigin, Write};
use crate::orders::Order;
use crate::registry::Digest;
use crate::rollouts::Pause;
use crate::testutil::{id, version};

const MOVING: Option<Why> = Some(Why::Gate(Gate::Moving));

/// The wide policy: everything may go at once, in one stage.
fn wide() -> RolloutPolicy {
    policy(|p| {
        p.max_updating = Some(5);
        p.live_rollouts = Some(4);
        p.stages = stages(&[100]);
    })
}

/// What a host that lists hostkeeper and `running` is told: `install svc`, `update svc`,
/// `uninstall svc`, or nothing.
fn told(farm: &Farm, host: &str, running: &[(&str, &str)]) -> Option<String> {
    let listed = std::iter::once((SELF, "1.0"))
        .chain(running.iter().copied())
        .map(|(name, at)| runs(name, at))
        .collect();
    told_beats(farm, host, listed)
}

/// What a host that lists the services `listed` is told ([`told`]).
fn told_beats(farm: &Farm, host: &str, listed: Vec<ServiceBeat>) -> Option<String> {
    let (reply, order) = ask_typed(&farm.world.rig, host, "storage", beat_of(INSTALLER, listed));
    match (reply, order) {
        (_, Some(Order::Install { service, .. })) => Some(format!("install {service}")),
        (_, Some(Order::Uninstall { service, .. })) => Some(format!("uninstall {service}")),
        (_, Some(Order::Update { service, .. })) => Some(format!("update {service}")),
        (_, Some(_)) => Some("other".to_owned()),
        (ServiceReply::Update { service, .. }, None) => {
            Some(format!("update {}", service.unwrap_or_default()))
        }
        (_, None) => None,
    }
}

/// An operator's hold of `package` on `host`.
fn held(farm: &Farm, package: &str, host: &str) {
    farm.world
        .coordinator()
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("test", "hold"),
            |state| {
                state.holds.push(HoldMark {
                    service: package.to_owned(),
                    scope: HoldScope::Host(host.to_owned()),
                    reason: "investigating".to_owned(),
                    by: "operator".to_owned(),
                    at_unix: 0,
                });
            },
        )
        .expect("held");
}

fn say(told: &str) -> Option<String> {
    Some(told.to_owned())
}

fn membership(group: &str, moving_from: Option<&str>) -> Membership {
    Membership {
        host_group: group.to_owned(),
        moving_from: moving_from.map(str::to_owned),
    }
}

/// A farm of the group `old`, which lists `oldsvc` and then its config `oldcfg`, and the group
/// `new` with `policy`, which lists `svc` and then its config `cfg`, both wanted at 2.0; the host
/// `m1` is in `old` and runs its two packages, and has been heard so.
async fn farm_of(name: &str, new: RolloutPolicy) -> Farm {
    let farm = Farm::new(name);
    farm.group("old", &["oldsvc", "oldcfg"], Some(wide()));
    farm.group("new", &["svc", "cfg"], Some(new));
    farm.host("m1", "old");
    farm.deploy("", "svc", "2.0", 1).await;
    farm.deploy("", "cfg", "2.0", 2).await;
    assert_eq!(
        told(&farm, "m1", &[("oldsvc", "1.0"), ("oldcfg", "1.0")]),
        None
    );
    farm
}

const OLD: [(&str, &str); 2] = [("oldsvc", "1.0"), ("oldcfg", "1.0")];

/// Proves: a move writes the one audit entry `host_move` for the host and the new group with the
/// reason `from old to new: ...`, and the moved host is shown `moving` for each package of the
/// group, those it must install and the packages it is to lose alike, at once, before it is heard.
///
/// Catches: a moved host decided by the plans of the group it joined before it is admitted.
#[tokio::test]
async fn a_move_is_audited_and_the_host_is_moving_from_the_call() {
    let farm = farm_of("mv-audit", wide()).await;
    let audited = farm.world.audit().len();

    let moved = farm.world.move_to("m1", "new").expect("moved");

    assert_eq!(moved.from_host_group, "old");
    assert_eq!(
        farm.world.audit_after(audited)[0],
        operator_entry(
            audited as u64 + 1,
            "host_move",
            Some("new"),
            Some("m1"),
            &format!("from old to new: {REASON}")
        )
    );
    assert_eq!(farm.world.membership("m1"), membership("new", Some("old")));
    assert_eq!(farm.why("m1", "svc"), MOVING);
    assert_eq!(farm.why("m1", "oldcfg"), MOVING);
}

/// Proves: an admitted host is uninstalled the old group's packages in the reverse of the old
/// list (the config package before its service), and then installed the new group's in its list
/// order, back to back as each order ends, with no wait for the plan's stages; when its packages
/// match its group its move ends in its record and it is a host like any other.
///
/// Catches: installs ordered before the uninstalls, uninstalls in list order, or a move that never
/// ends.
#[tokio::test]
async fn an_admitted_host_uninstalls_in_reverse_then_installs_in_order_and_the_move_ends() {
    let farm = farm_of("mv-sequence", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");

    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
    assert_eq!(farm.world.membership("m1"), membership("new", Some("old")));
    assert_eq!(
        told(&farm, "m1", &[("oldsvc", "1.0")]),
        say("uninstall oldsvc")
    );
    assert_eq!(told(&farm, "m1", &[]), say("install svc"));
    assert_eq!(told(&farm, "m1", &[("svc", "2.0")]), say("install cfg"));
    assert_eq!(farm.world.membership("m1"), membership("new", Some("old")));

    assert_eq!(told(&farm, "m1", &[("svc", "2.0"), ("cfg", "2.0")]), None);

    assert_eq!(farm.world.membership("m1"), membership("new", None));
    assert!(farm.world.intents().is_empty());
    assert!(!farm.world.coordinator().lock_fleet().is_moving(&id("m1")));
}

/// Proves: an installs and updates of an admitted host wait while one of its uninstalls is to run,
/// told `moving`, and a package both groups list is updated to the new group's version, a
/// downgrade included.
///
/// Catches: a shared package updated before the old group's config package is gone, or left at
/// the old group's version.
#[tokio::test]
async fn a_package_both_groups_list_is_updated_to_the_new_groups_version_even_down() {
    let farm = Farm::new("mv-shared");
    farm.group("old", &["shared", "oldcfg"], Some(wide()));
    farm.group("new", &["shared", "cfg"], Some(wide()));
    farm.host("m1", "old");
    farm.deploy("new", "shared", "2.0", 1).await;
    farm.deploy("new", "cfg", "2.0", 2).await;
    let running = [("shared", "3.0"), ("oldcfg", "1.0")];
    assert_eq!(told(&farm, "m1", &running), None);
    farm.world.move_to("m1", "new").expect("moved");

    assert_eq!(told(&farm, "m1", &running), say("uninstall oldcfg"));
    assert_eq!(
        told(&farm, "m1", &[("shared", "3.0")]),
        say("update shared")
    );
    assert_eq!(told(&farm, "m1", &[("shared", "2.0")]), say("install cfg"));
    assert_eq!(
        told(&farm, "m1", &[("shared", "2.0"), ("cfg", "2.0")]),
        None
    );
    assert_eq!(farm.world.membership("m1"), membership("new", None));
}

/// Proves: a host is ordered nothing and is shown `moving` while a plan of the new group it needs
/// is paused, halted, at a version marked bad, or has no free place in its stage, and is
/// admitted, and uninstalls, at the heartbeat after the plan lets it in.
///
/// Catches: a host half moved while the plan of its new package waits.
#[tokio::test]
async fn a_moving_host_waits_while_a_plan_it_needs_is_paused_halted_bad_or_full() {
    let farm = farm_of("mv-blocked", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");

    farm.world.seed_marks("new", "svc", |marks| {
        marks.rollout.paused = Some(Pause {
            by: "operator".to_owned(),
            reason: "looking".to_owned(),
        });
    });
    assert_eq!(told(&farm, "m1", &OLD), None);
    assert_eq!(farm.why("m1", "cfg"), MOVING);
    assert_eq!(farm.why("m1", "oldcfg"), MOVING);
    farm.world
        .seed_marks("new", "svc", |marks| marks.rollout.paused = None);

    farm.world.seed_marks("new", "cfg", |marks| {
        marks.halt = Some(Halt {
            node_id: "x".to_owned(),
            check: "artifact_changed".to_owned(),
            target: version("2.0"),
            report: None,
            origin: HaltOrigin::Updater,
        });
    });
    assert_eq!(told(&farm, "m1", &OLD), None);
    assert_eq!(farm.why("m1", "svc"), MOVING);
    farm.world
        .seed_marks("new", "cfg", |marks| marks.halt = None);

    let ops = farm.world.coordinator().ops();
    let mark_bad = |wanted: bool| {
        ops.change(
            Write::BeforeMemory,
            AuditEntry::new("test", "bad"),
            |state| {
                let bad = &mut state.services.entry("svc".to_owned()).or_default().bad;
                bad.retain(|_| false);
                if wanted {
                    bad.push(version("2.0"));
                }
            },
        )
        .expect("marked");
    };
    mark_bad(true);
    assert_eq!(told(&farm, "m1", &OLD), None);
    assert_eq!(farm.why("m1", "svc"), MOVING);
    mark_bad(false);

    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
}

/// Proves: a plan whose current stage is full admits no moving host: with a stage of one host that
/// another host of the group already started, the moving host is told nothing and is shown
/// `moving`; once that host is done the stage after it is entered and has a place, and the moving
/// host is admitted and uninstalls.
///
/// Catches: a moving host that takes a place no stage has, or one that waits for a stage that is
/// open.
#[tokio::test]
async fn a_moving_host_waits_for_a_free_place_in_the_stage_of_a_plan() {
    let farm = Farm::new("mv-full");
    farm.group("old", &["oldsvc", "oldcfg"], Some(wide()));
    farm.group(
        "new",
        &["svc", "cfg"],
        Some(policy(|p| {
            p.max_updating = Some(5);
            p.live_rollouts = Some(4);
            p.stages = stages(&[25, 100]);
        })),
    );
    for host in ["n1", "n2", "n3"] {
        farm.host(host, "new");
    }
    farm.host("m1", "old");
    let base = [("svc", "1.0"), ("cfg", "1.0")];
    for host in ["n1", "n2", "n3"] {
        assert_eq!(told(&farm, host, &base), None);
    }
    assert_eq!(told(&farm, "m1", &OLD), None);
    farm.deploy("", "svc", "2.0", 1).await;
    farm.deploy("", "cfg", "2.0", 2).await;
    farm.world.move_to("m1", "new").expect("moved");

    assert_eq!(told(&farm, "n1", &base), say("update svc"));
    assert_eq!(told(&farm, "m1", &OLD), None);
    assert_eq!(farm.why("m1", "svc"), MOVING);

    assert_eq!(
        told(&farm, "n1", &[("svc", "2.0"), ("cfg", "1.0")]),
        say("update cfg")
    );
    assert_eq!(told(&farm, "m1", &OLD), None);
    assert_eq!(farm.why("m1", "cfg"), MOVING);
    assert_eq!(told(&farm, "n1", &[("svc", "2.0"), ("cfg", "2.0")]), None);
    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
}

/// Proves: an admitted host keeps its place: it is ordered past a pause that comes after its
/// admission, and past a slot another host of the group holds, while a hold on a pair of it is
/// still obeyed.
///
/// Catches: an admitted host held at the plan's gate it was already let through, which would
/// leave it half moved.
#[tokio::test]
async fn an_admitted_host_goes_past_the_plans_gate_and_under_every_other_rule() {
    let farm = farm_of(
        "mv-past-gate",
        policy(|p| {
            p.max_updating = Some(1);
            p.live_rollouts = Some(4);
            p.stages = stages(&[100]);
        }),
    )
    .await;
    farm.host("n1", "new");
    assert_eq!(
        told(&farm, "n1", &[("svc", "1.0"), ("cfg", "1.0")]),
        say("update svc")
    );
    farm.world.move_to("m1", "new").expect("moved");
    held(&farm, "cfg", "m1");

    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
    farm.world.seed_marks("new", "svc", |marks| {
        marks.rollout.paused = Some(Pause {
            by: "operator".to_owned(),
            reason: "looking".to_owned(),
        });
    });
    assert_eq!(
        told(&farm, "m1", &[("oldsvc", "1.0")]),
        say("uninstall oldsvc")
    );
    assert_eq!(told(&farm, "m1", &[]), say("install svc"));
    assert_eq!(told(&farm, "m1", &[("svc", "2.0")]), None);
    assert_eq!(farm.why("m1", "cfg"), Some(Why::Gate(Gate::Held)));
}

/// Proves: a pair an operator holds on the host is left as it is, removed or not, and the move
/// goes on around it and ends: a held old package stays installed with its intent standing, a held
/// new package is not installed, and the host's record is no longer moving.
///
/// Catches: a move that waits for a held pair for good, or that touches one.
#[tokio::test]
async fn a_held_pair_is_left_alone_and_does_not_stop_the_move_ending() {
    let farm = farm_of("mv-held", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");
    held(&farm, "oldcfg", "m1");
    held(&farm, "cfg", "m1");

    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldsvc"));
    assert_eq!(told(&farm, "m1", &[("oldcfg", "1.0")]), say("install svc"));
    assert_eq!(farm.world.membership("m1"), membership("new", Some("old")));
    assert_eq!(
        told(&farm, "m1", &[("oldcfg", "1.0"), ("svc", "2.0")]),
        None
    );

    assert_eq!(farm.world.membership("m1"), membership("new", None));
    assert_eq!(
        farm.world.intents(),
        vec![("oldcfg".to_owned(), "m1".to_owned(), 1)]
    );
}

/// Proves: an update running on the host is not cut: while the host reports one of the old
/// group's packages updating it is ordered nothing, and the sequence goes on when the update has
/// ended.
///
/// Catches: an uninstall ordered over a running update.
#[tokio::test]
async fn a_running_update_is_not_cut() {
    let farm = farm_of("mv-running", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");
    let mut busy = runs("oldsvc", "1.0");
    busy.updating = Some(version("1.1"));

    assert_eq!(
        told_beats(
            &farm,
            "m1",
            vec![runs(SELF, "1.0"), busy, runs("oldcfg", "1.0")]
        ),
        None
    );
    assert_eq!(
        farm.why("m1", "oldcfg"),
        Some(Why::Gate(Gate::BusyElsewhere))
    );

    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
}

/// Proves: a host that is silent stays moving in its record, with nothing ordered, however long
/// it is silent, and its sequence runs at its first heartbeat.
///
/// Catches: a move that is lost, or ended, for want of a heartbeat.
#[tokio::test]
async fn a_silent_host_stays_moving_until_it_is_heard() {
    let farm = farm_of("mv-silent", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");

    farm.world.rig.at(9_000);

    assert_eq!(farm.world.membership("m1"), membership("new", Some("old")));
    assert_eq!(farm.why("m1", "svc"), MOVING);
    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
}

/// Proves: a second move replaces the target and starts from admission into the newest group: the
/// host admitted to `new` is told `moving` for the packages of `third` at once and nothing while
/// `third`'s plan is paused, its record names `new` as the group it moves from, and it is admitted
/// to `third` when the plan lets it in.
///
/// Catches: an admission to the first group carried over to the second.
#[tokio::test]
async fn a_second_move_starts_again_from_admission_into_the_newest_group() {
    let farm = farm_of("mv-second", wide()).await;
    farm.group("third", &["tsvc"], Some(wide()));
    farm.deploy("third", "tsvc", "2.0", 3).await;
    farm.world.move_to("m1", "new").expect("moved");
    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
    farm.world.seed_marks("third", "tsvc", |marks| {
        marks.rollout.paused = Some(Pause {
            by: "operator".to_owned(),
            reason: "looking".to_owned(),
        });
    });

    farm.world.move_to("m1", "third").expect("moved again");

    assert_eq!(
        farm.world.membership("m1"),
        membership("third", Some("new"))
    );
    assert_eq!(farm.why("m1", "tsvc"), MOVING);
    assert_eq!(told(&farm, "m1", &[("oldsvc", "1.0")]), None);
    assert_eq!(farm.why("m1", "tsvc"), MOVING);
    farm.world
        .seed_marks("third", "tsvc", |marks| marks.rollout.paused = None);
    assert_eq!(
        told(&farm, "m1", &[("oldsvc", "1.0")]),
        say("uninstall oldsvc")
    );
}

/// Proves: a coordinator that starts again knows no admission and makes it again: during its quiet
/// start the host is ordered nothing and shown `moving`, and after it the host is admitted and
/// ordered the sequence; the host's record has kept the move all along.
///
/// Catches: an admission that is assumed across a restart, or a quiet coordinator that admits.
#[tokio::test]
async fn a_restart_makes_the_admission_again() {
    let mut farm = farm_of("mv-restart", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");
    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
    farm.world.rig.at(2_000);
    farm.world.rig.coordinator = crate::host_groups_support::coordinator_on(
        &farm.world.fake,
        &farm.world.rig.clock,
        Some(std::sync::Arc::clone(&farm.world.enrollment.enrollment)),
    );

    farm.world.rig.at(2_010);
    assert_eq!(told(&farm, "m1", &OLD), None);
    assert_eq!(farm.why("m1", "oldcfg"), MOVING);
    assert_eq!(farm.world.membership("m1"), membership("new", Some("old")));

    farm.world.rig.at(2_100);
    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
}

/// A farm whose host `m1` runs `web` at the version both groups want, and has just been moved.
async fn matched(name: &str) -> Farm {
    let farm = Farm::new(name);
    farm.group("old", &["web"], Some(wide()));
    farm.group("new", &["web"], Some(wide()));
    farm.host("m1", "old");
    farm.deploy("", "web", "2.0", 1).await;
    farm.world.move_to("m1", "new").expect("moved");
    farm
}

/// Proves: a host whose packages already match its new group ends its move at its first
/// heartbeat and is ordered nothing.
///
/// Catches: a host left moving for good when there is nothing to change.
#[tokio::test]
async fn a_host_with_nothing_to_change_ends_its_move_at_once() {
    let farm = matched("mv-nothing").await;

    assert_eq!(told(&farm, "m1", &[("web", "2.0")]), None);

    assert_eq!(farm.world.membership("m1"), membership("new", None));
    assert!(!farm.world.coordinator().lock_fleet().is_moving(&id("m1")));
}

/// Proves: a record that cannot be read or written at whichever step of the heartbeat leaves the
/// host's move as it was or ends it, never half done, and the next heartbeat ends it; at least one
/// step leaves the move to the next heartbeat.
///
/// Catches: a failed write of the end that is never tried again.
#[tokio::test]
async fn a_failed_write_of_the_end_of_a_move_is_tried_again_at_the_next_heartbeat() {
    let mut retried = 0;
    for step in 1..=20 {
        let farm = matched(&format!("mv-failing-{step}")).await;
        let records = &farm.world.records;
        records.fail_at(records.count() + step);
        assert_eq!(told(&farm, "m1", &[("web", "2.0")]), None, "step {step}");
        records.fail_at(0);
        if farm.world.membership("m1").moving_from.is_some() {
            assert_eq!(farm.world.membership("m1"), membership("new", Some("old")));
            retried += 1;
        }

        assert_eq!(told(&farm, "m1", &[("web", "2.0")]), None, "step {step}");
        assert_eq!(
            farm.world.membership("m1"),
            membership("new", None),
            "step {step}"
        );
    }
    assert!(retried > 0);
}

/// Proves: the end of a move is written only for the group the host is in: a record that has the
/// host in another group by then is left as it is, and a host unknown to the store changes
/// nothing.
///
/// Catches: a stale heartbeat that overwrites the group of a host moved since.
#[tokio::test]
async fn the_end_of_a_move_does_not_overwrite_another_group() {
    let farm = matched("mv-stale").await;
    let store = farm.world.enrollment.enrollment.store();
    let host = hostkeeper_enrollment::names::HostId::parse("m1").expect("a host id");
    let other = hostkeeper_enrollment::names::HostId::parse("nobody").expect("a host id");
    let old = "old".parse().expect("a group name");

    crate::moving::clear(store, &host, &old).expect("cleared");
    crate::moving::clear(store, &other, &old).expect("nothing to clear");

    assert_eq!(farm.world.membership("m1"), membership("new", Some("old")));
}

/// Proves: a move puts every record the fleet keeps of the host in the new group at once: its node
/// and what each service last heard of it, the packages it is to lose included, so that no count
/// reads its old group until its next heartbeat.
///
/// Catches: a moved host counted in the plans of its old group by one record and of its new group
/// by another.
#[tokio::test]
async fn a_move_puts_every_record_of_the_host_in_the_new_group_at_once() {
    let farm = farm_of("mv-regroup", wide()).await;
    let fleet = farm.world.coordinator().lock_fleet();
    let before = ["oldsvc", "oldcfg"].map(|package| {
        fleet
            .get_in(package, &id("m1"))
            .map(|host| host.role_name().to_owned())
    });
    drop(fleet);
    assert_eq!(before, [Some("old".to_owned()), Some("old".to_owned())]);

    farm.world.move_to("m1", "new").expect("moved");

    let fleet = farm.world.coordinator().lock_fleet();
    assert_eq!(
        fleet.node(&id("m1")).map(|node| node.role.as_str()),
        Some("new")
    );
    for package in ["oldsvc", "oldcfg"] {
        let host = fleet.get_in(package, &id("m1")).expect("heard");
        assert_eq!(host.role_name(), "new", "{package}");
    }
}

/// Proves: the fleet's move of a host is what the host's record says: a host the record says is
/// moving into a group gets a move that is not admitted, a move into another group replaces it
/// and its admission, and a record that says it is not moving, or in no group, ends it.
///
/// Catches: an admission to one group carried over to a move into another.
#[test]
fn the_fleets_move_of_a_host_is_what_its_record_says() {
    let mut fleet = crate::fleet::Fleet::new(std::time::Duration::ZERO, std::time::Duration::ZERO);
    let (host, alpha, beta) = (
        id("m1"),
        "alpha".parse().expect("a group"),
        "beta".parse().expect("a group"),
    );

    fleet.sync_move(&host, Some(&alpha), true);
    assert!(fleet.waits_to_move(&host));
    fleet.admit(&host, std::collections::BTreeMap::new());
    assert!(fleet.is_admitted(&host));
    fleet.sync_move(&host, Some(&alpha), true);
    assert!(fleet.is_admitted(&host));
    fleet.sync_move(&host, Some(&beta), true);
    assert!(fleet.waits_to_move(&host));
    fleet.sync_move(&host, Some(&beta), false);
    assert!(!fleet.is_moving(&host));
    fleet.sync_move(&host, Some(&beta), true);
    fleet.sync_move(&host, None, true);
    assert!(!fleet.is_moving(&host));
}

/// Excludes `host` from `package`.
fn excluded(farm: &Farm, package: &str, host: &str) {
    farm.world
        .coordinator()
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("test", "exclude"),
            |state| {
                state.excluded.push(Exclusion {
                    service: package.to_owned(),
                    node_id: host.to_owned(),
                    reason: "for the test".to_owned(),
                });
            },
        )
        .expect("excluded");
}

/// Proves: the host's own exclusion and minimum version are left aside by the move, as a hold is:
/// a package it is excluded from is not installed, one wanted below the group's minimum version
/// for it is not, and the move ends once the old group's packages are gone.
///
/// Catches: a move that waits for good on a package the host's own rules never let in.
#[tokio::test]
async fn an_exclusion_and_a_minimum_version_are_left_aside_and_the_move_ends() {
    let farm = farm_of("mv-aside", wide()).await;
    excluded(&farm, "svc", "m1");
    farm.world.seed_marks("new", "cfg", |marks| {
        marks.min_version = Some(version("3.0"));
    });
    farm.world.move_to("m1", "new").expect("moved");

    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
    assert_eq!(
        told(&farm, "m1", &[("oldsvc", "1.0")]),
        say("uninstall oldsvc")
    );
    assert_eq!(told(&farm, "m1", &[]), None);

    assert_eq!(farm.world.membership("m1"), membership("new", None));
}

/// Proves: a package the host lists but does not run (staged, `absent`) at the wanted version is
/// still to be installed, and the move does not end before it is.
///
/// Catches: a staged copy taken for an installed package.
#[tokio::test]
async fn a_package_staged_but_not_run_is_still_to_install() {
    let farm = farm_of("mv-staged", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");
    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
    assert_eq!(
        told(&farm, "m1", &[("oldsvc", "1.0")]),
        say("uninstall oldsvc")
    );
    let staged = serde_json::from_value(
        serde_json::json!({"name": "svc", "version": "2.0", "health": "absent"}),
    )
    .expect("a service beat");

    assert_eq!(
        told_beats(&farm, "m1", vec![runs(SELF, "1.0"), staged]),
        say("install svc")
    );
    assert_eq!(farm.world.membership("m1"), membership("new", Some("old")));
}

/// Proves: a move does not end while an uninstall is to run even when every package of the new
/// group is as wanted, and ends when it has run.
///
/// Catches: a move ended before the old group's packages are gone.
#[tokio::test]
async fn a_move_does_not_end_while_an_uninstall_is_to_run() {
    let farm = Farm::new("mv-pending");
    farm.group("old", &["oldsvc"], Some(wide()));
    farm.group("new", &["web"], Some(wide()));
    farm.host("m1", "old");
    farm.deploy("", "web", "2.0", 1).await;
    let running = [("oldsvc", "1.0"), ("web", "2.0")];
    assert_eq!(told(&farm, "m1", &running), None);
    farm.world.move_to("m1", "new").expect("moved");

    assert_eq!(told(&farm, "m1", &running), say("uninstall oldsvc"));
    assert_eq!(farm.world.membership("m1"), membership("new", Some("old")));
    assert_eq!(told(&farm, "m1", &[("web", "2.0")]), None);

    assert_eq!(farm.world.membership("m1"), membership("new", None));
}

/// Proves: an uninstall intent for a package the host does not list, which an agent that cannot
/// report a package gone never settles, does not hold the move: it ends with the intent standing.
///
/// Catches: a move that waits for good on a package that is not on the host.
#[tokio::test]
async fn an_intent_for_a_package_the_host_does_not_list_does_not_hold_the_move() {
    let farm = farm_of("mv-unlisted", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");
    assert_eq!(farm.world.intents().len(), 2);
    let bare = beat_of(&["install-v1"], vec![runs(SELF, "1.0")]);

    assert_eq!(farm.order_on("m1", bare).map(|_| ()), None);

    assert_eq!(farm.world.membership("m1"), membership("new", None));
    assert_eq!(farm.world.intents().len(), 2);
}

/// Proves: a host a standing force of the new group orders is no host of that package's plan, so
/// the plan, full of another host's update, does not keep it waiting.
///
/// Catches: a forced host kept out by a plan that does not count it.
#[tokio::test]
async fn a_host_the_group_forces_takes_no_place_and_waits_for_none() {
    let farm = Farm::new("mv-forced");
    farm.group("old", &["oldsvc"], Some(wide()));
    farm.group(
        "new",
        &["svc"],
        Some(policy(|p| {
            p.max_updating = Some(5);
            p.live_rollouts = Some(4);
            p.stages = stages(&[50, 100]);
        })),
    );
    farm.host("n1", "new");
    farm.host("m1", "old");
    assert_eq!(told(&farm, "n1", &[("svc", "1.0")]), None);
    assert_eq!(told(&farm, "m1", &[("oldsvc", "1.0")]), None);
    farm.deploy("", "svc", "2.0", 1).await;
    farm.world.move_to("m1", "new").expect("moved");
    assert_eq!(told(&farm, "n1", &[("svc", "1.0")]), say("update svc"));
    assert_eq!(told(&farm, "m1", &[("oldsvc", "1.0")]), None);
    farm.world.seed_marks("new", "svc", |marks| {
        marks.force = Some(ForceRecord {
            version: version("2.0"),
            generation: 1,
            reason: "for the tests".to_owned(),
            by: "operator".to_owned(),
            digest: Digest::parse(&digest(1)).expect("a digest"),
            release_index: None,
            halt_generation: 0,
            targets: ForceTargets::Hosts(vec!["m1".to_owned()]),
            include_held: false,
            hosts: vec!["m1".to_owned()],
        });
    });

    assert_eq!(
        told(&farm, "m1", &[("oldsvc", "1.0")]),
        say("uninstall oldsvc")
    );
}

/// Proves: a plan that has rolled every other host admits the moving host although the stage
/// after the last it entered waits for an operator: its current stage has no free place, and the
/// plan is finished but for the host.
///
/// Catches: a moving host that waits for an Advance on a plan that is done.
#[tokio::test]
async fn a_plan_that_has_rolled_every_other_host_admits_the_host() {
    let farm = Farm::new("mv-finished");
    farm.group("old", &["oldsvc"], Some(wide()));
    farm.group(
        "new",
        &["svc"],
        Some(policy(|p| {
            p.max_updating = Some(5);
            p.live_rollouts = Some(4);
            p.stages = vec![
                PolicyStage {
                    percent: 50,
                    advance: StageAdvance::Auto.into(),
                },
                PolicyStage {
                    percent: 100,
                    advance: StageAdvance::Manual.into(),
                },
            ];
        })),
    );
    farm.host("n1", "new");
    farm.host("m1", "old");
    assert_eq!(told(&farm, "n1", &[("svc", "1.0")]), None);
    assert_eq!(told(&farm, "m1", &[("oldsvc", "1.0")]), None);
    farm.deploy("", "svc", "2.0", 1).await;
    assert_eq!(told(&farm, "n1", &[("svc", "1.0")]), say("update svc"));
    assert_eq!(told(&farm, "n1", &[("svc", "2.0")]), None);
    farm.world.move_to("m1", "new").expect("moved");

    assert_eq!(
        told(&farm, "m1", &[("oldsvc", "1.0")]),
        say("uninstall oldsvc")
    );
}

/// Proves: a plan whose first stage waits for an operator's Advance and has rolled no host has no
/// place for the moving host, though no other host is in it: the host is shown `moving`, and
/// is admitted once the stage is entered.
///
/// Catches: an empty plan read as a finished one.
#[tokio::test]
async fn a_plan_waiting_for_its_first_advance_admits_no_one() {
    let farm = farm_of(
        "mv-first-advance",
        policy(|p| {
            p.max_updating = Some(5);
            p.live_rollouts = Some(4);
            p.stages = vec![PolicyStage {
                percent: 100,
                advance: StageAdvance::Manual.into(),
            }];
        }),
    )
    .await;
    farm.world.move_to("m1", "new").expect("moved");

    assert_eq!(told(&farm, "m1", &OLD), None);
    assert_eq!(farm.why("m1", "svc"), MOVING);

    for package in ["svc", "cfg"] {
        farm.world.seed_marks("new", package, |marks| {
            marks.rollout.version = Some(version("2.0"));
            marks.rollout.entered = 1;
        });
    }
    assert_eq!(told(&farm, "m1", &OLD), say("uninstall oldcfg"));
}

/// Proves: an update a host runs of a service that does not count it is no busy host for the other
/// services, unless the host is moving: the move does not cut an update of a package it leaves,
/// while a coordinator without a move reads what it did before.
///
/// Catches: a host that stopped being counted for a service holding every other service's order
/// for as long as it says it updates, in a coordinator that moves no host.
#[test]
fn an_update_outside_the_counted_services_holds_the_host_only_while_it_moves() {
    use std::time::Duration;

    use crate::fleet::{Fleet, QUIET_START};
    use crate::testutil::{beat, role, updating};

    let now = Duration::from_secs(1_000);
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    fleet.record_outside_in(
        "left",
        id("h"),
        &role("daemon").to_string(),
        updating(beat("1.0"), "2.0"),
        now,
    );

    assert!(!fleet.is_busy_besides("other", &id("h"), now));
    assert!(!fleet.is_busy_besides("other", &id("h2"), now));

    fleet.begin_move(id("h"), "new".parse().expect("a group name"));

    assert!(fleet.is_busy_besides("other", &id("h"), now));
    assert!(!fleet.is_busy_besides("left", &id("h"), now));
    assert!(!fleet.is_busy_besides("other", &id("h2"), now));
}
