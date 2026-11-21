//! The `Maintenance` calls that read and end runs (`List`, `Get`, `Cancel`), the runs across a
//! restart and the audit entries of the changing calls ([`crate::maintenance_service`]). The
//! builders of a fleet that reports what is due, and of the requests, are shared with
//! `maintenance_service_tests.rs`.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MaintenanceRefusal as Refusal;
use hostkeeper_proto::hostkeeper::operator::v1::{
    MaintenanceHostStep, MaintenanceSkip, MaintenanceState,
};

use crate::host_groups_support::{REASON, SELF, UNIX, World, operator_entry};
use crate::maintenance::{FEATURE, text_of};
use crate::maintenance_runs::{Kind, Run, RunHost, Skip, Step};
use crate::maintenance_service::{host_proto, run_proto};
use crate::marks_support::{NEW_DAEMON, ask, beat_of, operator, runs};
use crate::operator::{Caller, entry_proto};
use crate::ops::{AuditEntry, Write};

/// The group the fleets of the tests are in.
pub(crate) const GROUP: &str = "alpha";

/// Nothing due.
pub(crate) const NONE: &[MaintenanceKind] = &[];
/// A macOS update due.
pub(crate) const MACOS: &[MaintenanceKind] = &[MaintenanceKind::Macos];
/// A Linux update due.
pub(crate) const LINUX: &[MaintenanceKind] = &[MaintenanceKind::Linux];
/// A Linux update and a reboot due.
pub(crate) const LINUX_REBOOT: &[MaintenanceKind] =
    &[MaintenanceKind::Linux, MaintenanceKind::Reboot];
/// A macOS and a Linux update due.
pub(crate) const MACOS_LINUX: &[MaintenanceKind] =
    &[MaintenanceKind::Macos, MaintenanceKind::Linux];

/// The id of the first run a world stores.
pub(crate) const FIRST: &str = "run-1900000000-1";

/// What a fleet is: the hosts of a group, each with what its report lists as due.
pub(crate) type Hosts<'a> = &'a [(&'a str, &'a [MaintenanceKind])];

/// The Mac facts of a Mac that may be ordered `macos`: no device-management conflict, an update
/// account that is present, an administrator and holds a Secure Token, and FileVault off.
pub(crate) fn clean_mac() -> agent::MacUpdateFacts {
    agent::MacUpdateFacts {
        filevault: agent::FileVaultState::Off as i32,
        update_account: Some(agent::UpdateAccountState {
            present: true,
            admin: true,
            secure_token: true,
            hidden: true,
        }),
        ..agent::MacUpdateFacts::default()
    }
}

/// [`clean_mac`] when `due` holds `macos`, as a Mac's report carries; none otherwise.
pub(crate) fn mac_of(due: &[MaintenanceKind]) -> Option<agent::MacUpdateFacts> {
    due.contains(&MaintenanceKind::Macos).then(clean_mac)
}

/// A heartbeat of `host` that lists one service and, when its daemon `announces` `maintenance-v1`,
/// carries a report that lists one due item for each of `due` (with [`mac_of`] `due`).
pub(crate) fn hear(world: &World, host: &str, announces: bool, due: &[MaintenanceKind]) {
    let mut features: Vec<&str> = NEW_DAEMON.to_vec();
    if announces {
        features.push(FEATURE);
    }
    let mut beat = beat_of(&features, vec![runs("web", "1.0")]);
    if announces {
        let report = agent::MaintenanceReport {
            due: due
                .iter()
                .map(|kind| agent::DueItem {
                    kind: *kind as i32,
                    label: "pkg".to_owned(),
                    ..agent::DueItem::default()
                })
                .collect(),
            mac: mac_of(due),
            ..agent::MaintenanceReport::default()
        };
        beat.maintenance = Some(text_of(&report));
    }
    ask(&world.rig, host, "storage", beat);
}

/// The host `host` enrolled and moved into the group `group`, not heard.
pub(crate) fn member(world: &World, group: &str, host: &str) {
    world.enroll(host);
    world.move_to(host, group).expect("moved");
}

/// The policy of a group whose maintenance batch is 3.
pub(crate) fn policy() -> proto::RolloutPolicy {
    proto::RolloutPolicy {
        maintenance: Some(proto::MaintenancePolicy {
            batch: Some(3),
            ..proto::MaintenancePolicy::default()
        }),
        ..proto::RolloutPolicy::default()
    }
}

/// The group `group` put, with the hosts `hosts` in it, each heard to announce `maintenance-v1`
/// and report what is due.
pub(crate) fn fleet(world: &World, group: &str, hosts: Hosts<'_>) {
    world
        .put_with(group, &[SELF], Some(policy()), None)
        .expect("put");
    for (host, due) in hosts {
        member(world, group, host);
        hear(world, host, true, due);
    }
}

/// A world named `name` with the group [`GROUP`] and its `hosts` ([`fleet`]).
pub(crate) fn alpha(name: &str, hosts: Hosts<'_>) -> World {
    let world = World::new(name);
    fleet(&world, GROUP, hosts);
    world
}

/// A `Schedule` of the group `group`, confirmed, with the test reason.
pub(crate) fn group_request(group: &str) -> proto::ScheduleMaintenanceRequest {
    proto::ScheduleMaintenanceRequest {
        host_group: group.to_owned(),
        reason: REASON.to_owned(),
        confirm: true,
        ..proto::ScheduleMaintenanceRequest::default()
    }
}

/// A `Schedule` of the hosts `ids`, confirmed, with the test reason.
pub(crate) fn hosts_request(ids: &[&str]) -> proto::ScheduleMaintenanceRequest {
    proto::ScheduleMaintenanceRequest {
        host_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
        reason: REASON.to_owned(),
        confirm: true,
        ..proto::ScheduleMaintenanceRequest::default()
    }
}

/// `request` made by the operator.
pub(crate) fn schedule(
    world: &World,
    request: proto::ScheduleMaintenanceRequest,
) -> Result<proto::ScheduleMaintenanceResponse, Refusal> {
    world
        .coordinator()
        .maintenance_schedule(&operator(), request)
}

/// `Get` of the run `id` by the operator.
pub(crate) fn get(world: &World, id: &str) -> Result<proto::GetMaintenanceRunResponse, Refusal> {
    world.coordinator().maintenance_get(
        &operator(),
        &proto::GetMaintenanceRunRequest {
            run_id: id.to_owned(),
        },
    )
}

/// `Cancel` of the run `id`, with `reason`, by the operator.
pub(crate) fn cancel(
    world: &World,
    id: &str,
    reason: &str,
) -> Result<proto::CancelMaintenanceRunResponse, Refusal> {
    world.coordinator().maintenance_cancel(
        &operator(),
        proto::CancelMaintenanceRunRequest {
            run_id: id.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

/// `List` by the operator of the runs in `states`, at most `limit`.
pub(crate) fn list(
    world: &World,
    states: &[i32],
    limit: u32,
) -> Result<Vec<proto::MaintenanceRun>, Refusal> {
    world
        .coordinator()
        .maintenance_list(
            &operator(),
            &proto::ListMaintenanceRunsRequest {
                states: states.to_vec(),
                limit,
            },
        )
        .map(|listed| listed.runs)
}

/// A host row at `step` since `since`, in the group `group` (empty: none), with `kinds`.
pub(crate) fn host_at(id: &str, group: &str, step: Step, since: u64, kinds: &[Kind]) -> RunHost {
    RunHost {
        host_id: id.to_owned(),
        host_group: group.to_owned(),
        step,
        since_unix: since,
        kinds: kinds.to_vec(),
        applied: Vec::new(),
        skipped_because: None,
        reason: String::new(),
        offered_again: 0,
        baked_beats: 0,
    }
}

/// A host row that waits, as a `Schedule` makes it.
pub(crate) fn waiting(id: &str, group: &str, kinds: &[Kind]) -> RunHost {
    host_at(id, group, Step::Waiting, 0, kinds)
}

/// A host row left out for `why`, as a `Schedule` makes it.
pub(crate) fn skipped(id: &str, group: &str, kinds: &[Kind], why: Skip) -> RunHost {
    RunHost {
        skipped_because: Some(why),
        ..host_at(id, group, Step::Skipped, 0, kinds)
    }
}

/// A run named `id`, scheduled at [`UNIX`] by the operator on the group [`GROUP`], with `hosts`.
pub(crate) fn run_of(id: &str, hosts: Vec<RunHost>) -> Run {
    Run {
        id: id.to_owned(),
        kinds: Vec::new(),
        host_ids: Vec::new(),
        host_group: GROUP.to_owned(),
        window_start_unix: 0,
        window_end_unix: 0,
        batch: None,
        drain_deadline_secs: None,
        take_last_host: false,
        reason: REASON.to_owned(),
        by: "operator".to_owned(),
        scheduled_unix: UNIX,
        cancelled_unix: None,
        hosts,
    }
}

/// Appends `runs` to the runs the store holds, as a later release's decisions leave them.
pub(crate) fn seed_runs(world: &World, runs: Vec<Run>) {
    edit_runs(world, |held| held.extend(runs));
}

/// Makes `change` to the runs the store holds.
pub(crate) fn edit_runs(world: &World, change: impl FnOnce(&mut Vec<Run>)) {
    world
        .coordinator()
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("test", "seed"),
            |state| change(&mut state.maintenance_runs),
        )
        .expect("seeded");
}

/// The state a refused call leaves, with the length of the audit.
pub(crate) fn kept(world: &World) -> (impl PartialEq + std::fmt::Debug, usize) {
    (world.state(), world.audit().len())
}

/// A world of the group [`GROUP`] with d1 (a macOS update due), d2 (a Linux update due) and d3
/// (nothing due), and the run a `Schedule` of the group stored there.
fn scheduled(name: &str) -> (World, String) {
    let world = alpha(name, &[("d1", MACOS), ("d2", LINUX), ("d3", NONE)]);
    let stored = schedule(&world, group_request(GROUP)).expect("scheduled");
    let id = stored.run.expect("a run").id;
    assert_eq!(id, FIRST);
    (world, id)
}

/// The row `host` of a run of the world [`scheduled`] makes, as `Get` says it, when the host
/// was not reached since `since`.
fn not_reached(host: &str, since: u64, kinds: &[MaintenanceKind]) -> proto::MaintenanceHost {
    proto::MaintenanceHost {
        host_id: host.to_owned(),
        host_group: GROUP.to_owned(),
        step: MaintenanceHostStep::NotReached.into(),
        since_unix: since,
        kinds: kinds.iter().map(|kind| i32::from(*kind)).collect(),
        applied: Vec::new(),
        skipped_because: MaintenanceSkip::Unspecified.into(),
        reason: String::new(),
    }
}

/// Proves: `Cancel` of a run no host has started ends it: the answer and `Get` say `cancelled`,
/// ended at the clock, with every host `not_reached` since then and its kinds kept; the audit
/// entry `maintenance_cancel` carries the reason followed by `run <id>`, and the hosts are free
/// to be in a new run.
///
/// Catches: a cancel that leaves a waiting host waiting, or that leaves its hosts in a turn that
/// has not ended so that no run can take them.
#[test]
fn cancel_of_a_run_nothing_started_ends_it_and_frees_its_hosts() {
    let (world, id) = scheduled("mc-cancel-none");
    let audited = world.audit().len() as u64;

    let cancelled = cancel(&world, &id, REASON).expect("cancelled");

    let run = cancelled.run.expect("a run");
    assert_eq!(run.id, id);
    assert_eq!(run.state(), MaintenanceState::Cancelled);
    assert_eq!(run.ended_unix, UNIX);
    assert_eq!(run.host_count, 2);
    let wanted = operator_entry(
        audited + 1,
        "maintenance_cancel",
        None,
        None,
        &format!("{REASON} run {id}"),
    );
    assert_eq!(cancelled.entry, Some(entry_proto(&wanted)));
    assert_eq!(world.audit().pop(), Some(wanted));
    let got = get(&world, &id).expect("got");
    assert_eq!(got.run, Some(run));
    assert_eq!(
        got.hosts,
        vec![
            not_reached("d1", UNIX, MACOS),
            not_reached("d2", UNIX, LINUX)
        ]
    );
    let again = schedule(&world, group_request(GROUP)).expect("its hosts are free");
    assert_eq!(again.run.expect("a run").id, "run-1900000000-2");
}

/// Proves: `Cancel` of a run in which one host is in its turn leaves that host as it is (still
/// draining, since when it was), ends the waiting one as `not_reached`, and answers `cancelled`;
/// the host in its turn is not free for another run, and the host that did not start is.
///
/// Catches: a cancel that aborts a host mid-turn or rewrites its step.
#[test]
fn cancel_leaves_a_host_in_its_turn_and_ends_the_waiting_ones() {
    let (world, id) = scheduled("mc-cancel-turn");
    edit_runs(&world, |held| {
        held[0].hosts[0].step = Step::Draining;
        held[0].hosts[0].since_unix = UNIX - 30;
    });

    let cancelled = cancel(&world, &id, REASON).expect("cancelled");

    assert_eq!(
        cancelled.run.expect("a run").state(),
        MaintenanceState::Cancelled
    );
    let hosts = get(&world, &id).expect("got").hosts;
    assert_eq!(
        hosts,
        vec![
            proto::MaintenanceHost {
                step: MaintenanceHostStep::Draining.into(),
                since_unix: UNIX - 30,
                ..not_reached("d1", 0, MACOS)
            },
            not_reached("d2", UNIX, LINUX),
        ]
    );
    assert_eq!(
        schedule(&world, hosts_request(&["d1"])).expect_err("still in its turn"),
        Refusal::HostInMaintenance
    );
    let free = schedule(&world, hosts_request(&["d2"])).expect("d2 did not start");
    assert_eq!(free.hosts.len(), 1);
}

/// Proves: every refusal of `Cancel` is its token and changes nothing: CI's token `forbidden`,
/// none `unauthorized`; an id that is not a run id (empty, with a space, 65 bytes) and a reason
/// that is empty, over 256 bytes or holds a control character `invalid_request`; an id no run
/// has `unknown_run`; a run already cancelled, one whose hosts are all done and one past its
/// window with no host in its turn `run_ended`; a store that fails `store_failed`, and one whose
/// audit line cannot be kept `audit_unavailable`.
///
/// Catches: a refusal that cancels the run all the same, or an ended run cancelled again with a
/// new time.
#[test]
fn every_refusal_of_a_cancel_changes_nothing() {
    let (world, id) = scheduled("mc-cancel-refusals");
    cancel(&world, &id, REASON).expect("cancelled once");
    seed_runs(
        &world,
        vec![
            run_of(
                "run-2-1",
                vec![host_at("d1", GROUP, Step::Done, UNIX - 9, &[Kind::Macos])],
            ),
            Run {
                window_end_unix: UNIX - 10,
                ..run_of(
                    "run-2-2",
                    vec![host_at("d1", GROUP, Step::Waiting, 0, &[Kind::Macos])],
                )
            },
            run_of(
                "run-2-3",
                vec![host_at("d1", GROUP, Step::Waiting, 0, &[Kind::Macos])],
            ),
        ],
    );
    let before = kept(&world);
    let coordinator = world.coordinator();
    let request = |run_id: &str, reason: &str| proto::CancelMaintenanceRunRequest {
        run_id: run_id.to_owned(),
        reason: reason.to_owned(),
    };
    assert_eq!(
        coordinator
            .maintenance_cancel(&Caller::Ci, request("run-2-3", REASON))
            .expect_err("refused"),
        Refusal::Forbidden
    );
    assert_eq!(
        coordinator
            .maintenance_cancel(&Caller::Anonymous, request("run-2-3", REASON))
            .expect_err("refused"),
        Refusal::Unauthorized
    );
    let long_id = "a".repeat(65);
    for bad in ["", "has space", long_id.as_str()] {
        assert_eq!(
            cancel(&world, bad, REASON).expect_err("refused"),
            Refusal::InvalidRequest,
            "{bad:?}"
        );
    }
    let long = "x".repeat(257);
    for reason in ["", "  ", long.as_str(), "a\u{7}b"] {
        assert_eq!(
            cancel(&world, "run-2-3", reason).expect_err("refused"),
            Refusal::InvalidRequest,
            "{reason:?}"
        );
    }
    assert_eq!(
        cancel(&world, "run-9-9", REASON).expect_err("refused"),
        Refusal::UnknownRun
    );
    for ended in [id.as_str(), "run-2-1", "run-2-2"] {
        assert_eq!(
            cancel(&world, ended, REASON).expect_err("refused"),
            Refusal::RunEnded,
            "{ended}"
        );
    }
    for (failure, refusal) in [
        (
            crate::store::store_failure as fn() -> _,
            Refusal::StoreFailed,
        ),
        (crate::store::audit_failure, Refusal::AuditUnavailable),
    ] {
        world.fake.refuse(move |_| Some(failure()));
        assert_eq!(
            cancel(&world, "run-2-3", REASON).expect_err("refused"),
            refusal
        );
    }
    world.fake.heal();
    assert_eq!(kept(&world), before);
    cancel(&world, "run-2-3", REASON).expect("a healthy store takes it");
}

/// Proves: `List` answers the newest run first with each run's state and end (running, scheduled
/// before its window opens, halted with a host halted, done, cancelled, window ended); `states`
/// keeps only the runs in them and `limit` takes the newest of those; an unspecified or unknown
/// state is `invalid_request`; CI's token is `forbidden` and none `unauthorized`.
///
/// Catches: a list in schedule order, a limit taken before the filter, or a state computed from
/// the stored fields without the clock.
#[test]
fn list_answers_newest_first_by_state_and_limit() {
    let world = World::new("mc-list");
    let one = |id: &str, step: Step, since: u64| {
        run_of(id, vec![host_at("d1", GROUP, step, since, &[Kind::Macos])])
    };
    seed_runs(
        &world,
        vec![
            one("run-1-1", Step::Waiting, 0),
            Run {
                cancelled_unix: Some(UNIX - 5),
                ..one("run-1-2", Step::NotReached, UNIX - 5)
            },
            one("run-1-3", Step::Done, UNIX - 50),
            Run {
                window_end_unix: UNIX - 1,
                ..one("run-1-4", Step::Waiting, 0)
            },
            Run {
                window_start_unix: UNIX + 500,
                window_end_unix: UNIX + 900,
                ..one("run-1-5", Step::Waiting, 0)
            },
            one("run-1-6", Step::Halted, UNIX - 2),
        ],
    );
    let before = kept(&world);
    let seen = |runs: Vec<proto::MaintenanceRun>| -> Vec<(String, MaintenanceState, u64)> {
        runs.into_iter()
            .map(|run| (run.id.clone(), run.state(), run.ended_unix))
            .collect()
    };
    let all = [
        ("run-1-6", MaintenanceState::Halted, 0),
        ("run-1-5", MaintenanceState::Scheduled, 0),
        ("run-1-4", MaintenanceState::WindowEnded, UNIX - 1),
        ("run-1-3", MaintenanceState::Done, UNIX - 50),
        ("run-1-2", MaintenanceState::Cancelled, UNIX - 5),
        ("run-1-1", MaintenanceState::Running, 0),
    ]
    .into_iter()
    .map(|(id, state, ended)| (id.to_owned(), state, ended))
    .collect::<Vec<_>>();
    assert_eq!(seen(list(&world, &[], 0).expect("listed")), all);
    let halted = list(&world, &[], 0).expect("listed");
    assert_eq!(halted[0].halted_groups, vec![GROUP.to_owned()]);
    assert_eq!(halted[0].host_count, 1);
    let states = |wanted: &[MaintenanceState], limit: u32| {
        let wanted: Vec<i32> = wanted.iter().map(|state| i32::from(*state)).collect();
        seen(list(&world, &wanted, limit).expect("listed"))
    };
    assert_eq!(states(&[MaintenanceState::Running], 0), all[5..].to_vec());
    assert_eq!(
        states(&[MaintenanceState::Cancelled, MaintenanceState::Done], 0),
        vec![all[3].clone(), all[4].clone()]
    );
    assert_eq!(
        states(
            &[
                MaintenanceState::Running,
                MaintenanceState::Cancelled,
                MaintenanceState::Done
            ],
            2
        ),
        vec![all[3].clone(), all[4].clone()]
    );
    assert_eq!(states(&[], 2), all[..2].to_vec());
    for bad in [vec![0], vec![99], vec![2, 0]] {
        assert_eq!(
            list(&world, &bad, 0).expect_err("refused"),
            Refusal::InvalidRequest,
            "{bad:?}"
        );
    }
    let empty = proto::ListMaintenanceRunsRequest::default();
    assert_eq!(
        world
            .coordinator()
            .maintenance_list(&Caller::Ci, &empty)
            .expect_err("refused"),
        Refusal::Forbidden
    );
    assert_eq!(
        world
            .coordinator()
            .maintenance_list(&Caller::Anonymous, &empty)
            .expect_err("refused"),
        Refusal::Unauthorized
    );
    assert_eq!(kept(&world), before);
}

/// Proves: `List` answers at most 256 runs, the newest, whether the limit is 0 or larger, and
/// as many as the limit when it is smaller.
///
/// Catches: a list that answers every run held, or a limit of 0 that answers none.
#[test]
fn list_is_capped_at_256_runs() {
    let world = World::new("mc-list-cap");
    seed_runs(
        &world,
        (1..=300)
            .map(|n| {
                run_of(
                    &format!("run-1-{n}"),
                    vec![host_at("d1", GROUP, Step::Waiting, 0, &[Kind::Macos])],
                )
            })
            .collect(),
    );
    for limit in [0, 1000, 256] {
        let runs = list(&world, &[], limit).expect("listed");
        assert_eq!(runs.len(), 256, "limit {limit}");
        assert_eq!(runs[0].id, "run-1-300");
        assert_eq!(runs[255].id, "run-1-45");
    }
    let few = list(&world, &[], 3).expect("listed");
    let ids: Vec<&str> = few.iter().map(|run| run.id.as_str()).collect();
    assert_eq!(ids, ["run-1-300", "run-1-299", "run-1-298"]);
}

/// Proves: `Get` answers the run with every field it holds (kinds, hosts named, window, batch
/// and drain deadline as given, whether it took a last host, the reason and who scheduled it)
/// and each host's row with its group, step, since, kinds, applied kinds, why it was left out
/// and its reason; an id no run has is `unknown_run`; an id that is not a run id (empty, a
/// space, 65 bytes) `invalid_request`; CI's token `forbidden` and none `unauthorized`.
///
/// Catches: a field of the run or a host row that is dropped on the way to the proto.
#[test]
fn get_answers_the_run_and_its_hosts() {
    let world = World::new("mc-get");
    seed_runs(
        &world,
        vec![
            run_of("run-3-1", vec![waiting("d9", "", &[Kind::Reboot])]),
            Run {
                kinds: vec![Kind::Linux, Kind::Reboot],
                host_ids: vec!["d1".to_owned(), "d2".to_owned()],
                host_group: String::new(),
                window_start_unix: UNIX - 100,
                window_end_unix: UNIX + 100,
                batch: Some(2),
                drain_deadline_secs: Some(30),
                take_last_host: true,
                reason: "why".to_owned(),
                by: "someone".to_owned(),
                scheduled_unix: UNIX - 200,
                ..run_of(
                    "run-3-2",
                    vec![
                        RunHost {
                            applied: vec![Kind::Linux],
                            reason: "needs a look".to_owned(),
                            ..host_at(
                                "d1",
                                "beta",
                                Step::Halted,
                                UNIX - 7,
                                &[Kind::Linux, Kind::Reboot],
                            )
                        },
                        skipped("d2", "", &[Kind::Reboot], Skip::Excluded),
                    ],
                )
            },
        ],
    );
    let before = kept(&world);

    let got = get(&world, "run-3-2").expect("got");

    assert_eq!(
        got.run,
        Some(proto::MaintenanceRun {
            id: "run-3-2".to_owned(),
            state: MaintenanceState::Halted.into(),
            kinds: vec![
                MaintenanceKind::Linux.into(),
                MaintenanceKind::Reboot.into()
            ],
            host_ids: vec!["d1".to_owned(), "d2".to_owned()],
            host_group: String::new(),
            window_start_unix: UNIX - 100,
            window_end_unix: UNIX + 100,
            batch: Some(2),
            drain_deadline_secs: Some(30),
            take_last_host: true,
            reason: "why".to_owned(),
            by_name: "someone".to_owned(),
            scheduled_unix: UNIX - 200,
            ended_unix: 0,
            halted_groups: vec!["beta".to_owned()],
            host_count: 2,
        })
    );
    assert_eq!(
        got.hosts,
        vec![
            proto::MaintenanceHost {
                host_id: "d1".to_owned(),
                host_group: "beta".to_owned(),
                step: MaintenanceHostStep::Halted.into(),
                since_unix: UNIX - 7,
                kinds: vec![
                    MaintenanceKind::Linux.into(),
                    MaintenanceKind::Reboot.into()
                ],
                applied: vec![MaintenanceKind::Linux.into()],
                skipped_because: MaintenanceSkip::Unspecified.into(),
                reason: "needs a look".to_owned(),
            },
            proto::MaintenanceHost {
                host_id: "d2".to_owned(),
                host_group: String::new(),
                step: MaintenanceHostStep::Skipped.into(),
                since_unix: 0,
                kinds: vec![MaintenanceKind::Reboot.into()],
                applied: Vec::new(),
                skipped_because: MaintenanceSkip::Excluded.into(),
                reason: String::new(),
            },
        ]
    );
    assert_eq!(
        get(&world, "run-3-1").expect("got").hosts,
        vec![host_proto(&waiting("d9", "", &[Kind::Reboot]))]
    );
    assert_eq!(
        get(&world, "run-9-9").expect_err("refused"),
        Refusal::UnknownRun
    );
    let long = "a".repeat(65);
    for bad in ["", "a b", long.as_str()] {
        assert_eq!(
            get(&world, bad).expect_err("refused"),
            Refusal::InvalidRequest,
            "{bad:?}"
        );
    }
    let one = proto::GetMaintenanceRunRequest {
        run_id: "run-3-1".to_owned(),
    };
    assert_eq!(
        world
            .coordinator()
            .maintenance_get(&Caller::Ci, &one)
            .expect_err("refused"),
        Refusal::Forbidden
    );
    assert_eq!(
        world
            .coordinator()
            .maintenance_get(&Caller::Anonymous, &one)
            .expect_err("refused"),
        Refusal::Unauthorized
    );
    assert_eq!(kept(&world), before);
}

/// Proves: runs survive a coordinator that starts again over what the store committed: `Get`
/// answers the same run and hosts, `List` the same runs, the new coordinator can cancel the run
/// and numbers the next run after it.
///
/// Catches: runs held only in memory, or lost by the ops file or rows that hold them.
#[test]
fn runs_survive_a_restart() {
    let (world, id) = scheduled("mc-restart");
    let before = get(&world, &id).expect("got");
    let listed = list(&world, &[], 0).expect("listed");

    let again = World::over("mc-restart-again", world.fake.clone());

    assert_eq!(get(&again, &id).expect("got"), before);
    assert_eq!(list(&again, &[], 0).expect("listed"), listed);
    assert_eq!(again.ops().maintenance_runs, world.ops().maintenance_runs);
    let cancelled = cancel(&again, &id, REASON).expect("cancelled");
    assert_eq!(
        cancelled.run.expect("a run").state(),
        MaintenanceState::Cancelled
    );
    assert_eq!(
        run_proto(&again.ops().maintenance_runs[0], UNIX).state(),
        MaintenanceState::Cancelled
    );
}

/// Proves: a confirmed `Schedule` of a group run writes exactly one audit entry,
/// `maintenance_schedule` by the operator with the group and the reason followed by
/// `run <id>`, the one it answers; one of hosts names no group; a `Cancel` writes exactly one;
/// `List`, `Get` and a
/// `Schedule` without `confirm` write none and commit nothing.
///
/// Catches: a change that is not audited, audited twice, or a read that writes.
#[test]
fn every_changing_call_writes_one_audit_entry_and_a_read_none() {
    let world = alpha("mc-audit", &[("d1", MACOS), ("d2", LINUX), ("d3", NONE)]);
    member(&world, "alpha", "d4");
    hear(&world, "d4", true, MACOS);
    world.enroll("d5");
    hear(&world, "d5", true, LINUX);
    let audited = world.audit().len() as u64;
    let commits = world.fake.commits();

    let preview = proto::ScheduleMaintenanceRequest {
        confirm: false,
        ..hosts_request(&["d5"])
    };
    let previewed = schedule(&world, preview).expect("previewed");
    assert_eq!(previewed.entry, None);
    assert_eq!(list(&world, &[], 0).expect("listed"), Vec::new());
    assert_eq!(
        get(&world, FIRST).expect_err("none yet"),
        Refusal::UnknownRun
    );
    assert_eq!(world.audit().len() as u64, audited);
    assert_eq!(world.fake.commits(), commits);

    let group_run = schedule(&world, group_request(GROUP)).expect("scheduled");
    let wanted = operator_entry(
        audited + 1,
        "maintenance_schedule",
        Some(GROUP),
        None,
        &format!("{REASON} run {FIRST}"),
    );
    assert_eq!(group_run.entry, Some(entry_proto(&wanted)));
    assert_eq!(world.audit().split_off(audited as usize), vec![wanted]);

    let hosts_run = schedule(&world, hosts_request(&["d5"])).expect("scheduled");
    let wanted = operator_entry(
        audited + 2,
        "maintenance_schedule",
        None,
        None,
        &format!("{REASON} run run-1900000000-2"),
    );
    assert_eq!(hosts_run.entry, Some(entry_proto(&wanted)));
    assert_eq!(world.audit().pop(), Some(wanted));

    let after_schedules = (world.audit().len(), world.fake.commits());
    list(&world, &[], 0).expect("listed");
    get(&world, FIRST).expect("got");
    assert_eq!((world.audit().len(), world.fake.commits()), after_schedules);

    cancel(&world, FIRST, "stop it").expect("cancelled");
    let wanted = operator_entry(
        audited + 3,
        "maintenance_cancel",
        None,
        None,
        &format!("stop it run {FIRST}"),
    );
    assert_eq!(world.audit().split_off(audited as usize + 2), vec![wanted]);
}

/// Proves: a `Schedule` whose store fails is `store_failed` and one whose audit line cannot be
/// kept `audit_unavailable`, each leaving the committed state, the held runs and the audit as
/// they were; once the store heals the same call stores the run under the first id, so the
/// failures used none.
///
/// Catches: a run kept in memory that the store refused, or an id used by a failed call.
#[test]
fn a_schedule_the_store_refuses_changes_nothing() {
    let world = alpha(
        "mc-audit-failures",
        &[("d1", MACOS), ("d2", LINUX), ("d3", NONE)],
    );
    let before = kept(&world);
    for (failure, refusal) in [
        (
            crate::store::store_failure as fn() -> _,
            Refusal::StoreFailed,
        ),
        (crate::store::audit_failure, Refusal::AuditUnavailable),
    ] {
        world.fake.refuse(move |_| Some(failure()));
        assert_eq!(
            schedule(&world, group_request(GROUP)).expect_err("refused"),
            refusal
        );
        assert_eq!(world.ops().maintenance_runs, Vec::new());
    }
    world.fake.heal();
    assert_eq!(kept(&world), before);
    let stored = schedule(&world, group_request(GROUP)).expect("scheduled");
    assert_eq!(stored.run.expect("a run").id, FIRST);
}
