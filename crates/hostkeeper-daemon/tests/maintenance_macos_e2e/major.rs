//! A major upgrade of macOS end to end (docs/design/maintenance-updates.md, sections 1, 3 and
//! 5.5), on Macs whose daemon runs as its launchd job starts it, with no flag: the daemon reports
//! the newest major upgrade it sees, and the group's `macos_major` alone decides whether it counts
//! as due, raises the group's `updates_due` notice and is ordered. Each Mac `h1` here is offered
//! the major upgrade alone: its `softwareupdate --list` lists nothing and `--list-full-installers`
//! offers macOS 27.1, so nothing else could make it due.

use super::*;

/// The gRPC status code `FAILED_PRECONDITION`, of a `Schedule` refused `nothing_due`.
const FAILED_PRECONDITION: i32 = 9;

/// A Mac ready for the daemon's install, offered the major upgrade alone.
fn offered_the_major_alone() -> Mac {
    Mac {
        minor_listed: false,
        ..Mac::default()
    }
}

/// `h1`, offered the major upgrade alone, and `h2`, with nothing to install, in [`GROUP`] under
/// `policy`, each heard with its report.
fn pair_offered_the_major(test: &str, policy: op::MaintenancePolicy) -> (Arc<World>, Node, Node) {
    let world = World::new(test);
    world.group(GROUP, &["h1", "h2"], policy);
    let mut h1 = Node::mac(&world, test, "h1", GROUP, offered_the_major_alone());
    let mut h2 = Node::mac(&world, test, "h2", GROUP, Mac::up_to_date());
    h1.report();
    h2.report();
    (world, h1, h2)
}

/// What the Mac goes through in a turn of the major upgrade, in order: the stage fetches the
/// full installer, the cutover lists both again, `startosinstall` runs and restarts the Mac into
/// the new major version, the check passes and the service is resumed.
fn major_turn() -> Vec<String> {
    let startosinstall = format!("{INSTALLER_APP}/Contents/Resources/startosinstall");
    vec![
        "prepare".to_owned(),
        ran(SW_VERS, &[]),
        ran(SOFTWAREUPDATE, &["--list"]),
        ran(SOFTWAREUPDATE, &["--list-full-installers"]),
        ran(
            SOFTWAREUPDATE,
            &[
                "--fetch-full-installer",
                "--full-installer-version",
                MAJOR_VERSION,
            ],
        ),
        format!("drain {SERVICE}"),
        "cutover".to_owned(),
        ran(FDESETUP, &["status"]),
        ran(SW_VERS, &[]),
        ran(SOFTWAREUPDATE, &["--list"]),
        ran(SOFTWAREUPDATE, &["--list-full-installers"]),
        ran_with(&startosinstall, &upgrade_args()),
        "boot boot-2".to_owned(),
        "verify".to_owned(),
        ran(SW_VERS, &[]),
        format!("resume {SERVICE}"),
    ]
}

/// The arguments of the installer app's `startosinstall`.
fn upgrade_args() -> Vec<String> {
    args(&[
        "--agreetolicense",
        "--nointeraction",
        "--user",
        ACCOUNT,
        "--stdinpass",
    ])
}

/// Proves: a Mac whose daemon was started with no flag, as its launchd job starts it, and which
/// is offered a major upgrade alone, reports it due; in a group whose `macos_major` is true the
/// group's `updates_due` notice stands, the Update button orders it `macos` with `macos_major`,
/// the stage fetches the full installer, the install runs `startosinstall` with the password on
/// its standard input alone, and the Mac restarts into macOS 27.1 with nothing due after and the
/// notice cleared.
///
/// Catches: a daemon that reports a major upgrade only when a flag of its own allows it, so a
/// group that allows one never has it due and the Update button on the Mac is refused
/// `nothing_due`.
#[test]
fn a_group_that_allows_a_major_upgrade_has_it_due_on_a_mac_started_with_no_flag() {
    let (world, mut h1, _h2) = pair_offered_the_major(
        "mac-e2e-major-no-flag",
        op::MaintenancePolicy {
            macos_major: true,
            ..one_at_a_time()
        },
    );
    assert_eq!(h1.due(), [pb::MaintenanceKind::Macos]);
    assert_eq!(world.alerts(), updates_due());

    let run = world.update(&["h1"]);
    let order = Ordered {
        macos_major: true,
        ..first_offer(&run, &[pb::MaintenanceKind::Macos])
    };
    assert_eq!(h1.beat(), Some(order.clone()));
    assert_eq!(h1.machine.applications(), [INSTALLER_NAME]);
    restart_to_done(&mut h1);

    assert_eq!(h1.machine.effects(), major_turn());
    let startosinstall = format!("{INSTALLER_APP}/Contents/Resources/startosinstall");
    assert_eq!(
        h1.machine.inputs(),
        [Input {
            program: startosinstall,
            args: upgrade_args(),
            stdin: format!("{PASSWORD}\n"),
        }]
    );
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

/// Proves: in a group whose `macos_major` is false (the default), a Mac offered a major upgrade
/// alone, whose daemon reports it, has nothing due: the group raises no `updates_due` notice, the
/// Update button on it is refused `nothing_due`, it is given no order, nothing runs on it, and it
/// stays on the macOS it runs.
///
/// Catches: a major upgrade counted as due, noticed or ordered because the daemon reports it, in a
/// group that has not allowed one.
#[test]
fn a_group_that_does_not_allow_a_major_upgrade_never_has_it_due() {
    let (world, mut h1, _h2) = pair_offered_the_major("mac-e2e-major-not-allowed", one_at_a_time());
    assert_eq!(h1.due(), [pb::MaintenanceKind::Macos]);
    assert_eq!(world.alerts(), no_alerts());
    assert_eq!(
        world.update_refused(&["h1"]),
        (FAILED_PRECONDITION, "nothing_due".to_owned())
    );
    for _ in 0..3 {
        assert_eq!(h1.beat(), None);
    }
    assert_eq!(h1.machine.effects(), Vec::<String>::new());
    assert_eq!(h1.orders(), Vec::<Ordered>::new());
    assert_eq!(h1.machine.applications(), Vec::<String>::new());
    assert_eq!(
        h1.machine.running(),
        (OLD_VERSION.to_owned(), OLD_BUILD.to_owned())
    );
}
