//! The `macos` kind from the hook's entry to its report: `hostkeeper maintenance-hook <step>
//! --params <file>` parsed by the binary's own command line, the parameters the proto3 JSON the
//! daemon writes into the run's directory, and the hook's entry ([`main_with`]) running the real
//! [`MacosKind`] over the fake Mac of `apply_tests.rs`, its record, its rebooter and a boot id the
//! test moves. Each step's standard output, the runs, the install's arguments and input, and the
//! files are asserted by equality.

use clap::Parser;

use super::*;
use crate::cli::{Cli, Command};
use crate::maintenance_hook::{HookArgs, main_with};

/// The boot after the Mac restarted.
const BOOT_B: &str = "7c1e2d3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f";

/// The parameters of a `macos` run on the path `install` with a major upgrade allowed, as the
/// daemon writes them.
const PARAMS_JSON: &str = r#"{"runId":"run-1","action":"MAINTAIN_ACTION_RUN","kinds":["MAINTENANCE_KIND_MACOS"],"macosPath":"MACOS_PATH_INSTALL","macosMajor":true}"#;

/// `sw_vers` once the upgrade is applied: macOS 27.1, build 26B70.
const SW_VERS_27_1: &str = "ProductName:\tmacOS\nProductVersion:\t27.1\nBuildVersion:\t26B70\n";

/// A boot id the test moves.
struct Boot(Mutex<&'static str>);

impl BootIdSource for Boot {
    fn read(&self) -> Result<BootId, BootIdError> {
        BootId::uuid("the test's boot id", &self.0.lock().expect("boot"))
    }
}

/// The arguments of `hostkeeper maintenance-hook <step> --params <file>`, parsed by the binary's
/// own command line.
fn parsed(step: &str, file: &Path) -> HookArgs {
    let words: [std::ffi::OsString; 5] = [
        "hostkeeper".into(),
        "maintenance-hook".into(),
        step.into(),
        "--params".into(),
        file.as_os_str().to_owned(),
    ];
    let cli = Cli::try_parse_from(words).expect("parses");
    let Command::MaintenanceHook(args) = cli.command else {
        panic!("not the maintenance hook: {:?}", cli.command);
    };
    args
}

/// The hook's entry with `args` over `mac`'s `macos` kind (its plan in `files`' run directory),
/// record and rebooter and `boot`: the outcome and the standard output.
fn run(mac: &Mac, files: &Files, boot: &Boot, args: &HookArgs) -> (Outcome, String) {
    let kinds = mac.kinds(files);
    let ports = HookPorts {
        kinds: &kinds,
        record: mac,
        rebooter: mac,
        boot_id: boot,
    };
    let mut stdout = Vec::new();
    let outcome = main_with(args, &ports, &mut std::io::empty(), &mut stdout);
    (outcome, String::from_utf8(stdout).expect("UTF-8"))
}

/// The report line `ending` writes.
fn line(ending: &Ending) -> String {
    format!("{}\n", ending.report().to_line())
}

/// Proves, from the command line to the report, for a major upgrade: `maintenance-hook cutover
/// --params <file>` reads the parameters from the run's directory, lists, records the reboot
/// under way and only then runs the installer app's `startosinstall` with exactly its arguments
/// and the password on its standard input, and reports `reboot_under_way`; after the restart (a
/// new boot id) `cutover` reads the new build and records the kind applied, running nothing else;
/// `verify` then reports `macos_build_not_target` and keeps the installer app while `sw_vers`
/// reports the old build, and once it reports the target build reports done and removes the app.
///
/// Catches: the parameters' `macosPath` or `macosMajor` lost between the file and the kind, the
/// plan kept somewhere the next process does not read, the install run before the record, the
/// password anywhere but its standard input, or the installer removed before the check passes.
#[test]
fn maintenance_hook_cutover_and_verify_install_a_major_upgrade_from_the_params_file() {
    let files = Files::new("macos-entry-major");
    let app = files.installer(APP_27, true);
    let params = files.run_dir.join("maintain-params.json");
    fs::write(&params, PARAMS_JSON).expect("the parameters");
    let mac = Mac::on_26(LIST_MAJOR);
    let boot = Boot(Mutex::new(BOOT_A));
    let cutover = parsed("cutover", &params);
    let verify = parsed("verify", &params);
    let done = Ending {
        outcome: Outcome::Success,
        reason: None,
        detail: None,
    };

    assert_eq!(
        run(&mac, &files, &boot, &cutover),
        (Outcome::Success, line(&restarting()))
    );
    let program = app.join("Contents/Resources/startosinstall");
    assert_eq!(
        mac.effects(),
        [
            "run /usr/bin/fdesetup status".to_owned(),
            "run /usr/bin/sw_vers".to_owned(),
            "run /usr/sbin/softwareupdate --list".to_owned(),
            "run /usr/sbin/softwareupdate --list-full-installers".to_owned(),
            format!("record {RUN} macos reboot_under_way {BOOT_A}"),
            format!("install {}", program.display()),
        ]
    );
    assert_eq!(
        mac.installs(),
        [Installed {
            program,
            deadline: INSTALL_DEADLINE,
            step: STEP_UPGRADE.to_owned(),
            args: strings(&[
                "--agreetolicense",
                "--nointeraction",
                "--user",
                ACCOUNT,
                "--stdinpass",
            ]),
            input: input(),
        }]
    );
    assert_eq!(
        files.plan(),
        format!(r#"{{"build":"26B70","installer":"{INSTALLER}","runId":"{RUN}"}}"#)
    );

    *boot.0.lock().expect("boot") = BOOT_B;
    mac.look(SW_VERS, &[], ok(SW_VERS_27_1));
    assert_eq!(
        run(&mac, &files, &boot, &cutover),
        (Outcome::Success, line(&done))
    );
    assert_eq!(
        mac.effects(),
        [
            "run /usr/bin/sw_vers".to_owned(),
            format!("record {RUN} macos applied 26B70"),
        ]
    );

    mac.look(SW_VERS, &[], ok(SW_VERS_26_6));
    let not_target = failed_ending(
        REASON_NOT_TARGET,
        "sw_vers reports the build 25G62, not the target build 26B70",
    );
    assert_eq!(
        run(&mac, &files, &boot, &verify),
        (Outcome::Failure, line(&not_target))
    );
    assert!(app.is_dir());
    mac.look(SW_VERS, &[], ok(SW_VERS_27_1));
    assert_eq!(
        run(&mac, &files, &boot, &verify),
        (Outcome::Success, line(&done))
    );
    assert!(!app.exists());
    assert_eq!(mac.installs().len(), 1);
    assert_eq!(mac.lines(), ["startosinstall: exited with code 0"]);
}
