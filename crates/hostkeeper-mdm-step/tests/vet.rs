//! The check of a whole plan before its first step, and the strict view of the fake host that
//! judges owners as the real host does.

use hostkeeper_mdm_step::fake::{FakeHost, Strict};
use hostkeeper_mdm_step::{
    Account, Cmd, ExecError, GETENT, Host, Kind, Mode, Plan, PlanError, Step, StepError, Trustee,
    USERADD, Way, execute,
};

const SVC: u32 = 900;
const STRANGER: u32 = 4242;

fn run(plan: &Plan, mode: Mode, host: &dyn Host) -> (Result<(), ExecError>, String) {
    let mut out = Vec::new();
    let result = execute(plan, mode, host, &mut out);
    (result, String::from_utf8(out).expect("utf8"))
}

fn svc() -> Account {
    Account::new("svc", "svc")
}

fn dir(path: &str, owner: &Account) -> Step {
    Step::Dir {
        path: path.to_owned(),
        mode: 0o700,
        owner: owner.clone(),
    }
}

fn file(path: &str, owner: &Account) -> Step {
    Step::File {
        path: path.to_owned(),
        mode: 0o644,
        owner: owner.clone(),
        bytes: b"topic\n".to_vec(),
    }
}

fn plan(steps: Vec<Step>) -> Plan {
    Plan { steps }
}

/// The `useradd` line a `Step::User` for `svc` with home `/state` runs.
const USERADD_SVC: [&str; 9] = [
    USERADD,
    "--system",
    "--user-group",
    "--home-dir",
    "/state",
    "--no-create-home",
    "--shell",
    "/usr/sbin/nologin",
    "svc",
];

/// A host where `svc` does not exist yet, and `useradd` makes it with uid 900.
fn host_without_svc() -> FakeHost {
    let host = FakeHost::new(0);
    host.reply(&[GETENT, "passwd", "svc"], 2, "");
    host.reply(&[GETENT, "group", "svc"], 2, "");
    host.reply(&USERADD_SVC, 0, "");
    host.effect_user(&USERADD_SVC, "svc", SVC);
    host
}

/// The plan of the move that failed: the service user made, its state directory made as its
/// own, then a root's record written into that directory.
fn user_state_and_record() -> Plan {
    plan(vec![
        Step::User {
            name: "svc".to_owned(),
            home: "/state".to_owned(),
        },
        dir("/state", &svc()),
        file("/state/topic", &Account::root()),
    ])
}

/// Both results, as text: a dry run and an apply agree when these are equal.
fn shown(result: &Result<(), ExecError>) -> String {
    match result {
        Ok(()) => "ok".to_owned(),
        Err(e) => e.to_string(),
    }
}

/// Catches a dry run that plans a step its apply refuses part way (the live move printed step
/// 43 in its dry run, then its apply stopped there with the steps before it done): with the
/// state granted to no one, the root's record below the service's new state directory is
/// refused by the dry run and by the apply alike, before any step, with the walk's words, and
/// the apply changes nothing and runs nothing but lookups.
#[test]
fn a_dry_run_refuses_what_its_apply_would_refuse() {
    let host = host_without_svc();
    let view = host.strict();
    let (dry, dry_out) = run(&user_state_and_record(), Mode::DryRun, &view);
    let (apply, apply_out) = run(&user_state_and_record(), Mode::Apply, &view);
    let want = "step 3 (write /state/topic mode 0644 owner root:root (6 bytes)) would fail, so no \
                step was carried out: refused: /state/topic: state is to be owned by svc, a user \
                this plan makes, which is not trusted for this path";
    assert_eq!(shown(&dry), want);
    assert_eq!(shown(&apply), want);
    assert!(matches!(
        apply,
        Err(ExecError::Refused {
            index: 3,
            source: StepError::Host(_),
            ..
        })
    ));
    assert_eq!((dry_out.as_str(), apply_out.as_str()), ("", ""));
    assert!(host.paths().is_empty(), "{:?}", host.paths());
    assert!(
        host.calls().iter().all(|c| c[0] == GETENT),
        "{:?}",
        host.calls()
    );
}

/// Catches a check whose words differ from the walk's, so that an operator reads two reasons
/// for one refusal: when the service user exists and owns the state already, the check refuses
/// with exactly the words the strict host's own write gives for the same path.
#[test]
fn the_check_refuses_in_the_words_of_the_walk() {
    let host = FakeHost::new(0);
    host.add_user("svc", SVC);
    host.put_dir("/state", 0o700, (SVC, SVC));
    let view = host.strict();
    let walk = view
        .write("/state/topic", 0o644, (0, 0), b"topic\n")
        .unwrap_err()
        .to_string();
    assert_eq!(
        walk,
        "refused: /state/topic: state is owned by uid 900, which is not trusted for this path"
    );
    let (apply, _) = run(
        &plan(vec![file("/state/topic", &Account::root())]),
        Mode::Apply,
        &view,
    );
    assert_eq!(
        shown(&apply),
        format!(
            "step 1 (write /state/topic mode 0644 owner root:root (6 bytes)) would fail, so no \
             step was carried out: {walk}"
        )
    );
}

/// Catches a grant made by the id the service user had when the run started (none, on a host
/// the plan makes the user on): a grant of the state by name trusts the user the plan makes,
/// so the dry run and the apply both carry out the whole plan, and the record is root's in the
/// service user's directory.
#[test]
fn a_grant_by_name_trusts_the_user_the_plan_makes() {
    let host = host_without_svc();
    let view = host.strict().trust_below_user("/state", "svc");
    let (dry, dry_out) = run(&user_state_and_record(), Mode::DryRun, &view);
    assert_eq!(shown(&dry), "ok");
    assert!(
        dry_out.contains("DRY-RUN: write /state/topic mode 0644 owner root:root (6 bytes)\n"),
        "{dry_out}"
    );
    assert!(host.paths().is_empty());
    let (apply, _) = run(&user_state_and_record(), Mode::Apply, &view);
    assert_eq!(shown(&apply), "ok");
    let state = host.node("/state").unwrap();
    assert_eq!((state.kind, state.mode, state.uid), (Kind::Dir, 0o700, SVC));
    let topic = host.node("/state/topic").unwrap();
    assert_eq!((topic.mode, topic.uid, topic.gid), (0o644, 0, 0));
}

/// Catches a grant that trusts more than its user below its directory: a directory a stranger
/// owns is refused on the way to a write, to a mkdir and to a removal, inside the granted
/// state as outside it, by the dry run and the apply alike; and a grant to a user that does
/// not exist and that no step makes trusts nobody.
#[test]
fn a_strangers_directory_is_refused_by_both_even_below_a_grant() {
    let host = FakeHost::new(0);
    host.add_user("svc", SVC);
    host.reply(&[GETENT, "passwd", "ghost"], 2, "");
    host.put_dir("/state", 0o700, (SVC, SVC));
    host.put_dir("/state/sub", 0o777, (STRANGER, STRANGER));
    host.put_dir("/srv", 0o755, (STRANGER, STRANGER));
    let view = host
        .strict()
        .trust_below_user("/state", "svc")
        .trust_below_user("/srv", "ghost");
    let cases = [
        (
            file("/state/sub/f", &Account::root()),
            "write /state/sub/f mode 0644 owner root:root (6 bytes)",
            "/state/sub/f: sub",
        ),
        (
            dir("/state/sub/d", &svc()),
            "mkdir /state/sub/d mode 0700 owner svc:svc",
            "/state/sub/d: sub",
        ),
        (
            Step::Absent {
                path: "/state/sub/f".to_owned(),
            },
            "remove /state/sub/f",
            "/state/sub/f: sub",
        ),
        (
            file("/srv/f", &Account::root()),
            "write /srv/f mode 0644 owner root:root (6 bytes)",
            "/srv/f: srv",
        ),
    ];
    for (step, what, at) in cases {
        let want = format!(
            "step 1 ({what}) would fail, so no step was carried out: refused: {at} is owned by \
             uid 4242, which is not trusted for this path"
        );
        let p = plan(vec![step]);
        for mode in [Mode::DryRun, Mode::Apply] {
            assert_eq!(shown(&run(&p, mode, &view).0), want, "{mode:?}");
        }
    }
    // Nothing was made or removed.
    assert_eq!(host.paths(), ["/srv", "/state", "/state/sub"]);
}

/// Catches a check that judges a directory by its owner before an earlier step changes it, or
/// that forgets the directories a mkdir makes on the way: a stranger's directory that a step
/// makes root's first is then trusted (the apply chowns it before the write), a directory a
/// step makes the service user's is then not trusted for a root's write without a grant, and
/// the directories a mkdir makes above its own are the effective user's.
#[test]
fn a_directory_step_sets_the_owner_the_later_steps_meet() {
    let host = FakeHost::new(0);
    host.add_user("svc", SVC);
    host.put_dir("/data", 0o755, (STRANGER, STRANGER));
    let view = host.strict();
    let reowned = plan(vec![
        dir("/data", &Account::root()),
        file("/data/f", &Account::root()),
    ]);
    for mode in [Mode::DryRun, Mode::Apply] {
        assert_eq!(shown(&run(&reowned, mode, &view).0), "ok", "{mode:?}");
    }
    assert_eq!(host.node("/data").unwrap().uid, 0);

    let nested = plan(vec![
        dir("/a/b/c", &svc()),
        file("/a/b/f", &Account::root()),
        file("/a/b/c/f", &Account::root()),
    ]);
    let want = "step 3 (write /a/b/c/f mode 0644 owner root:root (6 bytes)) would fail, so no \
                step was carried out: refused: /a/b/c/f: c is owned by uid 900, which is not \
                trusted for this path";
    for mode in [Mode::DryRun, Mode::Apply] {
        assert_eq!(shown(&run(&nested, mode, &view).0), want, "{mode:?}");
    }
    assert!(host.node("/a").is_none());
}

/// Catches a check that takes the directories a mkdir makes on the way as trusted whoever
/// makes them: with only the service user trusted (it owns the root here), the directory a
/// mkdir would make above its own is the effective user's (root's), which is refused, by the
/// check and by the strict host's own mkdir alike; the same plan passes once root is trusted.
#[test]
fn the_directories_a_mkdir_makes_on_the_way_are_the_effective_users() {
    let host = FakeHost::new(0);
    host.add_user("svc", SVC);
    host.put_dir("/", 0o755, (SVC, SVC));
    let only_svc = host.trusting(&[SVC]);
    let p = plan(vec![dir("/m/n", &svc())]);
    let walk = "refused: /m/n: m is owned by uid 0, which is not trusted for this path";
    assert_eq!(
        only_svc
            .mkdir("/m/n", 0o700, (SVC, SVC))
            .unwrap_err()
            .to_string(),
        walk
    );
    for mode in [Mode::DryRun, Mode::Apply] {
        assert_eq!(
            shown(&run(&p, mode, &only_svc).0),
            format!(
                "step 1 (mkdir /m/n mode 0700 owner svc:svc) would fail, so no step was carried \
                 out: {walk}"
            ),
            "{mode:?}"
        );
    }
    assert_eq!(shown(&run(&p, Mode::Apply, &host.trusting(&[0])).0), "ok");
    assert_eq!(host.node("/m/n").unwrap().uid, SVC);
}

/// Catches a check that passes a step it cannot judge: an owner whose lookup cannot run and a
/// path the walk refuses each refuse the plan before its first step, with their own error; a
/// path below a name that is neither there nor made is left to the step, which meets it.
#[test]
fn a_step_the_check_cannot_judge_refuses_the_plan() {
    let host = FakeHost::new(0);
    let view = host.strict();
    let (result, _) = run(
        &plan(vec![file("/f", &Account::new("nolookup", "nolookup"))]),
        Mode::DryRun,
        &view,
    );
    let err = result.unwrap_err();
    assert!(
        matches!(
            &err,
            ExecError::Refused {
                source: StepError::Lookup(PlanError::Host { .. }),
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "step 1 (write /f mode 0644 owner nolookup:nolookup (6 bytes)) would fail, so no step was \
         carried out: getent passwd nolookup: fake host: unexpected command /usr/bin/getent \
         passwd nolookup"
    );
    let dotted = plan(vec![Step::Absent {
        path: "/a/../f".to_owned(),
    }]);
    assert_eq!(
        shown(&run(&dotted, Mode::DryRun, &view).0),
        "step 1 (remove /a/../f) would fail, so no step was carried out: refused: /a/../f has a \
         . or .. component"
    );
    let below_nothing = plan(vec![
        file("/none/f", &Account::root()),
        Step::Run(Cmd::new("/bin/tool")),
    ]);
    assert_eq!(shown(&run(&below_nothing, Mode::DryRun, &view).0), "ok");
}

/// Catches a plain fake that judges owners (every existing test of a verb would need grants it
/// was never given) or looks anyone up for it: it says nothing is on the way, so a plan below
/// a stranger's directory and for an owner it cannot look up is printed as before.
#[test]
fn the_plain_fake_judges_no_owner() {
    let host = FakeHost::new(0);
    host.put_dir("/srv", 0o755, (STRANGER, STRANGER));
    assert_eq!(host.way("/srv/f").unwrap(), None);
    let p = plan(vec![file("/srv/f", &Account::new("nolookup", "nolookup"))]);
    let (result, out) = run(&p, Mode::DryRun, &host);
    result.unwrap();
    assert!(out.starts_with("DRY-RUN: write /srv/f"), "{out}");
    assert!(host.calls().is_empty(), "{:?}", host.calls());
}

/// Catches a strict view that judges only writes, that names the wrong directory, or that
/// changes what the fake answers: every operation on a path below a stranger's directory is
/// refused in the walk's words (the root included when a stranger holds it), a path that
/// names no file or has a `..` is refused, and what is trusted reads through.
#[test]
fn the_strict_view_judges_every_operation_on_a_path() {
    let host = FakeHost::new(0);
    host.add_user("svc", SVC);
    host.put_dir("/srv", 0o755, (STRANGER, STRANGER));
    host.put_file("/srv/f", 0o600, (0, 0), b"f");
    host.put_dir("/ok", 0o755, (0, 0));
    host.put_file("/ok/f", 0o600, (0, 0), b"ok");
    host.reply(&["/bin/echo"], 0, "hi\n");
    let view = host.strict();
    let refused = "refused: /srv/f: srv is owned by uid 4242, which is not trusted for this path";
    let errors = [
        view.stat("/srv/f").unwrap_err(),
        view.read("/srv/f").unwrap_err(),
        view.read_limited("/srv/f", 9).unwrap_err(),
        view.list("/srv/f").unwrap_err(),
        view.write("/srv/f", 0o600, (0, 0), b"x").unwrap_err(),
        view.mkdir("/srv/f", 0o700, (0, 0)).unwrap_err(),
        view.remove("/srv/f").unwrap_err(),
    ];
    for (i, e) in errors.iter().enumerate() {
        assert_eq!(e.to_string(), refused, "operation {i}");
    }
    assert_eq!(
        view.way("/srv/../g").unwrap_err().to_string(),
        "refused: /srv/../g has a . or .. component"
    );
    assert_eq!(
        view.stat("/").unwrap_err().to_string(),
        "refused: / names no file"
    );
    assert_eq!(
        view.way("/").unwrap_err().to_string(),
        "refused: / names no file"
    );
    assert_eq!(host.node("/srv/f").unwrap().bytes, b"f");

    assert_eq!(view.read("/ok/f").unwrap().unwrap(), b"ok");
    assert_eq!(view.read_limited("/ok/f", 9).unwrap().unwrap(), b"ok");
    assert_eq!(view.list("/ok").unwrap().unwrap(), ["f"]);
    assert_eq!(view.stat("/ok/f").unwrap().unwrap().size, 2);
    view.write("/ok/g", 0o600, (0, 0), b"g").unwrap();
    view.mkdir("/ok/d", 0o700, (SVC, SVC)).unwrap();
    view.remove("/ok/g").unwrap();
    assert!(host.node("/ok/g").is_none());
    assert_eq!(view.run(&Cmd::new("/bin/echo")).unwrap().stdout, "hi\n");
    assert_eq!(view.canonical("/ok/f").unwrap(), "/ok/f");
    assert_eq!((view.euid(), view.egid()), (0, 0));

    let stranger_root = FakeHost::new(0);
    stranger_root.put_dir("/", 0o755, (STRANGER, STRANGER));
    assert_eq!(
        stranger_root.strict().stat("/f").unwrap_err().to_string(),
        "refused: /f: / is owned by uid 4242, which is not trusted for this path"
    );
}

/// Catches a way that lists owners past a name that is not a directory or the path's own
/// owner, or grants that reach past their directory or are not resolved: the strict view says
/// the root's and each directory's owner up to the first that is not one, and who is trusted
/// there, a user that exists by its id, one that does not by its name, a grant of another
/// directory not at all.
#[test]
fn the_way_names_the_owners_and_whom_a_path_trusts() {
    let host = FakeHost::new(0);
    host.add_user("svc", SVC);
    host.reply(&[GETENT, "passwd", "later"], 2, "");
    host.put_dir("/state", 0o700, (SVC, SVC));
    host.put_dir("/state/a", 0o700, (SVC, SVC));
    host.put_file("/state/a/file", 0o600, (SVC, SVC), b"");
    let view: Strict<'_> = host
        .strict()
        .trust_below_user("/state", "svc")
        .trust_below_user("/state", "later")
        .trust_below("/state/a", 7)
        .trust_below("/state2", 8);
    assert_eq!(
        view.way("/state/a/file/below/x").unwrap(),
        Some(Way {
            root: "/".to_owned(),
            owners: vec![0, SVC, SVC],
            trusted: vec![
                Trustee::Uid(0),
                Trustee::Uid(0),
                Trustee::Uid(SVC),
                Trustee::User("later".to_owned()),
                Trustee::Uid(7),
            ],
        })
    );
}
