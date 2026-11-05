#![cfg(feature = "privileged")]
//! The observer's real adapters (`SystemdManager` over `systemctl show`, `ProcProbe` over
//! `/proc`), as the daemon builds them, as root against a real unit: the fixture's `serve`,
//! linked from a unit file in the test's scratch directory, so that it stays loaded once stopped.
//! The observer reports it live, with the version confirmed by the SHA-256 of the fixture binary
//! read through `/proc/<pid>/exe` of its main process; stopped once it is stopped; absent for a
//! unit that does not exist; and unobserved, never absent, when `systemctl` fails (design
//! section 5.8).

mod support;

use std::path::Path;
use std::time::Duration;

use hostkeeper_hook::Step;
use hostkeeper_observe::fake::{FakeExpectedExe, FakeHealthClient};
use hostkeeper_observe::{
    CommandOutput, CommandRunner, Current, Deadline, ObserveOptions, Observer, Ports, ProcProbe,
    ProcessRunner, RunError, ServiceInput, ServiceState, SystemdManager, UnobservedReason,
    VersionView,
};
use hostkeeper_privileged::fixture::{self, Mode};
use hostkeeper_privileged::rig::service_unit;
use hostkeeper_privileged::{Rig, SERVICE};
use hostkeeper_steps::systemd::SYSTEMCTL;
use hostkeeper_steps::{Bound, StepCall, StepLauncher, SystemdLauncher};
use support::Bench;

/// What the daemon gives `systemctl show` (crates/hostkeeper-daemon/src/observer.rs keeps its
/// own; these are as generous).
const OUTPUT_CAP: usize = hostkeeper_observe::command::MAX_STDOUT;
const DEADLINE: Duration = Duration::from_secs(30);
const VERSION: &str = "1.0.0";

/// The runner of `systemctl` with a flag it does not know put first: a real `systemctl` that
/// fails, as a broken or overloaded service manager would.
struct FailingSystemctl(ProcessRunner);

impl CommandRunner for FailingSystemctl {
    fn run(&self, argv: &[String], deadline: Duration) -> Result<CommandOutput, RunError> {
        let mut argv = argv.to_vec();
        argv.insert(1, "--no-such-flag".to_owned());
        self.0.run(&argv, deadline)
    }
}

/// An observer over the host's `systemctl` (through `runner`) and `/proc`, which knows the
/// fixture's `digest` as the sealed executable of [`VERSION`].
fn observer<R: CommandRunner + 'static>(runner: R, digest: [u8; 32]) -> Observer {
    let expected = FakeExpectedExe::default();
    expected.seal(SERVICE, VERSION, digest);
    Observer::new(
        Ports {
            manager: Box::new(SystemdManager::new(runner, DEADLINE)),
            process: Box::new(ProcProbe::new(Path::new("/proc"))),
            health: Box::new(FakeHealthClient::default()),
            expected: Box::new(expected),
        },
        ObserveOptions::default(),
    )
}

fn input(unit: &str) -> ServiceInput {
    ServiceInput {
        service: SERVICE.to_owned(),
        current: Some(Current {
            run: vec![service_unit(unit)],
            health_url: None,
        }),
        recorded_version: Some(VERSION.to_owned()),
    }
}

fn deadline() -> Deadline {
    Deadline {
        connect: DEADLINE,
        read: DEADLINE,
    }
}

/// The SHA-256 the fixture reports of its own executable (`print-pid`, run as a step under the
/// production launcher), which must be the digest of the binary cargo built.
fn fixture_digest(rig: &Rig) -> [u8; 32] {
    let unit = rig.unit();
    let pids = rig.pid_file("print");
    let params = rig
        .params("print", &Mode::PrintPid, Some(pids.as_path()))
        .expect("write the params");
    let call = StepCall {
        service: SERVICE,
        invocation: hostkeeper_hook::Invocation {
            step: Step::Verify,
            artifact: None,
            params: Some(params),
        },
        executable: rig.fixture(),
        unit: &unit,
        bound: Bound::Secs(std::num::NonZeroU64::new(600).expect("not 0")),
    };
    let exit = SystemdLauncher::system().run(&call).expect("run print-pid");
    assert_eq!(exit.code, Some(0), "{exit:?}");
    let reported = fixture::read_pid_file(&pids)
        .expect("the pid file")
        .exe_sha256
        .expect("the digest");
    let built = fixture::file_sha256(rig.fixture()).expect("hash the fixture");
    assert_eq!(reported, built, "the step ran the fixture binary");
    let bytes = hex::decode(&built).expect("hex");
    bytes.try_into().expect("32 bytes")
}

/// Catches an observer adapter that reads the wrong unit, reads the digest of something else
/// than the main process's executable, takes a stopped unit for an absent one, or reads a failed
/// `systemctl` as absent.
#[test]
fn the_observer_reads_active_state_and_the_main_pid_digest_of_a_real_unit() {
    let bench = Bench::new("observer");
    let rig = &bench.rig;
    let digest = fixture_digest(rig);
    let unit = rig.unit();
    let file = rig.unit_file(&unit).expect("write the unit file");
    let file = file.to_str().expect("a UTF-8 path");
    let service = service_unit(&unit);
    support::run_ok(SYSTEMCTL, &["link", "--system", file]);
    support::run_ok(SYSTEMCTL, &["start", "--system", service.as_str()]);
    support::wait_until(|| support::property(&unit, "ActiveState") == "active");
    let main_pid = support::property(&unit, "MainPID");
    let exe = std::fs::read_link(format!("/proc/{main_pid}/exe")).expect("the main process");
    let built = std::fs::canonicalize(support::FIXTURE).expect("the fixture binary");
    assert_eq!(exe, built, "the unit's main process is the fixture");

    let real = observer(ProcessRunner::new(OUTPUT_CAP), digest);
    let live = real.observe(&input(&unit), deadline());
    assert_eq!(live.state, ServiceState::Live, "{live:?}");
    assert_eq!(
        live.version,
        VersionView::Recorded {
            version: VERSION.to_owned(),
            confirmed: true,
        },
        "{live:?}"
    );
    // Another digest sealed: the same process is a mismatch, so the one above was compared.
    let other = observer(ProcessRunner::new(OUTPUT_CAP), [0; 32]);
    let mismatch = other.observe(&input(&unit), deadline());
    assert!(
        matches!(mismatch.version, VersionView::Mismatch { .. }),
        "{mismatch:?}"
    );

    let failing = observer(FailingSystemctl(ProcessRunner::new(OUTPUT_CAP)), digest);
    let failed = failing.observe(&input(&unit), deadline());
    assert!(
        matches!(
            failed.state,
            ServiceState::Unobserved {
                reason: UnobservedReason::ManagerError,
                ..
            }
        ),
        "{failed:?}"
    );
    assert_eq!(failed.state.token(), "unobserved");

    support::run_ok(SYSTEMCTL, &["stop", "--system", service.as_str()]);
    let stopped = real.observe(&input(&unit), deadline());
    assert_eq!(stopped.state, ServiceState::Stopped, "{stopped:?}");
    assert_eq!(stopped.version, VersionView::NotLive);

    let nowhere = rig.unit();
    let absent = real.observe(&input(&nowhere), deadline());
    assert_eq!(absent.state, ServiceState::Absent, "{absent:?}");
    let failed = failing.observe(&input(&nowhere), deadline());
    assert_eq!(failed.state.token(), "unobserved", "{failed:?}");
}
