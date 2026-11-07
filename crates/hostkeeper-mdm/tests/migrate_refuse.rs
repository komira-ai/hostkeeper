//! What `migrate` refuses before it changes anything, and the host failures it reports.

mod common;
mod migrate_support;

use common::kit;
use hostkeeper_mdm::MdmError;
use hostkeeper_mdm::migrate::MoveError;
use hostkeeper_mdm_core::migrate::MigrateError;
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{Host, Mode, SYSTEMCTL};
use migrate_support::*;

/// Runs `migrate` in both modes on `host`: each refuses with the same error, runs no command that
/// changes anything, and leaves the tree as it was. The error of the dry run.
fn refused(host: &FakeHost, a: &hostkeeper_mdm::migrate::Migrate) -> MdmError {
    let kit = kit();
    let before = snapshot(host);
    let mut errors = Vec::new();
    for mode in [Mode::DryRun, Mode::Apply] {
        let (result, _) = migrate(host, &kit, a, mode);
        errors.push(result.expect_err("refused"));
        assert_eq!(changes(host), Vec::<String>::new());
        assert_eq!(snapshot(host), before);
    }
    assert_eq!(errors[0].to_string(), errors[1].to_string());
    errors.remove(0)
}

/// Catches a migrate run by someone else: a user that is not root is refused (exit 2) before any
/// command runs, a dry run included.
#[test]
fn a_user_that_is_not_root_is_refused() {
    let kit = kit();
    let host = FakeHost::new(1000);
    let a = ask(&old_text());
    for mode in [Mode::DryRun, Mode::Apply] {
        let (result, shown) = migrate(&host, &kit, &a, mode);
        let err = result.unwrap_err();
        assert!(
            matches!(
                &err,
                MdmError::NeedsRoot {
                    verb: "migrate",
                    euid: 1000
                }
            ),
            "{err}"
        );
        assert_eq!(err.exit_code(), 2);
        assert_eq!(shown, "");
        assert!(host.calls().is_empty());
    }
}

/// Catches settings the new kit would not accept being written: a profile identifier that is not
/// one is refused by the core (exit 2) with no command run and nothing changed.
#[test]
fn settings_the_core_refuses_are_refused() {
    let host = live_host(&kit());
    let mut a = ask(&old_text());
    a.target.profile_id = "not an identifier".to_owned();
    host.clear_calls();
    let err = refused(&host, &a);
    assert!(
        matches!(&err, MdmError::Settings(MigrateError::New(_))),
        "{err}"
    );
    assert_eq!(err.exit_code(), 2);
    assert!(host.calls().is_empty());
}

/// Catches a move onto the old kit's own directories or names, which would change the state it
/// must leave untouched: each old directory the same as or inside a new one (or holding it), and
/// the same unit prefix, is refused (exit 2) with both settings named.
#[test]
fn old_and_new_that_share_a_directory_or_a_prefix_are_refused() {
    let host = live_host(&kit());
    let cases = [
        (
            "MDM_STATE=/var/lib/hostkeeper-mdm",
            "MDM_STATE=/var/lib/js-mdm",
            "the old kit's MDM_STATE /var/lib/hostkeeper-mdm and the new kit's MDM_STATE /var/lib/hostkeeper-mdm are the same or one is inside the other: the old kit's state stays untouched",
        ),
        (
            "MDM_STATE=/var/lib/hostkeeper-mdm/old",
            "MDM_STATE=/var/lib/js-mdm",
            "the old kit's MDM_STATE /var/lib/hostkeeper-mdm/old and the new kit's MDM_STATE /var/lib/hostkeeper-mdm are the same or one is inside the other: the old kit's state stays untouched",
        ),
        (
            "MDM_ETC=/etc",
            "MDM_ETC=/etc/js-mdm",
            "the old kit's MDM_ETC /etc and the new kit's MDM_ETC /etc/hostkeeper-mdm are the same or one is inside the other: the old kit's state stays untouched",
        ),
        (
            "MDM_BIN=/opt/hostkeeper-mdm/bin",
            "MDM_BIN=/opt/js-mdm/bin",
            "the old kit's MDM_BIN /opt/hostkeeper-mdm/bin and the new kit's MDM_BIN /opt/hostkeeper-mdm/bin are the same or one is inside the other: the old kit's state stays untouched",
        ),
        (
            "MDM_UNIT_PREFIX=hostkeeper-mdm\nMDM_USER=js-mdm",
            "MDM_USER=js-mdm",
            "the old kit's MDM_UNIT_PREFIX hostkeeper-mdm and the new kit's MDM_UNIT_PREFIX hostkeeper-mdm are the same or one is inside the other: the old kit's state stays untouched",
        ),
    ];
    for (with, without, want) in cases {
        let text = old_text().replace(without, with);
        assert_ne!(text, old_text(), "{with}");
        let err = refused(&host, &ask(&text));
        assert_eq!(err.to_string(), want);
        assert!(
            matches!(&err, MdmError::Move(MoveError::Overlap { .. })),
            "{err}"
        );
        assert_eq!(err.exit_code(), 2);
    }
}

/// Catches new settings written over a file that holds other settings: the file at `--settings`
/// with other bytes is refused (exit 2) and left as it is.
#[test]
fn existing_new_settings_with_other_bytes_are_refused() {
    let host = live_host(&kit());
    host.put_file(SETTINGS, 0o644, (0, 0), b"MDM_USER=somebody\n");
    let err = refused(&host, &ask(&old_text()));
    assert_eq!(
        err.to_string(),
        format!(
            "{SETTINGS} is there with other settings than the ones this run writes: move it aside, or read it and run again with the arguments that make it"
        )
    );
    assert!(matches!(&err, MdmError::Move(MoveError::SettingsDiffer { path }) if path == SETTINGS));
    assert_eq!(err.exit_code(), 2);
    assert_eq!(host.node(SETTINGS).unwrap().bytes, b"MDM_USER=somebody\n");
}

/// Catches a symbolic link in the push certificate tree being followed or skipped: a link
/// anywhere in the tree refuses the run (exit 2) with the link named, and nothing is copied.
#[test]
fn a_link_in_the_push_certificate_tree_is_refused() {
    let host = live_host(&kit());
    let link = format!("{OLD_STATE}/nanohub/mdm/push_cert/ab/cd/elsewhere");
    host.put_link(&link);
    let err = refused(&host, &ask(&old_text()));
    assert_eq!(
        err.to_string(),
        format!(
            "{link} is not a regular file or directory (a symbolic link or other kind): it is not followed, and nothing is copied until it is dealt with"
        )
    );
    assert!(matches!(&err, MdmError::Move(MoveError::WrongKind { path, .. }) if *path == link));
    assert_eq!(err.exit_code(), 2);
}

/// Catches sources and targets of the wrong kind being read or written through: a file where a
/// directory is expected and the reverse, on the old side and on the new, are each refused
/// (exit 2) with the path and the kind wanted.
#[test]
fn a_source_or_target_of_the_wrong_kind_is_refused() {
    type Setup = fn(&FakeHost);
    let push = format!("{OLD_STATE}/nanohub/mdm/push_cert");
    let dep = format!("{OLD_STATE}/dep/dep_names");
    let cases: Vec<(Setup, String, &str)> = vec![
        (
            |h| {
                h.put_file(
                    &format!("{OLD_STATE}/nanohub/mdm/push_cert"),
                    0o600,
                    (0, 0),
                    b"x",
                );
            },
            push.clone(),
            "directory",
        ),
        (
            |h| h.put_link(&format!("{OLD_STATE}/push-topic")),
            format!("{OLD_STATE}/push-topic"),
            "regular file",
        ),
        (
            |h| h.put_dir(&format!("{OLD_ETC}/dns.env"), 0o600, (0, 0)),
            format!("{OLD_ETC}/dns.env"),
            "regular file",
        ),
        (
            |h| h.put_link(&format!("{OLD_STATE}/dep/dep_names/key.js-mdm")),
            format!("{dep}/key.js-mdm"),
            "regular file",
        ),
        (
            |h| h.put_file(&format!("{OLD_STATE}/dep/dep_names"), 0o600, (0, 0), b"x"),
            dep.clone(),
            "directory",
        ),
        (
            |h| h.put_file(&format!("{NEW_STATE}/nanohub/mdm"), 0o600, (0, 0), b"x"),
            format!("{NEW_STATE}/nanohub/mdm"),
            "directory",
        ),
    ];
    for (setup, path, want) in cases {
        let host = live_host(&kit());
        setup(&host);
        let err = refused(&host, &ask(&old_text()));
        assert!(
            matches!(&err, MdmError::Move(MoveError::WrongKind { path: p, want: w }) if *p == path && *w == want),
            "{path}: {err}"
        );
        assert_eq!(err.exit_code(), 2);
    }
}

/// Catches a DNS credential copied from a file that others can read or that is not root's: a
/// `dns.env` of mode 0644, or of mode 0600 owned by the old service user, is refused (exit 2)
/// with the file, its mode and its owner named.
#[test]
fn a_dns_env_that_is_not_0600_and_root_is_refused() {
    for (mode, uid) in [(0o644, 0), (0o600, OLD_UID)] {
        let host = live_host(&kit());
        let path = format!("{OLD_ETC}/dns.env");
        host.put_file(&path, mode, (uid, 0), b"KEY=secret");
        let err = refused(&host, &ask(&old_text()));
        assert_eq!(
            err.to_string(),
            format!(
                "{path} has mode {mode:04o} and uid {uid}, not 0600 and root: chmod 600 {path}; chown root:root {path}"
            )
        );
        assert!(matches!(&err, MdmError::SecretMode { .. }), "{err}");
        assert_eq!(err.exit_code(), 2);
    }
}

/// Catches a port taken by some other process passing as the old kit's: with the old NanoHUB
/// unit not active, port 9004 held is refused (exit 2) and nothing changes; with it active the
/// plan is made (and says the old unit is stopped first).
#[test]
fn a_port_held_by_an_unknown_process_is_refused() {
    let kit = kit();
    let host = live_host(&kit);
    let old = old_units();
    host.reply(&[SYSTEMCTL, "is-active", &old.nanohub], 3, "inactive\n");
    let err = refused(&host, &ask(&old_text()));
    assert!(matches!(&err, MdmError::PortHeld(9004)), "{err}");
    assert_eq!(err.exit_code(), 2);

    host.reply(&[SYSTEMCTL, "is-active", &old.nanohub], 0, "active\n");
    let (result, shown) = migrate(&host, &kit, &ask(&old_text()), Mode::DryRun);
    result.unwrap();
    assert!(
        shown.contains("port 9004: held by kbf-mdm-nanohub.service (stopped first by this plan)\n"),
        "{shown}"
    );
}

/// Catches a wrong `--from` going on: with no unit file of the old kit and none of the new,
/// the run is refused (exit 2); with the new kit's installed it goes on and says the old units
/// are gone (no stop step); with the old ones all off it says so and plans no stop.
#[test]
fn a_host_with_no_unit_of_either_kit_is_refused() {
    let kit = kit();
    let host = live_host(&kit);
    let a = ask(&old_text());
    for unit in old_unit_files() {
        host.remove(&format!("{UNITS}/{unit}")).unwrap();
    }
    host.reply(&["/usr/bin/ss", "-Hltn"], 0, "");
    let err = refused(&host, &a);
    assert_eq!(
        err.to_string(),
        "no unit file of the old kit (prefix kbf-mdm) and none of this kit's is in /etc/systemd/system: wrong --from?"
    );
    assert!(
        matches!(&err, MdmError::Move(MoveError::NoKit { .. })),
        "{err}"
    );
    assert_eq!(err.exit_code(), 2);

    host.put_file(
        &format!("{UNITS}/{}", new_units().scep),
        0o644,
        (0, 0),
        b"[Unit]\n",
    );
    let (result, shown) = migrate(&host, &kit, &a, Mode::DryRun);
    result.unwrap();
    assert!(
        shown.contains("old kit: no unit file with prefix kbf-mdm (moved already)\n"),
        "{shown}"
    );
    assert!(!shown.contains("DRY-RUN: systemctl disable"), "{shown}");
}

/// Catches host failures hidden or turned into refusals: a path of the old state that cannot be
/// statted, a directory or a file that cannot be read, fail the run (exit 1) with the host's
/// error.
#[test]
fn a_host_that_cannot_be_read_fails_the_run() {
    let kit = kit();
    let a = ask(&old_text());
    for path in [
        format!("{OLD_STATE}/nanohub/mdm/push_cert"),
        format!("{OLD_STATE}/dep/dep_names"),
        format!("{OLD_STATE}/push-topic"),
    ] {
        let host = live_host(&kit);
        match path.ends_with("push_cert") {
            true => host.deny_stat(&path),
            false => host.deny_read(&path),
        }
        let (result, _) = migrate(&host, &kit, &a, Mode::Apply);
        let err = result.unwrap_err();
        assert!(matches!(&err, MdmError::Io(_)), "{path}: {err}");
        assert_eq!(err.exit_code(), 1);
        assert_eq!(changes(&host), Vec::<String>::new());
    }
}
