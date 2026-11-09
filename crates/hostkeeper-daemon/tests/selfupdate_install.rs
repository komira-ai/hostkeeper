//! hostkeeper's own update on a host the deb or the pkg bootstrapped, through the daemon: no
//! package of hostkeeper is installed there, so an `update` of it is refused and the coordinator
//! orders an `install`, which the daemon runs as the self-update.

mod support;

use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_placed::testing::{Event, UnitCall};
use hostkeeper_steps::Settled;
use hostkeeper_steps::record::State;
use support::selfupdate::{DEB_UNIT, DEB_WANTS, HK, NEW, OLD, SelfRig, awaiting};
use support::{d, install};

/// The version the packages directory's `current` of hostkeeper names, if any.
fn installed(s: &SelfRig) -> Option<String> {
    std::fs::read_link(s.rig.packages.join(HK).join("current"))
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

fn succeeded(resent: bool) -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent,
    }
}

/// Catches a bootstrapped host whose install of hostkeeper places the package and leaves the old
/// binary running (the coordinator would then read the host at the new version while the old
/// one runs), and an install whose success the old binary writes: the install of [`NEW`] runs the
/// release's steps, places the package (`current` of the packages directory names [`NEW`]),
/// stages the new binary and links `next` at it, leaving `current` on the old binary, and is
/// answered `Deferred` `awaiting_restart` with the daemon asked to exit and the record waiting; a
/// resend is answered the same and runs nothing. The binary of [`NEW`] finishes the switch and
/// confirms it at its start, and a resend is then answered `Succeeded` from the record.
#[test]
fn an_install_of_hostkeeper_on_a_bootstrapped_host_runs_the_self_update() {
    let s = SelfRig::bootstrapped("su-install-bootstrapped");
    assert_eq!(installed(&s), None);
    let daemon = s.daemon();
    let order = install(HK, NEW, &d(), 1);

    assert_eq!(daemon.order(&order), awaiting(false));
    assert!(daemon.ctx().restart().requested());
    assert_eq!(
        s.hk_steps(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    assert_eq!(installed(&s).as_deref(), Some(NEW));
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().next().expect("next").as_deref(), Some(NEW));
    let rec = s.rig.record(HK).expect("the record");
    assert!(matches!(rec.state, State::Done { .. }), "{rec:?}");
    assert_eq!(
        rec.progress.awaiting_restart.map(|a| a.version).as_deref(),
        Some(NEW)
    );
    assert_eq!(daemon.order(&order), awaiting(true));
    assert_eq!(s.hk_steps().len(), 4);

    let new = s.daemon_at(NEW);
    assert_eq!(
        new.settle_self_update().expect("settled"),
        Settled::Confirmed
    );
    assert_eq!(new.order(&order), succeeded(true));
    assert_eq!(s.current().as_deref(), Some(NEW));
    assert_eq!(s.layout().next().expect("next"), None);
}

/// Catches an install of the release that already runs that restarts the daemon or is never
/// recorded (its success held back for a binary that already runs): the daemon of [`OLD`] with no
/// package of hostkeeper is ordered the install of [`OLD`]; the steps run, the package is placed,
/// the binary directory is left as it is (nothing staged, no `next` linked), and the install is
/// `Succeeded` at once, with no restart.
#[test]
fn an_install_of_the_running_version_succeeds_without_a_restart() {
    let s = SelfRig::bootstrapped("su-install-running");
    s.release(OLD);
    let daemon = s.daemon();

    assert_eq!(daemon.order(&install(HK, OLD, &d(), 1)), succeeded(false));
    assert!(!daemon.ctx().restart().requested());
    assert_eq!(installed(&s).as_deref(), Some(OLD));
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().next().expect("next"), None);
    let rec = s.rig.record(HK).expect("the record");
    assert!(
        matches!(
            rec.state,
            State::Finished {
                verdict: Verdict::Succeeded { .. },
                ..
            }
        ),
        "{rec:?}"
    );
}

/// Catches the order the coordinator must give each kind of host going wrong: a bootstrapped
/// host refuses an `update` of hostkeeper (`not_installed`, nothing run), which is why it is
/// ordered an install; a host whose package of hostkeeper is installed refuses an `install`
/// (`already_installed`, nothing run), which is why it is ordered an update.
#[test]
fn hostkeeper_takes_an_install_only_without_its_package_and_an_update_only_with_it() {
    let s = SelfRig::bootstrapped("su-install-refusals-bootstrapped");
    let reply = s.daemon().order(&s.order(NEW, 1));
    assert_eq!(reply.refusal(), Some(Refusal::NotInstalled), "{reply:?}");
    assert_eq!(s.hk_steps(), []);

    let s = SelfRig::new("su-install-refusals-installed");
    let reply = s.daemon().order(&install(HK, NEW, &d(), 1));
    assert_eq!(
        reply.refusal(),
        Some(Refusal::AlreadyInstalled),
        "{reply:?}"
    );
    assert_eq!(s.hk_steps(), []);
    assert_eq!(s.current().as_deref(), Some(OLD));
}

/// Catches a failed first install of hostkeeper that stops the daemon's own unit (systemd does
/// not start a unit again after a stop, so the host would be left with no daemon) or takes the
/// deb's enable link with it: on a host the deb enabled, an install whose `verify` fails is
/// rolled back with no unit stopped; the unit link it placed and `current` go, `placed.json`
/// lists nothing, the deb's `WantedBy=` link still points where the deb made it, and the binary
/// directory stays on the running binary, with no restart asked.
#[test]
fn a_failed_first_install_of_hostkeeper_stops_no_unit_and_keeps_the_debs_enable_link() {
    let s = SelfRig::deb("su-install-rollback");
    s.rig.launcher.exits(HK, Step::Verify, &[1]);
    let daemon = s.daemon();
    s.rig.placed_log.clear();

    let reply = daemon.order(&install(HK, NEW, &d(), 1));
    assert!(
        matches!(
            reply,
            Reply::Finished {
                verdict: Verdict::Failed { .. },
                ..
            }
        ),
        "{reply:?}"
    );
    assert_eq!(
        s.hk_steps(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Rollback]
    );
    let stops: Vec<Event> = s
        .rig
        .placed_log
        .events()
        .into_iter()
        .filter(|event| matches!(event, Event::Unit(UnitCall::Stop(_))))
        .collect();
    assert_eq!(stops, []);
    assert!(
        s.rig
            .placed_log
            .events()
            .contains(&Event::Unit(UnitCall::DaemonReload))
    );
    assert_eq!(
        std::fs::read_link(s.rig.host(DEB_WANTS)).ok(),
        Some(DEB_UNIT.into())
    );
    assert!(!s.rig.there("/units/hostkeeper.service"));
    assert_eq!(installed(&s), None);
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert!(!daemon.ctx().restart().requested());
}
