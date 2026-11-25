//! A Mac maintenance turn end to end (docs/design/maintenance-updates.md, sections 2.4, 5.5, 5.6,
//! 5.7 and 8): the real coordinator schedules the run and orders each Mac's turn from its
//! heartbeats, and on each Mac the real daemon runs the turn from its run record (stage, drain,
//! the install that restarts the Mac, the check in the new boot, put back) through the real
//! maintenance hook's `macos` and `xcode` kinds over a fake Mac (`support_e2e`, over the fake Mac
//! of `support_mac`), across the Mac's restart, a daemon that stops in the middle of a turn and a
//! coordinator that restarts. Each test ends with exact equality on what the fake Mac went
//! through: the programs the hook ran, in order, with their arguments, what each install program
//! got on its standard input, the lines the hook logged, the run's steps and the alerts.

mod support;
mod support_e2e;
mod support_mac;
mod support_maintain;

use std::sync::Arc;

use hostkeeper_daemon::maintenance_run::Driven;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::operator::v1 as op;
use hostkeeper_steps::maintenance::{Ending, Kind};
use op::MaintenanceHostStep as Step;
use op::MaintenanceSkip as Skip;
use support_mac::{
    ACCOUNT, Account, CODESIGN, FDESETUP, FULL_DISK_FREE, INSTALLER_APP, INSTALLER_NAME, Input,
    MAJOR_BUILD, MAJOR_VERSION, MINOR_BUILD, MINOR_BYTES, MINOR_LABEL, MINOR_VERSION, Mac,
    NEW_XCODE, NEW_XCODE_ARCHIVE, NEW_XCODE_BUILD, Node, OLD_BUILD, OLD_VERSION, OLD_XCODE,
    Ordered, PASSWORD, PKGUTIL_PROGRAM, SERVICE, SOFTWAREUPDATE, SW_VERS, World, XCODE_SELECT, XIP,
    after_drained, args, first_offer, one_at_a_time, password_only_on_stdin, restart_plist, turns,
    xcodebuild,
};

/// The group of every test.
const GROUP: &str = "macs";

/// The seconds a Mac may stay silent in its restart before it is not back: the coordinator's
/// default for a Mac.
const MAC_RETURN_SECS: u64 = 3600;

/// The `updates_due` notice of [`GROUP`], alone.
fn updates_due() -> Vec<(op::AlertKind, String, String)> {
    vec![(op::AlertKind::UpdatesDue, GROUP.to_owned(), String::new())]
}

/// [`updates_due`] and the alert `kind` of `host`.
fn and_alert(kind: op::AlertKind, host: &str) -> Vec<(op::AlertKind, String, String)> {
    let mut alerts = updates_due();
    alerts.push((kind, GROUP.to_owned(), host.to_owned()));
    alerts
}

/// No maintenance alert.
fn no_alerts() -> Vec<(op::AlertKind, String, String)> {
    Vec::new()
}

/// `host` and `step`, as [`World::steps`] lists them.
fn at(host: &str, step: Step) -> (String, Step) {
    (host.to_owned(), step)
}

/// `program` run with `words`, as an effect line.
fn ran(program: &str, words: &[&str]) -> String {
    let mut line = vec![program];
    line.extend_from_slice(words);
    line.join(" ")
}

/// `program` run with the owned `words`, as an effect line.
fn ran_with(program: &str, words: &[String]) -> String {
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    ran(program, &words)
}

/// The two Macs of [`GROUP`] for the test `test`: `h1`, set up as `mac` says, and `h2`, with
/// nothing to install, which keeps serving while `h1` takes its turn; each heard with its report.
fn pair(test: &str, mac: Mac) -> (Arc<World>, Node, Node) {
    let world = World::new(test);
    world.group(GROUP, &["h1", "h2"], one_at_a_time());
    let mut h1 = Node::mac(&world, test, "h1", GROUP, mac);
    let mut h2 = Node::mac(&world, test, "h2", GROUP, Mac::up_to_date());
    h1.report();
    h2.report();
    (world, h1, h2)
}

/// The Mac's turn, which waits for the restart its install asked for: it reports that it
/// restarts, restarts, its new daemon's start ends the turn done, and three heartbeats report
/// that end and then what the next look found. No answer orders anything.
fn restart_to_done(node: &mut Node) {
    assert!(
        node.machine.restart_asked(),
        "the install asked for a restart"
    );
    assert_eq!(node.beat(), None);
    assert_eq!(node.reboot(), Some(Driven::Ended(Ending::Done)));
    for _ in 0..3 {
        assert_eq!(node.beat(), None);
    }
}

/// What the stage of the minor update runs: what the Mac runs, what Apple offers it, and the
/// download of the update.
fn minor_stage() -> Vec<String> {
    vec![
        "prepare".to_owned(),
        ran(SW_VERS, &[]),
        ran(SOFTWAREUPDATE, &["--list"]),
        ran(SOFTWAREUPDATE, &["--download", MINOR_LABEL]),
    ]
}

/// What the `cutover` of `macos` runs on a Mac with FileVault off before its install: the
/// update account's file is read, FileVault, what the Mac runs and what Apple offers it.
fn minor_cutover() -> Vec<String> {
    vec![
        "cutover".to_owned(),
        ran(FDESETUP, &["status"]),
        ran(SW_VERS, &[]),
        ran(SOFTWAREUPDATE, &["--list"]),
    ]
}

/// The arguments of `softwareupdate --install` of the minor update, with `--restart` when
/// `restart`.
fn install_args(restart: bool) -> Vec<String> {
    let mut words = vec!["--install", MINOR_LABEL, "--agree-to-license"];
    if restart {
        words.push("--restart");
    }
    words.extend(["--user", ACCOUNT, "--stdinpass"]);
    args(&words)
}

/// What the Mac goes through in a turn of the minor update with FileVault off, in order.
fn minor_turn() -> Vec<String> {
    let mut effects = minor_stage();
    effects.push(format!("drain {SERVICE}"));
    effects.extend(minor_cutover());
    effects.extend([
        ran_with(SOFTWAREUPDATE, &install_args(true)),
        "boot boot-2".to_owned(),
        "verify".to_owned(),
        ran(SW_VERS, &[]),
        format!("resume {SERVICE}"),
    ]);
    effects
}

/// The install of the minor update as `softwareupdate` got it, with `--restart` when `restart`:
/// the password and a newline on its standard input, nothing else.
fn minor_install(restart: bool) -> Input {
    Input {
        program: SOFTWAREUPDATE.to_owned(),
        args: install_args(restart),
        stdin: format!("{PASSWORD}\n"),
    }
}

/// Proves: the Update button on a Mac with FileVault off and a minor macOS update due runs the
/// whole turn from one order: the stage downloads the update while the Mac serves, the drain,
/// the install by `softwareupdate --install ... --restart` with the update account's password on
/// its standard input alone, the restart into the new build, the check of that build, the
/// resume, done; the next report shows nothing due and the group's `updates_due` notice clears.
///
/// Catches: an install that passes the password in an argument, or not on standard input, or
/// that logs it; an install without `--restart` on a Mac with FileVault off; a turn that checks
/// before the restart or resumes before the check; an install repeated after the restart; an
/// order given again; a report that still lists the update, or a notice that stands, after the
/// turn is done.
#[test]
fn the_update_button_installs_a_minor_update_and_restarts_the_mac_with_filevault_off() {
    let (world, mut h1, h2) = pair("mac-e2e-minor", Mac::default());
    assert_eq!(h1.due(), [pb::MaintenanceKind::Macos]);
    assert_eq!(world.alerts(), updates_due());

    let run = world.update(&["h1"]);
    let order = first_offer(&run, &[pb::MaintenanceKind::Macos]);
    assert_eq!(h1.beat(), Some(order.clone()));
    assert!(h1.machine.cordoned(), "the service is out of service");
    restart_to_done(&mut h1);

    assert_eq!(h1.machine.effects(), minor_turn());
    assert_eq!(h1.machine.inputs(), [minor_install(true)]);
    assert_eq!(
        h1.machine.logged(),
        ["softwareupdate --install: exited with code 0"]
    );
    password_only_on_stdin(&h1);
    assert_eq!(
        h1.machine.running(),
        (MINOR_VERSION.to_owned(), MINOR_BUILD.to_owned())
    );
    assert!(!h1.machine.cordoned(), "the service is back in service");
    assert_eq!(world.steps(&run), [at("h1", Step::Done)]);
    assert_eq!(h1.due(), Vec::<pb::MaintenanceKind>::new());
    assert_eq!(world.alerts(), no_alerts());
    assert_eq!(h1.orders(), [order]);
    assert_eq!(h2.machine.effects(), Vec::<String>::new());
}

/// Proves: a Mac whose FileVault was turned on after the report the coordinator ordered it on
/// (the report said off) is installed as a Mac with FileVault on: the `cutover` reads FileVault
/// again, finds it on, checks that `fdesetup` can restart it authenticated and that the update
/// account is a FileVault user, installs without `--restart`, then runs `fdesetup authrestart
/// -inputplist` with the account's name and password in a property list on its standard input
/// alone; the Mac comes back on the new build, the check passes, and its next report says
/// FileVault is on.
///
/// Catches: an install with `--restart` on a Mac with FileVault on, whose restart would stop at
/// the pre-boot login; an install that trusts the report's FileVault instead of reading it; an
/// authenticated restart with the password in an argument, or a property list that does not
/// escape it; a restart asked before the install ended.
#[test]
fn filevault_turned_on_after_the_report_installs_without_restart_then_restarts_authenticated() {
    let (world, mut h1, _h2) = pair("mac-e2e-filevault", Mac::default());
    h1.machine.turn_filevault_on();

    let run = world.update(&["h1"]);
    let order = first_offer(&run, &[pb::MaintenanceKind::Macos]);
    assert_eq!(h1.beat(), Some(order.clone()));
    restart_to_done(&mut h1);

    let mut effects = minor_stage();
    effects.extend([
        format!("drain {SERVICE}"),
        "cutover".to_owned(),
        ran(FDESETUP, &["status"]),
        ran(FDESETUP, &["supportsauthrestart"]),
        ran(FDESETUP, &["list"]),
        ran(SW_VERS, &[]),
        ran(SOFTWAREUPDATE, &["--list"]),
        ran_with(SOFTWAREUPDATE, &install_args(false)),
        ran(FDESETUP, &["authrestart", "-inputplist"]),
        "boot boot-2".to_owned(),
        "verify".to_owned(),
        ran(SW_VERS, &[]),
        format!("resume {SERVICE}"),
    ]);
    assert_eq!(h1.machine.effects(), effects);
    assert_eq!(
        h1.machine.inputs(),
        [
            minor_install(false),
            Input {
                program: FDESETUP.to_owned(),
                args: args(&["authrestart", "-inputplist"]),
                stdin: restart_plist(),
            },
        ]
    );
    assert_eq!(
        h1.machine.logged(),
        [
            "softwareupdate --install: exited with code 0",
            "fdesetup authrestart: exited with code 0",
        ]
    );
    password_only_on_stdin(&h1);
    assert_eq!(
        h1.machine.running(),
        (MINOR_VERSION.to_owned(), MINOR_BUILD.to_owned())
    );
    assert_eq!(world.steps(&run), [at("h1", Step::Done)]);
    assert_eq!(
        h1.last_report().mac.map(|mac| mac.filevault()),
        Some(pb::FileVaultState::On)
    );
    assert_eq!(h1.due(), Vec::<pb::MaintenanceKind>::new());
    assert_eq!(h1.orders(), [order]);
}

/// What [`update_beside_a_ready_mac`] saw: each turn of the run, what `h1` went through and the
/// orders it was given, the order `h2` was given, and the first offer of the run.
struct Beside {
    turns: Vec<(String, Step, Skip, String)>,
    effects: Vec<String>,
    orders: Vec<Ordered>,
    started: Option<Ordered>,
    offer: Ordered,
}

/// The run of the Update button on `h1`, set up as `mac` says, and `h2`, a ready Mac with the
/// minor update due, in a group of three whose `h3` has nothing due, after three heartbeats of
/// `h1` and one of `h2`.
fn update_beside_a_ready_mac(test: &str, mac: Mac) -> Beside {
    let world = World::new(test);
    world.group(GROUP, &["h1", "h2", "h3"], one_at_a_time());
    let mut h1 = Node::mac(&world, test, "h1", GROUP, mac);
    let mut h2 = Node::mac(&world, test, "h2", GROUP, Mac::default());
    let mut h3 = Node::mac(&world, test, "h3", GROUP, Mac::up_to_date());
    for node in [&mut h1, &mut h2, &mut h3] {
        node.report();
    }
    let run = world.update(&["h1", "h2"]);
    for _ in 0..3 {
        assert_eq!(h1.beat(), None);
    }
    let turns = turns(&world, &run);
    let started = h2.beat();
    Beside {
        turns,
        effects: h1.machine.effects(),
        orders: h1.orders(),
        started,
        offer: first_offer(&run, &[pb::MaintenanceKind::Macos]),
    }
}

/// Asserts that `h1`, set up as `mac`, was not ordered `macos` for the reason `why`, which the
/// run shows on its turn, skipped, while `h2` of the same run was ordered.
fn not_ordered(test: &str, mac: Mac, why: Skip) {
    let seen = update_beside_a_ready_mac(test, mac);
    assert_eq!(
        seen.turns,
        [
            ("h1".to_owned(), Step::Skipped, why, String::new()),
            (
                "h2".to_owned(),
                Step::Waiting,
                Skip::Unspecified,
                String::new()
            ),
        ]
    );
    assert_eq!(seen.effects, Vec::<String>::new(), "nothing ran on h1");
    assert_eq!(seen.orders, Vec::<Ordered>::new(), "h1 was given no order");
    assert_eq!(seen.started, Some(seen.offer), "the run goes on with h2");
}

/// Proves: a Mac with the minor update due and no update account's file is never ordered
/// `macos`: the run shows it skipped `no_update_account`, nothing runs on it, and the run goes on
/// with the next Mac.
///
/// Catches: a Mac ordered `macos` whose install has no account to authorise it, and halts.
#[test]
fn a_mac_with_no_update_account_is_not_ordered() {
    let mac = Mac {
        account: Account::Missing,
        ..Mac::default()
    };
    not_ordered("mac-e2e-no-account", mac, Skip::NoUpdateAccount);
}

/// Proves: a Mac whose update account holds no Secure Token (`sysadminctl -secureTokenStatus`
/// says disabled) is never ordered `macos`: skipped `no_update_account`.
///
/// Catches: a report that reads the account's file and not its Secure Token, or a coordinator
/// that orders a Mac whose account cannot authorise the update.
#[test]
fn a_mac_whose_update_account_holds_no_secure_token_is_not_ordered() {
    let mac = Mac {
        account: Account::NoSecureToken,
        ..Mac::default()
    };
    not_ordered("mac-e2e-no-token", mac, Skip::NoUpdateAccount);
}

/// Proves: a Mac on which device management sets a software-update setting that conflicts with
/// the daemon's install (`AutomaticallyInstallMacOSUpdates`) is never ordered `macos`: skipped
/// `mdm_conflict`.
///
/// Catches: a report that does not read device management's settings, or a coordinator that
/// orders a Mac whose updates device management also installs.
#[test]
fn a_mac_with_an_mdm_conflict_is_not_ordered() {
    let mac = Mac {
        mdm_conflict: true,
        ..Mac::default()
    };
    not_ordered("mac-e2e-mdm", mac, Skip::MdmConflict);
}

/// Proves: a Mac whose report says FileVault is on is not ordered `macos`: skipped
/// `filevault_not_ready`, as the coordinator's rule says today (the report carries no fact that
/// the update account is a FileVault user, so no Mac with FileVault on is ordered).
///
/// Catches: a coordinator that orders a Mac with FileVault on from a report that does not show it
/// can be restarted authenticated.
#[test]
fn a_mac_that_reports_filevault_on_is_not_ordered() {
    let mac = Mac {
        filevault_on: true,
        ..Mac::default()
    };
    not_ordered("mac-e2e-filevault-on", mac, Skip::FilevaultNotReady);
}

/// Proves: a stage on a data volume with less free space than twice the update defers the turn
/// before anything is downloaded: nothing is drained or installed, the Mac never leaves service,
/// the run shows it deferred with the stage's reason, and the update is still due.
///
/// Catches: a stage that downloads onto a full disk, or that halts the Mac or drains it for a
/// shortfall.
#[test]
fn a_disk_too_full_at_stage_defers_with_nothing_installed() {
    let mac = Mac {
        free: FULL_DISK_FREE,
        ..Mac::default()
    };
    let (world, mut h1, _h2) = pair("mac-e2e-full-disk", mac);

    let run = world.update(&["h1"]);
    let order = first_offer(&run, &[pb::MaintenanceKind::Macos]);
    assert_eq!(h1.beat(), Some(order.clone()));

    let why = format!(
        "stage: exit 75, free_space_short: /System/Volumes/Data has {FULL_DISK_FREE} bytes free, \
         less than twice the {MINOR_BYTES} bytes to download"
    );
    assert_eq!(
        h1.record().and_then(|record| record.deferred),
        Some(why.clone())
    );
    assert_eq!(h1.beat(), None);
    assert_eq!(
        turns(&world, &run),
        [("h1".to_owned(), Step::Deferred, Skip::Unspecified, why)]
    );
    assert_eq!(
        h1.machine.effects(),
        [
            "prepare".to_owned(),
            ran(SW_VERS, &[]),
            ran(SOFTWAREUPDATE, &["--list"]),
        ]
    );
    assert_eq!(h1.machine.inputs(), Vec::<Input>::new());
    assert!(!h1.machine.cordoned());
    assert!(!h1.machine.restart_asked());
    assert_eq!(h1.due(), [pb::MaintenanceKind::Macos]);
    assert_eq!(world.alerts(), updates_due());
    assert_eq!(h1.orders(), [order]);
}

/// Proves: in a group that allows a major upgrade, a Mac offered one is ordered `macos` with
/// `macos_major`, and its turn runs the upgrade alone: the stage fetches the full installer
/// (`softwareupdate --fetch-full-installer`), the install runs the installer app's
/// `startosinstall --agreetolicense --nointeraction --user <account> --stdinpass` with the
/// password on its standard input, the Mac restarts into the new major version, and the check
/// passes and then removes the installer app.
///
/// Catches: a stage that downloads the minor update beside the upgrade, or no installer; an
/// upgrade installed by `softwareupdate --install`; an installer app left behind, or removed
/// before the check.
#[test]
fn a_major_upgrade_fetches_the_full_installer_and_runs_startosinstall() {
    let test = "mac-e2e-major";
    let world = World::new(test);
    world.group(
        GROUP,
        &["h1", "h2"],
        op::MaintenancePolicy {
            macos_major: true,
            ..one_at_a_time()
        },
    );
    let mac = Mac {
        major: true,
        ..Mac::default()
    };
    let mut h1 = Node::mac(&world, test, "h1", GROUP, mac);
    let mut h2 = Node::mac(&world, test, "h2", GROUP, Mac::up_to_date());
    h1.report();
    h2.report();
    // The report lists the minor update and the major upgrade, both of kind `macos`.
    assert_eq!(
        h1.due(),
        [pb::MaintenanceKind::Macos, pb::MaintenanceKind::Macos]
    );

    let run = world.update(&["h1"]);
    let order = Ordered {
        macos_major: true,
        ..first_offer(&run, &[pb::MaintenanceKind::Macos])
    };
    assert_eq!(h1.beat(), Some(order.clone()));
    assert_eq!(h1.machine.applications(), [INSTALLER_NAME]);
    restart_to_done(&mut h1);

    let startosinstall = format!("{INSTALLER_APP}/Contents/Resources/startosinstall");
    let upgrade_args = args(&[
        "--agreetolicense",
        "--nointeraction",
        "--user",
        ACCOUNT,
        "--stdinpass",
    ]);
    assert_eq!(
        h1.machine.effects(),
        [
            "prepare".to_owned(),
            ran(SW_VERS, &[]),
            ran(SOFTWAREUPDATE, &["--list"]),
            ran(SOFTWAREUPDATE, &["--list-full-installers"]),
            ran(
                SOFTWAREUPDATE,
                &[
                    "--fetch-full-installer",
                    "--full-installer-version",
                    MAJOR_VERSION
                ]
            ),
            format!("drain {SERVICE}"),
            "cutover".to_owned(),
            ran(FDESETUP, &["status"]),
            ran(SW_VERS, &[]),
            ran(SOFTWAREUPDATE, &["--list"]),
            ran(SOFTWAREUPDATE, &["--list-full-installers"]),
            ran_with(&startosinstall, &upgrade_args),
            "boot boot-2".to_owned(),
            "verify".to_owned(),
            ran(SW_VERS, &[]),
            format!("resume {SERVICE}"),
        ]
    );
    assert_eq!(
        h1.machine.inputs(),
        [Input {
            program: startosinstall,
            args: upgrade_args,
            stdin: format!("{PASSWORD}\n"),
        }]
    );
    assert_eq!(h1.machine.logged(), ["startosinstall: exited with code 0"]);
    password_only_on_stdin(&h1);
    assert_eq!(h1.machine.applications(), Vec::<String>::new());
    assert_eq!(
        h1.machine.running(),
        (MAJOR_VERSION.to_owned(), MAJOR_BUILD.to_owned())
    );
    assert_eq!(world.steps(&run), [at("h1", Step::Done)]);
    assert_eq!(h1.due(), Vec::<pb::MaintenanceKind>::new());
    assert_eq!(world.alerts(), no_alerts());
    assert_eq!(h1.orders(), [order]);
}

/// Proves: a Mac with a macOS update due and, in a group that keeps one Xcode (`xcode_keep` 1,
/// the default `latest`), an older Xcode installed and the store's newer one not, is ordered
/// `macos` and `xcode` together with the store's Xcode (its size and SHA-256), the old one to
/// remove and the new one as the default. Its stage downloads the macOS update and fetches,
/// expands and checks the Xcode while the Mac serves; the turn installs macOS and restarts, and
/// only after the restart moves the new Xcode into place, accepts its licence, runs its first
/// launch, selects it and removes the old one; the check reads the macOS build and each fact of
/// the Xcode. The next report shows the new Xcode alone, selected, and nothing due.
///
/// Catches: an Xcode installed before the macOS restart (or a turn that restarts twice); an old
/// Xcode removed while the Mac still serves; an Xcode default not selected, or one not checked;
/// an order whose Xcode entry is not the store's.
#[test]
fn a_macos_and_xcode_turn_restarts_after_macos_and_installs_xcode_after_the_restart() {
    let test = "mac-e2e-xcode";
    let world = World::with_xcode_store(test);
    let entry = world.hold_xcode(NEW_XCODE, NEW_XCODE_BUILD, NEW_XCODE_ARCHIVE);
    world.group(
        GROUP,
        &["h1"],
        op::MaintenancePolicy {
            xcode_keep: 1,
            ..one_at_a_time()
        },
    );
    let mac = Mac {
        old_xcode: true,
        ..Mac::default()
    };
    let mut h1 = Node::mac(&world, test, "h1", GROUP, mac);
    h1.report();
    assert_eq!(h1.due(), [pb::MaintenanceKind::Macos]);
    assert_eq!(world.alerts(), updates_due());

    let run = world.schedule(op::ScheduleMaintenanceRequest {
        host_ids: vec!["h1".to_owned()],
        take_last_host: true,
        ..op::ScheduleMaintenanceRequest::default()
    });
    let order = Ordered {
        xcodes: vec![entry.clone()],
        xcode_remove: vec![OLD_XCODE.to_owned()],
        xcode_default: NEW_XCODE.to_owned(),
        ..first_offer(
            &run,
            &[pb::MaintenanceKind::Macos, pb::MaintenanceKind::Xcode],
        )
    };
    assert_eq!(h1.beat(), Some(order.clone()));
    let staging = "/Applications/.hostkeeper-staging";
    let old_app = format!("Xcode-{OLD_XCODE}.app");
    assert_eq!(
        h1.machine.applications(),
        [".hostkeeper-staging".to_owned(), old_app],
        "before the restart, the old Xcode stays and the new one is staged"
    );
    assert_eq!(
        h1.machine.listed(staging),
        std::slice::from_ref(&entry.sha256)
    );
    restart_to_done(&mut h1);

    let sha = &entry.sha256;
    let new_app = format!("/Applications/Xcode-{NEW_XCODE}.app");
    let mut effects = minor_stage();
    effects.extend([
        format!("GET /v1/xcode/{sha} from byte 0"),
        ran(
            PKGUTIL_PROGRAM,
            &["--check-signature", &format!("{staging}/{sha}/Xcode.xip")],
        ),
        ran(XIP, &["--expand", &format!("{staging}/{sha}/Xcode.xip")]),
        ran(
            CODESIGN,
            &[
                "--verify",
                "--deep",
                "--strict",
                &format!("{staging}/{sha}/expand/Xcode.app"),
            ],
        ),
        format!("drain {SERVICE}"),
    ]);
    effects.extend(minor_cutover());
    effects.extend([
        ran_with(SOFTWAREUPDATE, &install_args(true)),
        "boot boot-2".to_owned(),
        "cutover".to_owned(),
        ran(&xcodebuild(NEW_XCODE), &["-license", "accept"]),
        ran(&xcodebuild(NEW_XCODE), &["-runFirstLaunch"]),
        ran(XCODE_SELECT, &["-s", &new_app]),
        "verify".to_owned(),
        ran(SW_VERS, &[]),
        ran(&xcodebuild(NEW_XCODE), &["-version"]),
        ran(&xcodebuild(NEW_XCODE), &["-license", "check"]),
        ran(XCODE_SELECT, &["-p"]),
        format!("resume {SERVICE}"),
    ]);
    assert_eq!(h1.machine.effects(), effects);
    assert_eq!(h1.machine.inputs(), [minor_install(true)]);
    assert_eq!(
        h1.machine.applications(),
        [
            ".hostkeeper-staging".to_owned(),
            format!("Xcode-{NEW_XCODE}.app")
        ]
    );
    assert_eq!(h1.machine.listed(staging), Vec::<String>::new());
    assert_eq!(
        h1.machine.selected(),
        Some(format!("{new_app}/Contents/Developer"))
    );
    assert_eq!(
        h1.last_report().xcodes,
        [pb::InstalledXcode {
            version: NEW_XCODE.to_owned(),
            build: NEW_XCODE_BUILD.to_owned(),
            path: new_app,
            selected: true,
            foreign: false,
        }]
    );
    assert_eq!(world.steps(&run), [at("h1", Step::Done)]);
    assert_eq!(h1.due(), Vec::<pb::MaintenanceKind>::new());
    assert_eq!(world.alerts(), no_alerts());
    assert_eq!(h1.orders(), [order]);
}

/// Proves: a store archive of an Xcode that `xip` cannot expand defers the Mac's turn at the stage,
/// while the Mac serves: the macOS update was downloaded and the Xcode fetched, and nothing is
/// drained, installed or removed; the old Xcode stays selected and the staging left no archive.
///
/// Catches: a stage that drains the Mac or installs macOS when the Xcode of the same turn cannot
/// be staged; an Xcode stage that leaves a broken archive behind.
#[test]
fn an_xcode_that_does_not_expand_defers_the_turn_at_stage_with_nothing_installed() {
    let test = "mac-e2e-xcode-broken";
    let world = World::with_xcode_store(test);
    let entry = world.hold_xcode(NEW_XCODE, NEW_XCODE_BUILD, b"not an Xcode archive");
    world.group(
        GROUP,
        &["h1"],
        op::MaintenancePolicy {
            xcode_keep: 1,
            ..one_at_a_time()
        },
    );
    let mac = Mac {
        old_xcode: true,
        ..Mac::default()
    };
    let mut h1 = Node::mac(&world, test, "h1", GROUP, mac);
    h1.report();

    let run = world.schedule(op::ScheduleMaintenanceRequest {
        host_ids: vec!["h1".to_owned()],
        take_last_host: true,
        ..op::ScheduleMaintenanceRequest::default()
    });
    let kinds = [pb::MaintenanceKind::Macos, pb::MaintenanceKind::Xcode];
    assert_eq!(h1.beat().map(|order| order.kinds), Some(kinds.to_vec()));
    let why =
        "stage: exit 75, xcode_expand_failed: xip --expand exited 1: xip: error: the archive \
               is not an Xcode archive"
            .to_owned();
    assert_eq!(
        h1.record().and_then(|record| record.deferred),
        Some(why.clone())
    );
    let staging = "/Applications/.hostkeeper-staging";
    let sha = &entry.sha256;
    let mut effects = minor_stage();
    effects.extend([
        format!("GET /v1/xcode/{sha} from byte 0"),
        ran(
            PKGUTIL_PROGRAM,
            &["--check-signature", &format!("{staging}/{sha}/Xcode.xip")],
        ),
        ran(XIP, &["--expand", &format!("{staging}/{sha}/Xcode.xip")]),
    ]);
    assert_eq!(h1.machine.effects(), effects);
    assert_eq!(h1.machine.inputs(), Vec::<Input>::new());
    assert!(!h1.machine.cordoned());
    assert_eq!(
        h1.machine.applications(),
        [format!("Xcode-{OLD_XCODE}.app")]
    );
    assert_eq!(
        h1.machine.selected(),
        Some(format!(
            "/Applications/Xcode-{OLD_XCODE}.app/Contents/Developer"
        ))
    );
    assert_eq!(h1.beat(), None);
    assert_eq!(
        turns(&world, &run),
        [("h1".to_owned(), Step::Deferred, Skip::Unspecified, why)]
    );
    assert_eq!(h1.machine.listed(staging), Vec::<String>::new());
}

/// Proves: an install that fails (`softwareupdate --install` exits 1) halts the Mac's turn at the
/// apply: no resume and no rollback run, the Mac stays out of service and is not restarted, the
/// run shows it halted with the daemon's reason, `maintenance_halted` stands for it and the next
/// Mac of the group does not start. Put back runs the drained service's resume, the alert clears
/// and the group goes on: the next Mac takes its whole turn.
///
/// Catches: a failed install that resumes the service, rolls anything back, restarts the Mac or
/// lets the group go on; a halt reason or log line that holds the password; a Put back that
/// installs again.
#[test]
fn a_failed_install_halts_the_mac_cordoned_until_put_back_and_the_group_goes_on() {
    let test = "mac-e2e-halt";
    let world = World::new(test);
    world.group(GROUP, &["h1", "h2", "h3"], one_at_a_time());
    let mut h1 = Node::mac(&world, test, "h1", GROUP, Mac::default());
    let mut h2 = Node::mac(&world, test, "h2", GROUP, Mac::default());
    let mut h3 = Node::mac(&world, test, "h3", GROUP, Mac::up_to_date());
    h1.machine.fail_install();
    for node in [&mut h1, &mut h2, &mut h3] {
        node.report();
    }

    let run = world.schedule(op::ScheduleMaintenanceRequest {
        host_group: GROUP.to_owned(),
        ..op::ScheduleMaintenanceRequest::default()
    });
    let order = first_offer(&run, &[pb::MaintenanceKind::Macos]);
    assert_eq!(h1.beat(), Some(order.clone()));
    let mut halted_at = minor_stage();
    halted_at.push(format!("drain {SERVICE}"));
    halted_at.extend(minor_cutover());
    halted_at.push(ran_with(SOFTWAREUPDATE, &install_args(true)));
    assert_eq!(h1.machine.effects(), halted_at);
    assert!(h1.machine.cordoned(), "a halted Mac stays out of service");
    assert!(!h1.machine.restart_asked());
    let why = "apply macos: exit 1, macos_install_failed: softwareupdate --install: the program \
               exited with code 1"
        .to_owned();
    assert_eq!(
        h1.record().and_then(|record| record.halted),
        Some(why.clone())
    );
    assert_eq!(
        h1.machine.logged(),
        ["softwareupdate --install: the program exited with code 1"]
    );
    password_only_on_stdin(&h1);

    assert_eq!(h1.beat(), None);
    for _ in 0..2 {
        assert_eq!(h2.beat(), None, "a halted group starts no Mac");
        assert_eq!(h3.beat(), None);
    }
    assert_eq!(
        turns(&world, &run),
        [
            ("h1".to_owned(), Step::Halted, Skip::Unspecified, why),
            (
                "h2".to_owned(),
                Step::Waiting,
                Skip::Unspecified,
                String::new()
            ),
        ]
    );
    assert_eq!(
        world.alerts(),
        and_alert(op::AlertKind::MaintenanceHalted, "h1")
    );
    assert_eq!(h1.machine.effects(), halted_at, "nothing more ran");

    world.put_back(&run, "h1");
    let put_back = Ordered {
        order_try: 2,
        action: pb::MaintainAction::PutBack,
        ..order.clone()
    };
    assert_eq!(h1.beat(), Some(put_back.clone()));
    assert_eq!(h1.beat(), None);
    let mut resumed = halted_at.clone();
    resumed.push(format!("resume {SERVICE}"));
    assert_eq!(h1.machine.effects(), resumed);
    assert!(!h1.machine.cordoned(), "put back in service");
    assert_eq!(
        h1.machine.running(),
        (OLD_VERSION.to_owned(), OLD_BUILD.to_owned())
    );
    assert_eq!(world.alerts(), updates_due(), "h1's update is still due");

    assert_eq!(h2.beat(), Some(order.clone()));
    restart_to_done(&mut h2);
    assert_eq!(h2.machine.effects(), minor_turn());
    assert_eq!(
        world.steps(&run),
        [at("h1", Step::Done), at("h2", Step::Done)]
    );
    assert_eq!(h1.orders(), [order.clone(), put_back]);
    assert_eq!(h2.orders(), [order]);
    assert_eq!(h3.machine.effects(), Vec::<String>::new());
}

/// Proves: a Mac that comes back from its install's restart on a build other than the target
/// halts at the check, cordoned: the check fails `macos_build_not_target` naming both builds,
/// nothing is resumed or rolled back, `maintenance_halted` stands, and Put back resumes the
/// service.
///
/// Catches: a check that passes on the old build (an update that did not take); a failed check
/// that resumes the service.
#[test]
fn a_check_that_finds_the_wrong_build_halts_the_mac_cordoned_until_put_back() {
    let (world, mut h1, _h2) = pair("mac-e2e-wrong-build", Mac::default());
    h1.machine.come_back_on(OLD_VERSION, OLD_BUILD);

    let run = world.update(&["h1"]);
    let order = first_offer(&run, &[pb::MaintenanceKind::Macos]);
    assert_eq!(h1.beat(), Some(order.clone()));
    assert_eq!(h1.beat(), None);
    let why = format!(
        "check: exit 1, macos_build_not_target: sw_vers reports the build {OLD_BUILD}, not the \
         target build {MINOR_BUILD}"
    );
    assert_eq!(
        h1.reboot(),
        Some(Driven::Ended(Ending::Halted(why.clone())))
    );
    for _ in 0..3 {
        assert_eq!(h1.beat(), None);
    }

    let mut halted_at = minor_turn();
    halted_at.pop();
    assert_eq!(h1.machine.effects(), halted_at);
    assert!(h1.machine.cordoned(), "a halted Mac stays out of service");
    assert_eq!(
        turns(&world, &run),
        [("h1".to_owned(), Step::Halted, Skip::Unspecified, why)]
    );
    assert_eq!(
        world.alerts(),
        and_alert(op::AlertKind::MaintenanceHalted, "h1")
    );

    world.put_back(&run, "h1");
    let put_back = Ordered {
        order_try: 2,
        action: pb::MaintainAction::PutBack,
        ..order.clone()
    };
    assert_eq!(h1.beat(), Some(put_back.clone()));
    assert_eq!(h1.beat(), None);
    halted_at.push(format!("resume {SERVICE}"));
    assert_eq!(h1.machine.effects(), halted_at);
    assert!(!h1.machine.cordoned(), "put back in service");
    assert_eq!(world.steps(&run), [at("h1", Step::Done)]);
    assert_eq!(world.alerts(), updates_due(), "the update is still due");
    assert_eq!(h1.orders(), [order, put_back]);
}

/// Proves: a Mac silent in its install's restart raises `maintenance_not_back` at the run of the
/// coordinator's job `alerts` once the coordinator has not heard it for the Mac's `return_secs`
/// (3600 seconds by default), and not at the run one second before; when it comes back, its turn
/// ends done and the alert clears.
///
/// Catches: a not-back alert raised early (at a Linux host's 1200 seconds, or at once), never
/// raised, or left standing after the Mac came back.
#[test]
fn a_mac_silent_past_return_secs_is_not_back_and_not_before() {
    let (world, mut h1, _h2) = pair("mac-e2e-not-back", Mac::default());

    let run = world.update(&["h1"]);
    let order = first_offer(&run, &[pb::MaintenanceKind::Macos]);
    assert_eq!(h1.beat(), Some(order.clone()));
    assert_eq!(h1.beat(), None);
    assert_eq!(world.steps(&run), [at("h1", Step::Rebooting)]);
    h1.stop();

    world.advance(MAC_RETURN_SECS - 1);
    world.run_alerts_job();
    assert_eq!(world.alerts(), updates_due(), "not yet");
    assert_eq!(world.steps(&run), [at("h1", Step::Rebooting)]);
    world.advance(1);
    world.run_alerts_job();
    assert_eq!(
        world.alerts(),
        and_alert(op::AlertKind::MaintenanceNotBack, "h1")
    );
    assert_eq!(world.steps(&run), [at("h1", Step::NotBack)]);

    assert_eq!(h1.come_back(), Some(Driven::Ended(Ending::Done)));
    for _ in 0..3 {
        assert_eq!(h1.beat(), None);
    }
    assert_eq!(h1.machine.effects(), minor_turn());
    assert_eq!(world.steps(&run), [at("h1", Step::Done)]);
    assert_eq!(world.alerts(), no_alerts());
    assert_eq!(h1.orders(), [order]);
}

/// Proves: a daemon that stops in the middle of a Mac's turn (just after the drain) and a
/// coordinator that restarts, once while the turn is under way and once while the Mac restarts,
/// neither start the order again nor repeat a step: the new coordinator reads the turn from its
/// store and orders nothing, and the new daemon's start resumes the turn from its run record at
/// the apply, whose install restarts the Mac. The turn ends done with each step run once and one
/// install.
///
/// Catches: a coordinator that starts a Mac's turn again after a restart; a daemon that stages or
/// drains again, or installs twice, after a restart; a turn lost across the restarts.
#[test]
fn restarts_of_the_coordinator_and_the_daemon_mid_turn_install_once() {
    let (world, mut h1, _h2) = pair("mac-e2e-restart", Mac::default());
    h1.stop();
    let stop = after_drained(&h1, SERVICE);
    h1.start_crashing(stop);
    h1.report();

    let run = world.update(&["h1"]);
    let order = first_offer(&run, &[pb::MaintenanceKind::Macos]);
    assert_eq!(h1.beat(), Some(order.clone()));
    let mut drained = minor_stage();
    drained.push(format!("drain {SERVICE}"));
    assert_eq!(h1.machine.effects(), drained);
    assert_eq!(h1.beat(), None);

    world.restart();
    assert_eq!(h1.beat(), None, "the restarted coordinator starts nothing");
    assert_eq!(world.in_turn(&run), ["h1".to_owned()]);

    h1.stop();
    assert_eq!(h1.start(), Some(Driven::Waiting(Kind::Macos)));
    assert_eq!(h1.beat(), None);
    world.restart();
    restart_to_done(&mut h1);

    assert_eq!(h1.machine.effects(), minor_turn());
    assert_eq!(h1.machine.inputs(), [minor_install(true)]);
    assert_eq!(world.steps(&run), [at("h1", Step::Done)]);
    assert_eq!(h1.orders(), [order]);
    assert_eq!(h1.due(), Vec::<pb::MaintenanceKind>::new());
}
