//! The maintenance hook's `verify` of the Linux kinds, `linux` and `reboot`
//! (docs/design/maintenance-updates.md, section 5.6), over fake collectors, a fake list of the
//! packages hostkeeper installs and a fake observer of the services' health: each wrong fact
//! fails the step with its own reason and a detail that names what was found, the first wrong
//! fact winning; an updated, rebooted and healthy host ends done; a kind not in the order is not
//! checked.

mod support_maintenance;

use std::collections::BTreeSet;
use std::path::PathBuf;

use hostkeeper_daemon::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS;
use hostkeeper_daemon::maintenance_collect::{Collector, Look, Section, linux_item, reboot_item};
use hostkeeper_daemon::maintenance_hook::linux::{
    self, LinuxCheck, REASON_FACTS_UNREADABLE, REASON_HEALTH_UNREADABLE, REASON_KERNEL_NOT_NEWEST,
    REASON_REBOOT_NEEDED, REASON_SERVICE_UNHEALTHY, REASON_UPDATE_DUE, ServiceHealth,
};
use hostkeeper_daemon::maintenance_hook::{
    HookArgs, HookStep, Kind, KindError, KindState, MaintainParams, REASON_NOT_APPLIED,
    REASON_NOT_REBOOTED, Unsupported, main_with, run_step,
};
use hostkeeper_daemon::maintenance_own::OwnPackages;
use hostkeeper_hook::Outcome;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::agent::v1::{
    Health, LinuxScope as WireScope, Maintain, MaintenanceKind as WireKind,
};
use hostkeeper_steps::BootId;
use support_maintenance::{
    BOOT_A, BOOT_B, Effects, Ending, FakeKind, Host, RUN, done, failed, maintain, push, rebooting,
    taken,
};

/// The kernel the host runs.
const RUNNING: &str = "6.8.0-45-generic";
/// A newer kernel, installed and not running.
const NEWER: &str = "6.8.0-51-generic";

/// A collector named `name` that answers `answer`, logging `<name> <scope> [<own packages>]` for
/// each look.
struct FakeCollector {
    name: &'static str,
    answer: Result<Section, String>,
    effects: Effects,
}

impl Collector for FakeCollector {
    fn name(&self) -> &'static str {
        self.name
    }

    fn collect(&self, look: &Look<'_>) -> Result<Section, String> {
        let own: Vec<&str> = look.own_packages.iter().map(String::as_str).collect();
        push(
            &self.effects,
            format!("{} {:?} [{}]", self.name, look.scope, own.join(",")),
        );
        self.answer.clone()
    }
}

/// The packages hostkeeper installs, as the test sets them, logging `own packages`.
struct FakeOwn {
    answer: Result<BTreeSet<String>, String>,
    effects: Effects,
}

impl OwnPackages for FakeOwn {
    fn names(&self) -> Result<BTreeSet<String>, String> {
        push(&self.effects, "own packages".to_owned());
        self.answer.clone()
    }
}

/// The services' health, as the test sets it, logging `services`.
struct FakeServices {
    answer: Result<Vec<(String, Health)>, String>,
    effects: Effects,
}

impl ServiceHealth for FakeServices {
    fn services(&self) -> Result<Vec<(String, Health)>, String> {
        push(&self.effects, "services".to_owned());
        self.answer.clone()
    }
}

/// What the fakes of a host answer.
#[derive(Clone)]
struct Facts {
    host: Result<Section, String>,
    reboot: Result<Section, String>,
    packages: Option<Result<Section, String>>,
    own: Result<BTreeSet<String>, String>,
    services: Result<Vec<(String, Health)>, String>,
}

impl Facts {
    /// An updated, rebooted and healthy host: it runs the newest kernel installed, needs no
    /// reboot, has nothing due, and its services are healthy or have no health check.
    fn good() -> Facts {
        Facts {
            host: Ok(kernels(RUNNING, RUNNING)),
            reboot: Ok(Section::default()),
            packages: Some(Ok(Section::default())),
            own: Ok(BTreeSet::from([
                "hostkeeper".to_owned(),
                "js-agent".to_owned(),
            ])),
            services: Ok(vec![
                ("js-agent".to_owned(), Health::Healthy),
                ("tierstore".to_owned(), Health::Unspecified),
            ]),
        }
    }

    /// The check over fakes answering these facts, each logging to `effects`.
    fn check(self, effects: &Effects) -> LinuxCheck {
        let collector =
            |name: &'static str, answer: Result<Section, String>| -> Box<dyn Collector> {
                Box::new(FakeCollector {
                    name,
                    answer,
                    effects: effects.clone(),
                })
            };
        LinuxCheck::new(
            collector("host", self.host),
            collector("reboot", self.reboot),
            self.packages.map(|answer| collector("packages", answer)),
            Box::new(FakeOwn {
                answer: self.own,
                effects: effects.clone(),
            }),
            Box::new(FakeServices {
                answer: self.services,
                effects: effects.clone(),
            }),
        )
    }

    /// The host of fakes answering these facts, the kinds' other steps stubs, every kind of
    /// `applied` recorded applied.
    fn host(self, effects: &Effects, applied: &[Kind]) -> Host {
        let host = Host {
            kinds: linux::kinds(
                self.check(effects),
                Box::new(Unsupported(Kind::Linux)),
                Box::new(Unsupported(Kind::Reboot)),
            ),
            ..Host::new(effects, Vec::new())
        };
        host.boot.set(BOOT_B);
        for kind in applied {
            host.record.set(*kind, KindState::Applied);
        }
        host
    }
}

/// The host section of a host running `running`, with `newest` the newest kernel installed, and
/// the reboot a newer one makes due, as the collector `host` gives it.
fn kernels(running: &str, newest: &str) -> Section {
    let due = (running != newest)
        .then(|| reboot_item("kernel", running))
        .into_iter()
        .collect();
    Section {
        host: pb::MaintenanceHost {
            os_name: "ubuntu".to_owned(),
            os_version: "24.04".to_owned(),
            running_kernel: running.to_owned(),
            newest_kernel: newest.to_owned(),
            boot_id: BOOT_B.to_owned(),
            ..pb::MaintenanceHost::default()
        },
        due,
        ..Section::default()
    }
}

/// A section with `due` alone.
fn due(due: Vec<pb::DueItem>) -> Section {
    Section {
        due,
        ..Section::default()
    }
}

/// The security update of openssl.
fn openssl() -> pb::DueItem {
    linux_item(
        "openssl",
        "3.0.13-0ubuntu3.4",
        "3.0.13-0ubuntu3.5",
        true,
        false,
    )
}

/// Runs `verify` of the run `maintain` on `host`.
fn verify(host: &Host, maintain: Maintain) -> Ending {
    let params = MaintainParams::new(maintain).expect("valid parameters");
    let ending = run_step(HookStep::Verify, &params, &host.ports());
    Ending {
        line: ending.report().to_line(),
        outcome: ending.outcome,
    }
}

/// A `maintain` of `kinds` under `scope`.
fn scoped(kinds: &[WireKind], scope: WireScope) -> Maintain {
    Maintain {
        linux_scope: scope.into(),
        ..maintain(kinds)
    }
}

/// Proves: a host that runs the newest kernel, needs no reboot, has nothing of the order's scope
/// due and runs only healthy services (or services with no health check) passes `verify` of
/// `linux` and `reboot`: exit 0 and an empty report, the parameters read from standard input;
/// the host is read once, under the order's scope, the packages hostkeeper installs left out of
/// the package manager's look.
///
/// Catches: a check that fails a good host, or reads the host twice for one run.
#[test]
fn an_updated_rebooted_and_healthy_host_passes_verify_with_exit_0() {
    let effects = Effects::default();
    let host = Facts::good().host(&effects, &[Kind::Linux, Kind::Reboot]);
    let args = HookArgs {
        step: HookStep::Verify,
        params: PathBuf::from("-"),
        package_lock_wait_secs: DEFAULT_PACKAGE_LOCK_WAIT_SECS,
    };
    let json = format!(
        r#"{{"runId":"{RUN}","action":"MAINTAIN_ACTION_RUN","kinds":["MAINTENANCE_KIND_REBOOT","MAINTENANCE_KIND_LINUX"],"linuxScope":"LINUX_SCOPE_ALL"}}"#
    );
    let mut stdout = Vec::new();
    let outcome = main_with(&args, &host.ports(), &mut json.as_bytes(), &mut stdout);
    assert_eq!(outcome.code(), 0);
    assert_eq!(
        String::from_utf8(stdout).expect("utf-8"),
        format!("{}\n", done().line)
    );
    assert_eq!(
        taken(&effects),
        [
            "host All []",
            "reboot All []",
            "own packages",
            "packages All [hostkeeper,js-agent]",
            "services",
        ]
    );
}

/// Proves: a kind recorded applied after a reboot asked for in boot A fails `not_rebooted`,
/// naming the kind and the boot, while the host still runs boot A, before any fact is read; once
/// the host runs another boot the same record passes. A kind applied with no reboot needs no new
/// boot.
///
/// Catches: a verify that passes a host whose reboot never happened, so the old kernel serves on.
#[test]
fn a_boot_id_unchanged_after_a_needed_reboot_fails_not_rebooted() {
    let effects = Effects::default();
    let host = Facts::good().host(&effects, &[]);
    let asked_in = BootId::uuid("test", BOOT_A).expect("uuid");
    host.record
        .set(Kind::Linux, KindState::AppliedAfterReboot { asked_in });
    host.boot.set(BOOT_A);
    let ended = verify(&host, maintain(&[WireKind::Linux]));
    assert_eq!(
        ended,
        failed(
            REASON_NOT_REBOOTED,
            &format!(
                "linux needed a reboot, and the host still runs the boot {BOOT_A} it was asked in"
            ),
        )
    );
    assert_eq!(ended.outcome.code(), 1);
    assert_eq!(taken(&effects), Vec::<String>::new());

    host.boot.set(BOOT_B);
    assert_eq!(verify(&host, maintain(&[WireKind::Linux])), done());

    host.record.set(Kind::Linux, KindState::Applied);
    host.boot.set(BOOT_A);
    assert_eq!(verify(&host, maintain(&[WireKind::Linux])), done());
}

/// Proves: the run record of a real cutover across a reboot holds the boot the reboot was asked
/// in, and `verify` compares the boot the host runs with it.
///
/// Catches: a run record that forgets the boot of the reboot once the kind is applied, which
/// leaves `not_rebooted` nothing to compare.
#[test]
fn verify_compares_the_boot_with_the_one_the_cutover_recorded_its_reboot_in() {
    let effects = Effects::default();
    let host = Host::new(
        &effects,
        vec![FakeKind::rebooting(Kind::Linux, NEWER, &effects)],
    );
    let kinds = [WireKind::Linux];
    assert_eq!(host.run(HookStep::Cutover, &kinds), rebooting("linux"));
    host.boot.set(BOOT_B);
    assert_eq!(host.run(HookStep::Cutover, &kinds), done());
    let asked_in = BootId::uuid("test", BOOT_A).expect("uuid");
    assert_eq!(
        host.record.states.lock().expect("states").get(&Kind::Linux),
        Some(&KindState::AppliedAfterReboot { asked_in })
    );
    assert_eq!(host.run(HookStep::Verify, &kinds), done());
    host.boot.set(BOOT_A);
    assert_eq!(
        host.run(HookStep::Verify, &kinds),
        failed(
            REASON_NOT_REBOOTED,
            &format!(
                "linux needed a reboot, and the host still runs the boot {BOOT_A} it was asked in"
            ),
        )
    );
}

/// Proves: a host that runs a kernel older than the newest installed fails `kernel_not_newest`,
/// naming both, for `linux` and for `reboot`; the reboot that kernel makes due is not what is
/// named, and nothing after the kernel is read.
///
/// Catches: a reboot that came back on the old kernel (a boot entry not switched) passed as done.
#[test]
fn a_kernel_older_than_the_newest_installed_fails_kernel_not_newest() {
    for kind in [WireKind::Linux, WireKind::Reboot] {
        let effects = Effects::default();
        let facts = Facts {
            host: Ok(kernels(RUNNING, NEWER)),
            reboot: Ok(due(vec![reboot_item("reboot-required", "")])),
            ..Facts::good()
        };
        let host = facts.host(&effects, &[Kind::Linux, Kind::Reboot]);
        let ended = verify(&host, maintain(&[kind]));
        assert_eq!(
            ended,
            failed(
                REASON_KERNEL_NOT_NEWEST,
                &format!("the host runs kernel {RUNNING}, and {NEWER} is installed"),
            ),
            "{kind:?}"
        );
        assert_eq!(ended.outcome.code(), 1);
        assert_eq!(taken(&effects), ["host Security []"]);
    }
}

/// Proves: a host on the newest kernel that still says it needs a reboot (`/run/reboot-required`,
/// `needrestart`, `needs-restarting`) fails `reboot_needed`, naming the source, for `linux` and
/// for `reboot`, and reads nothing after it.
///
/// Catches: a verify that trusts the kernel alone while the distribution asks for a reboot.
#[test]
fn a_reboot_still_needed_fails_reboot_needed() {
    for kind in [WireKind::Linux, WireKind::Reboot] {
        let effects = Effects::default();
        let facts = Facts {
            reboot: Ok(due(vec![reboot_item("reboot-required", "")])),
            ..Facts::good()
        };
        let host = facts.host(&effects, &[Kind::Linux, Kind::Reboot]);
        let ended = verify(&host, maintain(&[kind]));
        assert_eq!(
            ended,
            failed(
                REASON_REBOOT_NEEDED,
                "the host still needs a reboot: reboot-required"
            ),
            "{kind:?}"
        );
        assert_eq!(ended.outcome.code(), 1);
        assert_eq!(taken(&effects), ["host Security []", "reboot Security []"]);
    }
}

/// Proves: an update of the order's scope still listed due fails `update_due`, naming the package
/// and the version it is due at; the package manager is asked under the order's scope (an order
/// with no scope under the default, `security`), the packages hostkeeper installs left out, and
/// nothing after it is read.
///
/// Catches: an upgrade that left a security update uninstalled passed as done, or the look made
/// under another scope than the order's.
#[test]
fn a_security_update_still_due_fails_update_due() {
    let cases = [
        (WireScope::Security, "Security"),
        (WireScope::Unspecified, "Security"),
        (WireScope::All, "All"),
    ];
    for (scope, word) in cases {
        let effects = Effects::default();
        let facts = Facts {
            packages: Some(Ok(due(vec![openssl()]))),
            ..Facts::good()
        };
        let host = facts.host(&effects, &[Kind::Linux]);
        let ended = verify(&host, scoped(&[WireKind::Linux], scope));
        assert_eq!(
            ended,
            failed(REASON_UPDATE_DUE, "openssl 3.0.13-0ubuntu3.5 is still due"),
            "{scope:?}"
        );
        assert_eq!(ended.outcome.code(), 1);
        assert_eq!(
            taken(&effects),
            [
                format!("host {word} []"),
                format!("reboot {word} []"),
                "own packages".to_owned(),
                format!("packages {word} [hostkeeper,js-agent]"),
            ]
        );
    }
}

/// Proves: one installed service unhealthy by its own health check fails `service_unhealthy`,
/// naming it (the first such in name order), and so does one installed and absent, and one whose
/// health is the coordinator's word `Silent` (a local read never gives it, and a health nobody
/// checked is not well); a service with no health check passes.
///
/// Catches: a host put back while a service it runs is down after the reboot.
#[test]
fn one_service_unhealthy_fails_service_unhealthy_naming_it() {
    let cases = [
        (Health::Unhealthy, "the service tierstore is unhealthy"),
        (Health::Absent, "the service tierstore is absent"),
        (Health::Silent, "the service tierstore is silent"),
    ];
    for (health, detail) in cases {
        let effects = Effects::default();
        let facts = Facts {
            services: Ok(vec![
                ("js-agent".to_owned(), Health::Healthy),
                ("tierstore".to_owned(), health),
                ("zeta".to_owned(), Health::Unhealthy),
            ]),
            ..Facts::good()
        };
        let host = facts.host(&effects, &[Kind::Reboot]);
        let ended = verify(&host, maintain(&[WireKind::Reboot]));
        assert_eq!(ended, failed(REASON_SERVICE_UNHEALTHY, detail));
        assert_eq!(ended.outcome.code(), 1);
        assert_eq!(
            taken(&effects),
            ["host Security []", "reboot Security []", "services"]
        );
    }
}

/// Proves: the first wrong fact is the one named: with the kernel old, a reboot needed, an update
/// due and a service unhealthy all at once, `verify` names the kernel; without the kernel, the
/// reboot; without both, the update; and only then the service.
///
/// Catches: a reason that names a later fact while an earlier one is wrong, which sends the
/// operator after the wrong cause.
#[test]
fn the_first_wrong_fact_is_the_one_named() {
    let all_wrong = Facts {
        host: Ok(kernels(RUNNING, NEWER)),
        reboot: Ok(due(vec![reboot_item("needrestart", RUNNING)])),
        packages: Some(Ok(due(vec![openssl()]))),
        services: Ok(vec![("tierstore".to_owned(), Health::Unhealthy)]),
        ..Facts::good()
    };
    let steps = [
        (
            all_wrong.clone(),
            REASON_KERNEL_NOT_NEWEST,
            format!("the host runs kernel {RUNNING}, and {NEWER} is installed"),
        ),
        (
            Facts {
                host: Ok(kernels(NEWER, NEWER)),
                ..all_wrong.clone()
            },
            REASON_REBOOT_NEEDED,
            "the host still needs a reboot: needrestart".to_owned(),
        ),
        (
            Facts {
                host: Ok(kernels(NEWER, NEWER)),
                reboot: Ok(Section::default()),
                ..all_wrong.clone()
            },
            REASON_UPDATE_DUE,
            "openssl 3.0.13-0ubuntu3.5 is still due".to_owned(),
        ),
        (
            Facts {
                host: Ok(kernels(NEWER, NEWER)),
                reboot: Ok(Section::default()),
                packages: Some(Ok(Section::default())),
                ..all_wrong
            },
            REASON_SERVICE_UNHEALTHY,
            "the service tierstore is unhealthy".to_owned(),
        ),
    ];
    for (facts, reason, detail) in steps {
        let effects = Effects::default();
        let host = facts.host(&effects, &[Kind::Linux]);
        assert_eq!(
            verify(&host, maintain(&[WireKind::Linux])),
            failed(reason, &detail)
        );
    }
}

/// Proves: a kind not in the order is not checked: a `reboot` run reads no package manager and
/// not the packages hostkeeper installs (an update due, and those packages unreadable, do not
/// fail it) and does not look at `linux` in the run record; a `linux` run does not look at
/// `reboot` in the run record.
///
/// Catches: a reboot-only run halted for an update nobody ordered, or a run halted for a kind
/// it does not hold.
#[test]
fn a_kind_not_in_the_order_is_not_checked() {
    let effects = Effects::default();
    let facts = Facts {
        packages: Some(Ok(due(vec![openssl()]))),
        own: Err("placed.json could not be read".to_owned()),
        ..Facts::good()
    };
    let host = facts.host(&effects, &[Kind::Reboot]);
    assert_eq!(verify(&host, maintain(&[WireKind::Reboot])), done());
    assert_eq!(
        taken(&effects),
        ["host Security []", "reboot Security []", "services"]
    );

    let host = Facts::good().host(&effects, &[Kind::Linux]);
    assert_eq!(verify(&host, maintain(&[WireKind::Linux])), done());
    assert_eq!(
        verify(&host, maintain(&[WireKind::Linux, WireKind::Reboot])),
        failed(REASON_NOT_APPLIED, "reboot is not recorded applied")
    );
}

/// Proves: a host with no package manager the look knows checks no updates; its kernel, its
/// reboot and its services still are.
///
/// Catches: a verify that fails, or skips the rest, on a host whose updates nobody can list.
#[test]
fn a_host_with_no_package_manager_checks_the_rest() {
    let effects = Effects::default();
    let facts = Facts {
        packages: None,
        ..Facts::good()
    };
    let host = facts.host(&effects, &[Kind::Linux]);
    assert_eq!(verify(&host, maintain(&[WireKind::Linux])), done());
    assert_eq!(
        taken(&effects),
        ["host Security []", "reboot Security []", "services"]
    );
}

/// Proves: a fact that cannot be read fails the step, never passes it: a collector's failure is
/// `facts_unreadable` naming the collector, as a failed look names it; the packages hostkeeper
/// installs unreadable is `facts_unreadable` too; services whose health cannot be listed are
/// `health_unreadable`.
///
/// Catches: a check that reads an error as "nothing due" and puts back a host it never looked at.
#[test]
fn a_fact_that_cannot_be_read_fails_verify_naming_it() {
    let cases = [
        (
            Facts {
                host: Err("uname -r exited 1: uname: not found".to_owned()),
                ..Facts::good()
            },
            REASON_FACTS_UNREADABLE,
            "host: uname -r exited 1: uname: not found",
        ),
        (
            Facts {
                reboot: Err("needrestart -b did not end by its deadline".to_owned()),
                ..Facts::good()
            },
            REASON_FACTS_UNREADABLE,
            "reboot: needrestart -b did not end by its deadline",
        ),
        (
            Facts {
                packages: Some(Err("apt-get update exited 100".to_owned())),
                ..Facts::good()
            },
            REASON_FACTS_UNREADABLE,
            "packages: apt-get update exited 100",
        ),
        (
            Facts {
                own: Err("placed.json could not be read".to_owned()),
                ..Facts::good()
            },
            REASON_FACTS_UNREADABLE,
            "own packages: placed.json could not be read",
        ),
        (
            Facts {
                services: Err("/var/lib/hostkeeper/packages: permission denied".to_owned()),
                ..Facts::good()
            },
            REASON_HEALTH_UNREADABLE,
            "/var/lib/hostkeeper/packages: permission denied",
        ),
    ];
    for (facts, reason, detail) in cases {
        let effects = Effects::default();
        let host = facts.host(&effects, &[Kind::Linux]);
        let ended = verify(&host, maintain(&[WireKind::Linux]));
        assert_eq!(ended, failed(reason, detail));
        assert_eq!(ended.outcome, Outcome::Failure);
    }
}

/// Proves: the Linux kinds put the check in front of the work they are given: `prepare`,
/// `cutover` and `after_reboot` are the work's (a cutover across a reboot records the version the
/// work answers after it), and `verify` is the check's, the work's own `verify` never asked.
///
/// Catches: the check wired in place of the install, or the install's stub `verify` left in
/// front of the check, either of which leaves a host never updated or never checked.
#[test]
fn the_check_is_put_in_front_of_the_work_of_each_kind() {
    let effects = Effects::default();
    let mut linux_work = FakeKind::rebooting(Kind::Linux, NEWER, &effects);
    linux_work.verify = Err(KindError::Failed {
        reason: "work_verify",
        detail: "the work's verify was asked".to_owned(),
    });
    let reboot_work = FakeKind::applying(Kind::Reboot, NEWER, &effects);
    let host = Host {
        kinds: linux::kinds(
            Facts::good().check(&effects),
            Box::new(linux_work),
            Box::new(reboot_work),
        ),
        ..Host::new(&effects, Vec::new())
    };
    let kinds = [WireKind::Linux];
    assert_eq!(
        host.run(HookStep::Prepare, &[WireKind::Linux, WireKind::Reboot]),
        done()
    );
    assert_eq!(taken(&effects), ["linux prepare", "reboot prepare"]);
    assert_eq!(host.run(HookStep::Cutover, &kinds), rebooting("linux"));
    host.boot.set(BOOT_B);
    assert_eq!(host.run(HookStep::Cutover, &kinds), done());
    assert_eq!(
        taken(&effects),
        [
            "linux cutover".to_owned(),
            format!("record {RUN} linux reboot_under_way {BOOT_A}"),
            "reboot".to_owned(),
            "linux after_reboot".to_owned(),
            format!("record {RUN} linux applied {NEWER}"),
        ]
    );
    assert_eq!(host.run(HookStep::Verify, &kinds), done());
    assert_eq!(
        taken(&effects),
        [
            "host Security []",
            "reboot Security []",
            "own packages",
            "packages Security [hostkeeper,js-agent]",
            "services",
        ]
    );
}
