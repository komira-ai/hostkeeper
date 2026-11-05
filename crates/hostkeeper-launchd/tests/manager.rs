//! The service manager over launchd: what `launchctl print` says becomes a unit state, and
//! anything that keeps it from being read is an error, which the observer turns into an
//! unobserved round. The launchctl behind it is the fake; nothing here runs a process.

use std::time::Duration;

use hostkeeper_launchd::launchctl::{LaunchctlOutput, print_args};
use hostkeeper_launchd::{FakeLaunchctl, Label, LaunchctlError, LaunchdServiceManager};
use hostkeeper_observe::fake::{FakeExpectedExe, FakeHealthClient, FakeProcessProbe};
use hostkeeper_observe::{
    ActiveState, Current, Deadline, HealthView, ObserveError, ObserveOptions, Observer, Ports,
    ServiceInput, ServiceManager, ServiceState, UnitState, UnobservedReason, VersionView,
};

const UNIT: &str = "dev.hostkeeper.web";
const OTHER: &str = "dev.hostkeeper.web-standby";
const DEADLINE: Deadline = Deadline {
    connect: Duration::from_secs(2),
    read: Duration::from_secs(3),
};

/// The text `launchctl print system/<label>` prints for a job whose depth-1 lines are `lines`.
fn printed(label: &str, lines: &[&str]) -> String {
    let body: String = lines.iter().map(|line| format!("\t{line}\n")).collect();
    format!("system/{label} = {{\n{body}}}\n")
}

fn label(text: &str) -> Label {
    Label::parse(text).expect("a valid label")
}

/// A manager over a fake that answers every print of `unit` with `answer`.
fn asking(
    unit: &str,
    answer: Result<LaunchctlOutput, LaunchctlError>,
) -> (LaunchdServiceManager<FakeLaunchctl>, FakeLaunchctl) {
    let fake = FakeLaunchctl::new();
    fake.always(&print_args(&label(unit)), answer);
    (LaunchdServiceManager::new(fake.clone()), fake)
}

fn state(active: ActiveState, sub: &str, main_pid: Option<u32>) -> UnitState {
    UnitState {
        active,
        sub: sub.to_owned(),
        main_pid,
    }
}

fn manager_error(unit: &str, why: &str) -> ObserveError {
    ObserveError::Manager {
        unit: unit.to_owned(),
        why: why.to_owned(),
    }
}

/// Catches a launchd job read as the wrong unit state: a running job must be active with its
/// pid, a spawn waiting to happen activating, a job that ended badly failed, a job that ended
/// well or never ran inactive, and a label launchd does not hold not found.
#[test]
fn print_output_maps_to_the_unit_states() {
    let cases: Vec<(LaunchctlOutput, UnitState)> = vec![
        (
            LaunchctlOutput::success(&printed(UNIT, &["state = running", "pid = 4242"])),
            state(ActiveState::Active, "running", Some(4242)),
        ),
        (
            LaunchctlOutput::success(&printed(UNIT, &["state = spawn scheduled"])),
            state(ActiveState::Activating, "spawn scheduled", None),
        ),
        (
            LaunchctlOutput::success(&printed(UNIT, &["state = waiting", "last exit code = 0"])),
            state(ActiveState::Inactive, "waiting", None),
        ),
        (
            LaunchctlOutput::success(&printed(UNIT, &["state = waiting"])),
            state(ActiveState::Inactive, "waiting", None),
        ),
        (
            LaunchctlOutput::success(&printed(
                UNIT,
                &["state = not running", "last exit code = 3"],
            )),
            state(ActiveState::Failed, "not running", None),
        ),
        (
            LaunchctlOutput::success(&printed(
                UNIT,
                &["state = waiting", "last terminating signal = Killed: 9"],
            )),
            state(ActiveState::Failed, "waiting", None),
        ),
        (
            LaunchctlOutput::exit(113, ""),
            state(ActiveState::NotFound, "not loaded", None),
        ),
        (
            LaunchctlOutput::exit(1, "Could not find service \"web\" in domain for system"),
            state(ActiveState::NotFound, "not loaded", None),
        ),
    ];
    for (output, expected) in cases {
        let (manager, fake) = asking(UNIT, Ok(output.clone()));
        assert_eq!(manager.unit_state(UNIT), Ok(expected), "for {output:?}");
        let asked = vec![vec!["print".to_owned(), format!("system/{UNIT}")]];
        assert_eq!(fake.calls(), asked, "for {output:?}");
    }
}

/// Catches a launchctl that could not be run or read being turned into a state: each failure is
/// an error naming the unit, so the observer never reports a service absent or stopped on it.
#[test]
fn a_launchctl_failure_or_unparsable_print_is_an_error_not_a_state() {
    let failures: Vec<Result<LaunchctlOutput, LaunchctlError>> = vec![
        Err(LaunchctlError::Spawn {
            args: "print system/dev.hostkeeper.web".to_owned(),
            why: "no such file".to_owned(),
        }),
        Ok(LaunchctlOutput::exit(1, "Bootstrap failed")),
        Err(LaunchctlError::TimedOut {
            args: "print system/dev.hostkeeper.web".to_owned(),
        }),
        Ok(LaunchctlOutput::success("")),
        Ok(LaunchctlOutput::success(
            "system/dev.hostkeeper.web = {\n\tstate = running\n",
        )),
        Ok(LaunchctlOutput::success(&printed(
            UNIT,
            &["state = frobnicating"],
        ))),
    ];
    for answer in failures {
        let (manager, _) = asking(UNIT, answer.clone());
        let result = manager.unit_state(UNIT);
        assert!(
            matches!(&result, Err(ObserveError::Manager { unit, .. }) if unit == UNIT),
            "for {answer:?}: {result:?}"
        );
    }

    let (manager, _) = asking(
        UNIT,
        Ok(LaunchctlOutput::success(&printed(
            UNIT,
            &["state = frobnicating"],
        ))),
    );
    assert_eq!(
        manager.unit_state(UNIT),
        Err(manager_error(
            UNIT,
            "the job state \"frobnicating\" is not known"
        ))
    );

    let (manager, _) = asking(UNIT, Ok(LaunchctlOutput::exit(1, "Bootstrap failed")));
    assert_eq!(
        manager.unit_state(UNIT),
        Err(manager_error(
            UNIT,
            "launchctl print exited with Some(1): Bootstrap failed"
        ))
    );

    let fake = FakeLaunchctl::new();
    let manager = LaunchdServiceManager::new(fake.clone());
    assert_eq!(
        manager.unit_state("bad label"),
        Err(manager_error(
            "bad label",
            "launchd label \"bad label\" holds the byte 0x20, which a label may not"
        ))
    );
    assert!(
        fake.calls().is_empty(),
        "a refused label is never passed to launchctl"
    );
}

fn observer_over(manager: LaunchdServiceManager<FakeLaunchctl>) -> Observer {
    let ports = Ports {
        manager: Box::new(manager),
        process: Box::new(FakeProcessProbe::default()),
        health: Box::new(FakeHealthClient::default()),
        expected: Box::new(FakeExpectedExe::default()),
    };
    Observer::new(ports, ObserveOptions::default())
}

fn web(run: &[&str]) -> ServiceInput {
    ServiceInput {
        service: "web".to_owned(),
        current: Some(Current {
            run: run.iter().map(|unit| (*unit).to_owned()).collect(),
            health_url: None,
        }),
        recorded_version: None,
    }
}

/// Catches a launchctl failure read as a stopped or absent service: through the real observer the
/// round is unobserved with the manager-error reason.
#[test]
fn a_launchd_error_makes_the_observer_round_unobserved() {
    let fake = FakeLaunchctl::new();
    fake.always(
        &print_args(&label(UNIT)),
        Ok(LaunchctlOutput::exit(1, "launchd is not answering")),
    );
    let observer = observer_over(LaunchdServiceManager::new(fake));

    let observation = observer.observe(&web(&[UNIT]), DEADLINE);

    assert!(
        matches!(
            observation.state,
            ServiceState::Unobserved {
                reason: UnobservedReason::ManagerError,
                ..
            }
        ),
        "{:?}",
        observation.state
    );
    assert_eq!(observation.state.token(), "unobserved");
    assert_eq!(observation.version, VersionView::NotLive);
    assert_eq!(observation.health, HealthView::NotProbed);
}

/// Catches a run set judged by its last label: one running job and one waiting after a clean exit
/// make the service live, as one active and one inactive unit do on Linux.
#[test]
fn a_service_with_one_active_label_and_one_inactive_is_live() {
    let fake = FakeLaunchctl::new();
    fake.always(
        &print_args(&label(UNIT)),
        Ok(LaunchctlOutput::success(&printed(
            UNIT,
            &["state = running", "pid = 4242"],
        ))),
    );
    fake.always(
        &print_args(&label(OTHER)),
        Ok(LaunchctlOutput::success(&printed(
            OTHER,
            &["state = waiting", "last exit code = 0"],
        ))),
    );
    let observer = observer_over(LaunchdServiceManager::new(fake));

    let observation = observer.observe(&web(&[UNIT, OTHER]), DEADLINE);

    assert_eq!(observation.state, ServiceState::Live);
    assert_eq!(observation.state.token(), "live");
}
