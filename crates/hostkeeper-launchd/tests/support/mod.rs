//! Shared by the launcher tests: a pace that moves only when the launcher waits, a hook file, a
//! step call, and `SimLaunchd`, a stand-in for launchd behind the `Launchctl` port that loads the
//! plist it is given, runs the scripted hook on the pace's clock, and answers `print`,
//! `bootstrap` and `bootout` as launchd does.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use hostkeeper_hook::{Invocation, Step};
use hostkeeper_launchd::Pace;
use hostkeeper_launchd::launchctl::{Launchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_steps::{Bound, StepCall};
use hostkeeper_testkit::Scratch;

/// A scratch directory under the target directory.
pub fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// An executable file `hook` in `dir`.
pub fn hook_file(dir: &Path) -> PathBuf {
    let hook = dir.join("hook");
    fs::write(&hook, "#!hook\n").expect("write the hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("make it executable");
    hook
}

/// The params file every call names.
pub const PARAMS: &str = "/var/lib/updater/state/web/params.toml";

/// A call of `step` of `web` through `executable` in `unit`.
pub fn call<'a>(executable: &'a Path, unit: &'a str, step: Step, bound: Bound) -> StepCall<'a> {
    StepCall {
        service: "web",
        hook_args: &[],
        invocation: Invocation {
            step,
            artifact: None,
            params: Some(PathBuf::from(PARAMS)),
        },
        executable,
        unit,
        bound,
    }
}

/// A bound of `n` seconds.
pub fn secs(n: u64) -> Bound {
    Bound::Secs(std::num::NonZeroU64::new(n).expect("a bound is not zero"))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("a test lock")
}

#[derive(Default)]
struct PaceState {
    now: Duration,
    sleeps: Vec<Duration>,
}

/// A clock that moves only when the launcher sleeps on it. Clones share it.
#[derive(Clone, Default)]
pub struct FakePace(Arc<Mutex<PaceState>>);

impl FakePace {
    /// The waits asked for, in order.
    pub fn sleeps(&self) -> Vec<Duration> {
        lock(&self.0).sleeps.clone()
    }

    pub fn now(&self) -> Duration {
        lock(&self.0).now
    }
}

impl Pace for FakePace {
    fn elapsed(&self) -> Duration {
        lock(&self.0).now
    }

    fn sleep(&self, wait: Duration) {
        let mut state = lock(&self.0);
        state.now += wait;
        state.sleeps.push(wait);
    }
}

/// What a hook does once started: runs for `secs`, writes `stdout` and `stderr`, and ends with
/// `code` (`None`: killed by a signal).
#[derive(Clone, Debug)]
pub struct Hook {
    pub secs: u64,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Its standard output file is gone once it has run.
    pub lose_stdout: bool,
}

impl Hook {
    /// A hook that runs `secs` and exits `code`, printing `line` and a line feed.
    pub fn exits(secs: u64, code: i32, line: &str) -> Hook {
        Hook {
            secs,
            code: Some(code),
            stdout: format!("{line}\n").into_bytes(),
            stderr: Vec::new(),
            lose_stdout: false,
        }
    }
}

/// One loaded job.
struct Job {
    started: Duration,
    hook: Hook,
}

#[derive(Default)]
struct SimState {
    scripted: BTreeMap<String, VecDeque<Hook>>,
    loaded: BTreeMap<String, Job>,
    calls: Vec<Vec<String>>,
    faults: BTreeMap<Vec<String>, VecDeque<Result<LaunchctlOutput, LaunchctlError>>>,
    plists: Vec<plist::Dictionary>,
    /// Per label, how long each job had run when it was booted out while running.
    stopped_after: Vec<(String, Duration)>,
}

/// launchd behind the `Launchctl` port, on the clock of a [`FakePace`]. Clones share it.
#[derive(Clone)]
pub struct SimLaunchd {
    pace: FakePace,
    state: Arc<Mutex<SimState>>,
}

fn text(out: &str) -> LaunchctlOutput {
    LaunchctlOutput::success(out)
}

impl SimLaunchd {
    pub fn new(pace: &FakePace) -> SimLaunchd {
        SimLaunchd {
            pace: pace.clone(),
            state: Arc::default(),
        }
    }

    /// The next job loaded under `label` runs `hook`.
    pub fn script(&self, label: &str, hook: Hook) {
        lock(&self.state)
            .scripted
            .entry(label.to_owned())
            .or_default()
            .push_back(hook);
    }

    /// A job `label` is loaded and runs until it is booted out.
    pub fn running(&self, label: &str) {
        let started = self.pace.now();
        let hook = Hook {
            secs: u64::MAX,
            code: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
            lose_stdout: false,
        };
        lock(&self.state)
            .loaded
            .insert(label.to_owned(), Job { started, hook });
    }

    /// The next call with exactly `args` answers `answer` instead of launchd.
    pub fn fail_next(&self, args: &[&str], answer: Result<LaunchctlOutput, LaunchctlError>) {
        let args = args.iter().map(|a| (*a).to_owned()).collect();
        lock(&self.state)
            .faults
            .entry(args)
            .or_default()
            .push_back(answer);
    }

    pub fn calls(&self) -> Vec<Vec<String>> {
        lock(&self.state).calls.clone()
    }

    /// Every plist bootstrapped, decoded.
    pub fn plists(&self) -> Vec<plist::Dictionary> {
        lock(&self.state).plists.clone()
    }

    /// For each job booted out while it ran: its label and how long it had run.
    pub fn stopped_after(&self) -> Vec<(String, Duration)> {
        lock(&self.state).stopped_after.clone()
    }

    pub fn is_loaded(&self, label: &str) -> bool {
        lock(&self.state).loaded.contains_key(label)
    }

    fn print(&self, state: &SimState, label: &str) -> LaunchctlOutput {
        let Some(job) = state.loaded.get(label) else {
            return LaunchctlOutput::exit(
                113,
                &format!("Could not find service \"{label}\" in domain for system"),
            );
        };
        let ran = self.pace.now().saturating_sub(job.started);
        let over = ran >= Duration::from_secs(job.hook.secs);
        let body = match (over, job.hook.code) {
            (false, _) => {
                "\tstate = running\n\tpid = 4242\n\tlast exit code = (never exited)\n".to_owned()
            }
            (true, Some(code)) => format!("\tstate = not running\n\tlast exit code = {code}\n"),
            (true, None) => {
                "\tstate = not running\n\tlast terminating signal = Killed: 9\n".to_owned()
            }
        };
        text(&format!(
            "system/{label} = {{\n\tactive count = 0\n{body}}}\n"
        ))
    }

    fn bootstrap(&self, state: &mut SimState, path: &str) -> LaunchctlOutput {
        let dict = plist::Value::from_file(path)
            .expect("the launcher writes a plist that decodes")
            .into_dictionary()
            .expect("a dictionary");
        let string = |key: &str| {
            dict.get(key)
                .and_then(plist::Value::as_string)
                .unwrap_or_else(|| panic!("the plist has the string {key}"))
                .to_owned()
        };
        let label = string("Label");
        state.plists.push(dict.clone());
        if state.loaded.contains_key(&label) {
            return LaunchctlOutput::exit(5, "Bootstrap failed: 5: Input/output error");
        }
        let hook = state
            .scripted
            .get_mut(&label)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| panic!("a hook is scripted for {label}"));
        for (key, bytes) in [
            ("StandardOutPath", &hook.stdout),
            ("StandardErrorPath", &hook.stderr),
        ] {
            // launchd appends to the files it is given.
            let mut file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(string(key))
                .expect("open an output file");
            file.write_all(bytes).expect("write the output");
        }
        if hook.lose_stdout {
            fs::remove_file(string("StandardOutPath")).expect("remove the output");
        }
        state.loaded.insert(
            label,
            Job {
                started: self.pace.now(),
                hook,
            },
        );
        text("")
    }

    fn bootout(&self, state: &mut SimState, label: &str) -> LaunchctlOutput {
        let Some(job) = state.loaded.remove(label) else {
            return LaunchctlOutput::exit(3, "Boot-out failed: 3: No such process");
        };
        let ran = self.pace.now().saturating_sub(job.started);
        if ran < Duration::from_secs(job.hook.secs) {
            state.stopped_after.push((label.to_owned(), ran));
        }
        text("")
    }
}

impl Launchctl for SimLaunchd {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        let mut state = lock(&self.state);
        state.calls.push(args.to_vec());
        if let Some(answer) = state.faults.get_mut(args).and_then(VecDeque::pop_front) {
            return answer;
        }
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(match words.as_slice() {
            ["print", target] => self.print(&state, target.trim_start_matches("system/")),
            ["bootstrap", "system", path] => self.bootstrap(&mut state, path),
            ["bootout", target] => self.bootout(&mut state, target.trim_start_matches("system/")),
            other => panic!("launchd has no command {other:?}"),
        })
    }
}
