//! `migrate` through the command line: the arguments reach the verb and the old state is granted
//! to the old service user.

mod common;
mod migrate_support;

use common::{FakeMachine, FakeNet, SVC, kit, with_system};
use hostkeeper_mdm::{Grant, run_with};
use migrate_support::*;

/// Catches a command line that does not carry `migrate`'s arguments or its directory grants: the
/// old state is granted to the old user's uid, the new state to the new user's, and the archive
/// directory to its owner; the settings file is read from `--from`; a dry run prints the plan
/// for `--settings`, with the profile identifier and the ACME overrides reaching the new
/// settings, and changes nothing.
#[test]
fn migrate_runs_through_the_command_line_with_its_grants() {
    let kit = kit();
    let host = live_host(&kit);
    host.put_file("/etc/js-mdm.conf", 0o644, (0, 0), old_text().as_bytes());
    host.put_dir("/srv/fetch", 0o755, (1000, 1000));
    let before = snapshot(&host);
    let machine = FakeMachine::new(&host);
    let net = FakeNet::new(Default::default());
    let words = [
        "hostkeeper-mdm",
        "migrate",
        "--from",
        "/etc/js-mdm.conf",
        "--settings",
        SETTINGS,
        "--bin-dir",
        "/srv/fetch",
        "--profile-id",
        "com.komira.hostkeeper.mdm",
        "--scep-ca-cn",
        "Example Hostkeeper SCEP CA",
        "--acme-resolvers",
        "192.0.2.53:53 198.51.100.53:53",
        "--acme-propagation-wait",
        "300s",
    ];
    let args: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = with_system(&machine, &net, |system| {
        run_with(&args, system, &kit.pins, &mut out, &mut err)
    });
    let (out, err) = (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    );
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert_eq!(snapshot(&host), before);
    assert!(out.contains("dry run: nothing changed"), "{out}");
    assert!(
        out.contains("-common_name Example Hostkeeper SCEP CA"),
        "{out}"
    );
    let grants = machine.grants.borrow();
    assert_eq!(
        grants.last().unwrap(),
        &[
            Grant {
                dir: OLD_STATE.to_owned(),
                uid: OLD_UID
            },
            Grant {
                dir: NEW_STATE.to_owned(),
                uid: SVC
            },
            Grant {
                dir: "/srv/fetch".to_owned(),
                uid: 1000
            },
        ]
    );
}

/// Catches a refusal that exits 0 or goes to the wrong stream: a user that is not root is
/// refused through the command line with REFUSED on stderr and exit 2.
#[test]
fn a_refusal_exits_2_through_the_command_line() {
    let kit = kit();
    let host = hostkeeper_mdm_step::fake::FakeHost::new(1000);
    host.add_user("js-mdm", OLD_UID);
    host.add_user("hostkeeper-mdm", SVC);
    host.put_file("/etc/js-mdm.conf", 0o644, (0, 0), old_text().as_bytes());
    host.put_dir("/srv/fetch", 0o755, (1000, 1000));
    let machine = FakeMachine::new(&host);
    let net = FakeNet::new(Default::default());
    let words = [
        "hostkeeper-mdm",
        "migrate",
        "--from",
        "/etc/js-mdm.conf",
        "--settings",
        SETTINGS,
        "--bin-dir",
        "/srv/fetch",
        "--profile-id",
        "com.komira.hostkeeper.mdm",
    ];
    let args: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = with_system(&machine, &net, |system| {
        run_with(&args, system, &kit.pins, &mut out, &mut err)
    });
    assert_eq!(code, 2);
    assert!(out.is_empty());
    let err = String::from_utf8(err).unwrap();
    assert!(
        err.starts_with("REFUSED: migrate reads root-only files"),
        "{err}"
    );
}

/// Catches grants for owners that are not there: on the live host before the move the new
/// service user does not exist and an archive directory may be missing, so the only grant is
/// the old state to the old user.
#[test]
fn only_the_owners_that_exist_are_granted() {
    let kit = kit();
    let host = live_host(&kit);
    host.put_file("/etc/js-mdm.conf", 0o644, (0, 0), old_text().as_bytes());
    host.reply(
        &[hostkeeper_mdm_step::GETENT, "passwd", "hostkeeper-mdm"],
        2,
        "",
    );
    let machine = FakeMachine::new(&host);
    let net = FakeNet::new(Default::default());
    let words = [
        "hostkeeper-mdm",
        "migrate",
        "--from",
        "/etc/js-mdm.conf",
        "--settings",
        SETTINGS,
        "--bin-dir",
        "/srv/none",
        "--profile-id",
        "com.komira.hostkeeper.mdm",
        "--acme-resolvers",
        "192.0.2.53:53",
        "--acme-propagation-wait",
        "300s",
    ];
    let args: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    with_system(&machine, &net, |system| {
        run_with(&args, system, &kit.pins, &mut out, &mut err)
    });
    let grants = machine.grants.borrow();
    assert_eq!(
        grants.last().unwrap(),
        &[Grant {
            dir: OLD_STATE.to_owned(),
            uid: OLD_UID
        }]
    );
}
