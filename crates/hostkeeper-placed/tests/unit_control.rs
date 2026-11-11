//! The start and is-active calls of the service-manager port (design section 5.6, steps 14 and
//! 15): the systemctl commands they are, and what the fake records, keeps and fails.

use std::collections::BTreeSet;

use hostkeeper_placed::testing::{Event, FakeLauncher, FakeUnitControl, Log, UnitCall};
use hostkeeper_placed::{LaunchError, Systemctl, UnitControl, UnitControlError};

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn set(units: &[&str]) -> BTreeSet<String> {
    units.iter().map(|unit| (*unit).to_owned()).collect()
}

/// Catches a start whose unit name could be read as an option, or an is-active that reads a
/// failed command as active: the exact argument vectors, a start error naming its action, and
/// `false` for an is-active the launcher fails.
#[test]
fn start_and_is_active_are_one_systemctl_command_each() {
    let systemctl = Systemctl::new(FakeLauncher::new());
    systemctl.start_unit("web@80.service").expect("start");
    assert_eq!(systemctl.is_active("web@80.service"), Ok(true));
    assert_eq!(
        systemctl.launcher().runs(),
        vec![
            argv(&["systemctl", "start", "--", "web@80.service"]),
            argv(&["systemctl", "is-active", "--quiet", "--", "web@80.service"]),
        ]
    );

    let failing = Systemctl::new(FakeLauncher::failing());
    assert_eq!(
        failing.start_unit("web@80.service"),
        Err(UnitControlError {
            action: "systemctl start -- web@80.service".to_owned(),
            why: "exit status 1".to_owned(),
        })
    );
    assert_eq!(failing.is_active("web@80.service"), Ok(false));
    assert_eq!(failing.launcher().runs().len(), 2);
}

/// Catches a fake that does not keep which units run: a start makes a unit active, a stop makes
/// it inactive, a unit never started is not active, `set_active` marks one active, and every call
/// is in the log in order.
#[test]
fn the_fake_keeps_which_units_are_active() {
    let log = Log::new();
    let units = FakeUnitControl::new(log.clone());
    assert_eq!(units.is_active("a.service"), Ok(false));
    units.start_unit("a.service").expect("start");
    assert_eq!(units.is_active("a.service"), Ok(true));
    units.set_active("b.service");
    assert_eq!(units.active(), set(&["a.service", "b.service"]));
    units.stop_unit("a.service").expect("stop");
    assert_eq!(units.is_active("a.service"), Ok(false));
    assert_eq!(units.active(), set(&["b.service"]));
    assert_eq!(
        log.events(),
        vec![
            Event::Unit(UnitCall::IsActive("a.service".to_owned())),
            Event::Unit(UnitCall::Start("a.service".to_owned())),
            Event::Unit(UnitCall::IsActive("a.service".to_owned())),
            Event::Unit(UnitCall::Stop("a.service".to_owned())),
            Event::Unit(UnitCall::IsActive("a.service".to_owned())),
        ]
    );
}

/// Catches a delay counted wrongly: a unit started with `active_after(unit, 2)` is not active for
/// two asks and active from the third on, and the delay does not touch another unit.
#[test]
fn the_fake_makes_a_unit_active_after_the_asks_it_was_told() {
    let units = FakeUnitControl::new(Log::new());
    units.active_after("slow.service", 2);
    units.start_unit("slow.service").expect("start");
    units.start_unit("quick.service").expect("start");
    assert_eq!(units.is_active("quick.service"), Ok(true));
    assert_eq!(units.is_active("slow.service"), Ok(false));
    assert_eq!(units.is_active("slow.service"), Ok(false));
    assert_eq!(units.is_active("slow.service"), Ok(true));
    assert_eq!(units.is_active("slow.service"), Ok(true));
}

/// Catches a fault that reaches the wrong call: `fail_start` fails the start of that unit only
/// and leaves it inactive, `fail_reload` fails `daemon-reload` only, each error naming the call;
/// and the fake that fails every call fails a start and an is-active too.
#[test]
fn the_fake_fails_the_calls_it_was_told_to() {
    let log = Log::new();
    let units = FakeUnitControl::new(log.clone());
    units.fail_start("bad.service");
    assert_eq!(
        units.start_unit("bad.service"),
        Err(UnitControlError {
            action: "Start(\"bad.service\")".to_owned(),
            why: "the fake fails this call".to_owned(),
        })
    );
    assert_eq!(units.is_active("bad.service"), Ok(false));
    units
        .start_unit("good.service")
        .expect("another unit starts");
    units
        .daemon_reload()
        .expect("the reload works before fail_reload");
    units.fail_reload();
    assert_eq!(
        units.daemon_reload(),
        Err(UnitControlError {
            action: "DaemonReload".to_owned(),
            why: "the fake fails this call".to_owned(),
        })
    );
    units.stop_unit("good.service").expect("a stop still works");
    assert_eq!(units.active(), BTreeSet::new());

    let failing = FakeUnitControl::failing(Log::new());
    let every = |action: &str| UnitControlError {
        action: action.to_owned(),
        why: "the fake fails every call".to_owned(),
    };
    assert_eq!(
        failing.start_unit("a.service"),
        Err(every("Start(\"a.service\")"))
    );
    assert_eq!(
        failing.is_active("a.service"),
        Err(every("IsActive(\"a.service\")"))
    );
    assert_eq!(failing.active(), BTreeSet::new());
}

/// Proves a stop of a unit the service manager has not loaded (`systemctl stop` exits 5, as for
/// the run unit of a first install whose package was never installed) is a stop that succeeded:
/// one command, the exact argument vector, no error.
///
/// Catches: the rollback of a first install failing because its unit does not exist yet.
#[test]
fn a_stop_of_a_unit_that_is_not_loaded_is_no_error() {
    let systemctl = Systemctl::new(FakeLauncher::failing_with(LaunchError::Exit(5)));
    assert_eq!(systemctl.stop_unit("js-agent-crun.service"), Ok(()));
    assert_eq!(
        systemctl.launcher().runs(),
        vec![argv(&["systemctl", "stop", "--", "js-agent-crun.service"])]
    );
}

/// Proves every other end of `systemctl stop` is still an error naming the command and the end:
/// exit 4 (no permission), exit 1 (a stop that failed), and an end with no exit code (a signal,
/// the deadline, even one whose text reads `exit status 5`); and that exit 5 is forgiven for a stop only, never for a start or a reload.
///
/// Catches: a real stop failure read as stopped, or a start of a missing unit read as started.
#[test]
fn a_stop_that_fails_otherwise_and_a_start_of_a_unit_not_loaded_are_errors() {
    let stop = |why: &str| UnitControlError {
        action: "systemctl stop -- web.service".to_owned(),
        why: why.to_owned(),
    };
    for (error, why) in [
        (LaunchError::Exit(4), "exit status 4"),
        (LaunchError::Exit(1), "exit status 1"),
        (
            LaunchError::Failed("ended by a signal".to_owned()),
            "ended by a signal",
        ),
        // Only the code counts, never a text that reads like it.
        (
            LaunchError::Failed("exit status 5".to_owned()),
            "exit status 5",
        ),
    ] {
        let systemctl = Systemctl::new(FakeLauncher::failing_with(error));
        assert_eq!(systemctl.stop_unit("web.service"), Err(stop(why)));
        assert_eq!(
            systemctl
                .stop_unit("web.service")
                .expect_err("still fails")
                .to_string(),
            format!("systemctl stop -- web.service failed: {why}")
        );
    }
    let absent = Systemctl::new(FakeLauncher::failing_with(LaunchError::Exit(5)));
    assert_eq!(
        absent.start_unit("web.service"),
        Err(UnitControlError {
            action: "systemctl start -- web.service".to_owned(),
            why: "exit status 5".to_owned(),
        })
    );
    assert_eq!(
        absent.daemon_reload(),
        Err(UnitControlError {
            action: "systemctl daemon-reload".to_owned(),
            why: "exit status 5".to_owned(),
        })
    );
    assert_eq!(absent.is_active("web.service"), Ok(false));
}
