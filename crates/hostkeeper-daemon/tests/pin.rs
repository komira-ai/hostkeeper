//! The pin of a service (docs/design/hostkeeper-design.md, Q16): root's file that holds the
//! coordinator's orders for the service, what is and is not a pin, `hostkeeper pin` and
//! `hostkeeper unpin`, and the coordinator's force that hands the service back.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::PathBuf;

use hostkeeper_config::names::Version;
use hostkeeper_daemon::boot::alert_line;
use hostkeeper_daemon::local::{self, PinArgs, UnpinArgs};
use hostkeeper_daemon::pin::{self, ALERT_NOT_A_PIN, Look, PINNED_RETRY_SECS};
use hostkeeper_daemon::{Refusal, Reply, Verb};
use hostkeeper_hook::Verdict;
use support::force::{UNIT, WEB, link_of};
use support::{BASE, alerting, d, lines, name};
use support_host::{NativeHost, runs, succeeded, unit};

/// `web` forced to 1.0.0 and then to 2.0.0, both kept: 2.0.0 is `current`, 1.0.0 `previous`.
fn two_kept(name: &str) -> NativeHost {
    let rig = NativeHost::new(name);
    rig.manifest(WEB, "1.0.0", &runs(WEB, "1.0.0", &[UNIT]));
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

/// The path of the pin of `web`.
fn pin_of(rig: &NativeHost) -> PathBuf {
    pin::path(&rig.state, &name(WEB))
}

/// The reply of an order the pin holds, for `reason`.
fn deferred(reason: &str) -> Reply {
    Reply::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: Some(PINNED_RETRY_SECS),
        },
        reason: Some(reason.to_owned()),
        resent: false,
    }
}

/// `hostkeeper pin --service <service> --version <version>` on the rig: its exit status, standard
/// output and standard error.
fn pin_cli(rig: &NativeHost, service: &str, version: &str) -> (u8, String, String) {
    let args = PinArgs {
        state_dir: rig.state.clone(),
        packages_dir: rig.packages.clone(),
        service: name(service),
        version: Version::new(version).expect("a version"),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = local::pin(&args, &mut out, &mut err);
    (code, text(out), text(err))
}

/// `hostkeeper unpin --service web` on the rig.
fn unpin_cli(rig: &NativeHost) -> (u8, String, String) {
    let args = UnpinArgs {
        state_dir: rig.state.clone(),
        service: name(WEB),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = local::unpin(&args, &mut out, &mut err);
    (code, text(out), text(err))
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("text")
}

/// The output of a verb that did what it says.
fn said(line: &str) -> (u8, String, String) {
    (0, line.to_owned(), String::new())
}

/// Catches a pin the daemon does not honour, or one that holds what it should not: with `web`
/// pinned at 2.0.0, the coordinator's force of 1.0.0, its `force_rollback`, its update to 2.1.0
/// and its uninstall are each deferred `pinned`, and nothing is fetched or switched; a
/// `clear_record` is not held (it changes no version); once `unpin` runs, the force of 1.0.0
/// runs to `Succeeded`.
#[test]
fn pin_blocks_the_agents_orders_for_that_service_until_unpin() {
    let rig = two_kept("pin-holds");
    assert_eq!(pin_cli(&rig, WEB, "2.0.0"), said("web: pinned at 2.0.0\n"));
    let daemon = rig.daemon();
    let downloads = rig.downloads();
    assert_eq!(
        daemon.order(&rig.force(WEB, "1.0.0", 3, 0)),
        deferred("pinned")
    );
    assert_eq!(
        daemon.order(&rig.force_rollback(WEB, "1.0.0", 4)),
        deferred("pinned")
    );
    assert_eq!(
        daemon.order(&rig.update(WEB, "2.1.0", 5)),
        deferred("pinned")
    );
    assert_eq!(
        daemon.order(&support::uninstall(WEB, 6)),
        deferred("pinned")
    );
    assert_eq!(rig.downloads(), downloads);
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("2.0.0"));
    let cleared = daemon.execute(Verb::ClearRecord(name(WEB)));
    assert_eq!(cleared.refusal(), Some(Refusal::RecordReads), "{cleared:?}");

    assert_eq!(unpin_cli(&rig), said("web: unpinned\n"));
    assert!(!pin_of(&rig).exists());
    assert_eq!(daemon.order(&rig.force(WEB, "1.0.0", 7, 0)), succeeded());
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("1.0.0"));
}

/// Catches an unpin that leaves the service held, or that fails on a service with no pin: a
/// service pinned at the version it runs has its update deferred; after `unpin` the next update
/// runs to `Succeeded`, and a second `unpin` says the service was not pinned and exits 0.
#[test]
fn unpin_restores_following_desired() {
    let rig = NativeHost::new("pin-unpin");
    rig.add(WEB, "0.9.0");
    let (daemon, _) = rig.booted();
    assert_eq!(pin_cli(&rig, WEB, BASE), said("web: pinned at 0.0.0\n"));
    assert_eq!(
        daemon.order(&rig.update(WEB, "0.9.0", 1)),
        deferred("pinned")
    );
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some(BASE));
    assert_eq!(unpin_cli(&rig), said("web: unpinned\n"));
    assert_eq!(daemon.order(&rig.update(WEB, "0.9.0", 2)), succeeded());
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("0.9.0"));
    assert_eq!(unpin_cli(&rig), said("web: not pinned\n"));
}

/// Catches a force that the pin holds when it names the pinned version, a pin dropped by a force
/// that did not run, or one left behind by a force that did: with `web` pinned at 2.0.0, the
/// coordinator's resend of the finished force of 2.0.0 is answered from the record and the pin
/// stays; pinned at 1.0.0 (kept, not running), a force of 1.0.0 with a stale try is refused and
/// the pin stays; the force of 1.0.0 with a new try runs to `Succeeded` and removes the pin, and
/// the next force, of 2.0.0, is not held. A pin that cannot be removed after its force ran stays,
/// and is the alert `pin_not_cleared`.
#[test]
fn a_forced_order_naming_the_pinned_release_clears_the_pin() {
    let rig = two_kept("pin-forced");
    let (daemon, log) = alerting(rig.daemon());
    assert_eq!(pin_cli(&rig, WEB, "2.0.0"), said("web: pinned at 2.0.0\n"));
    let resent = daemon.order(&rig.force(WEB, "2.0.0", 2, 0));
    assert!(
        matches!(resent, Reply::Finished { resent: true, .. }),
        "{resent:?}"
    );
    assert!(pin_of(&rig).exists());

    assert_eq!(pin_cli(&rig, WEB, "1.0.0"), said("web: pinned at 1.0.0\n"));
    let stale = daemon.order(&rig.force(WEB, "1.0.0", 1, 0));
    assert_eq!(stale.refusal(), Some(Refusal::StaleTry), "{stale:?}");
    assert!(pin_of(&rig).exists());
    assert_eq!(daemon.order(&rig.force(WEB, "1.0.0", 3, 0)), succeeded());
    assert!(!pin_of(&rig).exists());
    assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("1.0.0"));
    assert_eq!(daemon.order(&rig.force(WEB, "2.0.0", 4, 0)), succeeded());

    assert_eq!(pin_cli(&rig, WEB, "1.0.0"), said("web: pinned at 1.0.0\n"));
    let pins = rig.state.with_file_name(pin::PINS_DIR);
    fs::set_permissions(&pins, fs::Permissions::from_mode(0o500)).expect("read-only pins");
    assert_eq!(daemon.order(&rig.force(WEB, "1.0.0", 5, 0)), succeeded());
    fs::set_permissions(&pins, fs::Permissions::from_mode(0o700)).expect("the pins again");
    assert!(pin_of(&rig).exists());
    assert!(
        lines(&log)
            .iter()
            .any(|line| line.contains("alert=pin_not_cleared service=web: ")),
        "{:?}",
        lines(&log)
    );
}

/// Catches a file at a pin's path that anyone but root could have put there being taken as a
/// pin, or one that is ignored without a word: a link to a file that names a version, a file its
/// group and others may write, a directory, and a file owned by another account are each not a
/// pin, said with its reason; the first three hold nothing, and the daemon says so in the alert
/// `not_a_pin` naming the service and the reason.
#[test]
fn a_pin_that_is_a_link_or_owned_by_another_account_or_writable_is_not_a_pin_and_is_reported() {
    let rig = two_kept("pin-not-a-pin");
    let path = pin_of(&rig);
    fs::create_dir_all(path.parent().expect("the pins directory")).expect("the pins directory");
    let elsewhere = rig.scratch.path().join("elsewhere");
    fs::write(&elsewhere, "2.0.0\n").expect("a version");
    fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o600)).expect("its mode");
    let (daemon, log) = alerting(rig.daemon());
    let reported = |why: &str| {
        let why = format!("the file {} is not a pin: {why}", path.display());
        assert_eq!(pin::look(&path), Look::NotAPin(why.clone()));
        alert_line(ALERT_NOT_A_PIN, &format!("service={WEB}: {why}"))
    };

    symlink(&elsewhere, &path).expect("a link");
    let link = reported("it is not a regular file (a link, a directory or a device)");
    assert_eq!(daemon.order(&rig.force(WEB, "1.0.0", 3, 0)), succeeded());
    assert!(lines(&log).contains(&link), "{:?}", lines(&log));
    fs::remove_file(&path).expect("the link gone");

    fs::write(&path, "1.0.0\n").expect("a file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).expect("its mode");
    let writable = reported("its group or others may write it");
    assert_eq!(daemon.order(&rig.force(WEB, "2.0.0", 4, 0)), succeeded());
    assert!(lines(&log).contains(&writable), "{:?}", lines(&log));
    fs::remove_file(&path).expect("the file gone");

    fs::create_dir(&path).expect("a directory");
    let directory = reported("it is not a regular file (a link, a directory or a device)");
    assert_eq!(daemon.order(&rig.force(WEB, "1.0.0", 5, 0)), succeeded());
    assert_eq!(
        lines(&log)
            .iter()
            .filter(|line| **line == directory)
            .count(),
        2
    );
    fs::remove_dir(&path).expect("the directory gone");

    fs::write(&path, "1.0.0\n").expect("a file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("its mode");
    let owner = fs::metadata(&path).expect("its owner").uid();
    assert_eq!(
        pin::look_as(&path, &[owner.wrapping_add(1)]),
        Look::NotAPin(format!(
            "the file {} is not a pin: it is owned by the account {owner}, not root",
            path.display()
        ))
    );
    assert_eq!(
        pin::look(&path),
        Look::Pinned(Version::new("1.0.0").expect("a version"))
    );
}

/// Catches a pin that cannot be read letting an order through: root's file that holds no version,
/// an empty one, and one this account may not open, are unreadable, and the coordinator's force
/// and update are deferred `pin_unreadable` with nothing switched; a service with no file is not
/// pinned.
#[test]
fn an_unreadable_pin_holds_the_service() {
    let rig = two_kept("pin-unreadable");
    let path = pin_of(&rig);
    assert_eq!(pin::look(&path), Look::Absent);
    fs::create_dir_all(path.parent().expect("the pins directory")).expect("the pins directory");
    let daemon = rig.daemon();
    for (n, bytes) in [&b"not a version\n"[..], &b""[..]].into_iter().enumerate() {
        fs::write(&path, bytes).expect("a pin");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("its mode");
        assert_eq!(
            pin::look(&path),
            Look::Unreadable(format!(
                "the pin {} does not hold a version",
                path.display()
            ))
        );
        let tries = 3 + 2 * n as u64;
        assert_eq!(
            daemon.order(&rig.force(WEB, "1.0.0", tries, 0)),
            deferred("pin_unreadable")
        );
        assert_eq!(
            daemon.order(&rig.update(WEB, "2.1.0", tries + 1)),
            deferred("pin_unreadable")
        );
        assert_eq!(link_of(&rig, WEB, "current").as_deref(), Some("2.0.0"));
    }
    fs::write(&path, "1.0.0\n").expect("a pin");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("its mode");
    let look = pin::look(&path);
    let prefix = format!("the pin {} cannot be read: ", path.display());
    assert!(
        matches!(&look, Look::Unreadable(why) if why.starts_with(&prefix)),
        "{look:?}"
    );
    assert_eq!(
        daemon.order(&rig.force(WEB, "1.0.0", 7, 0)),
        deferred("pin_unreadable")
    );
}

/// Catches a pin of hostkeeper itself that holds its own path: a pin file of the service
/// `hostkeeper` leaves its force to the force's own refusal, `self_service`, not a deferral.
#[test]
fn a_pin_of_hostkeeper_holds_nothing() {
    let rig = two_kept("pin-hostkeeper");
    let path = pin::path(&rig.state, &name("hostkeeper"));
    fs::create_dir_all(path.parent().expect("the pins directory")).expect("the pins directory");
    fs::write(&path, "1.0.0\n").expect("a pin");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("its mode");
    let reply = rig
        .daemon()
        .order(&support::force("hostkeeper", "1.0.0", &d(), 1, 0));
    assert_eq!(reply.refusal(), Some(Refusal::SelfService), "{reply:?}");
}

/// Catches a `pin` that writes what the daemon would not honour, or that fails silently: it
/// refuses hostkeeper itself (`self_service`) and a version the host does not keep (`not_kept`,
/// naming what it keeps), writing nothing; a pins directory it cannot make is `store_failed`; an
/// `unpin` that cannot remove what is at the path is `store_failed` too; and a pin it writes is
/// root's file of the version and one newline, mode 0600 in a directory of mode 0700.
#[test]
fn pin_and_unpin_refuse_what_they_cannot_do_and_say_why() {
    let rig = two_kept("pin-cli");
    assert_eq!(
        pin_cli(&rig, "hostkeeper", "1.0.0"),
        (
            1,
            String::new(),
            "hostkeeper pin: refused: self_service: hostkeeper updates itself by its own path, \
             which no pin holds\n"
                .to_owned()
        )
    );
    assert_eq!(
        pin_cli(&rig, WEB, "3.0.0"),
        (
            1,
            String::new(),
            "hostkeeper pin: refused: not_kept: web 3.0.0 is not kept on this host, which keeps \
             2.0.0 (current), 1.0.0\n"
                .to_owned()
        )
    );
    let pins = rig.state.with_file_name(pin::PINS_DIR);
    assert!(!pins.exists());

    fs::write(&pins, "not a directory").expect("a file where the pins go");
    let (code, out, err) = pin_cli(&rig, WEB, "1.0.0");
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        err.starts_with("hostkeeper pin: refused: store_failed: the pin: "),
        "{err}"
    );
    fs::remove_file(&pins).expect("the file gone");

    assert_eq!(pin_cli(&rig, WEB, "1.0.0"), said("web: pinned at 1.0.0\n"));
    let path = pin_of(&rig);
    let mode = |path: &std::path::Path| fs::metadata(path).expect("a mode").mode() & 0o777;
    assert_eq!(fs::read_to_string(&path).expect("the pin"), "1.0.0\n");
    assert_eq!((mode(&path), mode(&pins)), (0o600, 0o700));
    assert_eq!(
        pin::look(&path),
        Look::Pinned(Version::new("1.0.0").expect("a version"))
    );

    fs::remove_file(&path).expect("the pin gone");
    fs::create_dir(&path).expect("a directory at the pin's path");
    let (code, out, err) = unpin_cli(&rig);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        err.starts_with("hostkeeper unpin: refused: store_failed: the pin: "),
        "{err}"
    );
    assert!(path.is_dir());
}
