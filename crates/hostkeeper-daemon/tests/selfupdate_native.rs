//! hostkeeper's own update through its `.deb` (docs/design/packages-native.md, section 10.4):
//! the new binary is staged from the version's tree and handed over to, and only once it has
//! started and confirmed itself does it install its own package, hold it and record it; the
//! verdict is reported after that `activate`. Its failure and its deferral, the bootstrap install
//! adopted at the start, and the refusal of a rollback below an installer the host holds
//! (section 10.6).

mod support;
mod support_apt;
mod support_native;

use std::fs;
use std::sync::Arc;

use hostkeeper_daemon::Reply;
use hostkeeper_daemon::own_package::{OwnActivate, REASON_ACTIVATE_PENDING};
use hostkeeper_daemon::refusal::Refusal;
use hostkeeper_daemon::start::retry_own_activate;
use hostkeeper_hook::Verdict;
use hostkeeper_placed::testing::{Event, UnitCall};
use hostkeeper_steps::Settled;
use hostkeeper_steps::record::{Activated, State};

use support::selfupdate::{HK, awaiting, fell_back};
use support_apt::seen::held;
use support_native::{
    DEB_UNIT_NAME, NativeRig, V1, V2, V3, activate_failed, binary_text, install_hk, pv,
    rollback_hk, succeeded, succeeded_state, update_hk,
};

/// The units the daemon asked the service manager to stop or start, in order.
fn unit_calls(nr: &NativeRig) -> Vec<UnitCall> {
    nr.rig
        .placed_log
        .events()
        .into_iter()
        .filter_map(|event| match event {
            Event::Unit(call @ (UnitCall::Stop(_) | UnitCall::Start(_))) => Some(call),
            _ => None,
        })
        .collect()
}

/// A host set up from hostkeeper's `.deb` of [`V1`], whose daemon adopted it at its start, with
/// the `.deb` of [`V2`] published.
fn adopted(name: &str) -> (NativeRig, String) {
    let nr = NativeRig::new(name);
    nr.host.set_installed(HK, &pv(V1), 'i', false);
    let started = nr.start(V1);
    assert_eq!(
        started.boot_report().adopted,
        Some(Ok(Some(pv(V1)))),
        "the bootstrap install is adopted"
    );
    drop(started);
    let pin = nr.publish(V2);
    (nr, pin)
}

/// The hand-over of an update of hostkeeper to [`V2`] through its `.deb`: the new binary is
/// staged from the version's tree, `next` names it, and nothing of the package changes before
/// the new binary runs.
fn handed_over(nr: &NativeRig, pin: &str) {
    let calls = nr.calls();
    let reply = nr.daemon_at(V1).order(&install_hk(V2, pin, 1));
    assert_eq!(reply, awaiting(false));
    let labels = nr.labels_since(calls);
    assert!(
        !labels.iter().any(|label| label == "apt-get install"),
        "nothing installed before the new binary ran: {labels:?}"
    );
    assert_eq!(nr.layout().next().unwrap().as_deref(), Some(V2));
    assert_eq!(
        fs::read_to_string(nr.layout().version_path(V2)).unwrap(),
        binary_text(V2),
        "the binary staged is the package's /usr/bin/hostkeeper"
    );
    assert_eq!(nr.layout().current().unwrap().as_deref(), Some(V1));
}

/// Proves the order of 10.4: an order of hostkeeper's `.deb` stages the new binary from
/// `tree/usr/bin/hostkeeper` and hands over to it with the package untouched; only the new
/// binary, once it confirmed itself at its start, runs `apt-get install hostkeeper=<V>`, holds
/// the package and records it, switches `current` of the packages directory, and records the
/// update `Succeeded` under the order's id; the running daemon's unit is neither stopped nor
/// started.
///
/// Catches: a package installed before the new binary proved it starts (the unit would start an
/// unproven binary for ever), or a self-update that restarts its own unit.
#[test]
fn the_new_binary_installs_its_package_only_after_it_confirmed_itself() {
    let (nr, pin) = adopted("native-order");
    handed_over(&nr, &pin);
    assert_eq!(nr.host.installed(HK), held(&pv(V1)));
    assert_eq!(nr.linked("current"), None);

    let calls = nr.calls();
    let started = nr.start(V2);
    let report = started.boot_report();
    assert_eq!(report.self_update, Some(Ok(Settled::Confirmed)));
    assert_eq!(
        report.own_activate,
        Some(OwnActivate::Ended(Activated::Succeeded { reason: None }))
    );
    assert!(
        nr.labels_since(calls)
            .contains(&"apt-get install".to_owned())
    );
    assert_eq!(nr.host.installed(HK), held(&pv(V2)));
    let installed = nr.installed().expect("hostkeeper is recorded");
    assert_eq!(
        (installed.installer.as_str(), installed.version.as_deref()),
        ("apt", Some(pv(V2).as_str()))
    );
    assert_eq!(nr.linked("current").as_deref(), Some(V2));
    assert_eq!(nr.layout().current().unwrap().as_deref(), Some(V2));
    let rec = nr.record().expect("the record");
    assert_eq!(rec.state, succeeded_state());
    assert_eq!(rec.progress.activate, None);
    assert_eq!(
        rec.progress.report_order_id.as_deref(),
        Some("install-hostkeeper-1.1.0")
    );
    assert!(
        !unit_calls(&nr).iter().any(|call| matches!(
            call,
            UnitCall::Stop(unit) | UnitCall::Start(unit) if unit == DEB_UNIT_NAME
        )),
        "the daemon's unit is not restarted: {:?}",
        unit_calls(&nr)
    );
}

/// Proves a new binary that never starts changes nothing of the package: the old binary's start
/// falls back (`Failed`, `self_update_fallback`, the version bad), runs no `apt-get install`,
/// and the package stays held at the version it was.
#[test]
fn a_new_binary_that_never_ran_leaves_the_package_as_it_was() {
    let (nr, pin) = adopted("native-fallback");
    handed_over(&nr, &pin);
    let calls = nr.calls();
    let started = nr.start(V1);
    assert_eq!(
        started.boot_report().self_update,
        Some(Ok(Settled::FellBack))
    );
    assert_eq!(started.boot_report().own_activate, None);
    assert!(
        !nr.labels_since(calls)
            .contains(&"apt-get install".to_owned())
    );
    assert_eq!(nr.host.installed(HK), held(&pv(V1)));
    let rec = nr.record().expect("the record");
    let (verdict, reason) = fell_back();
    assert_eq!(rec.state, State::Finished { verdict, reason });
    assert_eq!(nr.linked("current"), None);
}

/// Proves a failed `activate` after the confirmation: the update is `Failed` after the commit
/// with the installer's reason (`dpkg_error`), the new binary keeps running and is not rolled
/// back, the record of the package is written whole again so the heartbeat reports the drift
/// (the package database still at the old version), and a `force_rollback` hands back to the
/// old version from its sealed copy, whose package is still the installed one.
#[test]
fn a_failed_activate_leaves_the_new_binary_running_and_a_force_rollback_hands_back() {
    let nr = NativeRig::new("native-activate-fails");
    let one = nr.publish(V1);
    assert_eq!(
        nr.daemon_at(V1).order(&install_hk(V1, &one, 1)),
        succeeded()
    );
    let pin = nr.publish(V2);
    assert_eq!(
        nr.daemon_at(V1).order(&update_hk(V2, &pin, 2)),
        awaiting(false)
    );
    nr.host.edit(|s| s.faults.noop = true);
    let started = nr.start(V2);
    assert!(matches!(
        started.boot_report().own_activate,
        Some(OwnActivate::Ended(Activated::Failed { ref reason, .. })) if reason == "dpkg_error"
    ));
    assert_eq!(nr.record().unwrap().state, activate_failed("dpkg_error"));
    assert_eq!(nr.layout().current().unwrap().as_deref(), Some(V2));
    assert_eq!(nr.linked("current").as_deref(), Some(V1));
    assert_eq!(nr.host.installed(HK), held(&pv(V1)));
    let installed = nr.installed().expect("the record of the package");
    assert!(
        !installed.pending,
        "the drift is reported, not hidden as pending"
    );
    assert_eq!(installed.version.as_deref(), Some(pv(V2).as_str()));
    let daemon = Arc::clone(started.daemon());
    drop(started);
    nr.host.edit(|s| s.faults.noop = false);

    let calls = nr.calls();
    let reply = daemon.order(&rollback_hk(V1, &one, 3));
    assert_eq!(reply, awaiting(false));
    assert_eq!(nr.layout().next().unwrap().as_deref(), Some(V1));
    assert!(
        !nr.labels_since(calls)
            .iter()
            .any(|l| l.starts_with("apt-get")),
        "the old version is taken from its sealed copy"
    );
}

/// Proves an `activate` the package manager defers (its lock held) stays pending: the start's
/// try is `Deferred` `package_manager_busy` and the record keeps the `activate` and no verdict;
/// an order for another version is `busy`; an order for the version runs the `activate` again
/// and is answered with its verdict; the retry the start leaves running tries every
/// [`hostkeeper_daemon::own_package::DEFER_SECS`] of the daemon's clock until it ends.
#[test]
fn a_deferred_activate_is_tried_again_until_it_is_done() {
    let (nr, pin) = adopted("native-activate-busy");
    handed_over(&nr, &pin);
    nr.host.edit(|s| s.faults.busy.insert("install", 1));
    // The start's own retry sleeps first: it stays parked there while the test looks.
    let parked = nr.rig.clock.park_next_sleep();
    let started = nr.start(V2);
    assert!(matches!(
        started.boot_report().own_activate,
        Some(OwnActivate::Deferred { ref reason, .. }) if reason == "package_manager_busy"
    ));
    let rec = nr.record().expect("the record");
    assert_eq!(
        rec.progress.activate.as_ref().map(|p| p.version.as_str()),
        Some(V2)
    );
    assert!(!matches!(rec.state, State::Finished { .. }));
    assert_eq!(nr.host.installed(HK), held(&pv(V1)));

    let daemon = Arc::clone(started.daemon());
    let other = nr.publish(V3);
    assert!(matches!(
        daemon.order(&update_hk(V3, &other, 1)),
        Reply::Refused {
            refusal: Refusal::Busy,
            ..
        }
    ));
    nr.host.edit(|s| s.faults.busy.insert("install", 1));
    assert_eq!(
        daemon.order(&install_hk(V2, &pin, 2)),
        Reply::Finished {
            verdict: Verdict::Deferred {
                retry_after_secs: Some(60),
            },
            reason: Some(REASON_ACTIVATE_PENDING.to_owned()),
            resent: false,
        }
    );
    nr.host.edit(|s| s.faults.busy.insert("install", 2));
    let before = hostkeeper_steps::Clock::mono_secs(&*nr.rig.clock);
    let weak = Arc::downgrade(&daemon);
    retry_own_activate(&weak, &*nr.rig.clock);
    assert_eq!(
        hostkeeper_steps::Clock::mono_secs(&*nr.rig.clock) - before,
        3 * 60,
        "two more deferrals, then the try that installs"
    );
    assert_eq!(nr.record().unwrap().state, succeeded_state());
    assert_eq!(nr.host.installed(HK), held(&pv(V2)));
    drop((started, daemon));
    drop(parked);
}

/// Proves the retry the start leaves running ends with the daemon: once the daemon is gone,
/// its next tick does nothing.
#[test]
fn the_retry_of_an_activate_ends_with_the_daemon() {
    let nr = NativeRig::new("native-retry-gone");
    let weak = {
        let daemon = Arc::new(nr.daemon_at(V1));
        Arc::downgrade(&daemon)
    };
    let before = hostkeeper_steps::Clock::mono_secs(&*nr.rig.clock);
    retry_own_activate(&weak, &*nr.rig.clock);
    assert_eq!(
        hostkeeper_steps::Clock::mono_secs(&*nr.rig.clock) - before,
        60
    );
    assert!(nr.host.calls().is_empty());
}

/// Proves a crash between the confirmation and the end of the `activate` is resumed: the
/// confirmation alone (the daemon stopped before its `activate`) leaves it pending, and the next
/// start of the same binary installs the package and ends the update `Succeeded`.
#[test]
fn a_crash_after_the_confirmation_is_resumed_by_the_next_start() {
    let (nr, pin) = adopted("native-crash-confirm");
    handed_over(&nr, &pin);
    let settled = nr.daemon_at(V2).settle_self_update().unwrap();
    assert_eq!(settled, Settled::Confirmed);
    assert!(nr.record().unwrap().progress.activate.is_some());
    assert_eq!(nr.host.installed(HK), held(&pv(V1)));
    let started = nr.start(V2);
    assert_eq!(
        started.boot_report().self_update,
        Some(Ok(Settled::NotPending))
    );
    assert_eq!(
        started.boot_report().own_activate,
        Some(OwnActivate::Ended(Activated::Succeeded { reason: None }))
    );
    assert_eq!(nr.host.installed(HK), held(&pv(V2)));
    assert_eq!(nr.record().unwrap().state, succeeded_state());
}

/// Proves an install of the version that runs needs no restart: its package is installed at
/// once, under the order, and the order is answered `Succeeded`.
#[test]
fn an_install_of_the_version_that_runs_activates_its_package_under_the_order() {
    let nr = NativeRig::new("native-install-running");
    let pin = nr.publish(V1);
    let reply = nr.daemon_at(V1).order(&install_hk(V1, &pin, 1));
    assert_eq!(reply, succeeded());
    assert_eq!(nr.host.installed(HK), held(&pv(V1)));
    assert_eq!(nr.linked("current").as_deref(), Some(V1));
    assert_eq!(nr.layout().next().unwrap(), None);
    assert_eq!(nr.record().unwrap().state, succeeded_state());
}

/// What a case does to the host before the start.
type Setup = dyn Fn(&NativeRig);

/// Proves the adoption's conditions: nothing is adopted where the package is at another version
/// than the one that runs, not installed whole, or where a version of hostkeeper is already in
/// the packages directory; nor on a host that does not announce apt; and an adoption whose hold
/// fails records nothing, so the next start tries again.
#[test]
fn only_the_bootstrap_install_of_the_version_that_runs_is_adopted() {
    let cases: [(&str, &Setup); 4] = [
        ("other-version", &|nr| {
            nr.host.set_installed(HK, "0.9.0-1", 'i', false)
        }),
        ("half-configured", &|nr| {
            nr.host.set_installed(HK, &pv(V1), 'F', false)
        }),
        ("not-installed", &|_| {}),
        ("hold-fails", &|nr| {
            nr.host.set_installed(HK, &pv(V1), 'i', false);
            nr.host.cannot_run("apt-mark hold");
        }),
    ];
    for (name, setup) in cases {
        let nr = NativeRig::new(&format!("adopt-{name}"));
        setup(&nr);
        let started = nr.start(V1);
        let adopted = started.boot_report().adopted.clone();
        match name {
            "hold-fails" => assert!(matches!(adopted, Some(Err(_))), "{name}: {adopted:?}"),
            _ => assert_eq!(adopted, Some(Ok(None)), "{name}"),
        }
        assert_eq!(nr.installed(), None, "{name}");
    }

    let nr = NativeRig::new("adopt-no-apt");
    nr.host.set_installed(HK, &pv(V1), 'i', false);
    fs::create_dir_all(&nr.rig.state).unwrap();
    let started = hostkeeper_daemon::start::startup(
        &nr.rig.settings(),
        nr.rig.ports(),
        support::selfupdate::own(V1),
    );
    assert!(matches!(started, Ok(ref s) if s.boot_report().adopted == Some(Ok(None))));
    assert_eq!(nr.host.installed(HK).map(|i| i.held), Some(false));
}

/// Proves the compiled table guards a rollback of hostkeeper before anything is staged: on a
/// host where hostkeeper runs from its `.deb`, a `force_rollback` to a release before the first
/// that updates itself through it is `Failed`, `release`, `rollback_below_installer`, with no
/// tool run, nothing fetched, nothing in the binary directory and no step record; on a host
/// where a service is installed by apt, the same for a release before apt's row; a rollback to
/// the row's own release goes on to its stage.
#[test]
fn a_rollback_below_an_installer_the_host_holds_is_refused_before_anything_is_staged() {
    use hostkeeper_daemon::rollback_floor::{
        INSTALLER_FLOORS, REASON_ROLLBACK_BELOW_INSTALLER, SELF_NATIVE_FLOOR,
    };
    use hostkeeper_placed::InstalledEntry;
    use support::selfupdate::refused_before_commit;

    let older = |floor: hostkeeper_config::manifest::HostkeeperVersion| {
        hostkeeper_config::manifest::HostkeeperVersion {
            patch: floor.patch - 1,
            ..floor
        }
        .to_string()
    };
    for (holder, floor) in [
        ("hostkeeper", SELF_NATIVE_FLOOR),
        ("web", INSTALLER_FLOORS[0].1),
    ] {
        let nr = NativeRig::new(&format!("native-floor-{holder}"));
        let mut record = nr.rig.placed_record();
        record
            .installed
            .insert(holder.to_owned(), InstalledEntry::new("apt"));
        nr.rig.save_placed(&record);
        let daemon = nr.daemon_at(V1);
        let below = older(floor);
        let pin = nr.publish(&below);
        assert_eq!(
            daemon.order(&rollback_hk(&below, &pin, 1)),
            refused_before_commit(
                hostkeeper_hook::Class::Release,
                REASON_ROLLBACK_BELOW_INSTALLER
            ),
            "{holder}"
        );
        assert!(nr.host.calls().is_empty(), "{holder}: no tool ran");
        assert!(!nr.version_dir(&below).exists(), "{holder}: nothing staged");
        assert_eq!(nr.layout().next().unwrap(), None, "{holder}");
        assert_eq!(nr.record(), None, "{holder}: no step record");

        let at = floor.to_string();
        let pin = nr.publish(&at);
        let reply = daemon.order(&install_hk(&at, &pin, 2));
        assert_ne!(
            reply,
            refused_before_commit(
                hostkeeper_hook::Class::Release,
                REASON_ROLLBACK_BELOW_INSTALLER
            ),
            "{holder}: the row's own release goes on"
        );
        assert!(
            nr.host.labels().contains(&"apt-get update".to_owned()),
            "{holder}: its stage began"
        );
    }
}
