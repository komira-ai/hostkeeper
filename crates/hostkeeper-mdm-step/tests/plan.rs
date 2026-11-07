//! The planner and the executor on the fake host.

use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{
    Account, Cmd, DRY_RUN_NOTE, ExecError, GETENT, Host, Kind, Mode, NOTHING_TO_DO, Plan,
    PlanError, Planner, SYSTEMCTL, Step, StepError, USERADD, UnitVerb, execute, ids,
};

fn run(plan: &Plan, mode: Mode, host: &FakeHost) -> (Result<(), ExecError>, String) {
    let mut out = Vec::new();
    let result = execute(plan, mode, host, &mut out);
    (result, String::from_utf8(out).expect("utf8"))
}

fn svc() -> Account {
    Account::new("svc", "svc")
}

/// `lines`, each ended by a line feed: a whole output.
fn whole(lines: &[&str]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// Catches a plan line that prints a file's contents: a secret written by a step would
/// show in a dry run. The whole output is the lines naming path, mode, owner and size only,
/// and the note.
#[test]
fn a_dry_run_lists_names_sizes_and_modes_and_never_contents() {
    let host = FakeHost::new(0);
    host.add_user("svc", 900);
    let mut p = Planner::new(&host);
    assert!(p.dir("/srv/x", 0o700, &svc()).unwrap());
    assert!(
        p.file("/srv/x/key", 0o600, &Account::root(), b"TOPSECRET")
            .unwrap()
    );
    p.unit(UnitVerb::DaemonReload, &[]);
    p.unit(UnitVerb::Enable, &["a.service", "b.timer"]);
    p.unit(UnitVerb::Start, &["a.service"]);
    p.run(Cmd::new("/bin/tool").arg("go"));
    host.reply(&[GETENT, "passwd", "other"], 2, "");
    assert!(p.user("other", "/var/lib/other").unwrap());
    assert!(!p.absent("/nope").unwrap());
    let plan = p.finish();
    let (result, shown) = run(&plan, Mode::DryRun, &host);
    result.unwrap();
    assert_eq!(
        shown,
        whole(&[
            "DRY-RUN: mkdir /srv/x mode 0700 owner svc:svc",
            "DRY-RUN: write /srv/x/key mode 0600 owner root:root (9 bytes)",
            "DRY-RUN: systemctl daemon-reload",
            "DRY-RUN: systemctl enable -- a.service b.timer",
            "DRY-RUN: systemctl start -- a.service",
            "DRY-RUN: run /bin/tool go",
            "DRY-RUN: useradd other (system user and group, home /var/lib/other, no login)",
            DRY_RUN_NOTE,
        ])
    );
    // Nothing changed and nothing but lookups ran.
    assert!(host.node("/srv/x").is_none());
    for call in host.calls() {
        assert_eq!(call[0], GETENT, "{call:?}");
    }
}

/// Catches an apply that skips a step, does them out of order, sets a mode other than the
/// one asked for, or prints a file's contents: the whole output is one `+` line per step.
#[test]
fn apply_does_each_step_with_exact_modes_and_owners() {
    let host = FakeHost::new(0);
    host.add_user("svc", 900);
    host.reply(&[SYSTEMCTL, "disable", "--now", "--", "x.service"], 0, "");
    host.reply(&[SYSTEMCTL, "restart", "--", "x.service"], 0, "");
    host.reply(&["/bin/tool", "go"], 0, "");
    host.put_file("/srv/gone", 0o600, (0, 0), b"old");
    let mut p = Planner::new(&host);
    p.dir("/srv/x", 0o750, &svc()).unwrap();
    p.file("/srv/x/f", 0o640, &svc(), b"TOPSECRET").unwrap();
    p.absent("/srv/gone").unwrap();
    p.unit(UnitVerb::DisableNow, &["x.service"]);
    p.unit(UnitVerb::Restart, &["x.service"]);
    p.run(Cmd::new("/bin/tool").arg("go"));
    let (result, shown) = run(&p.finish(), Mode::Apply, &host);
    result.unwrap();
    assert_eq!(
        shown,
        whole(&[
            "+ mkdir /srv/x mode 0750 owner svc:svc",
            "+ write /srv/x/f mode 0640 owner svc:svc (9 bytes)",
            "+ remove /srv/gone",
            "+ systemctl disable --now -- x.service",
            "+ systemctl restart -- x.service",
            "+ run /bin/tool go",
        ])
    );
    let dir = host.node("/srv/x").unwrap();
    assert_eq!(
        (dir.kind, dir.mode, dir.uid, dir.gid),
        (Kind::Dir, 0o750, 900, 900)
    );
    let file = host.node("/srv/x/f").unwrap();
    assert_eq!(
        (file.mode, file.uid, file.gid, file.bytes),
        (0o640, 900, 900, b"TOPSECRET".to_vec())
    );
    assert!(host.node("/srv/gone").is_none());
    let ran: Vec<String> = host
        .calls()
        .iter()
        .filter(|c| c[0] != GETENT)
        .map(|c| c.join(" "))
        .collect();
    assert_eq!(
        ran,
        [
            "/usr/bin/systemctl disable --now -- x.service",
            "/usr/bin/systemctl restart -- x.service",
            "/bin/tool go"
        ]
    );
}

/// Catches a second run that plans again what the first did: a rerun must find nothing.
#[test]
fn a_second_plan_after_an_apply_is_empty() {
    let host = FakeHost::new(0);
    host.add_user("svc", 900);
    let plan_it = |host: &FakeHost| {
        let mut p = Planner::new(host);
        p.dir("/srv/x", 0o750, &svc()).unwrap();
        p.file("/srv/x/f", 0o640, &svc(), b"abc").unwrap();
        p.finish()
    };
    let first = plan_it(&host);
    assert_eq!(first.steps.len(), 2);
    run(&first, Mode::Apply, &host).0.unwrap();
    let second = plan_it(&host);
    assert!(second.steps.is_empty());
    let (result, shown) = run(&second, Mode::Apply, &host);
    result.unwrap();
    assert_eq!(shown, whole(&[NOTHING_TO_DO]));
    let (result, shown) = run(&second, Mode::DryRun, &host);
    result.unwrap();
    assert_eq!(shown, whole(&[NOTHING_TO_DO]));
}

/// Catches a planner that treats a differing mode, owner, content or kind as "the same":
/// each is a step.
#[test]
fn a_different_mode_owner_content_or_kind_is_planned() {
    let host = FakeHost::new(0);
    host.add_user("svc", 900);
    host.put_dir("/d", 0o750, (900, 900));
    host.put_file("/f", 0o640, (900, 900), b"abc");
    host.reply(&[GETENT, "passwd", "later"], 2, "");
    host.reply(&[GETENT, "group", "later"], 2, "");
    let steps = |f: &dyn Fn(&mut Planner<'_>) -> bool| {
        let mut p = Planner::new(&host);
        let added = f(&mut p);
        let count = p.finish().steps.len();
        assert_eq!(added, count == 1);
        count
    };
    assert_eq!(steps(&|p| p.dir("/d", 0o750, &svc()).unwrap()), 0);
    assert_eq!(steps(&|p| p.dir("/d", 0o700, &svc()).unwrap()), 1);
    assert_eq!(steps(&|p| p.dir("/d", 0o750, &Account::root()).unwrap()), 1);
    assert_eq!(steps(&|p| p.dir("/f", 0o640, &svc()).unwrap()), 1);
    assert_eq!(steps(&|p| p.file("/f", 0o640, &svc(), b"abc").unwrap()), 0);
    assert_eq!(steps(&|p| p.file("/f", 0o640, &svc(), b"abd").unwrap()), 1);
    assert_eq!(steps(&|p| p.file("/f", 0o600, &svc(), b"abc").unwrap()), 1);
    assert_eq!(
        steps(&|p| p.file("/f", 0o640, &Account::root(), b"abc").unwrap()),
        1
    );
    assert_eq!(steps(&|p| p.file("/d", 0o640, &svc(), b"").unwrap()), 1);
    assert_eq!(steps(&|p| p.file("/new", 0o640, &svc(), b"").unwrap()), 1);
    assert_eq!(steps(&|p| p.absent("/f").unwrap()), 1);
    // A user that does not exist yet cannot own anything yet: the step is planned.
    assert_eq!(
        steps(&|p| p.dir("/d", 0o750, &Account::new("later", "later")).unwrap()),
        1
    );
}

/// Catches a service plan that restarts what it should only start, or starts what runs:
/// enabled and active with no change is no step; a change restarts; idle starts.
#[test]
fn a_service_is_enabled_started_or_restarted_as_needed() {
    let host = FakeHost::new(0);
    let on = |unit: &str, enabled: i32, active: i32| {
        host.reply(&[SYSTEMCTL, "is-enabled", unit], enabled, "");
        host.reply(&[SYSTEMCTL, "is-active", unit], active, "");
    };
    on("up.service", 0, 0);
    on("idle.service", 1, 3);
    let mut p = Planner::new(&host);
    assert!(!p.service("up.service", false).unwrap());
    assert!(p.service("up.service", true).unwrap());
    assert!(p.service("idle.service", false).unwrap());
    assert!(p.service("idle.service", true).unwrap());
    let lines: Vec<String> = p.finish().steps.iter().map(Step::describe).collect();
    assert_eq!(
        lines,
        [
            "systemctl restart -- up.service",
            "systemctl enable -- idle.service",
            "systemctl start -- idle.service",
            "systemctl enable -- idle.service",
            "systemctl restart -- idle.service",
        ]
    );
}

/// Catches an apply without root: it must refuse before any step, and a dry run needs no
/// root at this layer (the verbs decide).
#[test]
fn applying_without_root_is_refused_and_does_nothing() {
    let host = FakeHost::new(1000);
    host.add_user("svc", 900);
    let mut p = Planner::new(&host);
    p.dir("/srv/x", 0o700, &svc()).unwrap();
    let plan = p.finish();
    let (result, shown) = run(&plan, Mode::Apply, &host);
    let err = result.unwrap_err();
    assert!(matches!(err, ExecError::NotRoot(1000)), "{err}");
    assert_eq!(
        err.to_string(),
        "applying a plan needs root (effective uid is 1000)"
    );
    assert_eq!(shown, "");
    assert!(host.node("/srv/x").is_none());
    run(&plan, Mode::DryRun, &host).0.unwrap();
}

/// Catches an executor that goes on after a failed step, or that prints what the failed
/// program wrote: the whole output is the failed step's line, and the error names the step
/// and the exit status only.
#[test]
fn a_failing_step_stops_the_plan_and_names_itself() {
    let host = FakeHost::new(0);
    host.reply(&["/bin/bad"], 7, "SECRET-IN-OUTPUT");
    host.reply(&["/bin/good"], 0, "");
    let mut p = Planner::new(&host);
    p.run(Cmd::new("/bin/bad"));
    p.run(Cmd::new("/bin/good"));
    let (result, shown) = run(&p.finish(), Mode::Apply, &host);
    let err = result.unwrap_err();
    assert!(
        matches!(
            &err,
            ExecError::Step {
                index: 1,
                source: StepError::Exit(7),
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "step 1 (run /bin/bad) failed: exit status 7"
    );
    assert_eq!(
        format!("{err:?}"),
        "Step { index: 1, what: \"run /bin/bad\", source: Exit(7) }"
    );
    assert_eq!(shown, whole(&["+ run /bin/bad"]));
    assert!(!host.calls().iter().any(|c| c[0] == "/bin/good"));
}

/// Catches a step whose host error, or a unit step whose failure, is lost or mislabelled: a
/// program that cannot be started, a failing unit action and a failed write each end the plan
/// with their own error.
#[test]
fn host_errors_and_failing_units_end_the_plan() {
    let host = FakeHost::new(0);
    let mut p = Planner::new(&host);
    p.run(Cmd::new("/bin/unknown"));
    let (result, shown) = run(&p.finish(), Mode::Apply, &host);
    let err = result.unwrap_err();
    assert!(
        matches!(
            &err,
            ExecError::Step {
                source: StepError::Host(_),
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "step 1 (run /bin/unknown) failed: fake host: unexpected command /bin/unknown"
    );
    assert_eq!(shown, whole(&["+ run /bin/unknown"]));

    host.reply(&[SYSTEMCTL, "daemon-reload"], 1, "");
    let mut p = Planner::new(&host);
    p.unit(UnitVerb::DaemonReload, &[]);
    let (result, _) = run(&p.finish(), Mode::Apply, &host);
    assert_eq!(
        result.unwrap_err().to_string(),
        "step 1 (systemctl daemon-reload) failed: exit status 1"
    );

    // A lookup of the owner that cannot run is a lookup error of the step.
    let mut plan = Plan::default();
    plan.steps.push(Step::File {
        path: "/f".to_owned(),
        mode: 0o600,
        owner: Account::new("nolookup", "nolookup"),
        bytes: b"x".to_vec(),
    });
    let (result, _) = run(&plan, Mode::Apply, &host);
    let err = result.unwrap_err();
    assert!(
        matches!(
            &err,
            ExecError::Step {
                source: StepError::Lookup(PlanError::Host { .. }),
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "step 1 (write /f mode 0600 owner nolookup:nolookup (1 bytes)) failed: \
         getent passwd nolookup: fake host: unexpected command /usr/bin/getent passwd nolookup"
    );
}

/// Catches an executor that swallows a failure to write the plan out: the error is
/// `ExecError::Write`, for a dry run, an apply and an empty plan.
#[test]
fn a_plan_that_cannot_be_written_out_is_an_error() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("closed"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let host = FakeHost::new(0);
    let mut one = Plan::default();
    one.steps.push(Step::Absent {
        path: "/x".to_owned(),
    });
    for (plan, mode) in [
        (&one, Mode::DryRun),
        (&one, Mode::Apply),
        (&Plan::default(), Mode::DryRun),
    ] {
        let err = execute(plan, mode, &host, &mut Broken).unwrap_err();
        assert!(matches!(err, ExecError::Write(_)), "{err:?}");
        assert_eq!(err.to_string(), "cannot write the plan out: closed");
    }
}

/// Catches a runner that lets an unconfigured command through: it fails closed, with an
/// error that names the line, and the line is still recorded.
#[test]
fn the_fake_runner_refuses_what_it_was_not_told() {
    let host = FakeHost::new(0);
    let err = host.run(&Cmd::new("/bin/surprise").arg("x")).unwrap_err();
    assert_eq!(
        err.to_string(),
        "fake host: unexpected command /bin/surprise x"
    );
    assert_eq!(
        host.calls(),
        [vec!["/bin/surprise".to_owned(), "x".to_owned()]]
    );
    host.clear_calls();
    assert!(host.calls().is_empty());
    // A planner over it surfaces the failure instead of guessing.
    let mut p = Planner::new(&host);
    let err = p.user("nobody-such", "/x").unwrap_err();
    assert_eq!(
        err.to_string(),
        "getent passwd nobody-such: fake host: unexpected command /usr/bin/getent passwd nobody-such"
    );
    assert_eq!(
        p.service("u.service", false).unwrap_err().to_string(),
        "systemctl is-enabled u.service: fake host: unexpected command \
         /usr/bin/systemctl is-enabled u.service"
    );
    // Prefix replies answer a family of lines, a later one for the same prefix replacing the
    // earlier; an exact reply wins.
    host.reply_prefix(&["/bin/fam"], 0, "replaced");
    host.reply_prefix(&["/bin/fam"], 0, "any");
    host.reply(&["/bin/fam", "one"], 5, "one");
    assert_eq!(
        host.run(&Cmd::new("/bin/fam").arg("two")).unwrap().stdout,
        "any"
    );
    assert_eq!(host.run(&Cmd::new("/bin/fam").arg("one")).unwrap().code, 5);
}

/// Catches a planner that hides a host error as "nothing there" or as "different": a stat or
/// a read the host refuses is an error naming the operation and the path.
#[test]
fn a_stat_or_read_the_host_refuses_is_an_error_naming_the_path() {
    let host = FakeHost::new(0);
    host.put_file("/f", 0o600, (0, 0), b"x");
    host.deny_stat("/s");
    host.deny_read("/f");
    let mut p = Planner::new(&host);
    for (err, want) in [
        (
            p.dir("/s", 0o700, &Account::root()).unwrap_err(),
            "stat /s: permission denied",
        ),
        (p.absent("/s").unwrap_err(), "stat /s: permission denied"),
        (
            p.file("/f", 0o600, &Account::root(), b"x").unwrap_err(),
            "read /f: permission denied",
        ),
    ] {
        assert!(matches!(err, PlanError::Host { .. }), "{err:?}");
        assert_eq!(err.to_string(), want);
    }
    assert!(p.finish().steps.is_empty());
}

/// Catches a user step that runs useradd with the wrong flags or that cannot own what it
/// creates in the same plan: the owner is looked up when the step runs, after the user is made.
#[test]
fn a_user_is_made_before_what_it_owns_and_with_no_login() {
    let host = FakeHost::new(0);
    host.reply(&[GETENT, "passwd", "svc"], 2, "");
    host.reply(&[GETENT, "group", "svc"], 2, "");
    let mut p = Planner::new(&host);
    assert!(p.user("svc", "/var/lib/svc").unwrap());
    p.dir("/var/lib/svc", 0o700, &svc()).unwrap();
    let plan = p.finish();
    // Before the user exists, the directory step fails: no such user.
    host.reply(
        &[
            USERADD,
            "--system",
            "--user-group",
            "--home-dir",
            "/var/lib/svc",
            "--no-create-home",
            "--shell",
            "/usr/sbin/nologin",
            "svc",
        ],
        0,
        "",
    );
    let (result, _) = run(&plan, Mode::Apply, &host);
    let err = result.unwrap_err();
    assert!(
        matches!(
            &err,
            ExecError::Step {
                index: 2,
                source: StepError::NoAccount { .. },
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "step 2 (mkdir /var/lib/svc mode 0700 owner svc:svc) failed: no user svc or group svc"
    );
    host.add_user("svc", 901);
    run(&plan, Mode::Apply, &host).0.unwrap();
    assert_eq!(host.node("/var/lib/svc").unwrap().uid, 901);
    // A failing useradd is a failed step.
    let host = FakeHost::new(0);
    host.reply(&[GETENT, "passwd", "svc"], 2, "");
    host.reply_prefix(&[USERADD], 9, "");
    let mut p = Planner::new(&host);
    p.user("svc", "/h").unwrap();
    let (result, _) = run(&p.finish(), Mode::Apply, &host);
    assert_eq!(
        result.unwrap_err().to_string(),
        "step 1 (useradd svc (system user and group, home /h, no login)) failed: exit status 9"
    );
}

/// Catches a service account that is accepted by its name alone: an existing user whose
/// entry has uid 0 (a second name for root, or root itself) would run every service of the
/// kit as root. The planner refuses it and plans nothing; a user with any other uid is taken
/// as it is.
#[test]
fn an_existing_account_with_uid_0_is_refused() {
    let host = FakeHost::new(0);
    host.reply(
        &[GETENT, "passwd", "toor"],
        0,
        "toor:x:0:0::/root:/bin/sh\n",
    );
    host.add_user("svc", 900);
    let mut p = Planner::new(&host);
    for name in ["toor", "root"] {
        let err = p.user(name, "/var/lib/svc").unwrap_err();
        assert!(
            matches!(&err, PlanError::RootAccount { user } if user == name),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            format!("refused: the user {name} exists with uid 0; a service does not run as root")
        );
    }
    assert!(!p.user("svc", "/var/lib/svc").unwrap());
    assert!(p.finish().steps.is_empty());
}

/// Catches `Cmd::as_user` that puts the user's command ahead of the user switch, and a
/// lookup that reads a failing getent as success.
#[test]
fn as_user_wraps_and_lookups_read_getent_exit_codes() {
    let cmd = Cmd::new("/bin/x").arg("a").as_user("svc");
    assert_eq!(cmd.line(), "/usr/sbin/runuser -u svc -- /bin/x a");
    assert_eq!(
        cmd.argv(),
        ["/usr/sbin/runuser", "-u", "svc", "--", "/bin/x", "a"]
    );
    let host = FakeHost::new(0);
    host.reply(&[GETENT, "passwd", "ghost"], 2, "");
    host.reply(&[GETENT, "group", "ghost"], 0, "ghost:x:5:\n");
    assert_eq!(ids(&host, &Account::new("ghost", "ghost")).unwrap(), None);
    host.reply(&[GETENT, "passwd", "odd"], 0, "garbage");
    host.reply(&[GETENT, "group", "odd"], 0, "odd:x:5:\n");
    assert_eq!(ids(&host, &Account::new("odd", "odd")).unwrap(), None);
    assert_eq!(ids(&host, &Account::root()).unwrap(), Some((0, 0)));
}

/// Catches fake mkdir that leaves a parent out, which a later `stat` of it would miss, and a
/// fake that ignores `deny_read`.
#[test]
fn the_fake_tree_makes_parents_and_can_deny_reads() {
    let host = FakeHost::new(0);
    assert_eq!((host.euid(), host.egid()), (0, 0));
    host.mkdir("/a/b/c", 0o700, (1, 2)).unwrap();
    assert_eq!(host.paths(), ["/a", "/a/b", "/a/b/c"]);
    assert_eq!(host.node("/a").unwrap().mode, 0o755);
    // A parent that is there is kept as it is.
    host.mkdir("/a/b/d", 0o750, (3, 4)).unwrap();
    assert_eq!(host.paths(), ["/a", "/a/b", "/a/b/c", "/a/b/d"]);
    assert_eq!(host.node("/a/b").unwrap().mode, 0o755);
    assert_eq!(host.node("/a/b/c").unwrap().uid, 1);
    host.write("/a/f", 0o600, (0, 0), b"x").unwrap();
    assert_eq!(host.stat("/a/f").unwrap().unwrap().size, 1);
    host.deny_read("/a/f");
    assert_eq!(
        host.read("/a/f").unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(host.read("/a/none").unwrap(), None);
    host.remove("/a/f").unwrap();
    assert!(host.stat("/a/f").unwrap().is_none());
}

/// Catches a fake that cannot stand for a link, which the verbs must refuse to follow, and
/// an effect that fires for a command other than its own.
#[test]
fn the_fake_can_hold_a_link_and_a_command_can_leave_a_file() {
    let host = FakeHost::new(0);
    host.put_link("/l");
    assert_eq!(host.stat("/l").unwrap().unwrap().kind, Kind::Other);
    host.reply(&["/bin/make"], 0, "");
    host.reply(&["/bin/other"], 0, "");
    host.effect(&["/bin/make"], "/made", 0o600, (1, 2), b"data");
    host.run(&Cmd::new("/bin/other")).unwrap();
    assert!(host.node("/made").is_none());
    host.run(&Cmd::new("/bin/make")).unwrap();
    let made = host.node("/made").unwrap();
    assert_eq!(
        (made.mode, made.uid, made.gid, made.bytes),
        (0o600, 1, 2, b"data".to_vec())
    );
}

/// Catches a plan that shows a secret when it is formatted by an assertion, a panic or a log
/// line: the whole `Debug` of every step prints the length of a file's bytes and never the
/// bytes.
#[test]
fn a_plan_formatted_with_debug_shows_no_file_contents() {
    let plan = Plan {
        steps: vec![
            Step::Dir {
                path: "/d".to_owned(),
                mode: 0o700,
                owner: Account::root(),
            },
            Step::File {
                path: "/s".to_owned(),
                mode: 0o600,
                owner: Account::root(),
                bytes: b"TOPSECRET-VALUE".to_vec(),
            },
            Step::Absent {
                path: "/a".to_owned(),
            },
            Step::User {
                name: "svc".to_owned(),
                home: "/h".to_owned(),
            },
            Step::Unit {
                verb: UnitVerb::Restart,
                units: vec!["a.service".to_owned()],
            },
            Step::Run(Cmd::new("/bin/x").arg("go")),
        ],
    };
    let root = "Account { user: \"root\", group: \"root\" }";
    assert_eq!(
        format!("{plan:?}"),
        format!(
            "Plan {{ steps: [\
             Dir {{ path: \"/d\", mode: 0700, owner: {root} }}, \
             File {{ path: \"/s\", mode: 0600, owner: {root}, bytes: 15 bytes }}, \
             Absent {{ path: \"/a\" }}, \
             User {{ name: \"svc\", home: \"/h\" }}, \
             Unit {{ verb: Restart, units: [\"a.service\"] }}, \
             Run(Cmd {{ program: \"/bin/x\", args: [\"go\"] }})] }}"
        )
    );
    assert_eq!(
        format!("{:#?}", plan.steps[1]),
        "File {\n    path: \"/s\",\n    mode: 0600,\n    owner: Account {\n        \
         user: \"root\",\n        group: \"root\",\n    },\n    bytes: 15 bytes,\n}"
    );
    // A failed equality assertion prints through Debug too: the panic message is whole, and
    // holds neither secret.
    let other = Step::File {
        path: "/s".to_owned(),
        mode: 0o600,
        owner: Account::root(),
        bytes: b"OTHER-SECRET".to_vec(),
    };
    let first = plan.steps[1].clone();
    let panicked = std::panic::catch_unwind(|| assert_eq!(first, other)).unwrap_err();
    let message = panicked
        .downcast_ref::<String>()
        .expect("a formatted panic");
    let file =
        |n: usize| format!("File {{ path: \"/s\", mode: 0600, owner: {root}, bytes: {n} bytes }}");
    assert_eq!(
        *message,
        format!(
            "assertion `left == right` failed\n  left: {}\n right: {}",
            file(15),
            file(12)
        )
    );
}

/// Catches a fake that cannot resolve links the way the real host does: a link in the middle
/// of a path is replaced by its target, the part that does not exist is kept, and a loop
/// of links is an error rather than a hang.
#[test]
fn the_fake_resolves_symbolic_links() {
    let host = FakeHost::new(0);
    host.put_symlink("/srv/op/archives", "/data/real");
    host.put_symlink("/data/real/inner", "/elsewhere");
    assert_eq!(host.canonical("/plain/path").unwrap(), "/plain/path");
    assert_eq!(host.canonical("/").unwrap(), "/");
    assert_eq!(
        host.canonical("/srv/op/archives/x").unwrap(),
        "/data/real/x"
    );
    assert_eq!(
        host.canonical("/srv/op/archives/inner/y").unwrap(),
        "/elsewhere/y"
    );
    host.put_symlink("/loop/a", "/loop/b");
    host.put_symlink("/loop/b", "/loop/a");
    assert_eq!(
        host.canonical("/loop/a").unwrap_err().to_string(),
        "fake host: too many symbolic links"
    );
}

/// Catches a fake that prints file contents in `Debug`, and one that reads a link or a file
/// above a limit as fine, which the real host refuses: the whole `Debug` of a node holds its
/// size and no byte of it.
#[test]
fn the_fake_node_hides_contents_and_reads_like_the_real_host() {
    let host = FakeHost::new(0);
    host.put_file("/s", 0o600, (0, 0), b"TOPSECRET");
    assert_eq!(
        format!("{:?}", host.node("/s").unwrap()),
        "Node { kind: File, mode: 0600, uid: 0, gid: 0, bytes: 9 bytes }"
    );
    assert_eq!(host.read_limited("/s", 9).unwrap().unwrap(), b"TOPSECRET");
    assert_eq!(
        host.read_limited("/s", 8).unwrap_err().kind(),
        std::io::ErrorKind::FileTooLarge
    );
    assert_eq!(host.read_limited("/none", 8).unwrap(), None);
    host.put_link("/l");
    assert_eq!(
        host.read("/l").unwrap_err().to_string(),
        "fake host: refused: a symbolic link"
    );
    host.deny_read("/s");
    assert_eq!(
        host.read("/s").unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
}
