//! What the hook is given and what it finds: the record directory (locked, missing), the params
//! file (each key, malformed), the command line (both forms, bad ones), and a real server behind
//! the real probe. Every run is read through the hook contract crate as the daemon reads it.

mod support;

use std::ffi::OsString;
use std::fs::{self, File};
use std::net::TcpListener;
use std::os::unix::ffi::OsStringExt;
use std::thread;

use hello::ports::HttpProbe;
use hello::server::{Answer, serve};
use hostkeeper_hook::{Class, Context, Next, Phase, Scalar, Step, Verdict};
use support::{
    FakeProbe, FakeRunner, Form, ParamsSpec, START_A, TARGET, Want, World, contract, ctx, files,
    usage,
};

/// A change to the params the world writes.
type Edit = fn(&mut ParamsSpec);

const STEPS: [Step; 6] = [
    Step::Prepare,
    Step::Cutover,
    Step::Verify,
    Step::Rollback,
    Step::Cleanup,
    Step::Uninstall,
];

fn failed_environment(detail: &str) -> Want<'_> {
    Want {
        class: Class::Environment,
        detail: Some(detail),
        ..Want::new(hostkeeper_hook::Outcome::Failure)
    }
}

/// What the updater does after `step` failed with class `environment` and nothing stated about
/// being reversible: the table of the contract.
fn next_after_environment_failure(step: Step) -> Next {
    let failed = |phase| Verdict::Failed {
        phase,
        class: Class::Environment,
    };
    match step {
        Step::Prepare | Step::Cutover => Next::Rollback {
            then: failed(Phase::BeforeCommit),
        },
        Step::Verify => Next::Rollback {
            then: failed(Phase::GateEnvironment),
        },
        Step::Rollback => Next::Report(Verdict::Halt),
        Step::Cleanup => Next::Report(failed(Phase::AfterCommit)),
        Step::Uninstall => Next::Report(failed(Phase::BeforeCommit)),
    }
}

/// Runs every step against a world that cannot run them, and checks each as an environment
/// failure naming `detail`, with the commands and the record untouched.
fn every_step_fails(world: &World, detail: &str) {
    let (runner, probe) = (FakeRunner::default(), FakeProbe::ready(TARGET));
    let before = world.record().is_dir().then(|| world.snapshot());
    for step in STEPS {
        let ran = world.step(step, &runner, &probe);
        let next = contract(step, &ran, &failed_environment(detail), &ctx());
        assert_eq!(
            next,
            next_after_environment_failure(step),
            "{step}: {ran:?}"
        );
    }
    assert!(runner.take_calls().is_empty(), "{detail}: no command runs");
    assert!(
        probe.take_calls().is_empty(),
        "{detail}: no question is asked"
    );
    assert_eq!(
        world.record().is_dir().then(|| world.snapshot()),
        before,
        "{detail}"
    );
}

/// Catches a step that runs while another holds the record (two steps changing the record and
/// the units at once), one that waits for the lock, and a lock that is not released at the end.
#[test]
fn a_record_held_by_another_step_changes_nothing() {
    let world = World::systemd("locked");
    let held = File::open(world.record()).expect("open the record directory");
    held.lock().expect("take the lock");
    every_step_fails(&world, "another step holds the record");

    drop(held);
    let (runner, probe) = (FakeRunner::default(), FakeProbe::ready(TARGET));
    let ran = world.step(Step::Prepare, &runner, &probe);
    assert_eq!(ran.code, 0, "{ran:?}");
    let ran = world.step(Step::Cutover, &runner, &probe);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(runner.take_calls(), [START_A]);
}

/// Catches a missing record directory not being made (nothing else makes it on a host), being
/// made open to other accounts, or a path that cannot be one being used.
#[test]
fn a_missing_record_directory_is_made_owner_only_and_a_file_there_fails_every_step() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::systemd("no-record");
    fs::remove_dir(world.record()).expect("remove the record directory");
    let (runner, probe) = (FakeRunner::default(), FakeProbe::default());
    let ran = world.run_args(&world.step_args(Step::Prepare), &runner, &probe);
    assert_eq!(ran.code, 0, "{ran:?}");
    let mode = fs::metadata(world.record())
        .expect("the hook made its record directory")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700, "owner only");
    assert_eq!(
        fs::read_to_string(world.record().join("next")).expect("recorded"),
        "a"
    );

    fs::remove_dir_all(world.record()).expect("remove the record directory");
    fs::write(world.record(), "").expect("a file where the directory should be");
    every_step_fails(&world, "is not a directory");

    let under_a_file = World::systemd("record-under-a-file");
    let blocked = under_a_file.dir.path().join("file");
    fs::write(&blocked, "").expect("a file in the way");
    let mut spec = under_a_file.spec();
    spec.record_dir = Some(Scalar::Text(blocked.join("record").display().to_string()));
    under_a_file.write_params(&spec);
    every_step_fails(&under_a_file, "cannot open the record directory");
}

/// Catches a params file that is missing, unreadable or not a table being read as empty.
#[test]
fn a_params_file_that_is_missing_unreadable_or_malformed_fails_every_step() {
    let world = World::systemd("bad-file");
    let args = |file: Option<&std::path::Path>, step: Step| {
        let mut args = vec![OsString::from(step.name())];
        if step == Step::Prepare {
            args.push(world.artifact().into_os_string());
        }
        if let Some(file) = file {
            args.push("--params".into());
            args.push(file.as_os_str().to_owned());
        }
        args
    };
    let (runner, probe) = (FakeRunner::default(), FakeProbe::default());
    let check = |file: Option<&std::path::Path>, detail: &str| {
        for step in STEPS {
            let ran = world.run_args(&args(file, step), &runner, &probe);
            let next = contract(step, &ran, &failed_environment(detail), &ctx());
            assert_eq!(next, next_after_environment_failure(step), "{step}");
        }
    };
    check(None, "needs --params <file>");

    let missing = world.dir.path().join("none.toml");
    check(Some(&missing), "cannot read the params file");
    check(Some(world.dir.path()), "cannot read the params file");

    for text in [
        "units = \n",
        "units = \"a\"\nunits = \"b\"\n",
        "[units]\n",
        "units = \"open\n",
    ] {
        fs::write(world.params_file(), text).expect("write");
        check(Some(&world.params_file()), "is malformed: line");
    }
    assert!(runner.take_calls().is_empty());
}

/// Catches a key of the params file that is missing or of the wrong kind or range being
/// defaulted, and the failure not naming the key.
#[test]
fn a_missing_or_bad_param_fails_every_step_naming_the_key() {
    let world = World::systemd("bad-params");
    let cases: [(&str, Edit, &str); 13] = [
        ("units", |spec| spec.units = None, "\"units\" is missing"),
        (
            "units",
            |spec| spec.units = Some(Scalar::Text("sysv".to_owned())),
            "\"units\" must be \"systemd\" or \"launchd\"",
        ),
        (
            "units",
            |spec| spec.units = Some(Scalar::Integer(1)),
            "\"units\" must be text",
        ),
        ("port", |spec| spec.port = None, "\"port\" is missing"),
        (
            "port",
            |spec| spec.port = Some(Scalar::Text("9310".to_owned())),
            "\"port\" must be an integer from 1 to 65535",
        ),
        (
            "port",
            |spec| spec.port = Some(Scalar::Integer(0)),
            "\"port\" must be an integer from 1 to 65535",
        ),
        (
            "port",
            |spec| spec.port = Some(Scalar::Integer(65536)),
            "\"port\" must be an integer from 1 to 65535",
        ),
        (
            "port",
            |spec| spec.port = Some(Scalar::Integer(-1)),
            "\"port\" must be an integer from 1 to 65535",
        ),
        (
            "record_dir",
            |spec| spec.record_dir = None,
            "\"record_dir\" is missing",
        ),
        (
            "record_dir",
            |spec| spec.record_dir = Some(Scalar::Bool(true)),
            "\"record_dir\" must be text",
        ),
        (
            "record_dir",
            |spec| spec.record_dir = Some(Scalar::Text("relative/dir".to_owned())),
            "\"record_dir\" must be an absolute path",
        ),
        (
            "record_dir",
            |spec| spec.record_dir = Some(Scalar::Text(String::new())),
            "\"record_dir\" must be an absolute path",
        ),
        (
            "updater.target_version",
            |spec| spec.target = Some(Scalar::Integer(3)),
            "\"updater.target_version\" must be text",
        ),
    ];
    for (key, edit, detail) in cases {
        let mut spec = world.spec();
        edit(&mut spec);
        world.write_params(&spec);
        assert!(detail.contains(key), "{detail}");
        every_step_fails(&world, detail);
    }

    // The ends of the port range are accepted.
    for port in [1, 65535] {
        let mut spec = world.spec();
        spec.port = Some(Scalar::Integer(port));
        world.write_params(&spec);
        let ran = world.step(Step::Prepare, &FakeRunner::default(), &FakeProbe::default());
        assert_eq!(ran.code, 0, "port {port}: {ran:?}");
    }
}

/// Catches a verify that runs without the target version (it could not tell which version
/// answers), and any other step that needs it.
#[test]
fn only_verify_needs_the_target_version() {
    let world = World::systemd("no-target");
    let mut spec = world.spec();
    spec.target = None;
    spec.extras = false;
    world.write_params(&spec);
    let (runner, probe) = (FakeRunner::default(), FakeProbe::ready(TARGET));
    for step in [Step::Prepare, Step::Cutover] {
        let ran = world.step(step, &runner, &probe);
        assert_eq!(ran.code, 0, "{step}: {ran:?}");
    }
    let ran = world.step(Step::Verify, &runner, &probe);
    let want = failed_environment("\"updater.target_version\" is missing");
    let next = contract(Step::Verify, &ran, &want, &ctx());
    assert_eq!(next, next_after_environment_failure(Step::Verify));
    assert!(
        probe.take_calls().is_empty(),
        "nothing is asked before the target is known"
    );
    for step in [Step::Rollback, Step::Uninstall] {
        let ran = world.step(step, &runner, &probe);
        assert_eq!(ran.code, 0, "{step}: {ran:?}");
    }
}

fn os(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

/// Catches a command line the hook cannot read being run as something, or exiting with a code
/// the contract does not define for it: it is exit 2 with the one line naming the fault.
#[test]
fn a_bad_command_line_is_exit_two_with_a_line_naming_it() {
    let world = World::systemd("bad-args");
    let (runner, probe) = (FakeRunner::default(), FakeProbe::default());
    let file = world.params_file();
    let file = file.to_str().expect("a path of text");
    let cases = [
        (os(&[]), "no step given"),
        (os(&["frobnicate"]), "unknown step \"frobnicate\""),
        (vec![OsString::from_vec(vec![0xff, b'x'])], "unknown step"),
        (
            os(&["prepare", "--params", file]),
            "prepare needs the staged release directory",
        ),
        (
            os(&["prepare", "/a", "/b", "--params", file]),
            "unexpected argument \"/b\"",
        ),
        (
            os(&["cutover", "/a", "--params", file]),
            "unexpected argument \"/a\"",
        ),
        (os(&["verify", "--params"]), "--params needs a file"),
        (
            os(&["verify", "--params", file, "--params", file]),
            "--params given twice",
        ),
    ];
    for (args, detail) in cases {
        for form in [Form::Direct, Form::UpdateHook] {
            let mut world = World::systemd("bad-args-form");
            world.form = form;
            let ran = world.run_args(&args, &runner, &probe);
            usage(&ran, detail);
            assert_eq!(
                ran.stderr,
                format!("hello update-hook: {}\n", stderr_detail(&ran))
            );
        }
    }
    assert!(runner.take_calls().is_empty());
    assert_eq!(world.snapshot(), files(&[]));

    // `update-hook` alone has no step.
    let ran = hello::run(
        &os(&["update-hook"]),
        &host(&runner, &probe),
        &mut Vec::new(),
        &mut Vec::new(),
    );
    assert_eq!(ran, 2);
}

fn host<'a>(runner: &'a FakeRunner, probe: &'a FakeProbe) -> hello::ports::Host<'a> {
    hello::ports::Host { runner, probe }
}

fn stderr_detail(ran: &support::Ran) -> String {
    let line = hostkeeper_hook::Report::read(&ran.stdout);
    line.detail.expect("a detail")
}

/// Catches the two ways the updater may start the hook (`update-hook <step>`, the manifest's
/// command, and `<step>`, what the daemon's launcher runs) reaching different code, and the
/// direct call of the hook module differing from the program's.
#[test]
fn both_invocation_forms_do_the_same_thing() {
    let mut outputs = Vec::new();
    for (name, form) in [
        ("form-update-hook", Form::UpdateHook),
        ("form-bare", Form::Bare),
        ("form-direct", Form::Direct),
    ] {
        let mut world = World::systemd(name);
        world.form = form;
        let (runner, probe) = (FakeRunner::default(), FakeProbe::ready(TARGET));
        let mut run = Vec::new();
        for step in [
            Step::Prepare,
            Step::Cutover,
            Step::Verify,
            Step::Cleanup,
            Step::Rollback,
            Step::Uninstall,
        ] {
            let ran = world.step(step, &runner, &probe);
            run.push((
                ran.code,
                ran.stdout
                    .replace(&world.dir.path().display().to_string(), "<dir>"),
            ));
        }
        run.push((0, format!("{:?}", runner.take_calls())));
        outputs.push(run);
    }
    assert_eq!(outputs[0], outputs[1], "update-hook <step> and <step>");
    assert_eq!(outputs[0], outputs[2], "the program and the hook module");
    assert_eq!(outputs[0][0].0, 0);
    assert!(
        outputs[0]
            .last()
            .is_some_and(|(_, calls)| calls.contains("hello@a.service"))
    );
}

/// Serves `version` (ready, or not) on a loopback port of the system's choosing, in a thread
/// that lives as long as the test binary. Returns the port.
fn server(version: &'static str, fail_ready: bool) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("the address").port();
    thread::spawn(move || {
        serve(
            listener,
            &Answer {
                version,
                fail_ready,
            },
        )
    });
    port
}

/// Catches the real probe and the real server not agreeing with the hook on what ready, not
/// ready, another version and a port nobody listens on mean: verify over a socket.
#[test]
fn verify_over_a_real_socket_reads_the_real_server() {
    let world = World::systemd("socket");
    let runner = FakeRunner::default();
    let go = |port: u16, step: Step| {
        let mut spec = world.spec();
        spec.port = Some(Scalar::Integer(i64::from(port)));
        world.write_params(&spec);
        world.step(step, &runner, &HttpProbe)
    };
    let prepare = go(server(TARGET, false), Step::Prepare);
    assert_eq!(prepare.code, 0, "{prepare:?}");
    assert_eq!(go(server(TARGET, false), Step::Cutover).code, 0);

    let ran = go(server(TARGET, false), Step::Verify);
    contract(
        Step::Verify,
        &ran,
        &Want::new(hostkeeper_hook::Outcome::Success),
        &ctx(),
    );
    assert!(world.snapshot().contains_key("verified"));

    let ran = go(server(TARGET, true), Step::Verify);
    let want = Want {
        reason: Some("not_ready"),
        detail: Some("503"),
        ..Want::new(hostkeeper_hook::Outcome::Failure)
    };
    contract(Step::Verify, &ran, &want, &ctx());

    let ran = go(server("9.9.9", false), Step::Verify);
    let want = Want {
        reason: Some("wrong_version"),
        detail: Some("\"9.9.9\""),
        ..want
    };
    contract(Step::Verify, &ran, &want, &ctx());

    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("the address").port()
    };
    let ran = go(closed, Step::Verify);
    let want = Want {
        reason: Some("not_listening"),
        retry_after_secs: Some(2),
        detail: Some("cannot connect"),
        ..Want::new(hostkeeper_hook::Outcome::Deferred)
    };
    let next = contract(Step::Verify, &ran, &want, &Context::default());
    assert_eq!(
        next,
        Next::Again {
            step: Step::Verify,
            after_secs: Some(2)
        }
    );
}
