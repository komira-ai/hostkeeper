//! The hookless `force` of an `apt` service (design section 5.6, packages-repositories.md
//! section 4): the version is staged, or found sealed, by the repository installer, step 11
//! activates it through the installer and switches `current`, and the links a package would
//! place are neither made nor removed.

mod support;
mod support_apt;

use hostkeeper_daemon::Reply;
use hostkeeper_hook::{Class, Verdict};
use hostkeeper_placed::testing::UnitCall;
use support::force;
use support_apt::seen::{
    failed, held, install_of, labels_since, linked, recorded, succeeded, unit_calls, update_of,
};
use support_apt::{AptRig, SERVICE, UNIT, entry, package, with_entry};

/// Proves a `force` of an apt service to its kept previous version downgrades from the sealed
/// copy: nothing is fetched from the repository, the installer's `activate` installs the older
/// package and holds it, `current` and `previous` swap, the unit is stopped and started again,
/// no link is made or removed, and the verdict is `Succeeded`.
///
/// Catches: a force that fetches the package again, switches `current` without installing the
/// package, or links the files of a package the package manager owns.
#[test]
fn a_force_to_the_previous_apt_version_installs_it_from_its_sealed_copy() {
    let apt = AptRig::new("apt-force-previous");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    let two = apt.publish(package("2.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    assert_eq!(
        daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 2)),
        succeeded()
    );
    apt.host.empty_repositories();
    apt.rig.placed_log.clear();
    let calls = apt.host.calls().len();
    let order = with_entry(force(SERVICE, "1.0.0", &one, 3, 0), &entry("1.0.0-1", &one));
    assert_eq!(daemon.order(&order), succeeded());
    let tools = labels_since(&apt, calls);
    assert!(
        !tools
            .iter()
            .any(|l| l == "apt-get update" || l == "apt-get download"),
        "nothing was fetched: {tools:?}"
    );
    assert!(tools.contains(&"apt-get install".to_owned()), "{tools:?}");
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
    assert_eq!(linked(&apt, "previous").as_deref(), Some("2.0.0"));
    assert_eq!(
        recorded(&daemon).and_then(|r| r.version).as_deref(),
        Some("1.0.0-1")
    );
    let units = unit_calls(&apt);
    assert_eq!(units.first(), Some(&UnitCall::Stop(UNIT.to_owned())));
    assert_eq!(units.last(), Some(&UnitCall::Start(UNIT.to_owned())));
    let calls = support::force::calls(&apt.rig);
    assert!(
        calls
            .iter()
            .all(|c| !c.starts_with("link ") && !c.starts_with("unlink ")),
        "no link was made or removed: {calls:?}"
    );
}

/// Proves a `force` of an apt service to a version that cannot be staged ends before the
/// commit point with the stage's own verdict and changes nothing: a version the index does not
/// list is `Deferred`, `not_in_repository`; an entry naming a repository the host does not
/// configure is `Failed` before the commit, `environment`, `repository_unknown`; no package is
/// installed, and the installed package, `current` and the version directories stay.
///
/// Catches: a force whose stage fails that is refused, or that goes on to switch `current` to a version that is not installed.
#[test]
fn a_force_whose_stage_fails_is_failed_before_commit_and_changes_nothing() {
    let apt = AptRig::new("apt-force-stage-fails");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    let calls = apt.host.calls().len();
    let unlisted = with_entry(force(SERVICE, "1.1.0", &one, 2, 0), &entry("1.1.0-1", &one));
    let reply = daemon.order(&unlisted);
    let Reply::Finished {
        verdict: Verdict::Deferred { .. },
        reason,
        ..
    } = &reply
    else {
        panic!("not deferred: {reply:?}");
    };
    assert_eq!(reason.as_deref(), Some("not_in_repository"));
    let mut elsewhere = entry("1.0.0-1", &one);
    elsewhere.repository = Some("elsewhere".to_owned());
    let unknown = with_entry(force(SERVICE, "1.2.0", &one, 3, 0), &elsewhere);
    assert_eq!(
        daemon.order(&unknown),
        failed(Class::Environment, "repository_unknown")
    );
    let tools = labels_since(&apt, calls);
    assert!(!tools.contains(&"apt-get install".to_owned()), "{tools:?}");
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
    assert_eq!(linked(&apt, "current").as_deref(), Some("1.0.0"));
    assert!(!apt.version_dir("1.1.0").exists());
}

/// Proves a force that installs over drift leaves no `drift_repaired` behind for a later order:
/// a package moved by hand is put back by a force, and the next update, which finds the package
/// where its record says, ends `Succeeded` with no reason.
///
/// Catches: a drift that a force repaired being reported by the next update, which repaired
/// nothing.
#[test]
fn a_force_that_repairs_drift_leaves_no_reason_for_the_next_update() {
    let apt = AptRig::new("apt-force-drift");
    let daemon = apt.daemon();
    let one = apt.publish(package("1.0.0-1"));
    let two = apt.publish(package("2.0.0-1"));
    assert_eq!(
        daemon.order(&install_of("1.0.0", "1.0.0-1", &one, 1)),
        succeeded()
    );
    apt.host.set_installed(SERVICE, "0.9.0-1", 'i', false);
    let order = with_entry(force(SERVICE, "1.0.0", &one, 2, 0), &entry("1.0.0-1", &one));
    assert_eq!(daemon.order(&order), succeeded());
    assert_eq!(apt.host.installed(SERVICE), held("1.0.0-1"));
    assert_eq!(
        daemon.order(&update_of("2.0.0", "2.0.0-1", &two, 3)),
        succeeded()
    );
}
