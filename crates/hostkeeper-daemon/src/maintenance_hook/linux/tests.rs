//! The tests of the `linux` kind's `prepare`: it is the stage, end to end through the hook's step
//! and from the hook's command line to its report (a short disk is a deferral, exit 75); a kind
//! given no stage does not prepare.

use clap::Parser;
use hostkeeper_hook::{Class, Outcome, Report};
use hostkeeper_proto::hostkeeper::agent::v1::LinuxScope as WireScope;
use hostkeeper_steps::ProcBootId;

use crate::cli::{Cli, Command};
use crate::maintenance_hook::linux::stage::REASON_BUSY;
use crate::maintenance_hook::linux::stage::REASON_SPACE_SHORT;
use crate::maintenance_hook::linux::stage::Stage;
use crate::maintenance_hook::linux::testing::{FakeHost, args, host_with, params, ran};
use crate::maintenance_hook::{
    Ending, HookArgs, HookPorts, HookStep, Kind, KindError, MaintenanceKind, Registry, Unwired,
    main_with, run_step,
};
use crate::maintenance_linux::kinds::{LinuxKind, RebootCheck};

const APT: &str = "/usr/bin/apt-get";

/// A fake host whose apt-get has one security update of 10 bytes to download.
fn apt_host() -> FakeHost {
    let fake = FakeHost::new();
    fake.always(&args(&["update", "-qq"]), ran(0, "", ""));
    fake.always(
        &args(&["--simulate", "-qq", "dist-upgrade"]),
        ran(
            0,
            "Inst openssl [3.0.13-0ubuntu3.2] (3.0.13-0ubuntu3.4 Ubuntu:24.04/noble-security [amd64])\n",
            "",
        ),
    );
    fake.always(
        &args(&[
            "--print-uris",
            "-qq",
            "install",
            "--only-upgrade",
            "openssl",
        ]),
        ran(
            0,
            "'http://archive.example.com/openssl.deb' openssl.deb 10 SHA512:1\n",
            "",
        ),
    );
    fake.always(
        &args(&[
            "--download-only",
            "-y",
            "-qq",
            "-o",
            "Dpkg::Options::=--force-confold",
            "install",
            "--only-upgrade",
            "openssl",
        ]),
        ran(0, "", ""),
    );
    fake
}

/// The `linux` kind staging with `stage`, its own packages `fake`'s; its install and reboot check
/// are never run by `prepare`.
fn staged(fake: &FakeHost, stage: Stage) -> LinuxKind {
    LinuxKind::new(None, Box::new(fake.clone()), RebootCheck::at_host()).with_stage(stage)
}

/// Runs the hook's `prepare` of a `linux` run over a registry whose `linux` kind is `kind`, the
/// boot id read (never, by `prepare`) from `boot_file`.
fn prepare(kind: LinuxKind, boot_file: &std::path::Path) -> Ending {
    let registry = Registry::new().with(Kind::Linux, Box::new(kind));
    let boot = ProcBootId::at(boot_file);
    let ports = HookPorts {
        kinds: &registry,
        record: &Unwired,
        rebooter: &Unwired,
        boot_id: &boot,
    };
    run_step(HookStep::Prepare, &params(WireScope::Security), &ports)
}

/// Proves: the hook's `prepare` of a `linux` run is the stage: with room it downloads and is
/// done; one byte short of twice the download it is deferred (exit 75), reason
/// `free_space_short`, with no download run.
///
/// Catches: the `linux` kind's `prepare` left a stub, or a short disk that fails the step and so
/// halts a host that was never touched.
#[test]
fn prepare_of_the_linux_kind_is_its_stage() {
    let root = host_with("linux-kind-prepare", APT);
    let fake = apt_host();
    fake.free(Ok(20));
    let boot_file = root.path().join("boot_id");
    let ending = prepare(staged(&fake, fake.stage(&root, 300)), &boot_file);
    assert_eq!(ending.outcome, Outcome::Success);
    assert_eq!(fake.arg_lists().len(), 4);

    let fake = apt_host();
    fake.free(Ok(19));
    let ending = prepare(staged(&fake, fake.stage(&root, 300)), &boot_file);
    assert_eq!(
        ending,
        Ending {
            outcome: Outcome::Deferred,
            reason: Some(REASON_SPACE_SHORT),
            detail: Some(
                "/var/cache/apt/archives has 19 bytes free; the download of 10 bytes needs 20"
                    .to_owned()
            ),
        }
    );
    assert_eq!(fake.arg_lists().len(), 3);
}

/// Proves: a `linux` kind given no stage answers `unsupported_kind` to `prepare` and runs
/// nothing, so a kind built without one is never a stage that does nothing.
///
/// Catches: a `prepare` that succeeds without staging.
#[test]
fn a_linux_kind_with_no_stage_does_not_prepare() {
    let fake = apt_host();
    let kind = LinuxKind::new(None, Box::new(fake.clone()), RebootCheck::at_host());
    assert_eq!(
        kind.prepare(&params(WireScope::Security)),
        Err(KindError::unsupported(Kind::Linux))
    );
    assert_eq!(fake.calls(), Vec::new());
}

/// The download apt is asked for by [`apt_host`]'s stage.
fn download() -> Vec<String> {
    args(&[
        "--download-only",
        "-y",
        "-qq",
        "-o",
        "Dpkg::Options::=--force-confold",
        "install",
        "--only-upgrade",
        "openssl",
    ])
}

/// The arguments of `hostkeeper maintenance-hook prepare --params <file> --package-lock-wait-secs
/// <secs>`, parsed by the binary's own command line.
fn parsed(file: &std::path::Path, secs: &str) -> HookArgs {
    let words = [
        "hostkeeper".into(),
        "maintenance-hook".into(),
        "prepare".into(),
        "--params".into(),
        file.as_os_str().to_owned(),
        "--package-lock-wait-secs".into(),
        secs.into(),
    ];
    let cli = Cli::try_parse_from::<_, std::ffi::OsString>(words).expect("parses");
    let Command::MaintenanceHook(args) = cli.command else {
        panic!("not the maintenance hook: {:?}", cli.command);
    };
    args
}

/// Runs the hook's entry ([`main_with`]) with `args` over a registry whose `linux` kind is the
/// stage of `fake`, its lock waited for the seconds `args` names: the outcome and the standard
/// output.
fn run_hook(
    fake: &FakeHost,
    root: &hostkeeper_testkit::Scratch,
    args: &HookArgs,
) -> (Outcome, String) {
    let kind = staged(fake, fake.stage(root, args.package_lock_wait_secs));
    let registry = Registry::new().with(Kind::Linux, Box::new(kind));
    let boot = ProcBootId::at(&root.path().join("boot_id"));
    let ports = HookPorts {
        kinds: &registry,
        record: &Unwired,
        rebooter: &Unwired,
        boot_id: &boot,
    };
    let mut stdout = Vec::new();
    let outcome = main_with(args, &ports, &mut std::io::empty(), &mut stdout);
    (outcome, String::from_utf8(stdout).expect("utf-8"))
}

/// The report line of an ending of `class`, `reason` and `detail`.
fn line(class: Class, reason: Option<&str>, detail: Option<&str>) -> String {
    let report = Report {
        class,
        reason: reason.map(str::to_owned),
        detail: detail.map(str::to_owned),
        ..Report::default()
    };
    format!("{}\n", report.to_line())
}

/// Proves, from the command line to the report: `maintenance-hook prepare --params <file>
/// --package-lock-wait-secs 3` of a `linux` run, parsed by the binary's own command line and
/// run by the hook's entry over a fake host's programs and disk,
/// - with room, runs apt's download with exactly its arguments, exits done and reports nothing
///   else;
/// - one byte short of twice the download, runs no download and exits deferred (75) with the
///   report of class `environment`, reason `free_space_short` and its detail;
/// - with the lock held throughout, waits 2 then 1 seconds (the 3 the flag gave) and exits
///   deferred, reason `package_manager_busy`.
///
/// Catches: a prepare whose download is not run from the hook's entry, a deferral reported as a
/// failure (which would halt a host that was never touched), or a flag that does not reach the
/// stage's wait.
#[test]
fn the_hook_command_line_runs_the_linux_stage_to_its_report() {
    let root = host_with("linux-kind-hook", APT);
    let file = root.path().join("params.json");
    std::fs::write(
        &file,
        r#"{"runId":"run-1","kinds":["MAINTENANCE_KIND_LINUX"],"linuxScope":"LINUX_SCOPE_SECURITY"}"#,
    )
    .expect("the params");
    let hook = parsed(&file, "3");
    assert_eq!(hook.step, HookStep::Prepare);
    assert_eq!(hook.package_lock_wait_secs, 3);

    let fake = apt_host();
    fake.free(Ok(20));
    let (outcome, stdout) = run_hook(&fake, &root, &hook);
    assert_eq!(outcome, Outcome::Success);
    assert_eq!(stdout, line(Class::Release, None, None));
    assert_eq!(fake.arg_lists().last(), Some(&download()));

    let short = apt_host();
    short.free(Ok(19));
    let (outcome, stdout) = run_hook(&short, &root, &hook);
    assert_eq!(outcome, Outcome::Deferred);
    assert_eq!(
        stdout,
        line(
            Class::Environment,
            Some(REASON_SPACE_SHORT),
            Some("/var/cache/apt/archives has 19 bytes free; the download of 10 bytes needs 20")
        )
    );
    assert!(!short.arg_lists().contains(&download()));

    let locked = apt_host();
    locked.always(
        &args(&["update", "-qq"]),
        ran(
            100,
            "",
            "E: Could not get lock /var/lib/apt/lists/lock. It is held by process 7 (apt)",
        ),
    );
    let (outcome, stdout) = run_hook(&locked, &root, &hook);
    assert_eq!(outcome, Outcome::Deferred);
    assert_eq!(
        stdout,
        line(
            Class::Environment,
            Some(REASON_BUSY),
            Some("the package manager's lock was held for 3s of 3s (apt-get update)")
        )
    );
    assert_eq!(locked.waits(), vec![2, 1]);
}
