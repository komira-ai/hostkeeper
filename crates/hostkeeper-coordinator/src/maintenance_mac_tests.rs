//! What a Mac must report before it is ordered `macos` ([`crate::maintenance_mac`]), each rule
//! through the coordinator: `Maintenance.Schedule` (the host left out, or waiting without
//! `macos`, with why), `Maintenance.Get`, and the heartbeat that would start the host's turn (no
//! order, and why stored on its turn), and the group's `macos_major`.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::{MaintenanceRefusal as Refusal, MaintenanceSkip};
use hostkeeper_wire::wire::Heartbeat;

use crate::decide_maintenance::Skipping;
use crate::decide_maintenance::tests::{ORDER, RUN, beat, group_with, maintain_id, table, tell};
use crate::host_groups_support::{UNIX, World};
use crate::maintenance::text_of;
use crate::maintenance_calls_tests::{
    GROUP, clean_mac, edit_runs, get, host_at, hosts_request, run_of, schedule, seed_runs, waiting,
};
use crate::maintenance_runs::{Kind, RunHost, Skip, Step};
use crate::orders::Order;

/// The Mac of the tests.
const MAC: &str = "m1";

/// A Linux host of the Mac's group, with a Linux update due, so that a run whose Mac is left out
/// still has a host that waits.
const LINUX_HOST: &str = "l1";

/// A due item of `kind` from the version `from` to `to`.
fn item(kind: MaintenanceKind, from: &str, to: &str) -> agent::DueItem {
    agent::DueItem {
        kind: kind as i32,
        from: from.to_owned(),
        to: to.to_owned(),
        label: "pkg".to_owned(),
        ..agent::DueItem::default()
    }
}

/// A minor update of macOS due: 26.1 to 26.2.
fn minor() -> agent::DueItem {
    item(MaintenanceKind::Macos, "26.1", "26.2")
}

/// A major upgrade of macOS due: 26.1 to 27.0.
fn major() -> agent::DueItem {
    item(MaintenanceKind::Macos, "26.1", "27.0")
}

/// An Xcode install due.
fn xcode() -> agent::DueItem {
    item(MaintenanceKind::Xcode, "27.1", "27.2")
}

/// A heartbeat of a daemon that announces `maintenance-v1` and reports `due` and the Mac facts
/// `mac`.
fn mac_beat(due: &[agent::DueItem], mac: Option<agent::MacUpdateFacts>) -> Heartbeat {
    let mut heartbeat = beat(&[], None);
    let report = agent::MaintenanceReport {
        due: due.to_vec(),
        mac,
        ..agent::MaintenanceReport::default()
    };
    heartbeat.maintenance = Some(text_of(&report));
    heartbeat
}

/// [`clean_mac`] with `change`.
fn mac_with(change: impl FnOnce(&mut agent::MacUpdateFacts)) -> Option<agent::MacUpdateFacts> {
    let mut mac = clean_mac();
    change(&mut mac);
    Some(mac)
}

/// [`clean_mac`] with the update account `account` (none: not reported).
fn account(account: Option<(bool, bool, bool)>) -> Option<agent::MacUpdateFacts> {
    mac_with(|mac| {
        mac.update_account =
            account.map(|(present, admin, secure_token)| agent::UpdateAccountState {
                present,
                admin,
                secure_token,
                hidden: true,
            });
    })
}

/// [`clean_mac`] with FileVault in `state` and `fdesetup supportsauthrestart` saying `authrestart`.
fn filevault(state: agent::FileVaultState, authrestart: bool) -> Option<agent::MacUpdateFacts> {
    mac_with(|mac| {
        mac.filevault = state as i32;
        mac.authrestart_supported = authrestart;
    })
}

/// [`clean_mac`] with the device-management conflicts `keys`.
fn conflicts(keys: &[&str]) -> Option<agent::MacUpdateFacts> {
    mac_with(|mac| mac.mdm_conflicts = keys.iter().map(|key| (*key).to_owned()).collect())
}

/// A world named `name` with the group [`GROUP`] of batch 2, `min_serving` 0 and `macos_major`
/// `macos_major`, holding [`MAC`] and [`LINUX_HOST`], each heard with a Linux update due, then
/// [`MAC`] heard with `due` and `mac`.
fn world_of(
    name: &str,
    macos_major: bool,
    due: &[agent::DueItem],
    mac: Option<agent::MacUpdateFacts>,
) -> World {
    let world = World::new(name);
    let mut settings = table(2, 0);
    settings.macos_major = macos_major;
    group_with(&world, GROUP, settings, &[LINUX_HOST, MAC]);
    tell(&world, MAC, mac_beat(due, mac));
    world
}

/// A confirmed `Schedule` of [`MAC`] and [`LINUX_HOST`] of the kinds `kinds` (none: every kind
/// due), which may take the group's last host.
fn schedule_both(
    world: &World,
    kinds: &[MaintenanceKind],
) -> Result<proto::ScheduleMaintenanceResponse, Refusal> {
    let mut asked = hosts_request(&[MAC, LINUX_HOST]);
    asked.take_last_host = true;
    asked.kinds = kinds.iter().map(|kind| *kind as i32).collect();
    schedule(world, asked)
}

/// [`MAC`]'s turn of the first run the world stores.
fn mac_turn(world: &World) -> RunHost {
    world.ops().maintenance_runs[0]
        .hosts
        .iter()
        .find(|host| host.host_id == MAC)
        .expect("the Mac is in the run")
        .clone()
}

/// The `maintain` the coordinator answers a heartbeat `beat` of [`MAC`] with, if any: its kinds
/// and `macos_major`.
fn mac_ordered(world: &World, heartbeat: Heartbeat) -> Option<(Vec<agent::MaintenanceKind>, bool)> {
    match tell(world, MAC, heartbeat)? {
        Order::Maintain {
            maintain, order_id, ..
        } => {
            assert_eq!(order_id, ORDER);
            Some((maintain.kinds, maintain.macos_major))
        }
        _ => None,
    }
}

/// The reasons of the coordinator's `maintenance_step` audit entries about [`MAC`].
fn mac_steps_audited(world: &World) -> Vec<String> {
    world
        .audit()
        .iter()
        .filter(|entry| {
            entry.action == "maintenance_step"
                && entry.by == "coordinator"
                && entry.node_id.as_deref() == Some(MAC)
                && entry.host_group.as_deref() == Some(GROUP)
        })
        .filter_map(|entry| entry.reason.clone())
        .collect()
}

/// Asserts, for a Mac reporting a minor update of macOS due with the facts `mac`, that a
/// `Schedule` of `macos` leaves it out with exactly `token`, which `Schedule` and `Get` show as
/// `shown_as`; and that a Mac scheduled while its report was clean, whose next heartbeat reports
/// `mac`, is sent no `maintain` and its turn is stored skipped with exactly `token`, at that
/// heartbeat, audited, and is not offered again.
fn left_out_for(
    name: &str,
    mac: Option<agent::MacUpdateFacts>,
    token: &str,
    shown_as: MaintenanceSkip,
) {
    let world = world_of(&format!("{name}-schedule"), false, &[minor()], mac.clone());
    let response = schedule_both(&world, &[]).expect("the Linux host waits");
    let shown = response
        .hosts
        .iter()
        .find(|host| host.host_id == MAC)
        .expect("the Mac is in the run");
    assert_eq!(
        (shown.step(), shown.skipped_because()),
        (proto::MaintenanceHostStep::Skipped, shown_as),
        "{name}: Schedule shows the Mac left out"
    );
    let turn = mac_turn(&world);
    assert_eq!(turn.step, Step::Skipped, "{name}");
    assert_eq!(turn.kinds, vec![Kind::Macos], "{name}");
    assert_eq!(
        turn.skipped_because.map(Skip::as_str),
        Some(token),
        "{name}"
    );
    let got = get(&world, RUN).expect("the run");
    let got = got
        .hosts
        .iter()
        .find(|host| host.host_id == MAC)
        .expect("the Mac");
    assert_eq!(got.skipped_because(), shown_as, "{name}: Get shows why");
    assert_eq!(
        mac_ordered(&world, mac_beat(&[minor()], mac.clone())),
        None,
        "{name}"
    );

    let world = world_of(
        &format!("{name}-heartbeat"),
        false,
        &[minor()],
        Some(clean_mac()),
    );
    schedule_both(&world, &[MaintenanceKind::Macos]).expect("the clean Mac waits");
    assert_eq!(mac_turn(&world).step, Step::Waiting, "{name}");
    assert_eq!(
        mac_ordered(&world, mac_beat(&[minor()], mac.clone())),
        None,
        "{name}: no order"
    );
    let turn = mac_turn(&world);
    assert_eq!(
        (turn.step, turn.since_unix, turn.kinds),
        (Step::Skipped, UNIX, vec![Kind::Macos]),
        "{name}"
    );
    assert_eq!(
        turn.skipped_because.map(Skip::as_str),
        Some(token),
        "{name}"
    );
    assert_eq!(
        mac_steps_audited(&world),
        vec![format!("run {RUN} skipped {token}")],
        "{name}"
    );
    assert_eq!(
        mac_ordered(&world, mac_beat(&[minor()], Some(clean_mac()))),
        None,
        "{name}: a skipped turn is not offered again"
    );
}

/// Proves: a Mac that reports any device-management conflict (a settings key, a declaration's
/// type, `xcode-app-store`), also with no update account and FileVault on, is left out of a
/// `macos` run at `Schedule` and at the heartbeat that would start it, with exactly
/// `mdm_conflict`, never ordered.
///
/// Catches: a check of one known key only, or a Mac whose other faults are named first, so the
/// form would hide the conflict the operator must fix in device management.
#[test]
fn a_mac_with_an_mdm_conflict_is_not_ordered_macos() {
    let cases = [
        ("mac-mdm-deferrals", conflicts(&["Deferrals"])),
        (
            "mac-mdm-declaration",
            conflicts(&["com.apple.configuration.softwareupdate.enforcement.specific"]),
        ),
        (
            "mac-mdm-two",
            conflicts(&["xcode-app-store", "forceDelayedSoftwareUpdates"]),
        ),
        (
            "mac-mdm-first",
            mac_with(|mac| {
                mac.mdm_conflicts = vec!["Beta".to_owned()];
                mac.update_account = None;
                mac.filevault = agent::FileVaultState::On as i32;
            }),
        ),
    ];
    for (name, mac) in cases {
        left_out_for(name, mac, "mdm_conflict", MaintenanceSkip::MdmConflict);
    }
}

/// Proves: a Mac whose update account is not reported, absent, not an administrator, or without a
/// Secure Token, or whose report carries no Mac facts at all, also with FileVault on, is left out
/// of a `macos` run at `Schedule` and at the heartbeat that would start it, with exactly
/// `no_update_account`, never ordered.
///
/// Catches: an order the daemon could not install (no volume owner's authorisation), or a check
/// of the account's presence alone.
#[test]
fn a_mac_without_a_ready_update_account_is_not_ordered_macos() {
    let cases = [
        ("mac-account-none", account(None)),
        ("mac-account-absent", account(Some((false, true, true)))),
        ("mac-account-not-admin", account(Some((true, false, true)))),
        ("mac-account-no-token", account(Some((true, true, false)))),
        ("mac-account-no-facts", None),
        (
            "mac-account-first",
            mac_with(|mac| {
                mac.update_account = None;
                mac.filevault = agent::FileVaultState::On as i32;
            }),
        ),
    ];
    for (name, mac) in cases {
        left_out_for(
            name,
            mac,
            "no_update_account",
            MaintenanceSkip::NoUpdateAccount,
        );
    }
}

/// Proves: a Mac whose FileVault is on (with or without `supportsauthrestart`), being turned on or
/// off, or not read, is left out of a `macos` run at `Schedule` and at the heartbeat that would
/// start it, with exactly `filevault_not_ready`, never ordered: the report does not show the
/// update account a FileVault user.
///
/// Catches: an order whose restart would stop at FileVault's pre-boot login, leaving the Mac out
/// of service until someone types a password at it.
#[test]
fn a_mac_with_filevault_not_off_is_not_ordered_macos() {
    use agent::FileVaultState::{Decrypting, Encrypting, On, Unspecified};
    let cases = [
        ("mac-filevault-on", filevault(On, false)),
        ("mac-filevault-on-authrestart", filevault(On, true)),
        ("mac-filevault-encrypting", filevault(Encrypting, true)),
        ("mac-filevault-decrypting", filevault(Decrypting, true)),
        ("mac-filevault-unread", filevault(Unspecified, true)),
    ];
    for (name, mac) in cases {
        left_out_for(
            name,
            mac,
            "filevault_not_ready",
            MaintenanceSkip::FilevaultNotReady,
        );
    }
}

/// Proves: a Mac whose report is clean (no conflict, an update account present, an administrator
/// and holding a Secure Token, FileVault off) waits in a `macos` run with no reason, and its
/// heartbeat is answered with the `maintain` of `macos`; so is one whose account is not hidden.
///
/// Catches: a rule that leaves out every Mac, or reads the account's `hidden` as required.
#[test]
fn a_mac_with_a_clean_report_is_ordered_macos() {
    let shown = mac_with(|mac| {
        mac.update_account = Some(agent::UpdateAccountState {
            present: true,
            admin: true,
            secure_token: true,
            hidden: false,
        });
    });
    for (name, mac) in [("mac-clean", Some(clean_mac())), ("mac-clean-shown", shown)] {
        let world = world_of(name, false, &[minor()], mac.clone());
        schedule_both(&world, &[MaintenanceKind::Macos]).expect("scheduled");
        let turn = mac_turn(&world);
        assert_eq!(
            (turn.step, turn.kinds, turn.skipped_because),
            (Step::Waiting, vec![Kind::Macos], None),
            "{name}"
        );
        assert_eq!(
            mac_ordered(&world, mac_beat(&[minor()], mac)),
            Some((vec![MaintenanceKind::Macos], false)),
            "{name}"
        );
    }
}

/// Proves: a Mac that reports a device-management conflict, no update account and FileVault on is
/// still ordered an `xcode`-only run, with no reason stored: the rules of `macos` hold back no
/// other kind.
///
/// Catches: a Mac left out of every run by a fault that only a macOS update needs fixed.
#[test]
fn a_mac_with_an_mdm_conflict_is_ordered_an_xcode_only_run() {
    let mac = mac_with(|mac| {
        mac.mdm_conflicts = vec!["Deferrals".to_owned()];
        mac.update_account = None;
        mac.filevault = agent::FileVaultState::On as i32;
    });
    let world = world_of("mac-xcode-only", false, &[minor(), xcode()], mac.clone());
    schedule_both(&world, &[MaintenanceKind::Xcode]).expect("scheduled");
    let turn = mac_turn(&world);
    assert_eq!(
        (turn.step, turn.kinds, turn.skipped_because),
        (Step::Waiting, vec![Kind::Xcode], None)
    );
    assert_eq!(
        mac_ordered(&world, mac_beat(&[minor(), xcode()], mac)),
        Some((vec![MaintenanceKind::Xcode], false))
    );
    assert!(mac_steps_audited(&world).is_empty());
}

/// Proves: in a run of `macos` and `xcode`, a Mac with a device-management conflict at `Schedule`
/// waits with `xcode` alone and `mdm_conflict`, and is ordered `xcode` alone; a Mac clean at
/// `Schedule` whose next heartbeat reports a conflict is sent nothing at that heartbeat, its turn
/// stored waiting with `xcode` alone and `mdm_conflict` (audited), and its next heartbeat is
/// ordered `xcode` alone.
///
/// Catches: a whole host left out for `macos`'s sake, or `macos` ordered beside `xcode` to a Mac
/// that cannot take it.
#[test]
fn a_mac_left_out_of_macos_still_goes_for_xcode_in_the_same_run() {
    let both = [MaintenanceKind::Macos, MaintenanceKind::Xcode];
    let due = [minor(), xcode()];
    let faulty = conflicts(&["Deferrals"]);

    let world = world_of("mac-mixed-schedule", false, &due, faulty.clone());
    let response = schedule_both(&world, &both).expect("scheduled");
    let shown = response
        .hosts
        .iter()
        .find(|host| host.host_id == MAC)
        .expect("the Mac");
    assert_eq!(
        (shown.step(), shown.skipped_because(), shown.kinds.clone()),
        (
            proto::MaintenanceHostStep::Waiting,
            MaintenanceSkip::MdmConflict,
            vec![MaintenanceKind::Xcode as i32]
        )
    );
    assert_eq!(
        mac_ordered(&world, mac_beat(&due, faulty.clone())),
        Some((vec![MaintenanceKind::Xcode], false))
    );

    let world = world_of("mac-mixed-heartbeat", false, &due, Some(clean_mac()));
    schedule_both(&world, &both).expect("scheduled");
    assert_eq!(mac_turn(&world).kinds, vec![Kind::Macos, Kind::Xcode]);
    assert_eq!(mac_ordered(&world, mac_beat(&due, faulty.clone())), None);
    let turn = mac_turn(&world);
    assert_eq!(
        (turn.step, turn.kinds, turn.skipped_because),
        (Step::Waiting, vec![Kind::Xcode], Some(Skip::MdmConflict))
    );
    assert_eq!(
        mac_steps_audited(&world),
        vec![format!("run {RUN} waiting mdm_conflict")]
    );
    assert_eq!(
        mac_ordered(&world, mac_beat(&due, faulty)),
        Some((vec![MaintenanceKind::Xcode], false))
    );
}

/// Proves: a Mac whose only macOS item due is a major upgrade (26.1 to 27.0) has nothing due in a
/// group whose `macos_major` is false (the `Schedule` of it alone is refused `nothing_due`), and
/// waits with `macos` and is ordered it with `macos_major` true in a group that allows one; with a
/// minor update due beside the major one, it is ordered `macos` with `macos_major` false; an item
/// that names no version the Mac has, or the same major version with no minor one (`27` to
/// `27.1`), counts as due without the group's leave.
///
/// Catches: a major upgrade counted as due, or ordered with `macos_major`, in a group that has not
/// allowed one.
#[test]
fn a_major_upgrade_is_not_ordered_unless_the_group_allows_it() {
    let world = world_of("mac-major-refused", false, &[major()], Some(clean_mac()));
    let mut asked = hosts_request(&[MAC]);
    asked.take_last_host = true;
    assert_eq!(schedule(&world, asked), Err(Refusal::NothingDue));

    let world = world_of("mac-major-allowed", true, &[major()], Some(clean_mac()));
    schedule_both(&world, &[MaintenanceKind::Macos]).expect("scheduled");
    assert_eq!(mac_turn(&world).kinds, vec![Kind::Macos]);
    assert_eq!(
        mac_ordered(&world, mac_beat(&[major()], Some(clean_mac()))),
        Some((vec![MaintenanceKind::Macos], true))
    );

    let due = [
        [major(), minor()].to_vec(),
        vec![item(MaintenanceKind::Macos, "", "27.0")],
        vec![item(MaintenanceKind::Macos, "27", "27.1")],
    ];
    for (n, due) in due.into_iter().enumerate() {
        let world = world_of(
            &format!("mac-major-minor-{n}"),
            false,
            &due,
            Some(clean_mac()),
        );
        schedule_both(&world, &[MaintenanceKind::Macos]).expect("scheduled");
        assert_eq!(mac_turn(&world).kinds, vec![Kind::Macos], "{n}");
        assert_eq!(
            mac_ordered(&world, mac_beat(&due, Some(clean_mac()))),
            Some((vec![MaintenanceKind::Macos], false)),
            "{n}"
        );
    }
}

/// Proves: a skip of `macos` that the store refuses stores nothing and sends no order, and the
/// Mac's next heartbeat, once the store takes writes, stores it; a skip decided before an
/// operator's `Cancel` and stored after it stores nothing and writes no audit entry.
///
/// Catches: a Mac whose skip is lost and then ordered, or a cancelled run's host changed.
#[test]
fn a_skip_that_cannot_be_stored_or_comes_after_a_cancel_changes_nothing() {
    let faulty = conflicts(&["Deferrals"]);
    let world = world_of("mac-skip-store", false, &[minor()], Some(clean_mac()));
    schedule_both(&world, &[MaintenanceKind::Macos]).expect("scheduled");
    world.fake.refuse(|_| Some(crate::store::store_failure()));
    assert_eq!(
        maintain_id(tell(&world, MAC, mac_beat(&[minor()], faulty.clone())).as_ref()),
        None
    );
    assert_eq!(
        (mac_turn(&world).step, mac_turn(&world).skipped_because),
        (Step::Waiting, None)
    );
    world.fake.heal();
    assert_eq!(mac_ordered(&world, mac_beat(&[minor()], faulty)), None);
    assert_eq!(mac_turn(&world).skipped_because, Some(Skip::MdmConflict));

    let world = world_of("mac-skip-cancelled", false, &[minor()], Some(clean_mac()));
    schedule_both(&world, &[MaintenanceKind::Macos]).expect("scheduled");
    edit_runs(&world, |runs| runs[0].cancel(UNIX));
    let before = world.ops().maintenance_runs;
    let skipping = Skipping {
        run_id: RUN.to_owned(),
        host_group: GROUP.to_owned(),
        why: Skip::MdmConflict,
    };
    world
        .coordinator()
        .skip_macos(&crate::testutil::id(MAC), &skipping, UNIX);
    assert_eq!(world.ops().maintenance_runs, before);
    assert!(mac_steps_audited(&world).is_empty());
}

/// Proves: a skip of `macos` stored for a Mac changes only the Mac's own turn of the run it
/// names, and only while that turn waits with `macos`: another run that holds the Mac waiting,
/// another host of the run, the Mac's turn once it is staging, and its waiting turn without
/// `macos` are each left as they were, with no audit entry; its waiting turn of `macos` and
/// `xcode` is stored waiting with `xcode` alone and why.
///
/// Catches: a skip stored on the wrong run or host, or on a turn that has started or no longer
/// holds `macos`.
#[test]
fn a_skip_changes_only_the_macs_waiting_turn_of_macos_in_its_run() {
    let world = world_of("mac-skip-which", false, &[minor()], Some(clean_mac()));
    seed_runs(
        &world,
        vec![
            run_of("run-1-1", vec![waiting(MAC, GROUP, &[Kind::Macos])]),
            run_of(
                RUN,
                vec![
                    waiting("a0", GROUP, &[Kind::Macos]),
                    host_at(MAC, GROUP, Step::Staging, UNIX, &[Kind::Macos]),
                ],
            ),
        ],
    );
    let skipping = Skipping {
        run_id: RUN.to_owned(),
        host_group: GROUP.to_owned(),
        why: Skip::NoUpdateAccount,
    };
    let skip = |world: &World| {
        world
            .coordinator()
            .skip_macos(&crate::testutil::id(MAC), &skipping, UNIX);
    };
    let held = world.ops().maintenance_runs;
    skip(&world);
    assert_eq!(world.ops().maintenance_runs, held, "the Mac is staging");

    edit_runs(&world, |runs| {
        runs[1].hosts[1] = waiting(MAC, GROUP, &[Kind::Xcode])
    });
    let held = world.ops().maintenance_runs;
    skip(&world);
    assert_eq!(world.ops().maintenance_runs, held, "no macos in the turn");
    assert!(mac_steps_audited(&world).is_empty());

    edit_runs(&world, |runs| {
        runs[1].hosts[1] = waiting(MAC, GROUP, &[Kind::Macos, Kind::Xcode]);
    });
    let mut expected = world.ops().maintenance_runs;
    expected[1].hosts[1] = RunHost {
        skipped_because: Some(Skip::NoUpdateAccount),
        ..waiting(MAC, GROUP, &[Kind::Xcode])
    };
    skip(&world);
    assert_eq!(world.ops().maintenance_runs, expected);
    assert_eq!(
        mac_steps_audited(&world),
        vec![format!("run {RUN} waiting no_update_account")]
    );
}

/// Proves: a Mac whose turn deferred and is offered again, but whose next heartbeat reports no
/// update account, has `macos` left out with `no_update_account` (its step stays deferred,
/// audited), and is then sent its other kinds under the order of its second offer.
///
/// Catches: a deferred Mac that is decided `macos`-skipped at every heartbeat without the skip
/// ever being stored, so its other kinds are never offered again.
#[test]
fn a_deferred_mac_offered_again_without_its_account_is_sent_its_other_kinds() {
    let world = world_of(
        "mac-deferred-again",
        false,
        &[minor(), xcode()],
        Some(clean_mac()),
    );
    schedule_both(&world, &[]).expect("scheduled");
    edit_runs(&world, |runs| {
        for host in runs[0].hosts.iter_mut().filter(|host| host.host_id == MAC) {
            host.step = Step::Deferred;
            host.since_unix = UNIX - 10_000;
        }
    });
    let no_account = || mac_beat(&[minor(), xcode()], account(None));
    assert_eq!(maintain_id(tell(&world, MAC, no_account()).as_ref()), None);
    let turn = mac_turn(&world);
    assert_eq!(
        (turn.step, turn.kinds, turn.skipped_because),
        (
            Step::Deferred,
            vec![Kind::Xcode],
            Some(Skip::NoUpdateAccount)
        )
    );
    assert_eq!(
        mac_steps_audited(&world),
        vec![format!("run {RUN} deferred no_update_account")]
    );
    match tell(&world, MAC, no_account()) {
        Some(Order::Maintain {
            maintain, order_id, ..
        }) => {
            assert_eq!(order_id, format!("{ORDER}-2"));
            assert_eq!(maintain.kinds, vec![agent::MaintenanceKind::Xcode]);
        }
        other => panic!("not the second offer: {other:?}"),
    }
}
