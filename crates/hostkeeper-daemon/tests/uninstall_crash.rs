//! An uninstall stopped at every durable write of its sequence (design section 4.7: "a crash
//! resumes at the tombstone; every step is idempotent"), and the daemon started again on the
//! same directories.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_daemon::{Outcome, Reply};
use hostkeeper_hook::Verdict;
use hostkeeper_placed::testing::{Event, FaultFs, FsWrite, Log};
use support::{Rig, d, install, uninstall};

/// A rig with `web` installed from a package that places a unit, its `WantedBy=` link and a
/// file, with an `uninstall` bound.
fn installed(name: &str) -> Rig {
    let rig = Rig::new(name);
    rig.placed("web", "1.0.0", Some(300));
    let reply = rig.daemon().order(&install("web", "1.0.0", &d(), 1));
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
    rig
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

/// Whether nothing of `web` is left on the host.
fn gone(rig: &Rig) -> bool {
    let record = rig.placed_record();
    let links = rig
        .placed_paths("web")
        .iter()
        .any(|path| fs::symlink_metadata(path).is_ok());
    !rig.packages.join("web").exists()
        & !rig.state.join("web").exists()
        & !links
        & record.of_service("web").next().is_none()
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
    let rig = installed("crash-count");
    let counting = Arc::new(FaultFs::new(Path::new("/"), Log::new()));
    let daemon = rig.daemon_on(rig.placement_on(counting.clone(), rig.units.clone()));
    assert!(matches!(
        daemon.order(&uninstall("web", 1)),
        Reply::Finished {
            verdict: Verdict::Succeeded { .. },
            ..
        }
    ));
    assert!(gone(&rig));
    let total = counting.writes();
    assert!(total > 20, "{total} writes");

    for n in 1..=total {
        let rig = installed(&format!("crash-{n}"));
        let first = Log::new();
        let failing = Arc::new(FaultFs::failing_at(Path::new("/"), n, first.clone()));
        let dying = rig.daemon_on(rig.placement_on(failing, rig.units.clone()));
        let reply = dying.order(&uninstall("web", 1));
        assert!(
            !matches!(
                reply,
                Reply::Finished {
                    verdict: Verdict::Succeeded { .. },
                    ..
                }
            ) | gone(&rig),
            "n={n}: {reply:?}"
        );
        drop(dying);
        let tombstone = rig.placed_record().uninstalling.get("web").cloned();
        // The hook runs again unless its success is recorded, or the uninstall had finished.
        let hook_again = match (&tombstone, gone(&rig)) {
            (Some(tombstone), _) => !tombstone.hook_done,
            (None, finished) => !finished,
        };
        let hooks_before = rig.launcher.additive_runs().len();

        let second = Log::new();
        let fs = Arc::new(FaultFs::new(Path::new("/"), second.clone()));
        let restarted = rig.daemon_on(rig.placement_on(fs, rig.units.clone()));
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
                if !gone(&rig) {
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
        assert!(gone(&rig), "n={n}: something of web is left");
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
