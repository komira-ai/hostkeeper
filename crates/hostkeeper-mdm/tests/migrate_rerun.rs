//! `migrate` on the live host as it is before the move, with no service user of the new kit:
//! the plan makes that user, then writes root's records below its state. Judged as the machine
//! judges it (root and the effective user trusted everywhere, the command line's grants), a dry
//! run and an apply agree, a stranger's directory is still refused, and an apply that stops at
//! any step is finished by the next.

mod common;
mod migrate_support;

use common::{FakeMachine, FakeNet, SVC, kit, with_system};
use hostkeeper_mdm::run_with;
use hostkeeper_mdm_step::Mode;
use migrate_support::*;

/// Catches the refusal of the live move (its apply stopped at the root's `push-topic` below the
/// state directory it had just made the new user's, after its dry run had planned that write):
/// on the host before the move, the dry run plans the records and changes nothing, the apply
/// carries the whole plan out, and the records are root's, 0644, in the new user's 0700 state.
#[test]
fn the_move_writes_roots_records_below_the_state_of_the_user_it_makes() {
    let kit = kit();
    let a = ask(&old_text());
    let host = host_before_the_move(&a);
    let before = snapshot(&host);
    let (dry, shown) = migrate(&host, &kit, &a, Mode::DryRun);
    dry.unwrap();
    assert_eq!(snapshot(&host), before);
    for name in ["push-topic", "push-cert.sha256"] {
        let path = format!("{NEW_STATE}/{name}");
        let line = format!("DRY-RUN: write {path} mode 0644 owner root:root (");
        assert!(shown.contains(&line), "{shown}");
    }

    let (apply, shown) = migrate(&host, &kit, &a, Mode::Apply);
    apply.unwrap_or_else(|e| panic!("{e}\n{shown}"));
    let state = host.node(NEW_STATE).unwrap();
    assert_eq!((state.mode, state.uid, state.gid), (0o700, SVC, SVC));
    for name in ["push-topic", "push-cert.sha256"] {
        let (old, new) = (format!("{OLD_STATE}/{name}"), format!("{NEW_STATE}/{name}"));
        let node = host.node(&new).unwrap();
        assert_eq!((node.mode, node.uid, node.gid), (0o644, 0, 0), "{new}");
        assert_eq!(node.bytes, bytes_of(&old), "{new}");
    }
}

/// Catches the move run through the command line with grants that wait for the new user to
/// exist: `migrate --apply` on the host before the move exits 0 with nothing on stderr.
#[test]
fn the_move_through_the_command_line_applies_on_the_host_before_the_move() {
    let kit = kit();
    let a = ask(&old_text());
    let host = host_before_the_move(&a);
    host.put_file("/etc/js-mdm.conf", 0o644, (0, 0), old_text().as_bytes());
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
        "--acme-resolvers",
        "192.0.2.53:53 198.51.100.53:53",
        "--acme-propagation-wait",
        "300s",
        "--apply",
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
    assert_eq!(
        host.node(&format!("{NEW_STATE}/push-topic")).unwrap().uid,
        0
    );
}

/// Catches a grant of the new state that trusts whoever owns a directory in it: a directory a
/// stranger owns below the new state (where the push certificate's tree goes) is refused by
/// the dry run and by the apply alike, in the walk's words, when the plan looks there and
/// before any step (exit 1, as every untrusted owner met on the way to a path a verb reads);
/// the apply changes nothing and makes no user.
#[test]
fn a_strangers_directory_below_the_new_state_is_refused_by_dry_run_and_apply_alike() {
    let kit = kit();
    let a = ask(&old_text());
    let host = host_before_the_move(&a);
    host.put_dir(NEW_STATE, 0o755, (0, 0));
    host.put_dir(&format!("{NEW_STATE}/nanohub"), 0o755, (0, 0));
    host.put_dir(&format!("{NEW_STATE}/nanohub/mdm"), 0o777, (4242, 4242));
    let before = snapshot(&host);
    let (dry, dry_out) = migrate(&host, &kit, &a, Mode::DryRun);
    let (apply, apply_out) = migrate(&host, &kit, &a, Mode::Apply);
    let (dry, apply) = (dry.unwrap_err(), apply.unwrap_err());
    let path = format!("{NEW_STATE}/nanohub/mdm/push_cert");
    let refusal =
        format!("refused: {path}: mdm is owned by uid 4242, which is not trusted for this path");
    assert_eq!(dry.to_string(), refusal);
    assert_eq!(apply.to_string(), refusal);
    assert_eq!((dry.exit_code(), apply.exit_code()), (1, 1));
    assert!(!dry_out.contains("DRY-RUN:"), "{dry_out}");
    assert!(!apply_out.contains("\n+ "), "{apply_out}");
    assert_eq!(snapshot(&host), before);
    assert_eq!(changes(&host), Vec::<String>::new());
}

/// Catches a rerun that cannot finish what an apply that stopped part way began (a step done
/// already that the rerun redoes wrongly or refuses: the user, the SCEP CA, a copy, the new
/// settings, a pending restart): for each step of the move, an apply that fails at that step
/// stops there, naming it, the dry run after it is accepted and changes nothing, and a second
/// apply then leaves the host exactly as one apply that did not fail; a rerun after an apply
/// that did not fail is accepted and changes no file. `scep_depot_integration` runs the same with the real CA
/// and the real openssl.
#[test]
fn a_rerun_after_an_apply_that_failed_at_any_step_converges() {
    let kit = kit();
    let a = ask(&old_text());
    rerun_converges_after_each_step(&|| host_before_the_move(&a), &|host, mode| {
        migrate_on(host, &kit, &a, mode)
    });
}
