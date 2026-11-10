//! An uninstall stopped at every durable write of its sequence (design section 4.7: "a crash
//! resumes at the tombstone; every step is idempotent"), and the daemon started again on the
//! same directories.

mod support;
mod support_apt;
mod support_hooked;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_daemon::{Outcome, Reply};
use hostkeeper_hook::Verdict;
use hostkeeper_placed::testing::{Event, FaultFs, FsWrite, Log};
use support::uninstall;
use support_hooked::Farm;

/// A host with `web` installed from a package that runs a unit, with an `uninstall` bound.
fn installed(name: &str) -> Farm {
    let farm = Farm::new(name);
    farm.publish_placed("web", "1.0.0", Some(300));
    let reply = farm.daemon().order(&farm.install("web", "1.0.0", 1));
    assert!(
        matches!(
            reply,
            Reply::Finished {
                verdict: Verdict::Succeeded { .. },
                ..
            }
        ),
        "{reply:?}"
    );
    // The uninstall deletes the sealed, read-only version directory, as root does on a host.
    farm.open_up();
    farm
}

/// The paths removed in `log`, files and directories.
fn removed(log: &Log) -> BTreeSet<PathBuf> {
    log.events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Fs(FsWrite::Remove(path) | FsWrite::RemoveDir(path)) => Some(path),
            _ => None,
        })
        .collect()
}

/// Whether nothing of `web` is left on the host: no package directory, state directory,
/// installed package, enablement link, record of its installer or tombstone.
fn gone(farm: &Farm) -> bool {
    let rig = farm.rig();
    let record = rig.placed_record();
    !rig.packages.join("web").exists()
        & !rig.state.join("web").exists()
        & farm.apt.host.installed("web").is_none()
        & !rig.there("/etc/systemd/system/multi-user.target.wants/web.service")
        & !record.installed.contains_key("web")
        & !record.uninstalling.contains_key("web")
}

/// Catches a resumed uninstall that starts again from the beginning, runs the hook again after
/// it was recorded done, removes again what was already removed, or leaves a link or directory:
/// for every n from the first durable write of the uninstall to its last, an uninstall whose
/// n-th write and every later one fail (as a daemon that died there) is followed by a start of
/// the daemon on the same directories. Where the tombstone was durable the start resumes the
/// uninstall without an order and finishes it; where it was not, nothing had changed and the
/// coordinator's resend finishes it. Either way the service is gone, the hook ran again only
/// when its success was not yet recorded, and nothing the first run removed is removed again.
#[test]
fn a_crash_during_uninstall_resumes_at_the_tombstone() {
    // How many writes a whole uninstall makes.
    let farm = installed("crash-count");
    let rig = farm.rig();
    let counting = Arc::new(FaultFs::new(rig.scratch.path(), Log::new()));
    let daemon = farm.daemon_with(counting.clone(), rig.units.clone());
    assert!(matches!(
        daemon.order(&uninstall("web", 1)),
        Reply::Finished {
            verdict: Verdict::Succeeded { .. },
            ..
        }
    ));
    assert!(gone(&farm));
    let total = counting.writes();
    assert!(total > 20, "{total} writes");

    for n in 1..=total {
        let farm = installed(&format!("crash-{n}"));
        let rig = farm.rig();
        let first = Log::new();
        let failing = Arc::new(FaultFs::failing_at(rig.scratch.path(), n, first.clone()));
        let dying = farm.daemon_with(failing, rig.units.clone());
        let reply = dying.order(&uninstall("web", 1));
        assert!(
            !matches!(
                reply,
                Reply::Finished {
                    verdict: Verdict::Succeeded { .. },
                    ..
                }
            ) | gone(&farm),
            "n={n}: {reply:?}"
        );
        drop(dying);
        let tombstone = rig.placed_record().uninstalling.get("web").cloned();
        // The hook runs again unless its success is recorded, or the uninstall had finished.
        let hook_again = match (&tombstone, gone(&farm)) {
            (Some(tombstone), _) => !tombstone.hook_done,
            (None, finished) => !finished,
        };
        let hooks_before = rig.launcher.additive_runs().len();

        let second = Log::new();
        let fs = Arc::new(FaultFs::new(rig.scratch.path(), second.clone()));
        let restarted = farm.daemon_with(fs, rig.units.clone());
        fs::create_dir_all(&rig.state).expect("the state directory");
        let report = restarted.boot_pass().expect("the start pass");
        match &tombstone {
            Some(_) => {
                assert!(
                    matches!(&report.services["web"].outcome, Outcome::Uninstall(done) if matches!(done.verdict, Verdict::Succeeded { .. })),
                    "n={n}: {report:?}"
                );
            }
            None => {
                if !gone(&farm) {
                    assert!(matches!(
                        restarted.order(&uninstall("web", 1)),
                        Reply::Finished {
                            verdict: Verdict::Succeeded { .. },
                            ..
                        }
                    ));
                }
            }
        }
        assert!(gone(&farm), "n={n}: something of web is left");
        let hooks_after = rig.launcher.additive_runs().len() - hooks_before;
        assert_eq!(
            hooks_after,
            usize::from(hook_again),
            "n={n}: the hook's runs after the start"
        );
        let again: Vec<PathBuf> = removed(&first)
            .intersection(&removed(&second))
            .cloned()
            .collect();
        assert_eq!(again, Vec::<PathBuf>::new(), "n={n}: removed twice");
    }
}
