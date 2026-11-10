//! hostkeeper's own update on a host the deb or the pkg bootstrapped, through the daemon: no
//! package of hostkeeper is installed there, so an `update` of it is refused and the coordinator
//! orders an `install`, which the daemon runs as the self-update.

mod support;

use hostkeeper_daemon::own_package::OwnActivate;
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::{Class, Step, Verdict};
use hostkeeper_placed::testing::{Event, UnitCall};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_steps::Settled;
use hostkeeper_steps::record::{Activated, State};
use support::apt::with_entry;
use support::selfupdate::{
    DEB_UNIT, DEB_WANTS, HK, NEW, OLD, SelfRig, awaiting, pv, refused_before_commit,
};
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

/// Catches a bootstrapped host whose install of hostkeeper installs the package before its new
/// binary runs (the coordinator would then read the host at the new version while the old one
/// runs), and an install whose success the old binary writes: the install of [`NEW`] runs the
/// release's steps (built in: the package has no hook), installs nothing (`current` of the
/// packages directory names none, the package manager was run for no `apt-get install`), stages
/// the new binary and links `next` at it, leaving `current` on the old binary, and is answered
/// `Deferred` `awaiting_restart` with the daemon asked to exit and the record waiting; a resend
/// is answered the same and runs nothing. The binary of [`NEW`] finishes the switch and confirms
/// it at its start, then installs the package (`current` of the packages directory names
/// [`NEW`]), and a resend is then answered `Succeeded` from the record.
#[test]
fn an_install_of_hostkeeper_on_a_bootstrapped_host_runs_the_self_update() {
    let s = SelfRig::bootstrapped("su-install-bootstrapped");
    assert_eq!(installed(&s), None);
    let daemon = s.daemon();
    let order = s.install_order(NEW, 1);

    assert_eq!(daemon.order(&order), awaiting(false));
    assert!(daemon.ctx().restart().requested());
    assert_eq!(s.hk_steps(), [], "the package's steps are built in");
    assert_eq!(
        installed(&s),
        None,
        "no package is installed before the new binary runs"
    );
    assert_eq!(s.host.installed(HK), None);
    assert!(!s.host.labels().contains(&"apt-get install".to_owned()));
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().next().expect("next").as_deref(), Some(NEW));
    let rec = s.rig.record(HK).expect("the record");
    assert_eq!(rec.step, Step::Cleanup, "{rec:?}");
    assert!(matches!(rec.state, State::Done { .. }), "{rec:?}");
    assert_eq!(
        rec.progress.awaiting_restart.map(|a| a.version).as_deref(),
        Some(NEW)
    );
    let ran = s.tool_runs();
    assert_eq!(daemon.order(&order), awaiting(true));
    assert_eq!(s.tool_runs(), ran, "a resend runs nothing");

    let new = s.daemon_at(NEW);
    assert_eq!(
        new.settle_self_update().expect("settled"),
        Settled::Confirmed
    );
    assert_eq!(s.current().as_deref(), Some(NEW));
    assert_eq!(s.layout().next().expect("next"), None);
    assert_eq!(
        new.activate_own_package(),
        Some(OwnActivate::Ended(Activated::Succeeded { reason: None }))
    );
    assert_eq!(installed(&s).as_deref(), Some(NEW));
    assert_eq!(s.host.installed(HK).map(|inst| inst.version), Some(pv(NEW)));
    assert_eq!(new.order(&order), succeeded(true));
}

/// Catches an install of the release that already runs that restarts the daemon or is never
/// recorded (its success held back for a binary that already runs): the daemon of [`OLD`] with no
/// package of hostkeeper is ordered the install of [`OLD`]; the steps run, the package is
/// installed, the binary directory is left as it is (nothing staged, no `next` linked), and the
/// install is `Succeeded` at once, with no restart.
#[test]
fn an_install_of_the_running_version_succeeds_without_a_restart() {
    let s = SelfRig::bootstrapped("su-install-running");
    s.release(OLD);
    let daemon = s.daemon();

    assert_eq!(daemon.order(&s.install_order(OLD, 1)), succeeded(false));
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
    assert_eq!(s.tool_runs(), 0, "nothing was staged");

    let s = SelfRig::new("su-install-refusals-installed");
    let reply = s.daemon().order(&s.install_order(NEW, 1));
    assert_eq!(
        reply.refusal(),
        Some(Refusal::AlreadyInstalled),
        "{reply:?}"
    );
    assert_eq!(s.hk_steps(), []);
    assert_eq!(s.tool_runs(), 0, "nothing was staged");
    assert_eq!(s.current().as_deref(), Some(OLD));
}

/// Catches a failed first install of hostkeeper that stops the daemon's own unit (systemd does
/// not start a unit again after a stop, so the host would be left with no daemon) or takes the
/// deb's enable link with it: on a host the deb enabled, an install whose entry pins a package the
/// repository does not hold is `Failed` before the commit, with nothing installed and no unit
/// stopped; the deb's `WantedBy=` link still points where the deb made it, no unit link of the
/// package is placed, `placed.json` lists nothing, and the binary directory stays on the running
/// binary, with no restart asked.
#[test]
fn a_failed_first_install_of_hostkeeper_stops_no_unit_and_keeps_the_debs_enable_link() {
    let s = SelfRig::deb("su-install-unpinned");
    let daemon = s.daemon();
    s.rig.placed_log.clear();

    let unpinned = Target {
        digest: d(),
        ..s.entry_of(HK, NEW)
    };
    let order = with_entry(install(HK, NEW, &d(), 1), &unpinned);
    let reply = daemon.order(&order);
    assert_eq!(
        reply,
        refused_before_commit(Class::Release, "package_digest_mismatch")
    );
    assert_eq!(s.hk_steps(), []);
    let stops: Vec<Event> = s
        .rig
        .placed_log
        .events()
        .into_iter()
        .filter(|event| matches!(event, Event::Unit(UnitCall::Stop(_))))
        .collect();
    assert_eq!(stops, []);
    assert_eq!(
        std::fs::read_link(s.rig.host(DEB_WANTS)).ok(),
        Some(DEB_UNIT.into())
    );
    assert!(!s.rig.there("/units/hostkeeper.service"));
    assert_eq!(s.rig.placed_record().installer_of(HK), None);
    assert_eq!(s.host.installed(HK), None);
    assert_eq!(installed(&s), None);
    assert_eq!(s.current().as_deref(), Some(OLD));
    assert_eq!(s.layout().next().expect("next"), None);
    assert!(!daemon.ctx().restart().requested());
}
