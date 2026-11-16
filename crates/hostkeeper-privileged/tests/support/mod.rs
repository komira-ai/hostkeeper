//! Shared by the privileged tests: the check that they run as root under systemd, a bench (a
//! scratch directory and a rig whose units are torn down when it is dropped, failed or not), the
//! host's programs they run, and a clock that moves only when told to; and, in [`self_update`],
//! the host of the self-update tests.
//!
//! A wait of this module ([`wait_until`], [`wait_for_file`]) polls a state systemd or the fixture
//! reaches (a file written, a unit active) and sleeps between looks; it looks at most [`LOOKS`]
//! times and then fails the test naming the wait, so that a state that never comes ends the test
//! with a failure (and the workflow's journal steps run) instead of a job cancelled at its
//! `timeout-minutes`; the bound counts looks, and nothing asserts how long a wait took.
//!
//! The first-boot tests' harness is in submodules: [`spawn`] (a child process killed when dropped,
//! and [`spawn::poll`], a wait of a bounded number of looks that fails with the last state it
//! saw, so that a failure names what it waited for; the bound counts looks, and nothing asserts
//! how long a wait took), [`pair`] (the coordinator and the daemon as programs, and the host paths the daemon fixes),
//! [`channel`] (an HTTPS server of the test's on loopback or a LAN) and [`aptrepo`] (the fixture
//! packages: `.deb`s in a signed apt repository, and the entry of an order for one).
#![allow(dead_code, reason = "each test file uses part of the shared support")]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hostkeeper_privileged::rig::{self, service_unit};
use hostkeeper_privileged::{Rig, SYSTEMD_RUNTIME_DIR, check_host};
use hostkeeper_steps::systemd::{SYSTEMCTL, SYSTEMD_RUN};
use hostkeeper_steps::{BootId, BootIdError, Clock};
use hostkeeper_testkit::Scratch;

#[allow(
    clippy::duplicate_mod,
    reason = "the fixture packages are shared by the two harnesses, which a test file may use both of"
)]
#[path = "../shared/aptrepo.rs"]
pub mod aptrepo;
pub mod channel;
#[allow(
    clippy::duplicate_mod,
    reason = "the operator's calls are shared by the two harnesses, which a test file may use both of"
)]
#[path = "../shared/operator_api.rs"]
pub mod operator_api;
pub mod pair;
pub mod self_update;
pub mod spawn;

/// The fixture program, built by cargo with these tests.
pub const FIXTURE: &str = env!("CARGO_BIN_EXE_hostkeeper-fixture-hook");
/// Where `journalctl` is.
pub const JOURNALCTL: &str = "/usr/bin/journalctl";
/// How long a wait sleeps between two looks.
const POLL: Duration = Duration::from_millis(50);

/// One test's scratch directory and rig. Dropping it stops, unlinks and resets every unit the
/// rig named, then removes the directory.
pub struct Bench {
    pub rig: Rig,
    // Dropped after `rig`, and after `Drop::drop` has torn the units down.
    _scratch: Scratch,
}

impl Bench {
    /// Refuses to go on unless the test runs as root with systemd as the service manager, then
    /// makes the bench of `test`.
    pub fn new(test: &str) -> Bench {
        let root = rustix::process::geteuid().is_root();
        if let Err(e) = check_host(root, Path::new(SYSTEMD_RUNTIME_DIR)) {
            panic!("{e}");
        }
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let scratch = Scratch::under(base, test).expect("create the scratch directory");
        let rig = Rig::new(scratch.path(), Path::new(FIXTURE), test, std::process::id())
            .expect("make the rig");
        Bench {
            rig,
            _scratch: scratch,
        }
    }
}

impl Bench {
    /// The test's scratch directory.
    pub fn scratch(&self) -> &Path {
        self._scratch.path()
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        for args in self.rig.teardown() {
            // A unit with nothing to undo fails some of these (a transient unit has no link to
            // remove); the next one goes on.
            let _ = Command::new(SYSTEMCTL)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Runs `program` with `args`, no input, and gives what it printed and how it ended.
pub fn run<S: AsRef<OsStr>>(program: &str, args: &[S]) -> Output {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("cannot run {program}: {e}"))
}

/// Runs `program` with `args` and fails the test unless it exits `0`.
pub fn run_ok<S: AsRef<OsStr>>(program: &str, args: &[S]) -> String {
    let out = run(program, args);
    assert!(
        out.status.success(),
        "{program} {:?} ended with {}: {}",
        args.iter()
            .map(|a| a.as_ref().to_owned())
            .collect::<Vec<OsString>>(),
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `properties` of `unit`, as `systemctl show` prints them.
pub fn show(unit: &str, properties: &[&str]) -> BTreeMap<String, String> {
    rig::parse_show(&run_ok(SYSTEMCTL, &rig::show_command(unit, properties)))
}

/// One property of `unit`.
pub fn property(unit: &str, name: &str) -> String {
    show(unit, &[name])
        .remove(name)
        .unwrap_or_else(|| panic!("systemctl show printed no {name} for {unit}"))
}

/// Starts `unit` as a transient service of the fixture's `serve` with no property, and returns
/// once it has started.
pub fn start_serve(rig: &Rig, unit: &str) {
    run_ok(SYSTEMD_RUN, &rig.serve_command(unit));
}

/// How many looks a wait of this module makes before it fails the test.
pub const LOOKS: u32 = 6000;

/// Waits until `done` holds, looking every [`POLL`], at most [`LOOKS`] times.
///
/// # Panics
/// `done` never held in [`LOOKS`] looks.
pub fn wait_until(done: impl FnMut() -> bool) {
    wait_looks(LOOKS, POLL, done);
}

/// Looks with `done` until it holds, at most `looks` times with `pause` between two looks.
///
/// # Panics
/// `done` never held in `looks` looks.
pub fn wait_looks(looks: u32, pause: Duration, mut done: impl FnMut() -> bool) {
    for _ in 0..looks {
        if done() {
            return;
        }
        std::thread::sleep(pause);
    }
    panic!("a wait for a state of systemd or the fixture: not reached in {looks} looks");
}

/// Waits until the file at `path` exists: the fixture writes its pid file once it runs. When
/// `ended` says the step's run has returned first, the file will never come: fails at once.
pub fn wait_for_file(path: &Path, ended: impl Fn() -> bool) {
    wait_until(|| {
        let written = path.exists();
        assert!(
            written || !ended(),
            "the step ended before it wrote {}",
            path.display()
        );
        written
    });
}

/// Stops `unit` when dropped: a test that fails while a step runs on a thread of a
/// `std::thread::scope` ends the step, so that the scope can join it and the failure is reported
/// at once rather than at the step's bound.
pub struct StopOnDrop<'a>(pub &'a str);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        let unit = service_unit(self.0);
        let _ = Command::new(SYSTEMCTL)
            .args(["stop", "--system", unit.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Whether the process `pid` has ended: it has no `/proc` entry, or it is a zombie (ended, not
/// yet reaped).
pub fn ended(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
    }
}

/// What the journal holds of `unit`, every field of every entry (`-o verbose`), after the
/// journal has stored everything logged so far (`journalctl --sync`).
pub fn journal(unit: &str) -> String {
    run_ok(JOURNALCTL, &["--sync"]);
    let unit = service_unit(unit);
    run_ok(
        JOURNALCTL,
        &["--no-pager", "--output=verbose", "--unit", unit.as_str()],
    )
}

/// A clock that stays where the test puts it.
pub struct FakeClock(AtomicU64);

impl FakeClock {
    pub fn at(secs: u64) -> FakeClock {
        FakeClock(AtomicU64::new(secs))
    }

    pub fn set(&self, secs: u64) {
        self.0.store(secs, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn mono_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the fake clock", "privileged-test-boot")
    }

    fn sleep_secs(&self, _secs: u64) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wait whose state never comes fails the test, naming the bound, after exactly that many
    /// looks, instead of looking for good.
    #[test]
    #[should_panic(expected = "not reached in 3 looks")]
    fn a_wait_for_a_state_that_never_comes_fails_after_its_looks() {
        let mut looked = 0;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            wait_looks(3, Duration::ZERO, || {
                looked += 1;
                false
            });
        }));
        assert_eq!(looked, 3);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// A wait returns at the look the state holds at and looks no more.
    #[test]
    fn a_wait_returns_at_the_look_the_state_holds_at() {
        let mut looked = 0;
        wait_looks(10, Duration::ZERO, || {
            looked += 1;
            looked == 4
        });
        assert_eq!(looked, 4);
    }
}
