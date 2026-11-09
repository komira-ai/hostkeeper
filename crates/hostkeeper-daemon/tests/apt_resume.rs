//! An `apt` update the daemon did not finish is finished by its next start (design section 6.5,
//! packages-lifecycle.md section 6.1: the start pass rebuilds the order from the sealed
//! `target.json` and its built-in steps); and a package that carries a hook at the fixed path
//! has its steps run by that hook, not by the built-in steps.

mod support;
mod support_apt;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_daemon::Reply;
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::Finished;
use hostkeeper_steps::record::CrashPoint;
use support::crash_at;
use support_apt::seen::{held, install_of, linked, succeeded, update_of};
use support_apt::{AptRig, SERVICE, package};

/// The hook the package carries, at its fixed path in the package's tree.
const HOOK: &str = "usr/lib/web/hostkeeper-hook";

/// Proves an `apt` update with the built-in steps that the daemon died in, after any write of
/// its step record, is finished by the daemon's next start in the same boot without a new order: the start pass rebuilds
/// the order from the sealed version's `target.json`, runs the step it stopped at again (the
/// installer's `activate` is run again where the switch had not been confirmed), and the update
/// ends `Succeeded` with the package at the target version, held, and `current` naming it; the
/// order sent again is answered from the record. A death before the first write leaves nothing
/// to resume, and the order sent again runs it.
///
/// Catches: an `apt` update that the start pass cannot rebuild (a version directory with no
/// `manifest.toml`), that it resumes with the hook path instead of the built-in steps, or that
/// it ends with the package and `current` disagreeing.
#[test]
fn an_apt_update_killed_after_any_write_of_its_record_is_finished_by_the_next_start() {
    let mut writes = 0;
    for n in 1.. {
        let apt = AptRig::new(&format!("apt-resume-{n}"));
        let one = apt.publish(package("1.0.0-1"));
        let two = apt.publish(package("2.0.0-1"));
        // The host's first start writes the boot id: the restart below is in the same boot.
        std::fs::create_dir_all(&apt.rig.state).expect("the state dir");
        std::fs::set_permissions(&apt.rig.state, std::fs::Permissions::from_mode(0o700))
            .expect("its mode");
        let first = apt.daemon();
        let report = first.boot_pass().expect("the first start pass");
        assert!(report.boot_id_written);
        assert_eq!(
            first.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
            succeeded()
        );
        drop(first);
        let count = Arc::new(AtomicU64::new(0));
        let dying = apt.daemon().with_stores(crash_at(
            Some(CrashPoint::AfterWrite(n)),
            Arc::clone(&count),
        ));
        let order = update_of("2.0.0", "2.0.0-1", &two, 2);
        let reply = dying.order(&order);
        drop(dying);
        if count.load(Ordering::SeqCst) < n {
            // Every write of the update was made before the gate: it ran to its end.
            assert_eq!(reply, succeeded(), "after {n} writes");
            writes = n - 1;
            break;
        }
        assert!(
            matches!(reply, Reply::Interrupted { .. }),
            "the daemon died after write {n}: {reply:?}"
        );

        let daemon = apt.daemon();
        daemon.boot_pass().expect("the start pass");
        let ended = apt.rig.verdict(SERVICE);
        assert_eq!(
            ended,
            Some(Finished {
                verdict: Verdict::Succeeded {
                    promotion_pending: false,
                },
                reason: None,
            }),
            "after write {n}"
        );
        assert_eq!(
            apt.host.installed(SERVICE),
            held("2.0.0-1"),
            "after write {n}"
        );
        assert_eq!(linked(&apt, "current").as_deref(), Some("2.0.0"));
        assert_eq!(apt.rig.launcher.steps(SERVICE), Vec::<Step>::new());
        let resent = daemon.order(&order);
        assert!(
            matches!(
                resent,
                Reply::Finished {
                    verdict: Verdict::Succeeded { .. },
                    resent: true,
                    ..
                }
            ),
            "after write {n}: {resent:?}"
        );
    }
    assert!(writes >= 4, "the update wrote its record {writes} times");
}

/// Proves a package that carries a hook at `usr/lib/<package>/hostkeeper-hook` runs its steps by
/// that hook: the update's `prepare`, `cutover`, `verify` and `cleanup` are each the program in
/// the sealed tree, `prepare` given the version directory; and when its `verify` fails, its
/// `rollback` runs after the installer put the previous version back, and the update ends
/// `Failed` at the release gate with the package at the previous version.
///
/// Catches: a package's hook that the daemon never finds (the built-in steps run instead), runs
/// from another path than the sealed tree, or whose failure leaves the new package installed.
#[test]
fn a_package_that_carries_a_hook_runs_its_steps_by_it() {
    let apt = AptRig::new("apt-hook");
    let daemon = apt.daemon();
    let hooked = |version: &str| package(version).file(HOOK, 0o755, "#!/bin/sh\nexit 0\n");
    let one = apt.publish(hooked("1.0.0-1"));
    let two = apt.publish(hooked("2.0.0-1"));
    let three = apt.publish(hooked("3.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    assert_eq!(
        daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 2)),
        succeeded()
    );
    let program = apt.version_dir("2.0.0").join("tree").join(HOOK);
    let ran: Vec<_> = apt
        .rig
        .launcher
        .runs()
        .into_iter()
        .filter(|ran| {
            ran.executable
                .ends_with("2.0.0/tree/usr/lib/web/hostkeeper-hook")
        })
        .collect();
    assert_eq!(
        ran.iter().map(|r| r.step).collect::<Vec<_>>(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    assert!(
        ran.iter()
            .all(|r| r.executable == program.canonicalize().expect("it"))
    );
    assert_eq!(
        ran[0].artifact.as_deref(),
        Some(program.ancestors().nth(5).expect("the version"))
    );
    assert_eq!(apt.host.installed(SERVICE), held("2.0.0-1"));

    apt.rig.launcher.exits(SERVICE, Step::Verify, &[1]);
    assert_eq!(
        daemon.order(&update_of("3.0.0", "3.0.0-1", &three, 3)),
        Reply::Finished {
            verdict: Verdict::Failed {
                phase: Phase::GateRelease,
                class: Class::Release,
            },
            reason: None,
            resent: false,
        }
    );
    let steps = apt.rig.launcher.steps(SERVICE);
    assert_eq!(steps.last(), Some(&Step::Rollback));
    assert_eq!(apt.host.installed(SERVICE), held("2.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("2.0.0"));
}
