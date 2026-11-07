//! `migrate` on the live host as it is before the move, with no service user of the new kit:
//! the plan makes that user, then writes root's records below its state. Judged as the machine
//! judges it (root and the effective user trusted everywhere, the command line's grants), a dry
//! run and an apply agree, a stranger's directory is still refused, and an apply that stops at
//! any step is finished by the next.

mod common;
mod migrate_support;

use std::cell::Cell;
use std::io;

use common::{FakeMachine, FakeNet, SVC, kit, strict, with_system};
use hostkeeper_mdm::migrate::Migrate;
use hostkeeper_mdm::run_with;
use hostkeeper_mdm_step::fake::{FakeHost, Strict};
use hostkeeper_mdm_step::{Cmd, GETENT, Host, Meta, Mode, Output, SYSTEMCTL, USERADD, Way};
use migrate_support::*;

/// What `useradd` is run with for the new service user.
const USERADD_NEW: [&str; 9] = [
    USERADD,
    "--system",
    "--user-group",
    "--home-dir",
    NEW_STATE,
    "--no-create-home",
    "--shell",
    "/usr/sbin/nologin",
    "hostkeeper-mdm",
];

/// The live host before the move: the old kit's stack, and no user `hostkeeper-mdm` until
/// `useradd` makes it (with the uid the fixtures' files of the new user have).
fn host_before_the_move(a: &Migrate) -> FakeHost {
    let host = live_host(&kit());
    host.reply(&[GETENT, "passwd", "hostkeeper-mdm"], 2, "");
    host.reply(&[GETENT, "group", "hostkeeper-mdm"], 2, "");
    host.reply(&USERADD_NEW, 0, "");
    host.effect_user(&USERADD_NEW, "hostkeeper-mdm", SVC);
    ca_is_made(&host, a);
    host
}

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

/// A host that fails its `fail_at`-th change (a write, a mkdir, a removal, or a command that is
/// not a lookup or a question), as an apply that stops at that step, and counts the changes.
struct FailsAt<'a> {
    host: Strict<'a>,
    fail_at: Option<usize>,
    changes: Cell<usize>,
}

impl<'a> FailsAt<'a> {
    fn new(host: Strict<'a>, fail_at: Option<usize>) -> FailsAt<'a> {
        FailsAt {
            host,
            fail_at,
            changes: Cell::new(0),
        }
    }

    /// One more change; the error the test makes when it is the `fail_at`-th.
    fn change(&self) -> io::Result<()> {
        let n = self.changes.get() + 1;
        self.changes.set(n);
        match self.fail_at == Some(n) {
            true => Err(io::Error::other("the change the test fails")),
            false => Ok(()),
        }
    }
}

/// Whether `cmd` only asks: an account lookup, `systemctl is-*`, and the host checks.
fn asks(cmd: &Cmd) -> bool {
    let program = cmd.program.as_str();
    program == GETENT
        || (program == SYSTEMCTL && cmd.args.first().is_some_and(|a| a.starts_with("is-")))
        || [
            "/usr/bin/uname",
            "/usr/bin/ip",
            "/usr/bin/ss",
            "/usr/bin/openssl",
        ]
        .contains(&program)
}

impl Host for FailsAt<'_> {
    fn euid(&self) -> u32 {
        self.host.euid()
    }
    fn egid(&self) -> u32 {
        self.host.egid()
    }
    fn canonical(&self, path: &str) -> io::Result<String> {
        self.host.canonical(path)
    }
    fn stat(&self, path: &str) -> io::Result<Option<Meta>> {
        self.host.stat(path)
    }
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        self.host.read(path)
    }
    fn read_limited(&self, path: &str, limit: u64) -> io::Result<Option<Vec<u8>>> {
        self.host.read_limited(path, limit)
    }
    fn list(&self, path: &str) -> io::Result<Option<Vec<String>>> {
        self.host.list(path)
    }
    fn write(&self, path: &str, mode: u32, owner: (u32, u32), bytes: &[u8]) -> io::Result<()> {
        self.change()?;
        self.host.write(path, mode, owner, bytes)
    }
    fn mkdir(&self, path: &str, mode: u32, owner: (u32, u32)) -> io::Result<()> {
        self.change()?;
        self.host.mkdir(path, mode, owner)
    }
    fn remove(&self, path: &str) -> io::Result<()> {
        self.change()?;
        self.host.remove(path)
    }
    fn run(&self, cmd: &Cmd) -> io::Result<Output> {
        if !asks(cmd) {
            self.change()?;
        }
        self.host.run(cmd)
    }
    fn way(&self, path: &str) -> io::Result<Option<Way>> {
        self.host.way(path)
    }
}

/// Catches a rerun that cannot finish what an apply that stopped part way began (a step done
/// already that the rerun redoes wrongly or refuses: the user, the SCEP CA, a copy, the new
/// settings, a pending restart): for each step of the move, an apply that fails at that step
/// stops there, naming it, and a second apply then leaves the host exactly as one apply that
/// did not fail.
#[test]
fn a_rerun_after_an_apply_that_failed_at_any_step_converges() {
    let kit = kit();
    let a = ask(&old_text());
    let host = host_before_the_move(&a);
    let whole = FailsAt::new(strict(&host, &grants()), None);
    let (result, shown) = migrate_on(&whole, &kit, &a, Mode::Apply);
    result.unwrap_or_else(|e| panic!("{e}\n{shown}"));
    let steps = whole.changes.get();
    let applied = shown.lines().filter(|l| l.starts_with("+ ")).count();
    assert_eq!(steps, applied, "one change per step\n{shown}");
    let done = snapshot(&host);

    for k in 1..=steps {
        let host = host_before_the_move(&a);
        let failing = FailsAt::new(strict(&host, &grants()), Some(k));
        let (first, _) = migrate_on(&failing, &kit, &a, Mode::Apply);
        let first = first.unwrap_err().to_string();
        assert!(
            first.starts_with(&format!("step {k} ("))
                && first.ends_with("the change the test fails"),
            "{first}"
        );
        let (again, shown) = migrate(&host, &kit, &a, Mode::Apply);
        again.unwrap_or_else(|e| panic!("rerun after step {k}: {e}\n{shown}"));
        assert_eq!(snapshot(&host), done, "rerun after a failure at step {k}");
    }
}
