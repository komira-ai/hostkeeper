//! The maintenance hook's command line, its parameters and the kinds of each platform: what
//! `hostkeeper maintenance-hook` parses, which parameters it refuses and how it words each
//! refusal, the stubs every platform holds until its work is written, and the record and reboot
//! that stand in until they are.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_daemon::cli::{self, Cli, Command, DEFAULT_PACKAGE_LOCK_WAIT_SECS, SystemdVars};
use hostkeeper_daemon::maintenance_hook::{
    Cutover, HookArgs, HookPorts, HookStep, Kind, KindError, KindState, MAX_PARAMS_BYTES,
    MaintainParams, MaintenanceKind, ParamsError, RebootError, Rebooter, RecordError, Registry,
    RunRecord, Unsupported, Unwired, main_with, registry_for, unwired_record,
};
use hostkeeper_hook::{Class, Outcome, Report};
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_proto::hostkeeper::agent::v1::{Maintain, MaintenanceKind as WireKind};
use hostkeeper_steps::{BootId, ProcBootId};
use hostkeeper_testkit::Scratch;

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch")
}

/// The JSON of a run of `kinds`, each given by its enum name.
fn json(run_id: &str, kinds: &[&str]) -> String {
    let kinds: Vec<String> = kinds.iter().map(|kind| format!("{kind:?}")).collect();
    format!(r#"{{"runId":"{run_id}","kinds":[{}]}}"#, kinds.join(","))
}

fn read(text: &str) -> Result<MaintainParams, ParamsError> {
    MaintainParams::read(&mut text.as_bytes())
}

fn with_kinds(kinds: Vec<i32>) -> Result<MaintainParams, ParamsError> {
    MaintainParams::new(Maintain {
        run_id: "run-1".to_owned(),
        kinds,
        ..Maintain::default()
    })
}

/// Proves: the kinds of the parameters come out in the order a cutover applies them (macOS,
/// Xcode, Linux, reboot), the run id as given, and the rest of the `Maintain` kept.
///
/// Catches: the kinds kept in the order's order, or a field of the order dropped on the way to
/// the kinds.
#[test]
fn the_parameters_give_the_kinds_in_the_order_of_a_cutover() {
    let params = read(&json(
        "run-1",
        &[
            "MAINTENANCE_KIND_REBOOT",
            "MAINTENANCE_KIND_LINUX",
            "MAINTENANCE_KIND_XCODE",
            "MAINTENANCE_KIND_MACOS",
        ],
    ))
    .expect("valid");
    assert_eq!(params.run_id(), "run-1");
    assert_eq!(params.kinds().collect::<Vec<_>>(), Kind::ALL);
    assert_eq!(params.maintain().kinds, [4, 3, 2, 1]);
}

/// Proves: each refusal of the parameters, at each bound, word for word: a run id of 0 or 65
/// bytes (64 taken), no kinds or five, an unspecified or unknown kind, a kind twice, input
/// that is not one JSON `Maintain` (empty, trailing text, a wrong type), and more than 64 KiB
/// (64 KiB taken).
///
/// Catches: a bound off by one, an order with no kind run as done, or an unknown kind passed
/// over.
#[test]
fn each_refusal_of_the_parameters_at_its_bound() {
    let at_bound = "r".repeat(64);
    assert_eq!(
        read(&json(&at_bound, &["MAINTENANCE_KIND_LINUX"]))
            .expect("64 bytes")
            .run_id(),
        at_bound
    );
    let mut padded = json("run-1", &["MAINTENANCE_KIND_LINUX"]);
    padded.push_str(&" ".repeat(MAX_PARAMS_BYTES as usize - padded.len()));
    assert!(read(&padded).is_ok());
    padded.push(' ');
    let cases: [(Result<MaintainParams, ParamsError>, ParamsError, &str); 11] = [
        (
            read(&padded),
            ParamsError::TooLarge,
            "the parameters are more than 65536 bytes",
        ),
        (
            read(&json("", &["MAINTENANCE_KIND_LINUX"])),
            ParamsError::RunId,
            "run_id is not 1 to 64 bytes",
        ),
        (
            read(&json(&"r".repeat(65), &["MAINTENANCE_KIND_LINUX"])),
            ParamsError::RunId,
            "run_id is not 1 to 64 bytes",
        ),
        (
            read(&json("run-1", &[])),
            ParamsError::KindCount(0),
            "kinds holds 0 entries, not 1 to 4",
        ),
        (
            with_kinds(vec![1, 2, 3, 4, 1]),
            ParamsError::KindCount(5),
            "kinds holds 5 entries, not 1 to 4",
        ),
        (
            read(&json("run-1", &["MAINTENANCE_KIND_UNSPECIFIED"])),
            ParamsError::UnknownKind,
            "kinds holds a kind this hook does not know",
        ),
        (
            with_kinds(vec![3, 99]),
            ParamsError::UnknownKind,
            "kinds holds a kind this hook does not know",
        ),
        (
            with_kinds(vec![3, 4, 3]),
            ParamsError::DuplicateKind(Kind::Linux),
            "kinds holds linux twice",
        ),
        (
            read(""),
            ParamsError::NotMaintain,
            "the parameters are not the JSON of a hostkeeper.agent.v1.Maintain",
        ),
        (
            read(&format!("{} x", json("run-1", &["MAINTENANCE_KIND_LINUX"]))),
            ParamsError::NotMaintain,
            "the parameters are not the JSON of a hostkeeper.agent.v1.Maintain",
        ),
        (
            read(r#"{"runId":7,"kinds":["MAINTENANCE_KIND_LINUX"]}"#),
            ParamsError::NotMaintain,
            "the parameters are not the JSON of a hostkeeper.agent.v1.Maintain",
        ),
    ];
    for (got, error, text) in cases {
        assert_eq!(got, Err(error.clone()), "{text}");
        assert_eq!(error.to_string(), text);
    }
}

/// Proves: a field this release does not know is passed over, and the fields it knows are read:
/// parameters a newer daemon wrote are read by an older hook.
///
/// Catches: parameters refused whole for one added field, which would fail every step of a run
/// across a release that adds one.
#[test]
fn a_field_this_release_does_not_know_is_passed_over() {
    let params = read(
        r#"{"runId":"run-1","kinds":["MAINTENANCE_KIND_LINUX"],"addedLater":{"x":1},"linuxScope":"LINUX_SCOPE_ALL"}"#,
    )
    .expect("read");
    assert_eq!(params.kinds().collect::<Vec<_>>(), [Kind::Linux]);
    assert_eq!(params.maintain().linux_scope, 2);
}

/// Proves: each kind's word, its wire value both ways, and that `UNSPECIFIED` and a value this
/// release does not know are no kind.
///
/// Catches: a kind read as another, which would apply an update the order did not name.
#[test]
fn each_kind_maps_to_its_wire_value_and_word() {
    let words = Kind::ALL.map(|kind| kind.to_string());
    assert_eq!(words, ["macos", "xcode", "linux", "reboot"]);
    let wire = [
        WireKind::Macos,
        WireKind::Xcode,
        WireKind::Linux,
        WireKind::Reboot,
    ];
    for (kind, value) in Kind::ALL.into_iter().zip(wire) {
        assert_eq!(kind.wire(), value);
        assert_eq!(Kind::from_wire(i32::from(value)), Some(kind));
        assert_eq!(kind.name(), kind.to_string());
    }
    assert_eq!(Kind::from_wire(0), None);
    assert_eq!(Kind::from_wire(5), None);
}

/// Proves: a Mac holds `macos`, `xcode` and `reboot`, a Linux host (systemd or OpenRC) `linux`
/// and `reboot`, each a stub whose four steps fail `unsupported_kind` naming the kind; a kind
/// the platform does not hold fails the same way.
///
/// Catches: a platform offered a kind it can never apply, or a stub that answers done.
#[test]
fn each_platform_holds_its_kinds_as_stubs_until_their_work_is_written() {
    let platforms = [
        (
            PlatformKind::Macos,
            vec![Kind::Macos, Kind::Xcode, Kind::Reboot],
        ),
        (PlatformKind::Linux, vec![Kind::Linux, Kind::Reboot]),
        (PlatformKind::Openrc, vec![Kind::Linux, Kind::Reboot]),
    ];
    let params = with_kinds(vec![3]).expect("valid");
    for (platform, kinds) in platforms {
        let registry = Registry::for_platform(platform);
        assert_eq!(registry.kinds().collect::<Vec<_>>(), kinds, "{platform:?}");
        for kind in Kind::ALL {
            let unsupported = KindError::unsupported(kind);
            let Ok(work) = registry.get(kind) else {
                assert!(!kinds.contains(&kind), "{platform:?} {kind}");
                assert_eq!(registry.get(kind).err(), Some(unsupported));
                continue;
            };
            assert_eq!(work.prepare(&params), Err(unsupported.clone()));
            assert_eq!(work.cutover(&params), Err(unsupported.clone()));
            assert_eq!(work.after_reboot(&params), Err(unsupported.clone()));
            assert_eq!(work.verify(&params), Err(unsupported));
        }
    }
    assert_eq!(
        Unsupported(Kind::Xcode).verify(&params),
        Err(KindError::Failed {
            reason: "unsupported_kind",
            detail: "xcode is not supported on this host".to_owned(),
        })
    );
}

/// Proves: the words of a kind's failure and deferral, the record's and the reboot's errors.
///
/// Catches: an error whose text loses its reason or its detail.
#[test]
fn the_errors_say_what_failed() {
    let deferred = KindError::Deferred {
        reason: "disk_short",
        detail: "12 GB free".to_owned(),
    };
    assert_eq!(deferred.to_string(), "deferred, disk_short: 12 GB free");
    assert_eq!(
        KindError::unsupported(Kind::Linux).to_string(),
        "failed, unsupported_kind: linux is not supported on this host"
    );
    assert_eq!(
        RecordError::Failed("no space left".to_owned()).to_string(),
        "the run record failed: no space left"
    );
    assert_eq!(
        RebootError::NotWired.to_string(),
        "this host has no reboot wired yet"
    );
}

/// Proves: until the run record and the reboot are written, the host's stand-in holds every kind
/// pending and refuses every write and the reboot.
///
/// Catches: a stand-in that claims a write it did not make, or a reboot it did not ask for.
#[test]
fn the_stand_in_record_holds_nothing_and_refuses_writes_and_the_reboot() {
    let boot = BootId::uuid("test", "0f6b1c3e-5a2d-4c8e-9b7f-1d2e3f4a5b6c").expect("uuid");
    for kind in Kind::ALL {
        assert_eq!(Unwired.state("run-1", kind), Ok(KindState::Pending));
        assert_eq!(
            Unwired.reboot_under_way("run-1", kind, &boot),
            Err(RecordError::NotWired)
        );
        assert_eq!(
            Unwired.applied("run-1", kind, "1"),
            Err(RecordError::NotWired)
        );
    }
    assert_eq!(Unwired.reboot(), Err(RebootError::NotWired));
}

/// Runs `step` through `main_with` with `--params <path>`, over a registry holding `kinds` and
/// the stand-in record and reboot: its outcome and its standard output.
fn main_on_file(registry: &Registry, step: HookStep, path: &Path) -> (Outcome, String) {
    let boot = ProcBootId::at(path);
    let ports = HookPorts {
        kinds: registry,
        record: &Unwired,
        rebooter: &Unwired,
        boot_id: &boot,
    };
    let args = HookArgs {
        step,
        params: path.to_owned(),
        package_lock_wait_secs: DEFAULT_PACKAGE_LOCK_WAIT_SECS,
    };
    let mut stdout = Vec::new();
    let mut stdin = io::empty();
    let outcome = main_with(&args, &ports, &mut stdin, &mut stdout);
    (outcome, String::from_utf8(stdout).expect("utf-8"))
}

/// A kind that applies itself, done in every step.
struct Applies;

impl MaintenanceKind for Applies {
    fn prepare(&self, _params: &MaintainParams) -> Result<(), KindError> {
        Ok(())
    }

    fn cutover(&self, _params: &MaintainParams) -> Result<Cutover, KindError> {
        Ok(Cutover::Applied {
            version: "1".to_owned(),
        })
    }

    fn after_reboot(&self, _params: &MaintainParams) -> Result<String, KindError> {
        Ok("1".to_owned())
    }

    fn verify(&self, _params: &MaintainParams) -> Result<(), KindError> {
        Ok(())
    }
}

fn line(outcome: Outcome, reason: Option<&str>, detail: Option<&str>) -> (Outcome, String) {
    let report = Report {
        class: Class::Release,
        reason: reason.map(str::to_owned),
        detail: detail.map(str::to_owned),
        ..Report::default()
    };
    (outcome, format!("{}\n", report.to_line()))
}

/// Proves: `--params <file>` reads the file, not standard input; a file that is not there fails
/// `params_invalid` naming the file; `verify` over the stand-in record fails `not_applied`.
///
/// Catches: the file ignored for standard input, or a missing file taken as no parameters.
#[test]
fn the_parameters_come_from_the_file_params_names() {
    let scratch = scratch("mh-params-file");
    let file = scratch.path().join("params.json");
    fs::write(&file, json("run-1", &["MAINTENANCE_KIND_LINUX"])).expect("write");
    let registry = Registry::new().with(Kind::Linux, Box::new(Applies));
    assert_eq!(
        main_on_file(&registry, HookStep::Prepare, &file),
        line(Outcome::Success, None, None)
    );
    assert_eq!(
        main_on_file(&registry, HookStep::Verify, &file),
        line(
            Outcome::Failure,
            Some("not_applied"),
            Some("linux is not recorded applied")
        )
    );
    let missing = scratch.path().join("missing.json");
    let expected = format!(
        "the parameters could not be read: {}: No such file or directory (os error 2)",
        missing.display()
    );
    assert_eq!(
        main_on_file(&registry, HookStep::Prepare, &missing),
        line(Outcome::Failure, Some("params_invalid"), Some(&expected))
    );
}

/// Proves: `hostkeeper maintenance-hook <step> --params <file>` parses into the step and the
/// file for each of the three steps; another step word, or no `--params`, is clap's usage error,
/// exit 2.
///
/// Catches: the verb missing from the command line, a step of an update (`rollback`) taken, or a
/// run with no parameters.
#[test]
fn the_command_line_takes_the_three_steps_and_the_params_file() {
    for (word, step) in [
        ("prepare", HookStep::Prepare),
        ("cutover", HookStep::Cutover),
        ("verify", HookStep::Verify),
    ] {
        let cli = Cli::try_parse_from(["hostkeeper", "maintenance-hook", word, "--params", "-"])
            .expect("parses");
        let Command::MaintenanceHook(args) = cli.command else {
            panic!("not the maintenance hook: {:?}", cli.command);
        };
        assert_eq!(
            args,
            HookArgs {
                step,
                params: PathBuf::from("-"),
                package_lock_wait_secs: DEFAULT_PACKAGE_LOCK_WAIT_SECS,
            }
        );
    }
    let refused = Cli::try_parse_from([
        "hostkeeper",
        "maintenance-hook",
        "rollback",
        "--params",
        "-",
    ])
    .expect_err("no such step");
    assert_eq!(refused.kind(), ErrorKind::InvalidValue);
    assert_eq!(refused.exit_code(), 2);
    let refused =
        Cli::try_parse_from(["hostkeeper", "maintenance-hook", "prepare"]).expect_err("no params");
    assert_eq!(refused.kind(), ErrorKind::MissingRequiredArgument);
    assert_eq!(refused.exit_code(), 2);
}

/// Proves: the binary's command line runs the hook on this host's kinds (the `prepare` of `linux`
/// is not written yet) and exits with the step's code (1, `unsupported_kind`); started from a packaged path, the hook is run
/// there and not handed to `current` as the daemon is.
///
/// Catches: `maintenance-hook` dispatched to the daemon, or exec'd into another binary.
#[test]
fn the_binary_runs_the_hook_and_exits_with_its_code() {
    let scratch = scratch("mh-cli");
    let file = scratch.path().join("params.json");
    fs::write(&file, json("run-1", &["MAINTENANCE_KIND_LINUX"])).expect("write");
    let words = [
        OsString::from("hostkeeper"),
        "maintenance-hook".into(),
        "prepare".into(),
        "--params".into(),
        file.clone().into_os_string(),
    ];
    let failed = format!("{:?}", ExitCode::from(1));
    let code = cli::run(Cli::parse_from(&words));
    assert_eq!(format!("{code:?}"), failed);

    let image = scratch.path().join("hostkeeper");
    let execs = Mutex::new(Vec::new());
    let exec = |path: &Path, _args: &[OsString]| {
        execs.lock().expect("execs").push(path.to_owned());
        io::Error::other("no exec in a test")
    };
    let code = cli::run_from(
        words.to_vec(),
        &image,
        &[image.as_path()],
        SystemdVars::default(),
        &exec,
    );
    assert_eq!(format!("{code:?}"), failed);
    assert_eq!(execs.lock().expect("execs").len(), 0);
}

/// Proves: `--package-lock-wait-secs` of the hook parses as given from 1 to 3600 seconds, is the
/// daemon's default when absent, and 0, 3601 or a word is a usage error (exit 2, as clap reports
/// it).
///
/// Catches: a wait of none (every lock an unattended upgrade holds would fail the install at
/// once), or a wait past the longest the daemon's flag allows.
#[test]
fn the_hooks_package_lock_wait_flag_is_taken_within_its_range() {
    let parse = |extra: &[&str]| {
        let mut words = vec!["hostkeeper", "maintenance-hook", "cutover", "--params", "-"];
        words.extend(extra);
        Cli::try_parse_from(words)
    };
    let Ok(Cli {
        command: Command::MaintenanceHook(args),
    }) = parse(&[])
    else {
        panic!("the hook parses with no wait");
    };
    assert_eq!(args.package_lock_wait_secs, DEFAULT_PACKAGE_LOCK_WAIT_SECS);
    for secs in [1, 42, cli::MAX_PACKAGE_LOCK_WAIT_SECS] {
        let given = secs.to_string();
        let parsed = parse(&["--package-lock-wait-secs", &given]);
        let Ok(Cli {
            command: Command::MaintenanceHook(args),
        }) = parsed
        else {
            panic!("{secs} parses: {parsed:?}");
        };
        assert_eq!(args.package_lock_wait_secs, secs);
    }
    let over = (cli::MAX_PACKAGE_LOCK_WAIT_SECS + 1).to_string();
    for refused in ["0", over.as_str(), "five"] {
        let error = parse(&["--package-lock-wait-secs", refused]).expect_err(refused);
        assert_eq!(error.exit_code(), 2, "{refused}");
    }
}

/// Proves: the binary's `cutover` on this host is refused before any kind runs while the host
/// keeps no run record: exit 1, and the report line is `run_record_failed` saying so; `prepare`
/// and `verify` are not refused this way. `unwired_record` names the cutover alone.
///
/// Catches: a cutover that installs updates on a host that cannot record that it did.
#[test]
fn a_cutover_is_refused_while_the_host_keeps_no_run_record() {
    let ending = unwired_record(HookStep::Cutover).expect("a cutover is refused");
    assert_eq!(ending.outcome, Outcome::Failure);
    assert_eq!(ending.reason, Some("run_record_failed"));
    assert_eq!(
        ending.detail.as_deref(),
        Some("this host keeps no run record yet")
    );
    assert!(unwired_record(HookStep::Prepare).is_none());
    assert!(unwired_record(HookStep::Verify).is_none());

    let scratch = scratch("mh-unwired");
    let file = scratch.path().join("params.json");
    fs::write(&file, json("run-1", &["MAINTENANCE_KIND_REBOOT"])).expect("write");
    let words = [
        OsString::from("hostkeeper"),
        "maintenance-hook".into(),
        "cutover".into(),
        "--params".into(),
        file.into_os_string(),
    ];
    let code = cli::run(Cli::parse_from(&words));
    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(1)));
}

/// Proves: a Linux host's registry holds the `linux` and `reboot` kinds and a Mac's its three:
/// a Linux `prepare` and `verify` still answer `unsupported_kind`, as does a Mac's `macos`
/// `prepare`; a Mac's `xcode` stages (an order with no Xcode stages nothing and succeeds) and its
/// `cutover`, `after_reboot` and `verify` answer `unsupported_kind`.
///
/// Catches: a platform's kinds put in the wrong platform's registry.
#[test]
fn the_registry_of_a_platform_holds_its_kinds() {
    let kinds = |platform: &Platform| registry_for(platform, DEFAULT_PACKAGE_LOCK_WAIT_SECS);
    let linux = kinds(&Platform::linux());
    assert_eq!(
        linux.kinds().collect::<Vec<_>>(),
        [Kind::Linux, Kind::Reboot]
    );
    let openrc = kinds(&Platform::openrc());
    assert_eq!(
        openrc.kinds().collect::<Vec<_>>(),
        [Kind::Linux, Kind::Reboot]
    );
    let params = MaintainParams::new(Maintain {
        run_id: "run-1".to_owned(),
        kinds: vec![WireKind::Linux.into()],
        ..Maintain::default()
    })
    .expect("valid");
    let refused = linux.get(Kind::Linux).expect("held").prepare(&params);
    assert_eq!(refused, Err(KindError::unsupported(Kind::Linux)));
    let refused = linux.get(Kind::Linux).expect("held").verify(&params);
    assert_eq!(refused, Err(KindError::unsupported(Kind::Linux)));
    let reboot = linux.get(Kind::Reboot).expect("held");
    assert_eq!(
        reboot.prepare(&params),
        Err(KindError::unsupported(Kind::Reboot))
    );
    assert_eq!(
        reboot.verify(&params),
        Err(KindError::unsupported(Kind::Reboot))
    );
    let mac = kinds(&Platform::macos());
    assert_eq!(
        mac.kinds().collect::<Vec<_>>(),
        [Kind::Macos, Kind::Xcode, Kind::Reboot]
    );
    let refused = mac.get(Kind::Macos).expect("held").prepare(&params);
    assert_eq!(refused, Err(KindError::unsupported(Kind::Macos)));
    let xcode = mac.get(Kind::Xcode).expect("held");
    assert_eq!(xcode.prepare(&params), Ok(()));
    let unsupported = KindError::unsupported(Kind::Xcode);
    assert_eq!(xcode.cutover(&params), Err(unsupported.clone()));
    assert_eq!(xcode.after_reboot(&params), Err(unsupported.clone()));
    assert_eq!(xcode.verify(&params), Err(unsupported));
}
