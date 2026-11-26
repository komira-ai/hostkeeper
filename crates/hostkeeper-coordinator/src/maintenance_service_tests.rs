//! `Maintenance.Schedule` ([`crate::maintenance_service`]): which hosts a run takes and which
//! it leaves out, the Update button, the last host of a group, every refusal, and a request
//! that is only asked about. The builders are in `maintenance_calls_tests.rs`.

use hostkeeper_enrollment::enroll::Membership;
use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MaintenanceRefusal as Refusal;
use hostkeeper_proto::hostkeeper::operator::v1::MaintenanceState;
use hostkeeper_wire::Platform;

use crate::holds::{HoldMark, HoldScope};
use crate::host_groups::GroupName;
use crate::host_groups_support::{REASON, UNIX, World};
use crate::maintenance_calls_tests::{
    FIRST, GROUP, LINUX, LINUX_REBOOT, MACOS, MACOS_LINUX, NONE, alpha, edit_runs, fleet,
    group_request, hear, hosts_request, kept, member, run_of, schedule, skipped, waiting,
};
use crate::maintenance_effective::effective;
use crate::maintenance_runs::{Kind, Run, Skip};
use crate::maintenance_service::{MAX_SECS, host_proto, run_proto};
use crate::marks_support::{hold, scope_host};
use crate::operator::Caller;
use crate::ops::{Exclusion, OpsState};

/// A report that lists a reboot and then a Linux update twice.
const REBOOT_LINUX: &[MaintenanceKind] = &[
    MaintenanceKind::Reboot,
    MaintenanceKind::Linux,
    MaintenanceKind::Linux,
];

/// `request` with its `kinds` set to `kinds`.
fn naming(
    mut request: proto::ScheduleMaintenanceRequest,
    kinds: &[MaintenanceKind],
) -> proto::ScheduleMaintenanceRequest {
    request.kinds = kinds.iter().map(|kind| *kind as i32).collect();
    request
}

/// `request` that is only asked about: not confirmed.
fn asking(mut request: proto::ScheduleMaintenanceRequest) -> proto::ScheduleMaintenanceRequest {
    request.confirm = false;
    request
}

/// The group request of [`GROUP`] after `edit`.
fn changed(
    edit: impl FnOnce(&mut proto::ScheduleMaintenanceRequest),
) -> proto::ScheduleMaintenanceRequest {
    let mut request = group_request(GROUP);
    edit(&mut request);
    request
}

/// Makes `change` to the ops state the store holds, as another release's decisions leave it.
fn seed(world: &World, change: impl FnOnce(&mut OpsState)) {
    world
        .coordinator()
        .ops()
        .change(
            crate::ops::Write::BeforeMemory,
            crate::ops::AuditEntry::new("test", "seed"),
            change,
        )
        .expect("seeded");
}

/// Proves: a `Schedule` of a group with two hosts that have something due and one with nothing
/// due stores one run with exactly two host rows (each waiting, since 0, its group, and the kinds
/// its report lists due), none for the third, and answers the run as stored: its id, `running`,
/// two hosts, and the run's fields as the request gave them.
///
/// Catches: a run that holds a row for a host with nothing due, or rows whose kinds are not the
/// host's own.
#[test]
fn a_group_run_has_a_row_for_each_host_with_something_due() {
    let world = alpha(
        "ms-group-rows",
        &[("d1", MACOS), ("d2", LINUX_REBOOT), ("d3", NONE)],
    );

    let response = schedule(&world, group_request(GROUP)).expect("scheduled");

    let rows = vec![
        waiting("d1", GROUP, &[Kind::Macos]),
        waiting("d2", GROUP, &[Kind::Linux, Kind::Reboot]),
    ];
    let stored = world.ops().maintenance_runs;
    assert_eq!(stored, vec![run_of(FIRST, rows.clone())]);
    assert_eq!(
        response.hosts,
        rows.iter().map(host_proto).collect::<Vec<_>>()
    );
    let run = response.run.expect("a run");
    assert_eq!(run, run_proto(&stored[0], UNIX));
    assert_eq!(run.id, FIRST);
    assert_eq!(run.state(), MaintenanceState::Running);
    assert_eq!(run.host_count, 2);
    assert_eq!(run.host_group, GROUP);
    assert_eq!(run.scheduled_unix, UNIX);
    assert_eq!(run.by_name, "operator");
    assert_eq!(run.reason, REASON);
    assert!(response.entry.is_some());
}

/// Proves: a `Schedule` of hosts keeps the ids as named in the run and puts one row for each
/// host that has something due, by host id, each in the group it is enrolled in; a host named
/// with nothing due has no row; the run names no group.
///
/// Catches: ids that are sorted or dropped in the run's `host_ids`, or rows in the order named.
#[test]
fn a_run_of_hosts_keeps_the_ids_as_named_and_its_rows_by_id() {
    let world = alpha(
        "ms-hosts-rows",
        &[("d1", MACOS), ("d2", LINUX), ("d3", NONE)],
    );

    let response = schedule(&world, hosts_request(&["d2", "d1", "d3"])).expect("scheduled");

    assert_eq!(
        world.ops().maintenance_runs,
        vec![Run {
            host_ids: vec!["d2".to_owned(), "d1".to_owned(), "d3".to_owned()],
            host_group: String::new(),
            ..run_of(
                FIRST,
                vec![
                    waiting("d1", GROUP, &[Kind::Macos]),
                    waiting("d2", GROUP, &[Kind::Linux]),
                ]
            )
        }]
    );
    assert_eq!(response.hosts.len(), 2);
}

/// Proves: a report that lists a kind twice, or in another order, gives the kind once and in the
/// order of the kinds; a `Schedule` that names kinds takes the due kinds of those alone, and a
/// host with none of them due is not in the run; when no host has one the run is `nothing_due`.
///
/// Catches: a row whose kinds repeat, or a run of kinds the request did not ask for.
#[test]
fn the_kinds_of_a_row_are_the_due_kinds_once_and_of_those_asked() {
    let world = alpha(
        "ms-kinds",
        &[("d1", LINUX), ("d2", REBOOT_LINUX), ("d3", NONE)],
    );
    let before = kept(&world);

    let all = schedule(&world, asking(group_request(GROUP))).expect("asked");
    assert_eq!(
        all.hosts,
        vec![
            host_proto(&waiting("d1", GROUP, &[Kind::Linux])),
            host_proto(&waiting("d2", GROUP, &[Kind::Linux, Kind::Reboot])),
        ]
    );
    let reboot = schedule(
        &world,
        asking(naming(group_request(GROUP), &[MaintenanceKind::Reboot])),
    )
    .expect("asked");
    assert_eq!(
        reboot.hosts,
        vec![host_proto(&waiting("d2", GROUP, &[Kind::Reboot]))]
    );
    let macos = naming(group_request(GROUP), &[MaintenanceKind::Macos]);
    assert_eq!(
        schedule(&world, macos).expect_err("nothing of macOS is due"),
        Refusal::NothingDue
    );
    assert_eq!(kept(&world), before);
}

/// Proves: the Update button, a `Schedule` that names no kind, no window, no batch and no drain
/// deadline, takes the same hosts with the same kinds as a `Schedule` that names every due kind;
/// it stores a running run of no kinds and window 0 whose batch and drain deadline are absent,
/// which means each group's, and the batch that applies is the group's (3, as the group's policy
/// says).
///
/// Catches: an Update button that stores a batch or drain deadline of its own, or one that takes
/// other hosts than naming the kinds does.
#[test]
fn the_update_button_is_a_schedule_of_every_due_kind_with_the_groups_batch() {
    let world = alpha(
        "ms-update-button",
        &[
            ("d1", MACOS),
            ("d2", LINUX_REBOOT),
            ("d3", MACOS_LINUX),
            ("d4", NONE),
        ],
    );
    let every = [
        MaintenanceKind::Macos,
        MaintenanceKind::Linux,
        MaintenanceKind::Reboot,
    ];
    let named = schedule(&world, asking(naming(group_request(GROUP), &every))).expect("asked");
    assert_eq!(named.run.expect("a run").id, "");

    let button = schedule(&world, group_request(GROUP)).expect("scheduled");

    assert_eq!(button.hosts, named.hosts);
    assert_eq!(button.hosts.len(), 3);
    let stored = world.ops().maintenance_runs;
    assert_eq!(stored.len(), 1);
    let run = &stored[0];
    assert_eq!(run.kinds, Vec::<Kind>::new());
    assert_eq!(
        (run.batch, run.drain_deadline_secs),
        (None, None),
        "absent: each group's"
    );
    assert_eq!((run.window_start_unix, run.window_end_unix), (0, 0));
    assert!(!run.take_last_host);
    let shown = button.run.expect("a run");
    assert_eq!(shown.state(), MaintenanceState::Running);
    assert_eq!((shown.batch, shown.drain_deadline_secs), (None, None));
    assert_eq!(shown.kinds, Vec::<i32>::new());
    let name: GroupName = GROUP.parse().expect("a group name");
    let group = world
        .coordinator()
        .desired
        .host_group(&name)
        .expect("the group");
    assert_eq!(effective(&group, Platform::LinuxX86_64).batch, 3);
}

/// Proves: a window, a batch and a drain deadline of 0 are stored as given (the run is
/// `scheduled` while its window has not opened and ends at no time yet), the largest batch 10000
/// and the smallest 1 are taken, and a call that is only asked about answers the same run with
/// no id.
///
/// Catches: a batch or drain deadline that is replaced by the group's at schedule time, or a
/// window that is dropped.
#[test]
fn a_window_batch_and_drain_deadline_are_stored_as_given() {
    let world = alpha("ms-given", &[("d1", MACOS), ("d2", NONE)]);
    for batch in [1, 10_000] {
        let asked = changed(|r| r.batch = Some(batch));
        schedule(&world, asking(asked)).expect("the bounds are taken");
    }

    let response = schedule(
        &world,
        changed(|r| {
            r.window_start_unix = UNIX + 100;
            r.window_end_unix = UNIX + 1_000;
            r.batch = Some(5);
            r.drain_deadline_secs = Some(0);
        }),
    )
    .expect("scheduled");

    assert_eq!(
        world.ops().maintenance_runs,
        vec![Run {
            window_start_unix: UNIX + 100,
            window_end_unix: UNIX + 1_000,
            batch: Some(5),
            drain_deadline_secs: Some(0),
            ..run_of(FIRST, vec![waiting("d1", GROUP, &[Kind::Macos])])
        }]
    );
    let run = response.run.expect("a run");
    assert_eq!(run.state(), MaintenanceState::Scheduled);
    assert_eq!(run.ended_unix, 0);
    assert_eq!(
        (run.window_start_unix, run.window_end_unix),
        (UNIX + 100, UNIX + 1_000)
    );
    assert_eq!((run.batch, run.drain_deadline_secs), (Some(5), Some(0)));
}

/// Proves: a group of one host is refused `last_host` without `take_last_host` and stored with
/// it (the run says it took the last host); a group of three whose three hosts are due is refused
/// `last_host` by group and by naming the three hosts, and asking with `take_last_host` is
/// answered; a group of three with two due is taken; each refusal changes nothing.
///
/// Catches: a run that takes every host of a group without being told it may.
#[test]
fn a_run_may_take_a_groups_last_host_only_when_told() {
    let world = World::new("ms-last-host");
    fleet(&world, "solo", &[("d1", MACOS)]);
    fleet(
        &world,
        "trio",
        &[("d2", MACOS), ("d3", LINUX), ("d4", LINUX_REBOOT)],
    );
    fleet(
        &world,
        "pair",
        &[("d5", MACOS), ("d6", MACOS), ("d7", NONE)],
    );
    let before = kept(&world);

    assert_eq!(
        schedule(&world, group_request("solo")).expect_err("the last host"),
        Refusal::LastHost
    );
    assert_eq!(
        schedule(&world, group_request("trio")).expect_err("every host"),
        Refusal::LastHost
    );
    assert_eq!(
        schedule(&world, hosts_request(&["d2", "d3", "d4"])).expect_err("every host"),
        Refusal::LastHost
    );
    assert_eq!(kept(&world), before);
    let asked = proto::ScheduleMaintenanceRequest {
        take_last_host: true,
        ..asking(group_request("trio"))
    };
    schedule(&world, asked).expect("asked, with the last host");
    assert_eq!(kept(&world), before);

    schedule(&world, group_request("pair")).expect("two of three");
    let solo = proto::ScheduleMaintenanceRequest {
        take_last_host: true,
        ..group_request("solo")
    };
    let response = schedule(&world, solo).expect("the last host, told");

    let stored = world.ops().maintenance_runs;
    assert_eq!(stored.len(), 2);
    assert!(!stored[0].take_last_host);
    assert!(stored[1].take_last_host);
    assert_eq!(stored[1].host_group, "solo");
    assert!(response.run.expect("a run").take_last_host);
}

/// Proves: a host whose daemon does not announce `maintenance-v1` is in the run, skipped
/// `no_maintenance_v1`, with the kinds the request named (none when it named none) and since 0; a
/// host the coordinator never heard is not in the run; a host with nothing due is not either;
/// the host that is due waits.
///
/// Catches: an old daemon that gets an order it cannot read, or one that is silently left out of
/// the run so that the operator is not told.
#[test]
fn a_daemon_without_maintenance_v1_is_skipped_and_an_unheard_host_is_not_in_the_run() {
    let world = World::new("ms-no-v1");
    fleet(&world, GROUP, &[("d1", MACOS), ("d4", NONE)]);
    member(&world, GROUP, "d2");
    hear(&world, "d2", false, NONE);
    member(&world, GROUP, "d3");

    let named = naming(group_request(GROUP), &[MaintenanceKind::Macos]);
    let response = schedule(&world, named).expect("scheduled");

    let rows = vec![
        waiting("d1", GROUP, &[Kind::Macos]),
        skipped("d2", GROUP, &[Kind::Macos], Skip::NoMaintenanceV1),
    ];
    assert_eq!(
        world.ops().maintenance_runs,
        vec![Run {
            kinds: vec![Kind::Macos],
            ..run_of(FIRST, rows.clone())
        }]
    );
    assert_eq!(
        response.hosts,
        rows.iter().map(host_proto).collect::<Vec<_>>()
    );

    let world = World::new("ms-no-v1-unnamed");
    fleet(&world, GROUP, &[("d1", MACOS), ("d4", NONE)]);
    member(&world, GROUP, "d2");
    hear(&world, "d2", false, NONE);
    let asked = schedule(&world, asking(group_request(GROUP))).expect("asked");
    assert_eq!(
        asked.hosts,
        vec![
            host_proto(&waiting("d1", GROUP, &[Kind::Macos])),
            host_proto(&skipped("d2", GROUP, &[], Skip::NoMaintenanceV1)),
        ]
    );
}

/// Proves: a host a hold stands on (a `Services.Hold` of one host, or a hold of a host recorded
/// on another package) is skipped `held`, and one excluded from a package skipped `excluded`,
/// each for any package; a host both held and excluded is `held`; the other hosts wait.
///
/// Catches: a held or excluded host that is ordered, or one left out without the reason.
#[test]
fn a_held_or_excluded_host_is_skipped_with_its_reason() {
    let world = alpha(
        "ms-held-excluded",
        &[
            ("d1", MACOS),
            ("d2", MACOS),
            ("d3", MACOS),
            ("d4", MACOS),
            ("d5", MACOS),
            ("d6", NONE),
        ],
    );
    hold(&world.rig, "web", scope_host("d2")).expect("held");
    seed(&world, |state| {
        for node in ["d2", "d3"] {
            state.excluded.push(Exclusion {
                service: "db".to_owned(),
                node_id: node.to_owned(),
                reason: "broken".to_owned(),
            });
        }
        state.holds.push(HoldMark {
            service: "db".to_owned(),
            scope: HoldScope::Host("d4".to_owned()),
            reason: "looking".to_owned(),
            by: "operator".to_owned(),
            at_unix: UNIX,
        });
    });

    schedule(&world, group_request(GROUP)).expect("scheduled");

    let kinds = [Kind::Macos];
    assert_eq!(
        world.ops().maintenance_runs,
        vec![run_of(
            FIRST,
            vec![
                waiting("d1", GROUP, &kinds),
                skipped("d2", GROUP, &kinds, Skip::Held),
                skipped("d3", GROUP, &kinds, Skip::Excluded),
                skipped("d4", GROUP, &kinds, Skip::Held),
                waiting("d5", GROUP, &kinds),
            ]
        )]
    );
}

/// Proves: a hold of a group's name covers a host in that group and a hold of a role covers a
/// host in no group of that role, each skipped `held`; a call whose every due host is skipped is
/// `nothing_due` and changes nothing.
///
/// Catches: a role hold read against a host that is in a group, or a run with only skipped
/// hosts.
#[test]
fn a_hold_by_group_or_role_skips_and_a_run_of_skipped_hosts_has_nothing_due() {
    let world = World::new("ms-held-scopes");
    // d9 is heard before any group exists, so its role is the certificate's, `storage`; a host
    // heard once the coordinator holds a group has the name of its group as its role.
    world.enroll("d9");
    hear(&world, "d9", true, MACOS);
    fleet(&world, GROUP, &[("d1", MACOS), ("d2", NONE)]);
    let hold_of = |scope: HoldScope| HoldMark {
        service: "db".to_owned(),
        scope,
        reason: "looking".to_owned(),
        by: "operator".to_owned(),
        at_unix: UNIX,
    };
    seed(&world, |state| {
        state
            .holds
            .push(hold_of(HoldScope::Role("storage".to_owned())));
    });
    let before = kept(&world);
    assert_eq!(
        schedule(&world, hosts_request(&["d9"])).expect_err("only skipped"),
        Refusal::NothingDue
    );
    let grouped = schedule(&world, asking(hosts_request(&["d1"]))).expect("asked");
    assert_eq!(
        grouped.hosts,
        vec![host_proto(&waiting("d1", GROUP, &[Kind::Macos]))],
        "a host in a group is not covered by its role"
    );
    assert_eq!(kept(&world), before);

    seed(&world, |state| {
        state.holds.push(hold_of(HoldScope::Role(GROUP.to_owned())));
    });
    let before = kept(&world);
    assert_eq!(
        schedule(&world, group_request(GROUP)).expect_err("only skipped"),
        Refusal::NothingDue
    );
    assert_eq!(kept(&world), before);
}

/// Proves: each rule of the request is `invalid_request` and changes nothing and writes no
/// audit entry: a kind unspecified, unknown, `repair` (the coordinator's own) or named twice; both or neither of the hosts and the
/// group; 33 host ids, a host id twice, a host id that is not one; a group name that is not one;
/// a window start without an end, an end equal to or before its start, an end or a drain
/// deadline above the largest time a database keeps (2^63 - 1 seconds); a batch of 0 or 10001; a
/// reason that is missing, has a control character or is over 256 bytes when the call is
/// confirmed, and one with a control character when it is not; the rules do not refuse 32 host
/// ids (they are not enrolled), a missing reason when only asking, or the batches 1 and 10000.
///
/// Catches: a request that breaks a rule and is read all the same, or a rule that refuses a
/// request at its bound.
#[test]
fn each_rule_of_the_request_is_invalid_request() {
    let world = alpha("ms-invalid", &[("d1", MACOS), ("d2", MACOS), ("d3", NONE)]);
    let before = kept(&world);
    let ids = |count: usize| -> Vec<String> { (1..=count).map(|n| format!("x{n}")).collect() };
    let cases = [
        ("kind unspecified", changed(|r| r.kinds = vec![0])),
        ("kind unknown", changed(|r| r.kinds = vec![99])),
        (
            "kind repair, the coordinator's own",
            changed(|r| r.kinds = vec![MaintenanceKind::Repair as i32]),
        ),
        ("kind twice", changed(|r| r.kinds = vec![1, 3, 1])),
        ("neither target", changed(|r| r.host_group.clear())),
        (
            "both targets",
            changed(|r| r.host_ids = vec!["d1".to_owned()]),
        ),
        (
            "33 host ids",
            proto::ScheduleMaintenanceRequest {
                host_group: String::new(),
                host_ids: ids(33),
                ..group_request(GROUP)
            },
        ),
        ("a host id twice", hosts_request(&["d1", "d2", "d1"])),
        ("a bad host id", hosts_request(&["d1", "not a host"])),
        ("a bad group name", group_request("Bad Name")),
        (
            "start without end",
            changed(|r| r.window_start_unix = UNIX + 10),
        ),
        (
            "end equal to start",
            changed(|r| {
                r.window_start_unix = UNIX + 10;
                r.window_end_unix = UNIX + 10;
            }),
        ),
        (
            "end before start",
            changed(|r| {
                r.window_start_unix = UNIX + 10;
                r.window_end_unix = UNIX + 5;
            }),
        ),
        (
            "end above the largest time",
            changed(|r| r.window_end_unix = MAX_SECS + 1),
        ),
        (
            "drain deadline above the largest time",
            changed(|r| r.drain_deadline_secs = Some(MAX_SECS + 1)),
        ),
        ("batch 0", changed(|r| r.batch = Some(0))),
        ("batch 10001", changed(|r| r.batch = Some(10_001))),
        ("no reason", changed(|r| r.reason.clear())),
        ("a blank reason", changed(|r| r.reason = "  ".to_owned())),
        (
            "a control character",
            changed(|r| r.reason = "a\u{7}b".to_owned()),
        ),
        ("a long reason", changed(|r| r.reason = "x".repeat(257))),
        (
            "a control character, asked",
            asking(changed(|r| r.reason = "a\u{7}b".to_owned())),
        ),
    ];
    for (label, request) in cases {
        assert_eq!(
            schedule(&world, request).expect_err(label),
            Refusal::InvalidRequest,
            "{label}"
        );
    }
    assert_eq!(kept(&world), before);

    let thirty_two = proto::ScheduleMaintenanceRequest {
        host_group: String::new(),
        host_ids: ids(32),
        ..group_request(GROUP)
    };
    assert_eq!(
        schedule(&world, thirty_two).expect_err("not enrolled"),
        Refusal::UnknownHost
    );
    let asked = asking(changed(|r| r.reason.clear()));
    assert_eq!(
        schedule(&world, asked)
            .expect("asked")
            .run
            .expect("a run")
            .reason,
        ""
    );
    assert_eq!(kept(&world), before);
}

/// Proves: CI's token is `forbidden` and none `unauthorized`; a window that ended (its end is
/// now or before) is `window_ended`; a group no one made is `unknown_group`; a host not enrolled
/// is `unknown_host`, wherever it is in the list; a group whose hosts have nothing due, a group
/// with no host and a call that names kinds no host has due are `nothing_due`; each changes
/// nothing and writes no audit entry.
///
/// Catches: a refusal that leaves a run behind, or one answered for the wrong reason because
/// rules are checked in another order.
#[test]
fn the_other_refusals_of_a_schedule_change_nothing() {
    let world = alpha("ms-refusals", &[("d1", MACOS), ("d2", MACOS), ("d3", NONE)]);
    fleet(&world, "idle", &[("d4", NONE), ("d5", NONE)]);
    world
        .put_with("empty", &[crate::host_groups_support::SELF], None, None)
        .expect("put");
    let before = kept(&world);
    let coordinator = world.coordinator();
    assert_eq!(
        coordinator
            .maintenance_schedule(&Caller::Ci, group_request(GROUP))
            .expect_err("refused"),
        Refusal::Forbidden
    );
    assert_eq!(
        coordinator
            .maintenance_schedule(&Caller::Anonymous, group_request(GROUP))
            .expect_err("refused"),
        Refusal::Unauthorized
    );
    assert_eq!(
        coordinator
            .maintenance_schedule(&Caller::Anonymous, group_request("Bad Name"))
            .expect_err("refused"),
        Refusal::Unauthorized,
        "the caller is read before the request"
    );
    for (start, end) in [(0, UNIX), (0, UNIX - 1), (UNIX - 100, UNIX - 1)] {
        let ended = changed(|r| {
            r.window_start_unix = start;
            r.window_end_unix = end;
        });
        assert_eq!(
            schedule(&world, ended).expect_err("refused"),
            Refusal::WindowEnded,
            "{start} {end}"
        );
    }
    assert_eq!(
        schedule(&world, group_request("beta")).expect_err("refused"),
        Refusal::UnknownGroup
    );
    let lists: [&[&str]; 3] = [&["nobody"], &["d1", "nobody"], &["nobody", "d1"]];
    for ids in lists {
        assert_eq!(
            schedule(&world, hosts_request(ids)).expect_err("refused"),
            Refusal::UnknownHost,
            "{ids:?}"
        );
    }
    for group in ["idle", "empty"] {
        assert_eq!(
            schedule(&world, group_request(group)).expect_err("refused"),
            Refusal::NothingDue,
            "{group}"
        );
    }
    let linux_only = naming(group_request(GROUP), &[MaintenanceKind::Linux]);
    assert_eq!(
        schedule(&world, linux_only).expect_err("refused"),
        Refusal::NothingDue
    );
    assert_eq!(kept(&world), before);
}

/// Proves: a host that waits in an earlier run that has not ended cannot be in another:
/// `host_in_maintenance` for a run of the host, a run of its group, and nothing is changed; a
/// host the earlier run left out (skipped) is free; once the earlier run ended (its window ended
/// with no host in its turn) the host can be taken again.
///
/// Catches: two runs that order the same host.
#[test]
fn a_host_waiting_in_an_earlier_run_is_in_maintenance() {
    let world = alpha(
        "ms-in-maintenance",
        &[("d1", MACOS), ("d2", MACOS), ("d3", NONE), ("d4", NONE)],
    );
    hold(&world.rig, "web", scope_host("d2")).expect("held");
    schedule(&world, group_request(GROUP)).expect("scheduled");
    let before = kept(&world);

    assert_eq!(
        schedule(&world, hosts_request(&["d1"])).expect_err("refused"),
        Refusal::HostInMaintenance
    );
    assert_eq!(
        schedule(&world, group_request(GROUP)).expect_err("refused"),
        Refusal::HostInMaintenance
    );
    assert_eq!(kept(&world), before);
    assert_eq!(
        schedule(&world, hosts_request(&["d2"])).expect_err("skipped before, held now"),
        Refusal::NothingDue,
        "d2 is held: skipped, so it waits for no turn"
    );

    edit_runs(&world, |held| held[0].window_end_unix = UNIX - 1);
    let again = schedule(&world, group_request(GROUP)).expect("its window ended");
    assert_eq!(again.run.expect("a run").id, "run-1900000000-2");
}

/// Proves: a `Schedule` without `confirm` answers the run and its hosts as a confirmed one would
/// (state, host count, reason, kinds) with an empty id and no audit entry, and stores nothing,
/// commits nothing and writes no audit entry; the same call confirmed afterwards stores the first
/// id.
///
/// Catches: a call that is only asked about that stores a run, uses an id, or audits.
#[test]
fn a_schedule_without_confirm_stores_nothing() {
    let world = alpha(
        "ms-unconfirmed",
        &[("d1", MACOS), ("d2", LINUX), ("d3", NONE)],
    );
    let before = kept(&world);
    let commits = world.fake.commits();

    let asked = schedule(&world, asking(group_request(GROUP))).expect("asked");

    let run = asked.run.expect("a run");
    assert_eq!(run.id, "");
    assert_eq!(run.state(), MaintenanceState::Running);
    assert_eq!(run.host_count, 2);
    assert_eq!(run.reason, REASON);
    assert_eq!(asked.hosts.len(), 2);
    assert_eq!(asked.entry, None);
    assert_eq!(kept(&world), before);
    assert_eq!(world.fake.commits(), commits);
    assert_eq!(world.ops().maintenance_runs, Vec::<Run>::new());
    let stored = schedule(&world, group_request(GROUP)).expect("scheduled");
    assert_eq!(stored.run.expect("a run").id, FIRST);
    assert_eq!(stored.hosts, asked.hosts);
}

/// Proves: when the enrollment store cannot be read the call is `store_failed` and changes
/// nothing: reading a group's hosts, reading a named host, and reading a group's hosts again to
/// see whether the run takes its last host (every read from the first up to those of the second
/// fails the call, and the call after them all is `last_host`).
///
/// Catches: a failed read taken for a host that is not there, or for a group with room to spare.
#[test]
fn an_enrollment_store_that_fails_is_store_failed() {
    let world = World::new("ms-records-fail");
    fleet(&world, "solo", &[("d1", MACOS)]);
    let before = kept(&world);
    let mut answers = Vec::new();
    for step in 1..=12 {
        world.records.fail_at(world.records.count() + step);
        answers.push(schedule(&world, group_request("solo")).expect_err("refused"));
        world.records.fail_at(0);
    }
    let failed = answers
        .iter()
        .take_while(|answer| **answer == Refusal::StoreFailed)
        .count();
    assert!(failed >= 4, "{answers:?}");
    assert_eq!(failed % 2, 0, "the same reads twice: {answers:?}");
    assert!(
        answers[failed..]
            .iter()
            .all(|answer| *answer == Refusal::LastHost),
        "{answers:?}"
    );
    assert!(failed < answers.len(), "{answers:?}");

    world.records.fail_at(world.records.count() + 1);
    assert_eq!(
        schedule(&world, hosts_request(&["d1"])).expect_err("refused"),
        Refusal::StoreFailed
    );
    world.records.fail_at(0);
    assert_eq!(kept(&world), before);
}

/// Proves: a host whose record names a group that is not a group name (a record another release
/// wrote) is in a run of hosts with that name in its row, and does not make it a last host.
///
/// Catches: a run refused or a crash on a record whose group cannot be named.
#[test]
fn a_host_in_a_group_that_is_not_a_name_is_taken_as_recorded() {
    let world = alpha("ms-odd-group", &[("d1", MACOS)]);
    let id = hostkeeper_enrollment::names::HostId::parse("d1").expect("a host id");
    world
        .enrollment
        .enrollment
        .store()
        .set_membership(
            &id,
            Membership {
                host_group: "Not A Group".to_owned(),
                moving_from: None,
            },
        )
        .expect("recorded");

    let response = schedule(&world, hosts_request(&["d1"])).expect("scheduled");

    assert_eq!(
        response.hosts,
        vec![host_proto(&waiting("d1", "Not A Group", &[Kind::Macos]))]
    );
    assert_eq!(world.ops().maintenance_runs.len(), 1);
}

/// Proves: a window end and a drain deadline of exactly the largest time a database keeps
/// (`i64::MAX` seconds) are taken: asked about, they are answered as given, and confirmed, they
/// are stored as given; one second more of either is `invalid_request` and changes nothing.
///
/// Catches: a bound that refuses its own largest value, or a value a database cannot keep that
/// is stored.
#[test]
fn the_largest_time_is_taken_and_one_more_second_is_not() {
    let world = alpha("ms-max-secs", &[("d1", MACOS), ("d2", MACOS), ("d3", NONE)]);
    let before = kept(&world);
    let largest = changed(|r| {
        r.window_end_unix = MAX_SECS;
        r.drain_deadline_secs = Some(MAX_SECS);
    });

    let asked = schedule(&world, asking(largest.clone())).expect("asked at the largest");
    let run = asked.run.expect("a run");
    assert_eq!(run.window_end_unix, MAX_SECS);
    assert_eq!(run.drain_deadline_secs, Some(MAX_SECS));
    assert_eq!(kept(&world), before);
    for over in [
        changed(|r| r.window_end_unix = MAX_SECS + 1),
        changed(|r| r.drain_deadline_secs = Some(MAX_SECS + 1)),
    ] {
        assert_eq!(
            schedule(&world, over).expect_err("one second over"),
            Refusal::InvalidRequest
        );
    }
    assert_eq!(kept(&world), before);

    schedule(&world, largest).expect("scheduled at the largest");
    let stored = world.ops().maintenance_runs;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].window_end_unix, MAX_SECS);
    assert_eq!(stored[0].drain_deadline_secs, Some(MAX_SECS));
}

/// Proves: a host that waits in a run that has not ended is `host_in_maintenance` for a
/// `Schedule` that is only asked about as well as for a confirmed one, which checks it where it
/// stores the run; neither stores a run, commits or audits, and the host's run is as it was.
///
/// Catches: a preview that promises a run a confirm would refuse, or a confirm that stores a
/// second run for a host already waiting in one.
#[test]
fn a_host_waiting_in_another_run_is_refused_asked_or_confirmed() {
    let world = alpha(
        "ms-in-maintenance-both",
        &[("d1", MACOS), ("d2", MACOS), ("d3", NONE), ("d4", NONE)],
    );
    schedule(&world, hosts_request(&["d1"])).expect("scheduled");
    let before = kept(&world);
    let commits = world.fake.commits();
    let runs = world.ops().maintenance_runs;

    assert_eq!(
        schedule(&world, asking(hosts_request(&["d1"]))).expect_err("asked"),
        Refusal::HostInMaintenance
    );
    assert_eq!(
        schedule(&world, hosts_request(&["d1"])).expect_err("confirmed"),
        Refusal::HostInMaintenance
    );
    assert_eq!(
        schedule(&world, asking(hosts_request(&["d1", "d2"]))).expect_err("asked, with d2"),
        Refusal::HostInMaintenance
    );
    assert_eq!(
        schedule(&world, hosts_request(&["d1", "d2"])).expect_err("confirmed, with d2"),
        Refusal::HostInMaintenance
    );
    assert_eq!(kept(&world), before);
    assert_eq!(world.fake.commits(), commits);
    assert_eq!(world.ops().maintenance_runs, runs);
    schedule(&world, hosts_request(&["d2"])).expect("d2 waits in no run");
}

/// Proves: a request that takes every host of a group and names a host that waits in another run
/// is answered `last_host`, asked or confirmed, and with `take_last_host` the same request is
/// answered `host_in_maintenance`, asked or confirmed: the last host of a group is checked
/// before the hosts in maintenance; nothing is stored by any of them.
///
/// Catches: the two refusals answered in the other order.
#[test]
fn the_last_host_is_refused_before_a_host_in_maintenance() {
    let world = World::new("ms-last-before-in-maintenance");
    fleet(&world, "pair", &[("d1", MACOS), ("d2", MACOS)]);
    let taking = proto::ScheduleMaintenanceRequest {
        take_last_host: true,
        ..group_request("pair")
    };
    schedule(&world, taking.clone()).expect("scheduled, taking the last host");
    let before = kept(&world);

    for request in [group_request("pair"), asking(group_request("pair"))] {
        assert_eq!(
            schedule(&world, request).expect_err("both"),
            Refusal::LastHost
        );
    }
    for request in [taking.clone(), asking(taking)] {
        assert_eq!(
            schedule(&world, request).expect_err("told, but in maintenance"),
            Refusal::HostInMaintenance
        );
    }
    assert_eq!(kept(&world), before);
    assert_eq!(world.ops().maintenance_runs.len(), 1);
}
