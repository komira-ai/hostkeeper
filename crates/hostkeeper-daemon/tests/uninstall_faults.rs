//! An uninstall whose hook step meets a fault in the launcher or in the service's own files: it
//! ends interrupted with the reason, and the service's package is not removed.

mod support;
mod support_apt;
mod support_host;

use std::fs;

use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_steps::record::PARAMS_FILE;
use support::uninstall;
use support_host::NativeHost;

/// `web` installed at 1.0.0 with an `uninstall` bound, on a host whose tests may delete it.
fn installed(label: &str) -> (NativeHost, hostkeeper_daemon::Daemon) {
    let farm = NativeHost::new(label);
    let daemon = farm.daemon();
    farm.publish_placed("web", "1.0.0", Some(300));
    let done = daemon.order(&farm.install("web", "1.0.0", 1));
    assert!(
        matches!(
            done,
            Reply::Finished {
                verdict: hostkeeper_hook::Verdict::Succeeded { .. },
                ..
            }
        ),
        "{done:?}"
    );
    farm.open_up();
    (farm, daemon)
}

/// A launcher that cannot clear the unit of an earlier run of the step stops the uninstall
/// before the hook runs: `launcher_failed`, no hook run, and the package is still there.
#[test]
fn an_uninstall_whose_launcher_cannot_clear_the_unit_is_interrupted() {
    let (farm, daemon) = installed("uninstall-faults-stop");
    farm.launcher.fail_stops("web");
    let reply = daemon.order(&uninstall("web", 1));
    assert_eq!(reply.refusal(), Some(Refusal::LauncherFailed), "{reply:?}");
    assert!(farm.launcher.additive_runs().is_empty());
    assert!(farm.packages.join("web").exists());
}

/// A params file that cannot be written (a directory is in its place) stops the uninstall before
/// the hook runs: `store_failed`, no hook run, and the package is still there.
#[test]
fn an_uninstall_whose_params_file_cannot_be_written_is_interrupted() {
    let (farm, daemon) = installed("uninstall-faults-params");
    fs::create_dir(farm.state.join("web").join(PARAMS_FILE)).expect("a directory in the way");
    let reply = daemon.order(&uninstall("web", 1));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert!(farm.launcher.additive_runs().is_empty());
    assert!(farm.packages.join("web").exists());
}

/// A params file that cannot be removed after the hook ran (the hook left a directory in its
/// place) ends the uninstall interrupted, `store_failed`; the package is still there.
#[test]
fn an_uninstall_whose_params_file_cannot_be_removed_is_interrupted() {
    let (farm, daemon) = installed("uninstall-faults-remove");
    farm.launcher.during_additive(|call| {
        fs::remove_file(call.params).expect("the params file");
        fs::create_dir_all(call.params.join("held")).expect("a directory in its place");
    });
    let reply = daemon.order(&uninstall("web", 1));
    assert_eq!(reply.refusal(), Some(Refusal::StoreFailed), "{reply:?}");
    assert_eq!(farm.launcher.additive_runs().len(), 1);
    assert!(farm.packages.join("web").exists());
}

/// A service whose uninstall failed keeps its tombstone: an `update` of it is refused
/// `uninstalling`, and the refusal says the release is being uninstalled.
#[test]
fn an_update_of_a_service_being_uninstalled_is_refused_uninstalling() {
    let (farm, daemon) = installed("uninstall-faults-update");
    farm.launcher.additive_exits("web", &[1]);
    let failed = daemon.order(&uninstall("web", 1));
    assert!(
        matches!(
            failed,
            Reply::Finished {
                verdict: hostkeeper_hook::Verdict::Failed { .. },
                ..
            }
        ),
        "{failed:?}"
    );
    farm.publish_placed("web", "2.0.0", None);
    assert_eq!(
        daemon.order(&farm.update("web", "2.0.0", 2)),
        Reply::refused(
            Refusal::Uninstalling,
            "update of web: its installed release is 1.0.0, and it is being uninstalled"
        )
    );
}
