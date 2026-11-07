//! How the daemon observes its own service, `hostkeeper`: through the unit or launchd job it
//! runs as, which the platform's adapters name, and its version confirmed against the file of
//! that version in the binary directory its unit starts `current` of.
//!
//! The Mac's set is the real one (`Adapters::macos`: `launchctl print` read by the launchd service
//! manager, the process read by the launchd process probe), over a fake `launchctl` and a fake
//! pid lookup that names a scratch file; the Linux set's service manager is a fake.

mod support;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::adapters::Adapters;
use hostkeeper_daemon::admin::service_state;
use hostkeeper_daemon::cli::Unwired;
use hostkeeper_daemon::link_adapters::observation;
use hostkeeper_daemon::observer::{OwnService, STATUS_DEADLINE, SealedExe, observe, observer_with};
use hostkeeper_daemon::selfunits::MAIN_UNIT;
use hostkeeper_daemon::selfupdate::binary_dir;
use hostkeeper_launchd::launchctl::print_args;
use hostkeeper_launchd::{FakeLaunchctl, Label, LaunchctlOutput, PidInfo, PidInfoError, Platform};
use hostkeeper_link::Liveness as LinkLiveness;
use hostkeeper_observe::proc::digest_open_file;
use hostkeeper_observe::{
    ActiveState, Current, ExeStat, ExpectedExe, Observation, ServiceInput, ServiceState,
    VersionView, beat_health,
};
use hostkeeper_proto::hostkeeper::agent::v1::Health;
use hostkeeper_proto::hostkeeper::local::v1::{Liveness, Unhealthy};
use hostkeeper_steps::Layout;
use hostkeeper_unpack::{MacVerify, SignatureVerdict, VerifyIoError};
use support::{OWN, Rig, d};

/// The service of hostkeeper itself.
const HK: &str = "hostkeeper";
/// The version the host runs.
const RUNS: &str = "0.1.134";
/// The version before it, which a fallback puts back.
const BEFORE: &str = "0.1.132";
/// The pid of the daemon's job.
const PID: u32 = 500;

fn hk() -> ServiceName {
    ServiceName::new(HK).expect("a service name")
}

/// The daemon's job.
fn job() -> String {
    Label::daemon().to_string()
}

/// A pid lookup whose one process runs `path`, as `proc_pidpath` and its mapped image name it.
struct RunsFile(PathBuf);

impl PidInfo for RunsFile {
    fn path(&self, _pid: u32) -> Result<PathBuf, PidInfoError> {
        Ok(self.0.clone())
    }

    fn start_time(&self, _pid: u32) -> Result<u64, PidInfoError> {
        Ok(41)
    }

    fn image(&self, pid: u32) -> Result<(u64, u64), PidInfoError> {
        let meta = fs::metadata(&self.0).map_err(|e| PidInfoError {
            pid,
            why: e.to_string(),
        })?;
        Ok((meta.dev(), meta.ino()))
    }
}

/// A package check that passes everything: these tests unpack nothing.
struct Passes;

impl MacVerify for Passes {
    fn quarantine(&self, _path: &Path) -> Result<bool, VerifyIoError> {
        Ok(false)
    }

    fn signature(&self, _binary: &Path) -> Result<SignatureVerdict, VerifyIoError> {
        Ok(SignatureVerdict::Valid)
    }
}

/// What `launchctl print system/<label>` says of a job in `state`, with `extra` lines.
fn printed(label: &str, state: &str, extra: &str) -> LaunchctlOutput {
    LaunchctlOutput::success(&format!(
        "system/{label} = {{\n\tstate = {state}\n{extra}}}\n"
    ))
}

/// A Mac host: hostkeeper's package of `version` installed and sealed with no `[units]` (as the
/// macOS package is), the binary directory holding `staged`, each with its own bytes, and
/// `current` naming `version`.
fn mac_host(name: &str, version: &str, staged: &[&str]) -> Rig {
    let rig = Rig::new(name);
    rig.running(HK, version);
    rig.store.seal(HK, version, &d());
    let layout = Layout::new(&binary_dir(&rig.state));
    for one in staged {
        let file = rig.scratch.path().join(format!("binary-{one}"));
        fs::write(&file, format!("#!hostkeeper {one}\n")).expect("a binary");
        layout.stage(one, &file).expect("staged");
    }
    if staged.contains(&version) {
        layout.switch(version).expect("current");
    }
    rig
}

/// The observer of a Mac whose `launchctl print` of the daemon's job answers `print` and whose
/// daemon process runs `exe`, built from the macOS adapters as the daemon's start builds it.
fn mac_observer(rig: &Rig, print: LaunchctlOutput, exe: &Path) -> hostkeeper_observe::Observer {
    let platform = Platform {
        unit_dir: rig.scratch.path().to_path_buf(),
        ..Platform::macos()
    };
    let launchctl = FakeLaunchctl::new();
    let asked = FakeLaunchctl::new();
    asked.always(&print_args(&Label::daemon()), Ok(print));
    let adapters = Adapters::macos(
        &platform,
        Arc::new(Unwired),
        launchctl,
        asked,
        RunsFile(exe.to_path_buf()),
        Arc::new(Passes),
    )
    .expect("the macOS set");
    assert_eq!(adapters.own_unit, job());
    observer_with(
        adapters.manager,
        adapters.process,
        &rig.packages,
        OWN,
        &OwnService {
            unit: adapters.own_unit,
            binary_dir: binary_dir(&rig.state),
        },
    )
}

fn version_file(rig: &Rig, version: &str) -> PathBuf {
    Layout::new(&binary_dir(&rig.state)).version_path(version)
}

fn of_hk(rig: &Rig, observer: &hostkeeper_observe::Observer) -> Observation {
    observe(observer, &rig.packages, &OWN, &hk()).expect("hostkeeper is observed")
}

/// Catches the farm's Mac reporting its own service `unmonitored`, at no version, unhealthy: the
/// macOS package of hostkeeper names no `[units] run` (the installer pkg places the daemon's
/// launchd job), so the observer asked launchd nothing. Through the macOS adapters, the daemon
/// observes its job `dev.hostkeeper.hostkeeper` as the service `hostkeeper`: live, at the version
/// `current` names, confirmed by the executable the job's process runs (the binary directory's
/// file of that version), and healthy, in the admin `Status` as in the heartbeat.
#[test]
fn a_mac_daemon_observes_its_own_launchd_job_live_healthy_and_at_its_version() {
    let rig = mac_host("own-mac-live", RUNS, &[BEFORE, RUNS]);
    let running = printed(&job(), "running", &format!("\tpid = {PID}\n"));
    let observer = mac_observer(&rig, running, &version_file(&rig, RUNS));

    let seen = of_hk(&rig, &observer);
    assert_eq!(
        (
            seen.state.clone(),
            seen.version.clone(),
            seen.reason_token()
        ),
        (
            ServiceState::Live,
            VersionView::Recorded {
                version: RUNS.to_owned(),
                confirmed: true,
            },
            None
        )
    );
    let wire = service_state(&seen);
    assert_eq!(
        (wire.liveness, wire.version.as_deref(), wire.healthy),
        (Liveness::Live as i32, Some(RUNS), true),
        "{wire:?}"
    );
    assert_eq!(beat_health(&seen), (Health::Healthy, Some(RUNS.to_owned())));
    let beat = observation(&seen);
    assert_eq!(
        (
            beat.liveness,
            beat.version.as_ref().map(|v| v.as_str()),
            beat.health
        ),
        (
            LinkLiveness::Running,
            Some(RUNS),
            hostkeeper_link::Health::Healthy
        )
    );
}

/// Catches the daemon's own job read as live, or healthy, when it is not, and a version
/// confirmed that the job does not run: a job launchd has loaded but is not running (it exited
/// with 1) is stopped; one launchd has not loaded is absent; a job whose process runs the
/// previous version's file (a fallback put it back while the package names the new one) is
/// `version_mismatch` and unhealthy; and one whose version has no file in the binary directory
/// is live and healthy at an unconfirmed version, as a service whose executable the daemon cannot
/// find.
#[test]
fn the_daemons_own_job_is_stopped_absent_or_mismatched_as_launchd_and_its_process_say() {
    let rig = mac_host("own-mac-states", RUNS, &[BEFORE, RUNS]);
    let exe = version_file(&rig, RUNS);

    let stopped = printed(&job(), "not running", "\tlast exit code = 1\n");
    let seen = of_hk(&rig, &mac_observer(&rig, stopped, &exe));
    assert_eq!(seen.state, ServiceState::Stopped);
    let wire = service_state(&seen);
    assert_eq!(
        (wire.liveness, wire.healthy),
        (Liveness::Stopped as i32, false)
    );

    let absent = LaunchctlOutput {
        code: Some(113),
        stdout: Vec::new(),
        stderr: b"Could not find service\n".to_vec(),
    };
    let seen = of_hk(&rig, &mac_observer(&rig, absent, &exe));
    assert_eq!(seen.state, ServiceState::Absent);

    let running = printed(&job(), "running", &format!("\tpid = {PID}\n"));
    let seen = of_hk(
        &rig,
        &mac_observer(&rig, running.clone(), &version_file(&rig, BEFORE)),
    );
    assert_eq!(
        (seen.state.clone(), seen.reason_token()),
        (ServiceState::Live, Some("version_mismatch"))
    );
    let wire = service_state(&seen);
    assert_eq!(
        (wire.version.as_deref(), wire.healthy, wire.unhealthy),
        (None, false, Unhealthy::VersionMismatch as i32)
    );

    let unstaged = mac_host("own-mac-unstaged", RUNS, &[BEFORE]);
    let seen = of_hk(
        &unstaged,
        &mac_observer(&unstaged, running, &version_file(&unstaged, BEFORE)),
    );
    assert_eq!(
        (seen.state.clone(), seen.reason_token()),
        (ServiceState::Live, Some("version_unconfirmed"))
    );
    assert!(service_state(&seen).healthy);
}

/// Catches the daemon's own unit given to another service, or put in place of the units a
/// package names: a service with no `[units] run` other than `hostkeeper` stays unmonitored, and
/// asks launchd nothing; a hostkeeper package that names its unit (the Linux package's
/// `hostkeeper.service`) is observed through that unit.
#[test]
fn only_hostkeeper_with_no_unit_of_its_own_is_observed_through_the_daemons_job() {
    let rig = mac_host("own-mac-others", RUNS, &[RUNS]);
    rig.running("web", "1.0.0");
    let platform = Platform {
        unit_dir: rig.scratch.path().to_path_buf(),
        ..Platform::macos()
    };
    let asked = FakeLaunchctl::new();
    let adapters = Adapters::macos(
        &platform,
        Arc::new(Unwired),
        FakeLaunchctl::new(),
        asked.clone(),
        RunsFile(version_file(&rig, RUNS)),
        Arc::new(Passes),
    )
    .expect("the macOS set");
    let observer = observer_with(
        adapters.manager,
        adapters.process,
        &rig.packages,
        OWN,
        &OwnService {
            unit: adapters.own_unit,
            binary_dir: binary_dir(&rig.state),
        },
    );
    let web = ServiceName::new("web").expect("a name");
    let seen = observe(&observer, &rig.packages, &OWN, &web).expect("observed");
    assert_eq!(seen.state, ServiceState::Unmonitored);
    assert_eq!(asked.calls(), Vec::<Vec<String>>::new());

    let named = ServiceInput {
        service: HK.to_owned(),
        current: Some(Current {
            run: vec!["dev.hostkeeper.other".to_owned()],
            health_url: None,
        }),
        recorded_version: Some(RUNS.to_owned()),
    };
    observer.observe(&named, STATUS_DEADLINE);
    let other = Label::parse("dev.hostkeeper.other").expect("a label");
    // The fake answers nothing, so the round ends at the first read: of the package's unit,
    // never of the daemon's job.
    assert_eq!(asked.calls(), vec![print_args(&other)]);
}

/// Catches a Linux daemon that cannot confirm the version of its own unit: `hostkeeper.service`
/// starts `bin/current` of the binary directory, which is not under the package's `current`, so
/// its version was never confirmed. The sealed executable of a version of hostkeeper is the
/// binary directory's file of it; one with no seal, or no file there, has none, and without the
/// binary directory hostkeeper's is found through its unit as any service's. The Linux
/// observer, over a fake systemd, confirms the daemon's version through `hostkeeper.service`.
#[test]
fn the_sealed_executable_of_hostkeeper_is_its_file_in_the_binary_directory() {
    let rig = mac_host("own-linux-sealed", RUNS, &[RUNS]);
    let exe =
        SealedExe::new(&rig.packages, &rig.packages, OWN).with_binary_dir(&binary_dir(&rig.state));
    let file = fs::File::open(version_file(&rig, RUNS)).expect("the binary");
    let digest = digest_open_file(&file).expect("its digest");
    assert_eq!(exe.digest(HK, RUNS), Some(digest));
    // Not sealed: no digest, whatever the binary directory holds.
    rig.stage(HK, BEFORE);
    assert_eq!(exe.digest(HK, BEFORE), None);
    // Sealed, and no file of it in the binary directory.
    rig.store.seal(HK, BEFORE, &d());
    assert_eq!(exe.digest(HK, BEFORE), None);
    // Without the binary directory, hostkeeper's version is found through its unit as any
    // other's, and the macOS package has none.
    let plain = SealedExe::new(&rig.packages, &rig.packages, OWN);
    assert_eq!(plain.digest(HK, RUNS), None);

    let observer = observer_with(
        Box::new(rig.manager.clone()),
        Box::new(rig.process.clone()),
        &rig.packages,
        OWN,
        &OwnService {
            unit: MAIN_UNIT.to_owned(),
            binary_dir: binary_dir(&rig.state),
        },
    );
    rig.manager.set(MAIN_UNIT, ActiveState::Active, Some(PID));
    let stat = ExeStat {
        size: 1,
        mtime_secs: 2,
        mtime_nanos: 3,
    };
    rig.process.set(PID, 41, stat, digest);
    let seen = of_hk(&rig, &observer);
    assert_eq!(
        (seen.state.clone(), seen.reason_token()),
        (ServiceState::Live, None)
    );
    assert_eq!(
        service_state(&seen).version.as_deref(),
        Some(RUNS),
        "{seen:?}"
    );
}
