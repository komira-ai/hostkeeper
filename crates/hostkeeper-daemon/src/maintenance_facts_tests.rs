//! The tests of `maintenance_facts`: the sink over scripted collectors, an injected clock and a
//! dispatcher that keeps each look until the test runs it; the reports, the reasons and the log
//! lines are asserted by equality.

use std::panic::{AssertUnwindSafe, catch_unwind};

use hostkeeper_link::testing::{FakeClock, ManualDispatch, RecordingLog};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::maintenance_collect::{linux_item, reboot_item};

const START: u64 = 2_000_000_000;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

type Calls = Arc<Mutex<Vec<&'static str>>>;
type Answer = Arc<Mutex<Result<Section, String>>>;

/// A collector that gives the answer it holds, which the test can change, and counts its calls.
struct Scripted {
    name: &'static str,
    answer: Answer,
    calls: Calls,
}

impl Collector for Scripted {
    fn name(&self) -> &'static str {
        self.name
    }

    fn collect(&self, _look: &Look<'_>) -> Result<Section, String> {
        lock(&self.calls).push(self.name);
        lock(&self.answer).clone()
    }
}

/// A collector that finds package updates `(package, security, kernel)` and reports those the
/// look says are due.
struct Updating(Vec<(&'static str, bool, bool)>);

impl Collector for Updating {
    fn name(&self) -> &'static str {
        "updating"
    }

    fn collect(&self, look: &Look<'_>) -> Result<Section, String> {
        Ok(Section {
            due: self
                .0
                .iter()
                .filter(|(package, security, kernel)| look.due(package, *security, *kernel))
                .map(|(package, security, kernel)| {
                    linux_item(package, "1", "2", *security, *kernel)
                })
                .collect(),
            ..Section::default()
        })
    }
}

/// A collector whose worker dies.
struct Panicking;

impl Collector for Panicking {
    fn name(&self) -> &'static str {
        "panicking"
    }

    fn collect(&self, _look: &Look<'_>) -> Result<Section, String> {
        panic!("the collector died");
    }
}

/// The packages of hostkeeper's own, as given.
struct Own(Result<BTreeSet<String>, String>);

impl OwnPackages for Own {
    fn names(&self) -> Result<BTreeSet<String>, String> {
        self.0.clone()
    }
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// The sink, and the fakes it was given.
struct Rig {
    sink: MaintenanceFacts,
    clock: Arc<FakeClock>,
    dispatch: Arc<ManualDispatch>,
    log: Arc<RecordingLog>,
}

fn rig(
    collectors: Vec<Box<dyn Collector>>,
    own: Result<BTreeSet<String>, String>,
    scope: LinuxScope,
) -> Rig {
    let clock = Arc::new(FakeClock::new(START, 7));
    let dispatch = Arc::new(ManualDispatch::default());
    let log = Arc::new(RecordingLog::default());
    Rig {
        sink: MaintenanceFacts::new(
            collectors,
            Box::new(Own(own)),
            scope,
            clock.clone(),
            dispatch.clone(),
            log.clone(),
        ),
        clock,
        dispatch,
        log,
    }
}

/// A sink over `collectors`, with no package of hostkeeper's own, under the default scope.
fn simple(collectors: Vec<Box<dyn Collector>>) -> Rig {
    rig(collectors, Ok(BTreeSet::new()), LinuxScope::default())
}

/// A collector `name` that answers `answer`, and the handles to change it and to see its calls.
fn scripted(
    name: &'static str,
    answer: Result<Section, String>,
    calls: &Calls,
) -> (Scripted, Answer) {
    let answer = Arc::new(Mutex::new(answer));
    (
        Scripted {
            name,
            answer: answer.clone(),
            calls: calls.clone(),
        },
        answer,
    )
}

impl Rig {
    /// Runs the look that was started.
    fn run_look(&self) {
        assert!(self.dispatch.run_next(), "a look was started");
    }
}

fn due_section(items: Vec<pb::DueItem>) -> Section {
    Section {
        due: items,
        ..Section::default()
    }
}

/// Proves the first report starts a look and does not wait for it: a report made before the look
/// ends is the default one, and asks for no second look; once the look ends, the report is what
/// it found, its time the second it started, and the log says what it found.
#[test]
fn the_first_report_starts_a_look_and_is_the_default_until_it_ends() {
    let calls = Calls::default();
    let host = pb::MaintenanceHost {
        os_name: "ubuntu".to_owned(),
        os_version: "24.04".to_owned(),
        running_kernel: "6.8.0-1".to_owned(),
        ..pb::MaintenanceHost::default()
    };
    let item = linux_item("openssl", "3.0.1", "3.0.2", true, false);
    let (collector, _) = scripted(
        "apt",
        Ok(Section {
            host: host.clone(),
            due: vec![item.clone()],
            unparsed: 1,
            ..Section::default()
        }),
        &calls,
    );
    let rig = simple(vec![Box::new(collector)]);
    assert_eq!(rig.sink.report(), pb::MaintenanceReport::default());
    assert_eq!(rig.dispatch.pending(), 1);
    assert_eq!(rig.sink.report(), pb::MaintenanceReport::default());
    assert_eq!(rig.dispatch.pending(), 1);
    assert!(lock(&calls).is_empty());
    rig.run_look();
    assert_eq!(
        rig.sink.report(),
        pb::MaintenanceReport {
            host: Some(host),
            due: vec![item],
            looked_unix: START,
            unparsed_lines: 1,
            ..pb::MaintenanceReport::default()
        }
    );
    assert_eq!(rig.dispatch.pending(), 0);
    assert_eq!(
        rig.log.lines(),
        ["maintenance: looked: 1 due, 1 unparsed lines"]
    );
}

/// Proves the sections join as the module says: the host texts the first section that has one
/// wins, the boot time likewise, each text is kept to its bound; the Mac facts are the last
/// section's that has any; the Xcodes and the unparsed lines (saturating) add up; the due items
/// are the reboots first (a reboot named twice kept once, the first), then the security updates,
/// then the rest, each group in the order found, at most 12 with the count of the rest, and an
/// item's texts kept to their bounds.
#[test]
fn the_sections_join_in_order_with_the_due_items_ordered_and_capped() {
    let calls = Calls::default();
    let xcode = pb::InstalledXcode {
        version: "15.4".to_owned(),
        ..pb::InstalledXcode::default()
    };
    let (first, _) = scripted(
        "first",
        Ok(Section {
            host: pb::MaintenanceHost {
                os_name: "ubuntu".to_owned(),
                running_kernel: "6.8.0-1".to_owned(),
                ..pb::MaintenanceHost::default()
            },
            xcodes: vec![xcode.clone()],
            mac: Some(pb::MacUpdateFacts::default()),
            due: vec![
                linux_item("other-1", "1", "2", false, false),
                reboot_item("kernel", "6.8.0-1"),
                linux_item("sec-1", "1", "2", true, false),
            ],
            unparsed: 2,
        }),
        &calls,
    );
    let (second, _) = scripted(
        "second",
        Ok(Section {
            host: pb::MaintenanceHost {
                os_name: "debian".to_owned(),
                os_version: "12".to_owned(),
                hardware_serial: "s".repeat(40),
                newest_kernel: "6.8.0-2".to_owned(),
                boot_id: "b".repeat(70),
                boot_unix: 77,
                ..pb::MaintenanceHost::default()
            },
            mac: Some(pb::MacUpdateFacts {
                authrestart_supported: true,
                ..pb::MacUpdateFacts::default()
            }),
            due: vec![
                reboot_item("kernel", "6.8.0-9"),
                reboot_item("reboot-required", ""),
                linux_item("sec-2", "1", "2", true, true),
            ],
            unparsed: 3,
            ..Section::default()
        }),
        &calls,
    );
    let mut rest = vec![pb::DueItem {
        kind: pb::MaintenanceKind::Linux.into(),
        from: "f".repeat(40),
        to: "t".repeat(40),
        needs_reboot: false,
        label: "l".repeat(70),
        security: false,
    }];
    rest.extend((0..9).map(|n| linux_item(&format!("pkg-{n}"), "1", "2", false, false)));
    let (third, _) = scripted(
        "third",
        Ok(Section {
            due: rest,
            unparsed: u32::MAX,
            ..Section::default()
        }),
        &calls,
    );
    let rig = simple(vec![Box::new(first), Box::new(second), Box::new(third)]);
    rig.sink.report();
    rig.run_look();
    let mut due = vec![
        reboot_item("kernel", "6.8.0-1"),
        reboot_item("reboot-required", ""),
        linux_item("sec-1", "1", "2", true, false),
        linux_item("sec-2", "1", "2", true, true),
        linux_item("other-1", "1", "2", false, false),
        pb::DueItem {
            kind: pb::MaintenanceKind::Linux.into(),
            from: "f".repeat(32),
            to: "t".repeat(32),
            needs_reboot: false,
            label: "l".repeat(64),
            security: false,
        },
    ];
    due.extend((0..6).map(|n| linux_item(&format!("pkg-{n}"), "1", "2", false, false)));
    assert_eq!(
        rig.sink.report(),
        pb::MaintenanceReport {
            host: Some(pb::MaintenanceHost {
                os_name: "ubuntu".to_owned(),
                os_version: "12".to_owned(),
                hardware_serial: "s".repeat(32),
                running_kernel: "6.8.0-1".to_owned(),
                newest_kernel: "6.8.0-2".to_owned(),
                boot_id: "b".repeat(64),
                boot_unix: 77,
                ..pb::MaintenanceHost::default()
            }),
            xcodes: vec![xcode],
            mac: Some(pb::MacUpdateFacts {
                authrestart_supported: true,
                ..pb::MacUpdateFacts::default()
            }),
            due,
            due_left_out: 3,
            looked_unix: START,
            unparsed_lines: u32::MAX,
            ..pb::MaintenanceReport::default()
        }
    );
    assert_eq!(
        rig.log.lines(),
        ["maintenance: looked: 15 due, 4294967295 unparsed lines"]
    );
}

/// Proves a due kernel update appears in the report and clears from it once the next look finds
/// nothing due; that no look starts before [`LOOK_SECS`] after the last one started, and one
/// does at that second.
#[test]
fn a_due_update_appears_and_clears_at_the_next_look_an_hour_later() {
    let calls = Calls::default();
    let kernel = linux_item("linux-image-6.8.0-2", "6.8.0-1", "6.8.0-2", true, true);
    let (collector, answer) = scripted("apt", Ok(due_section(vec![kernel.clone()])), &calls);
    let rig = simple(vec![Box::new(collector)]);
    rig.sink.report();
    rig.run_look();
    assert_eq!(rig.sink.report().due, [kernel]);
    *lock(&answer) = Ok(Section::default());
    rig.clock.advance(LOOK_SECS - 1);
    assert_eq!(rig.sink.report().due.len(), 1);
    assert_eq!(rig.dispatch.pending(), 0);
    rig.clock.advance(1);
    rig.sink.report();
    assert_eq!(rig.dispatch.pending(), 1);
    rig.run_look();
    let report = rig.sink.report();
    assert!(report.due.is_empty());
    assert_eq!(report.looked_unix, START + LOOK_SECS);
    assert_eq!(*lock(&calls), ["apt", "apt"]);
}

/// Proves `look_again` makes the next report start a look at once, and when called while a look
/// runs, the look after it starts at the first report after it ends and not before.
#[test]
fn look_again_starts_a_look_at_once_also_while_one_runs() {
    let calls = Calls::default();
    let (collector, _) = scripted("apt", Ok(Section::default()), &calls);
    let rig = simple(vec![Box::new(collector)]);
    rig.sink.report();
    rig.run_look();
    rig.sink.report();
    assert_eq!(rig.dispatch.pending(), 0);
    rig.sink.look_again();
    rig.sink.report();
    assert_eq!(rig.dispatch.pending(), 1);
    rig.sink.look_again();
    rig.sink.report();
    assert_eq!(rig.dispatch.pending(), 1);
    rig.run_look();
    assert_eq!(rig.dispatch.pending(), 0);
    rig.sink.report();
    assert_eq!(rig.dispatch.pending(), 1);
    rig.run_look();
    assert_eq!(*lock(&calls), ["apt", "apt", "apt"]);
}

/// Proves a collector that fails stops the look at it (the collectors after it are not asked),
/// names itself and its reason in `look_failed`, leaves the rest of the report as the last look
/// that succeeded left it, and logs the reason; the next look that succeeds clears it.
#[test]
fn a_failing_collector_sets_look_failed_and_keeps_the_last_report() {
    let calls = Calls::default();
    let item = linux_item("openssl", "3.0.1", "3.0.2", true, false);
    let (first, _) = scripted("host", Ok(due_section(vec![item.clone()])), &calls);
    let (second, answer) = scripted("apt", Ok(Section::default()), &calls);
    let (third, _) = scripted("reboot", Ok(Section::default()), &calls);
    let rig = simple(vec![Box::new(first), Box::new(second), Box::new(third)]);
    rig.sink.report();
    rig.run_look();
    let kept = rig.sink.report();
    assert_eq!(kept.due, [item]);
    assert_eq!(kept.look_failed, "");
    *lock(&answer) = Err("apt-get update failed with exit code 100: E: boom".to_owned());
    rig.clock.advance(LOOK_SECS);
    rig.sink.report();
    rig.run_look();
    assert_eq!(
        rig.sink.report(),
        pb::MaintenanceReport {
            look_failed: "apt: apt-get update failed with exit code 100: E: boom".to_owned(),
            ..kept.clone()
        }
    );
    assert_eq!(*lock(&calls), ["host", "apt", "reboot", "host", "apt"]);
    assert_eq!(
        rig.log.lines(),
        [
            "maintenance: looked: 1 due, 0 unparsed lines",
            "error: maintenance: the look failed: apt: apt-get update failed with exit code 100: E: boom",
        ]
    );
    *lock(&answer) = Ok(Section::default());
    rig.clock.advance(LOOK_SECS);
    rig.sink.report();
    rig.run_look();
    let cleared = rig.sink.report();
    assert_eq!(cleared.look_failed, "");
    assert_eq!(cleared.looked_unix, START + 2 * LOOK_SECS);
}

/// Proves a reason longer than the bound is cut to it, and that a look that fails before any look
/// succeeded leaves the default report with the reason and `looked_unix` 0.
#[test]
fn a_long_reason_is_cut_and_a_first_look_that_fails_leaves_the_default_report() {
    let calls = Calls::default();
    let (collector, _) = scripted("dnf", Err("x".repeat(500)), &calls);
    let rig = simple(vec![Box::new(collector)]);
    rig.sink.report();
    rig.run_look();
    let reason = format!("dnf: {}", "x".repeat(500));
    let reason = &reason[..MAX_LOOK_FAILED];
    assert_eq!(
        rig.sink.report(),
        pb::MaintenanceReport {
            look_failed: reason.to_owned(),
            ..pb::MaintenanceReport::default()
        }
    );
    assert_eq!(
        rig.log.lines(),
        [format!("error: maintenance: the look failed: {reason}")]
    );
}

/// Proves hostkeeper's own packages cannot be read: the look fails with `own packages: ` and the
/// reason, and no collector is asked.
#[test]
fn own_packages_that_cannot_be_read_fail_the_look_before_any_collector() {
    let calls = Calls::default();
    let (collector, _) = scripted("apt", Ok(Section::default()), &calls);
    let rig = rig(
        vec![Box::new(collector)],
        Err("placed.json is damaged".to_owned()),
        LinuxScope::default(),
    );
    rig.sink.report();
    rig.run_look();
    assert_eq!(
        rig.sink.report().look_failed,
        "own packages: placed.json is damaged"
    );
    assert!(lock(&calls).is_empty());
    assert_eq!(
        rig.log.lines(),
        ["error: maintenance: the look failed: own packages: placed.json is damaged"]
    );
}

/// Proves a report that breaks a wire bound fails the look with `report: ` and the bound, and is
/// not kept.
#[test]
fn a_report_that_breaks_a_bound_fails_the_look() {
    let calls = Calls::default();
    let (collector, _) = scripted(
        "mac",
        Ok(Section {
            xcodes: vec![pb::InstalledXcode::default(); 7],
            ..Section::default()
        }),
        &calls,
    );
    let rig = simple(vec![Box::new(collector)]);
    rig.sink.report();
    rig.run_look();
    assert_eq!(
        rig.sink.report(),
        pb::MaintenanceReport {
            look_failed: "report: xcodes holds 7 entries, more than 6".to_owned(),
            ..pb::MaintenanceReport::default()
        }
    );
}

/// Proves a look whose job its dispatcher drops, and one whose worker dies, each end, and the
/// next report starts the look again instead of waiting an hour.
#[test]
fn a_look_that_was_dropped_or_died_is_tried_again_at_the_next_report() {
    let rig = simple(vec![Box::new(Panicking)]);
    rig.sink.report();
    assert!(rig.dispatch.drop_next());
    rig.sink.report();
    assert_eq!(rig.dispatch.pending(), 1);
    assert!(catch_unwind(AssertUnwindSafe(|| rig.dispatch.run_next())).is_err());
    assert_eq!(rig.sink.report(), pb::MaintenanceReport::default());
    assert_eq!(rig.dispatch.pending(), 1);
    assert!(rig.log.lines().is_empty());
}

/// Proves hostkeeper's own packages are never due, with or without an architecture suffix, under
/// the security scope (where only security updates and kernels are) and under all.
#[test]
fn hostkeepers_own_packages_never_appear() {
    let updates = vec![
        ("nginx", true, false),
        ("hostkeeper", true, false),
        ("hostkeeper:amd64", true, true),
        ("curl", false, false),
    ];
    for (scope, due) in [
        (LinuxScope::Security, vec!["nginx"]),
        (LinuxScope::All, vec!["nginx", "curl"]),
    ] {
        let rig = rig(
            vec![Box::new(Updating(updates.clone()))],
            Ok(set(&["hostkeeper"])),
            scope,
        );
        rig.sink.report();
        rig.run_look();
        let labels: Vec<String> = rig
            .sink
            .report()
            .due
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert_eq!(labels, due, "{scope:?}");
    }
}

/// Proves a Mac has no maintenance facts (it announces and sends nothing) and a Linux host, under
/// systemd or OpenRC, has.
#[test]
fn only_a_linux_host_has_maintenance_facts() {
    let scratch = scratch("facts-at-host");
    let log = Arc::new(RecordingLog::default());
    assert!(
        MaintenanceFacts::at_host(
            PlatformKind::Macos,
            LinuxScope::Security,
            log.clone(),
            scratch.path()
        )
        .is_none()
    );
    for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
        assert!(
            MaintenanceFacts::at_host(kind, LinuxScope::All, log.clone(), scratch.path()).is_some()
        );
    }
}
