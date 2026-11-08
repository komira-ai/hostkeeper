//! The built-in steps of hostkeeper's own package (docs/design/two-programs.md, section 4.4):
//! the step runner, given an order whose steps are built in, writes the record the hook's no-op
//! command (`bin/hostkeeper update-hook`, [`hostkeeper_daemon::update_hook`]) gives when it is
//! run for each step, byte for byte, and runs nothing.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hostkeeper_daemon::update_hook;
use hostkeeper_hook::{Params, Step, Verdict};
use hostkeeper_steps::record::RECORD_FILE;
use hostkeeper_steps::{
    BootId, BootIdError, Bounds, Clock, Finished, LaunchError, RecordStore, Staged, StepCall,
    StepExit, StepLauncher, StepRunner, UpdateOrder,
};
use hostkeeper_testkit::Scratch;

/// The hook of the package, as the host's launcher runs it: the program with the step's own
/// arguments (the binary answers `hostkeeper <step> ...` for `update-hook <step> ...`), here the
/// hook's own code on exactly those arguments ([`update_hook::run`]), which prints nothing: its
/// exit code and no last line. Every step it ran is kept.
#[derive(Default)]
struct NoOpCommand {
    ran: Mutex<Vec<(Step, PathBuf)>>,
}

impl StepLauncher for NoOpCommand {
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        self.ran
            .lock()
            .expect("the runs")
            .push((call.invocation.step, call.executable.to_path_buf()));
        let outcome = update_hook::run(call.invocation.to_args())
            .map_err(|error| LaunchError::NoStart(error.to_string()))?;
        Ok(StepExit {
            code: Some(outcome.code()),
            last_line: None,
        })
    }

    fn stop(&self, _unit: &str) -> Result<(), LaunchError> {
        Ok(())
    }
}

/// A launcher that runs nothing and keeps every step it was asked to run.
#[derive(Default)]
struct Nothing {
    asked: Mutex<Vec<Step>>,
}

impl StepLauncher for Nothing {
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        self.asked
            .lock()
            .expect("the asks")
            .push(call.invocation.step);
        Err(LaunchError::NoStart("nothing runs here".to_owned()))
    }

    fn stop(&self, _unit: &str) -> Result<(), LaunchError> {
        Ok(())
    }
}

/// A clock that does not move: second 1000 of the boot `boot-1`.
struct Frozen;

impl Clock for Frozen {
    fn mono_secs(&self) -> u64 {
        1000
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the test", "boot-1")
    }

    fn sleep_secs(&self, _secs: u64) {}
}

/// Every file under `dir`, by its path, with its bytes.
fn files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in fs::read_dir(&at).expect("list") {
            let path = entry.expect("an entry").path();
            match path.is_dir() {
                true => stack.push(path),
                false => {
                    let bytes = fs::read(&path).expect("read");
                    out.insert(path, bytes);
                }
            }
        }
    }
    out
}

/// A release of hostkeeper, `bin/hostkeeper` in it, sealed, under `scratch`.
fn release(scratch: &Path, version: &str) -> Staged {
    let dir = scratch.join("packages").join(version);
    fs::create_dir_all(dir.join("bin")).expect("the release");
    fs::write(dir.join("bin/hostkeeper"), b"the release's binary").expect("its binary");
    fs::set_permissions(
        dir.join("bin/hostkeeper"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("its mode");
    fs::write(dir.join("manifest.toml"), b"name = \"hostkeeper\"\n").expect("a manifest");
    Staged::seal(&dir).expect("sealed")
}

/// The update of hostkeeper from 0.1.60 to 0.1.70 with the bounds of its package's hook, its
/// steps built in or not.
fn order(scratch: &Path, builtin_steps: bool) -> UpdateOrder {
    UpdateOrder {
        service: "hostkeeper".to_owned(),
        target_version: "0.1.70".to_owned(),
        previous_version: Some("0.1.60".to_owned()),
        artifact_digest: "ab".repeat(32),
        staged: release(scratch, "0.1.70"),
        previous: Some(release(scratch, "0.1.60")),
        hook_binary: PathBuf::from("bin/hostkeeper"),
        bounds: Bounds {
            prepare_secs: 300,
            cutover_secs: 300,
            verify_secs: 300,
            rollback_secs: 300,
            cleanup_secs: 300,
            verify_deadline_secs: 600,
            promotion_ceiling_secs: u64::MAX,
            cleanup_retries: 3,
        },
        declared_params: Params::default(),
        min_updater_protocol: None,
        order_try: Some(4),
        builtin_steps,
    }
}

/// Runs `order` on the state directory `state` through `launcher`, the success held back as
/// for hostkeeper's own update, on a clock that does not move.
fn run(state: &Path, launcher: Arc<dyn StepLauncher>, order: &UpdateOrder) -> Finished {
    let runner =
        StepRunner::new(launcher, Arc::new(Frozen), RecordStore::new(state)).with_held_success();
    runner.run(order).expect("the update runs to its verdict")
}

/// Catches built-in steps whose record differs from the one the hook's command gives (another
/// step, code, line, stamp or field, or a file more or less in the state directory), and
/// built-in steps that still launch a command: the same order, once with the no-op command run
/// for each of its four steps and once built in, leaves the same files byte for byte and the same
/// verdict, and the built-in run asks the launcher for nothing.
#[test]
fn the_builtin_steps_write_the_record_the_no_op_command_writes() {
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "builtin-steps")
        .expect("a scratch dir");
    fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o755)).expect("its mode");
    let state = scratch.path().join("state").join("hostkeeper");
    fs::create_dir_all(&state).expect("the state of hostkeeper");
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).expect("its mode");

    let command = Arc::new(NoOpCommand::default());
    let by_command = run(&state, command.clone(), &order(scratch.path(), false));
    let release = scratch.path().join("packages/0.1.70/bin/hostkeeper");
    assert_eq!(
        *command.ran.lock().expect("the runs"),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
            .map(|step| (step, release.clone()))
    );
    let written = files(&state);
    assert!(
        written.contains_key(&state.join(RECORD_FILE)),
        "{written:?}"
    );
    for path in written.keys() {
        fs::remove_file(path).expect("start again from nothing");
    }

    let nothing = Arc::new(Nothing::default());
    let built_in = run(&state, nothing.clone(), &order(scratch.path(), true));
    assert!(nothing.asked.lock().expect("the asks").is_empty());
    assert_eq!(built_in, by_command);
    assert_eq!(
        built_in.verdict,
        Verdict::Succeeded {
            promotion_pending: false
        }
    );
    assert_eq!(files(&state), written);
}
