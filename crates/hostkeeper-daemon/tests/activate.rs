//! `hostkeeper activate` (docs/admin.md, "Roll a service back on the host"): root's switch of a
//! service to a version the host keeps, with no coordinator and no network, run on the daemon's
//! own state as the force's journal (`hostkeeper_daemon::verbs::force::activate`).

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_config::names::Version;
use hostkeeper_daemon::local::{self, ActivateArgs, KeepAll};
use hostkeeper_daemon::{Daemon, Ports, Reply, pin};
use hostkeeper_hook::Verdict;
use hostkeeper_install::repository::Network;
use hostkeeper_steps::record::RECORD_FILE;
use hostkeeper_steps::{CrashPoint, ForceFailure, ForceVerdict};
use support::force::{HostState, UNIT, WEB, host_state, link_of, set};
use support::{OWN, config, crash_at, name};
use support_host::{NativeHost, runs, unit};

/// `web` forced to 1.0.0 and then to 2.0.0 by the coordinator, with no prune: 2.0.0 is
/// `current`, 1.0.0 `previous`, and both are kept whole. Each version runs [`UNIT`] and a unit
/// of its own (`web-old.service`, `web-new.service`).
fn two_kept(name: &str) -> NativeHost {
    let rig = NativeHost::new(name);
    rig.manifest(
        WEB,
        "1.0.0",
        &runs(WEB, "1.0.0", &[UNIT, &unit(WEB, "old")]),
    );
    rig.manifest(
        WEB,
        "2.0.0",
        &runs(WEB, "2.0.0", &[UNIT, &unit(WEB, "new")]),
    );
    let daemon = rig.daemon();
    rig.forced(&daemon, "1.0.0", 1);
    rig.forced(&daemon, "2.0.0", 2);
    rig
}

/// The daemon `hostkeeper activate` puts together on the rig: its ports, and a prune that keeps
/// every version.
fn local(rig: &NativeHost) -> Daemon {
    let ports = Ports {
        prune: Arc::new(KeepAll),
        ..rig.ports()
    };
    Daemon::new(config(), rig.dirs(), ports, OWN)
}

/// `hostkeeper activate --service <service> --version <version> [--pin]` on the rig's directories.
fn args(rig: &NativeHost, service: &str, version: &str, pin: bool) -> ActivateArgs {
    ActivateArgs {
        config: rig.config.clone(),
        state_dir: rig.state.clone(),
        packages_dir: rig.packages.clone(),
        service: name(service),
        version: Version::new(version).expect("a version"),
        pin,
    }
}

/// `activate` run with `daemon`: its exit status, standard output and standard error.
fn activate(daemon: &Daemon, args: &ActivateArgs) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = local::activate_on(daemon, args, &mut out, &mut err);
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("text");
    (code, text(out), text(err))
}

/// The path of the pin of `web`.
fn pin_of(rig: &NativeHost) -> PathBuf {
    pin::path(&rig.state, &name(WEB))
}

/// The reply of an order the pin holds.
fn deferred(reason: &str) -> Reply {
    Reply::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: Some(pin::PINNED_RETRY_SECS),
        },
        reason: Some(reason.to_owned()),
        resent: false,
    }
}

/// The regular files under `dir`, in name order.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .expect("a directory")
        .map(|entry| entry.expect("an entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let meta = fs::symlink_metadata(&path).expect("its metadata");
        if meta.is_dir() {
            found.extend(files_under(&path));
        } else if meta.is_file() {
            found.push(path);
        }
    }
    found
}

/// Catches an activate that fetches, or asks the package store or the network for anything: with
/// the host's package manager unable to download or refresh, `web` goes back from 2.0.0 to 1.0.0,
/// which it keeps; every run of the package manager is offline, none downloads and the package
/// store is asked nothing; `current` is 1.0.0 and `previous` 2.0.0, 1.0.0's units run and
/// 2.0.0's do not; the force record names no order and keeps the try of the coordinator's last
/// order; and 2.0.0 is still kept.
#[test]
fn activate_switches_to_a_kept_release_with_the_network_unreachable() {
    let rig = two_kept("activate-offline");
    rig.apt.host.cannot_run("apt-get download");
    rig.apt.host.cannot_run("apt-get update");
    let seen = rig.apt.host.calls().len();
    let asked = rig.pins.asked().len();
    let (code, out, err) = activate(&local(&rig), &args(&rig, WEB, "1.0.0", false));
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (0, "web: 1.0.0 is current, 2.0.0 previous\n", "")
    );
    let calls = rig.apt.host.calls();
    let labels: Vec<&String> = calls[seen..].iter().map(|call| &call.label).collect();
    assert!(
        calls[seen..]
            .iter()
            .all(|call| matches!(call.run.network, Network::Offline)),
        "{labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|label| label.as_str() == "apt-get download"),
        "{labels:?}"
    );
    assert_eq!(rig.pins.asked().len(), asked);
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("1.0.0"));
    assert_eq!(link_of(&rig, WEB, "previous").as_deref(), Some("2.0.0"));
    assert_eq!(rig.units.active(), set(&[UNIT, &unit(WEB, "old")]));
    let record = rig.record(WEB).expect("the step record");
    assert_eq!(record.order_try, Some(2));
    let force = record.force.expect("a force record");
    assert_eq!(
        (
            force.target_version.as_str(),
            force.from_version.as_deref(),
            force.order_id.as_deref(),
            force.order_try,
            force.verdict,
        ),
        (
            "1.0.0",
            Some("2.0.0"),
            None,
            2,
            Some(ForceVerdict::Succeeded)
        )
    );
    assert!(rig.version_dir(WEB, "2.0.0").is_dir());
    assert!(!pin_of(&rig).exists());
}

/// Catches an activate that stages, or switches to, a version the host does not keep, or one that
/// says nothing of what it keeps: a version published but not on the host is refused
/// `not_kept`, naming the versions kept, newest first, and which is current; nothing on the host
/// changes, the step record included, and `--pin` writes no pin; and a service the host keeps
/// nothing of is refused naming none.
#[test]
fn activate_refuses_an_unkept_release_and_lists_the_kept_ones() {
    let rig = two_kept("activate-unkept");
    rig.pin(WEB, "3.0.0");
    let before = host_state(&rig);
    let record = rig.record(WEB);
    let (code, out, err) = activate(&local(&rig), &args(&rig, WEB, "3.0.0", true));
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (
            1,
            "",
            "hostkeeper activate: refused: not_kept: web 3.0.0 is not kept on this host, which \
             keeps 2.0.0 (current), 1.0.0\n"
        )
    );
    assert_eq!(host_state(&rig), before);
    assert_eq!(rig.record(WEB), record);
    assert!(!pin_of(&rig).exists());
    assert!(!rig.version_dir(WEB, "3.0.0").exists());

    let (code, out, err) = activate(&local(&rig), &args(&rig, "api", "1.0.0", false));
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (
            1,
            "",
            "hostkeeper activate: refused: not_kept: api 1.0.0 is not kept on this host, which \
             keeps none\n"
        )
    );
}

/// Catches an activate that trusts a seal without looking at the tree again: one byte appended
/// to a file of the kept 1.0.0 makes the activate of it refused `release_unreadable`, with
/// nothing on the host changed and no pin written.
#[test]
fn activate_reverifies_the_tree_digest_and_refuses_a_modified_release() {
    let rig = two_kept("activate-modified");
    let file = files_under(&rig.version_dir(WEB, "1.0.0"))
        .into_iter()
        .next()
        .expect("a file of 1.0.0");
    let mode = fs::metadata(&file).expect("its mode").permissions();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("writable");
    let mut bytes = fs::read(&file).expect("the file");
    bytes.push(b'\n');
    fs::write(&file, bytes).expect("a changed file");
    fs::set_permissions(&file, mode).expect("its mode again");
    let before = host_state(&rig);
    let (code, out, err) = activate(&local(&rig), &args(&rig, WEB, "1.0.0", true));
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        err.starts_with(
            "hostkeeper activate: refused: release_unreadable: web 1.0.0 is not as its seal says: "
        ),
        "{err}"
    );
    assert_eq!(host_state(&rig), before);
    assert!(!pin_of(&rig).exists());
}

/// Catches an activate that runs anything of the release it leaves: going back from 2.0.0 to
/// 1.0.0, no hook step of either release runs, no run of the package manager names 2.0.0's
/// version directory in its arguments or as its working directory, and 2.0.0's own unit is
/// stopped by name only.
#[test]
fn activate_executes_nothing_from_the_release_being_left() {
    let rig = two_kept("activate-left");
    let left = rig.version_dir(WEB, "2.0.0");
    let left_text = left.to_string_lossy().into_owned();
    let seen = rig.apt.host.calls().len();
    let ran = rig.launcher.runs().len();
    let (code, _, err) = activate(&local(&rig), &args(&rig, WEB, "1.0.0", false));
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(rig.launcher.runs().len(), ran);
    for call in &rig.apt.host.calls()[seen..] {
        assert!(
            !call.run.args.iter().any(|arg| arg.contains(&left_text)),
            "{}: {:?}",
            call.label,
            call.run.args
        );
        assert!(
            !call
                .run
                .working_dir
                .as_ref()
                .is_some_and(|dir| dir.starts_with(&left)),
            "{}",
            call.label
        );
    }
    assert!(!rig.units.active().contains(&unit(WEB, "new")));
}

/// Catches an activate whose crash leaves the host between two releases, or loses the pin: a
/// crash before and after each of the eight writes of its force record, with `--pin`, followed
/// by the daemon's start, leaves the host as an activate that ran to its end and a start leave
/// it (1.0.0), or, when the crash came before the force record was first written, as the host
/// was (2.0.0); and the pin, written before the record, names 1.0.0 every time.
#[test]
fn a_crash_at_every_kill_point_of_activate_resumes_to_the_target_or_the_previous_release() {
    let (previous, target, writes) = baselines();
    assert_eq!(writes, 8);
    assert_eq!(previous.current.as_deref(), Some("2.0.0"));
    assert_eq!(target.current.as_deref(), Some("1.0.0"));
    for n in 1..=writes {
        for point in [CrashPoint::BeforeWrite(n), CrashPoint::AfterWrite(n)] {
            let rig = two_kept("activate-crash");
            let seam = crash_at(Some(point), Arc::new(AtomicU64::new(0)));
            let daemon = local(&rig).with_stores(seam);
            let (code, out, err) = activate(&daemon, &args(&rig, WEB, "1.0.0", true));
            assert_eq!((code, out.as_str()), (1, ""), "{point:?}");
            assert!(
                err.starts_with("hostkeeper activate: refused: store_failed: "),
                "{point:?}: {err}"
            );
            drop(daemon);
            rig.booted();
            let expected = match point {
                CrashPoint::BeforeWrite(1) => &previous,
                _ => &target,
            };
            assert_eq!(&host_state(&rig), expected, "{point:?}");
            assert_eq!(
                fs::read_to_string(pin_of(&rig)).ok().as_deref(),
                Some("1.0.0\n"),
                "{point:?}"
            );
        }
    }
}

/// The host as a start leaves it, untouched and after an uninterrupted `activate --pin` of 1.0.0,
/// and how many record writes that activate makes.
fn baselines() -> (HostState, HostState, u64) {
    let rig = two_kept("activate-crash-previous");
    rig.booted();
    let previous = host_state(&rig);
    let rig = two_kept("activate-crash-target");
    let writes = Arc::new(AtomicU64::new(0));
    let daemon = local(&rig).with_stores(crash_at(None, Arc::clone(&writes)));
    let (code, _, err) = activate(&daemon, &args(&rig, WEB, "1.0.0", true));
    assert_eq!((code, err.as_str()), (0, ""));
    drop(daemon);
    rig.booted();
    (previous, host_state(&rig), writes.load(Ordering::SeqCst))
}

/// Catches an activate that puts the release it left back, or says it succeeded, when the
/// target's units do not start: it fails `start_failed`, says the host stays on 1.0.0 with
/// nothing put back, `current` is 1.0.0 and `previous` 2.0.0, and the force record holds the
/// failure.
#[test]
fn activate_whose_units_do_not_start_stays_on_the_target_and_reports_start_failed() {
    let rig = two_kept("activate-start-failed");
    rig.units.fail_start(&unit(WEB, "old"));
    let (code, out, err) = activate(&local(&rig), &args(&rig, WEB, "1.0.0", false));
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (
            1,
            "",
            "hostkeeper activate: failed: start_failed: web is on 1.0.0, and nothing is put \
             back\n"
        )
    );
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("1.0.0"));
    assert_eq!(link_of(&rig, WEB, "previous").as_deref(), Some("2.0.0"));
    let force = rig
        .record(WEB)
        .and_then(|rec| rec.force)
        .expect("the force");
    assert_eq!(
        force.verdict,
        Some(ForceVerdict::Failed {
            reason: ForceFailure::StartFailed
        })
    );
}

/// Catches an `activate --pin` whose pin the daemon does not honour, or one that is not root's
/// alone: the pin holds `1.0.0` and one newline, mode 0600 in a directory of mode 0700, and the
/// daemon started after it defers the coordinator's force and update of 2.0.0 `pinned`, with
/// `current` still 1.0.0.
#[test]
fn activate_with_pin_writes_the_pin_and_the_daemon_then_holds_the_coordinators_orders() {
    let rig = two_kept("activate-pin");
    let (code, out, err) = activate(&local(&rig), &args(&rig, WEB, "1.0.0", true));
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (
            0,
            "web: 1.0.0 is current, 2.0.0 previous, pinned at 1.0.0\n",
            ""
        )
    );
    let pin = pin_of(&rig);
    assert_eq!(fs::read_to_string(&pin).expect("the pin"), "1.0.0\n");
    let mode = |path: &Path| fs::metadata(path).expect("a mode").permissions().mode() & 0o777;
    assert_eq!(mode(&pin), 0o600);
    assert_eq!(mode(pin.parent().expect("the pins directory")), 0o700);
    let (daemon, _) = rig.booted();
    assert_eq!(
        daemon.order(&rig.force(WEB, "2.0.0", 3, 0)),
        deferred("pinned")
    );
    assert_eq!(
        daemon.order(&rig.update(WEB, "2.0.0", 4)),
        deferred("pinned")
    );
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("1.0.0"));
}

/// Catches an activate that switches what it must leave alone: hostkeeper itself is refused
/// `self_service`; a step record that does not read is refused `record_unreadable` with no pin
/// written; and a pin that cannot be written is refused `store_failed` with nothing switched.
#[test]
fn activate_refuses_hostkeeper_an_unreadable_record_and_a_pin_it_cannot_write() {
    let rig = two_kept("activate-refusals");
    let (code, out, err) = activate(&local(&rig), &args(&rig, "hostkeeper", "1.0.0", true));
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (
            1,
            "",
            "hostkeeper activate: refused: self_service: hostkeeper updates itself by its own \
             path, never by an activate\n"
        )
    );

    let blocker = rig.state.with_file_name(pin::PINS_DIR);
    fs::write(&blocker, "not a directory").expect("a file where the pins go");
    let (code, out, err) = activate(&local(&rig), &args(&rig, WEB, "1.0.0", true));
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        err.starts_with("hostkeeper activate: refused: store_failed: the pin: "),
        "{err}"
    );
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("2.0.0"));
    fs::remove_file(&blocker).expect("the file gone");

    let record = rig.state.join(WEB).join(RECORD_FILE);
    fs::write(&record, "{ not json").expect("a corrupt record");
    let (code, out, err) = activate(&local(&rig), &args(&rig, WEB, "1.0.0", true));
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        err.starts_with("hostkeeper activate: refused: record_unreadable: "),
        "{err}"
    );
    assert!(!pin_of(&rig).exists());
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("2.0.0"));
}
