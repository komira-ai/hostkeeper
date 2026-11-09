//! An `apt` update the daemon did not finish is finished by its next start (design section 6.5,
//! packages-lifecycle.md section 6.1: the start pass rebuilds the order from the sealed
//! `target.json` and its built-in steps); and an entry whose manifest names a hook command has its
//! steps run by that command's program, taken from the version's sealed tree.

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
use support_apt::seen::{
    held, install_hooked, install_of, linked, succeeded, update_hooked, update_of,
};
use support_apt::{AptRig, HOOK_PROGRAM, SERVICE, hooked_package, package};

/// Where a version staged by an older daemon carries its hook, read for such a version.
const LEGACY_HOOK: &str = "usr/lib/web/hostkeeper-hook";

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

/// The runs of the hook of `version` the launcher was given: its steps, the executable and the
/// arguments given before each step.
fn hook_runs(apt: &AptRig, version: &str) -> Vec<hostkeeper_daemon::testing::Ran> {
    let tree = apt.version_dir(version).join("tree");
    apt.rig
        .launcher
        .runs()
        .into_iter()
        .filter(|ran| {
            ran.executable
                .starts_with(tree.canonicalize().unwrap_or(tree.clone()))
        })
        .collect()
}

/// Proves an entry whose manifest names the hook command `[<program>, "update-hook"]` has each
/// step of its update launched as the sealed copy of that program in the version's tree, with
/// `update-hook` given before the step, in order (`prepare`, `cutover`, `verify`, `cleanup`),
/// whether `<program>` is the file itself or a link to it; and when `verify` fails, `rollback`
/// runs the same program with the same arguments and the package stays at the previous version.
///
/// Catches: a hook command that is ignored (the built-in steps run), launched from a path of the
/// host instead of the sealed tree, launched without its leading arguments, or a link left
/// unfollowed.
#[test]
fn a_manifest_hook_command_runs_the_packages_program_from_the_sealed_tree() {
    for (name, program) in [("file", "/usr/lib/web/web"), ("link", "/usr/bin/web")] {
        let apt = AptRig::new(&format!("apt-hook-{name}"));
        let daemon = apt.daemon();
        let command = [program, "update-hook"];
        let one = apt.publish(hooked_package("1.0.0-1"));
        let two = apt.publish(hooked_package("2.0.0-1"));
        let three = apt.publish(hooked_package("3.0.0-1"));
        assert_eq!(
            daemon.order(&install_hooked("1.0.0", "1.0.0-1", &one, 1, &command)),
            succeeded()
        );
        assert_eq!(
            daemon.order(&update_hooked("2.0.0", "2.0.0-1", &two, 2, &command)),
            succeeded()
        );
        let program_in_tree = apt.version_dir("2.0.0").join("tree").join(HOOK_PROGRAM);
        let ran = hook_runs(&apt, "2.0.0");
        assert_eq!(
            ran.iter().map(|r| r.step).collect::<Vec<_>>(),
            [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup],
            "{name}"
        );
        for step in &ran {
            assert_eq!(
                step.executable,
                program_in_tree.canonicalize().expect("the program"),
                "{name}"
            );
            assert_eq!(step.hook_args, ["update-hook"], "{name}");
        }
        assert_eq!(apt.host.installed(SERVICE), held("2.0.0-1"));

        apt.rig.launcher.exits(SERVICE, Step::Verify, &[1]);
        assert_eq!(
            daemon.order(&update_hooked("3.0.0", "3.0.0-1", &three, 3, &command)),
            Reply::Finished {
                verdict: Verdict::Failed {
                    phase: Phase::GateRelease,
                    class: Class::Release,
                },
                reason: None,
                resent: false,
            }
        );
        let ran = hook_runs(&apt, "3.0.0");
        let last = ran.last().expect("a run");
        assert_eq!(last.step, Step::Rollback, "{name}");
        assert_eq!(
            last.executable,
            apt.version_dir("3.0.0")
                .join("tree")
                .join(HOOK_PROGRAM)
                .canonicalize()
                .expect("the program"),
            "{name}"
        );
        assert_eq!(last.hook_args, ["update-hook"], "{name}");
        assert_eq!(apt.host.installed(SERVICE), held("2.0.0-1"));
        assert_eq!(linked(&apt, "current").as_deref(), Some("2.0.0"));
    }
}

/// Proves an entry whose hook command names a program the package does not install ends the
/// stage `Failed` with the reason `manifest_invalid`, the alert names the command, and nothing is
/// installed, no version directory is made and no step runs.
///
/// Catches: a hook program that is not checked against the package (the order would run built-in
/// steps or fail later at `prepare`), or a stage that leaves the package installed.
#[test]
fn a_hook_command_naming_a_program_the_package_lacks_fails_the_stage() {
    let apt = AptRig::new("apt-hook-missing");
    let (daemon, log) = support::alerting(apt.daemon());
    let one = apt.publish(package("1.0.0-1"));
    let command = ["/usr/lib/web/absent", "update-hook"];
    let reply = daemon.order(&install_hooked("1.0.0", "1.0.0-1", &one, 1, &command));
    let Reply::Finished {
        verdict: Verdict::Failed { .. },
        reason,
        ..
    } = reply
    else {
        panic!("not a failed verdict: {reply:?}");
    };
    assert_eq!(reason.as_deref(), Some("manifest_invalid"));
    let said = support::lines(&log).join("\n");
    assert!(said.contains("/usr/lib/web/absent"), "{said}");
    assert_eq!(apt.host.installed(SERVICE), None);
    assert_eq!(linked(&apt, "current"), None);
    assert!(apt.rig.launcher.runs().is_empty());
}

/// Proves a version staged by an older daemon (a sealed `target.json` with an empty command and
/// the package's program at `usr/lib/<package>/hostkeeper-hook`) is still run by that program,
/// with no leading arguments, when its update is resumed by the next start.
///
/// Catches: a reader that drops the old fixed-path hook and finishes such a version with the
/// built-in steps.
#[test]
fn a_version_staged_with_the_old_fixed_path_hook_still_runs_it_when_resumed() {
    let mut resumed = 0;
    for n in 1.. {
        let apt = AptRig::new(&format!("apt-legacy-hook-{n}"));
        let carrying =
            |version: &str| package(version).file(LEGACY_HOOK, 0o755, "#!/bin/sh\nexit 0\n");
        let one = apt.publish(carrying("1.0.0-1"));
        let two = apt.publish(carrying("2.0.0-1"));
        std::fs::create_dir_all(&apt.rig.state).expect("the state dir");
        std::fs::set_permissions(&apt.rig.state, std::fs::Permissions::from_mode(0o700))
            .expect("its mode");
        let first = apt.daemon();
        first.boot_pass().expect("the first start pass");
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
        let reply = dying.order(&update_of("2.0.0", "2.0.0-1", &two, 2));
        drop(dying);
        if count.load(Ordering::SeqCst) < n {
            assert_eq!(reply, succeeded());
            break;
        }
        let before = hook_runs(&apt, "2.0.0").len();
        apt.daemon().boot_pass().expect("the start pass");
        let after = hook_runs(&apt, "2.0.0");
        let legacy_program = apt.version_dir("2.0.0").join("tree").join(LEGACY_HOOK);
        for ran in &after[before..] {
            assert_eq!(
                ran.executable,
                legacy_program.canonicalize().expect("the program")
            );
            assert!(ran.hook_args.is_empty());
        }
        resumed += after.len() - before;
        assert_eq!(
            apt.host.installed(SERVICE),
            held("2.0.0-1"),
            "after write {n}"
        );
    }
    assert!(resumed > 0, "no start resumed a step by the old hook");
}
