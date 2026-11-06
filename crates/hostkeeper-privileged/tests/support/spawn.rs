//! The child processes of the first-boot tests and the wait on what they do.
//!
//! A [`Running`] is one program started by a test, its standard output and error written to files
//! of the test's scratch directory; dropping it kills the program and reaps it, so a test that
//! fails leaves nothing running. [`poll`] looks at a state a fixed number of times, with a pause
//! between two looks, and fails with the last state it saw; nothing asserts how long a wait took.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The pause between two looks of [`poll`].
const PAUSE: Duration = Duration::from_millis(250);

/// How many looks a wait takes at most before it fails: enough for the coordinator's quiet start
/// (30 seconds after each start, in which it orders nothing), the enroller's pause between two
/// steps (5 seconds) and a package's steps, many times over. A bound against a hang, which no
/// test reads as a time.
pub const LOOKS: u32 = 1200;

/// A program a test started, killed when dropped.
pub struct Running {
    name: String,
    child: Child,
    out: PathBuf,
    err: PathBuf,
}

impl Running {
    /// Starts `program` with `args` and no input; what it prints goes to `<dir>/<name>.out` and
    /// `<dir>/<name>.err`, which a restart of the same name appends to.
    pub fn start(name: &str, dir: &Path, program: &str, args: &[String]) -> Running {
        let out = dir.join(format!("{name}.out"));
        let err = dir.join(format!("{name}.err"));
        let append = |path: &Path| {
            File::options()
                .create(true)
                .append(true)
                .open(path)
                .unwrap_or_else(|e| panic!("open {}: {e}", path.display()))
        };
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(append(&out))
            .stderr(append(&err))
            .spawn()
            .unwrap_or_else(|e| panic!("cannot start {name} ({program}): {e}"));
        Running {
            name: name.to_owned(),
            child,
            out,
            err,
        }
    }

    /// What the program has written to its standard output so far (and before it, every earlier
    /// run of the same name).
    pub fn stdout(&self) -> String {
        std::fs::read_to_string(&self.out).unwrap_or_default()
    }

    /// What the program has written to its standard error so far.
    pub fn stderr(&self) -> String {
        std::fs::read_to_string(&self.err).unwrap_or_default()
    }

    /// Fails the test at once if the program has ended: a wait on what it does would never end.
    pub fn assert_running(&mut self) {
        if let Some(status) = self.child.try_wait().expect("look at the child") {
            panic!(
                "{} ended with {status}; its standard error:\n{}",
                self.name,
                self.stderr()
            );
        }
    }

    /// Kills the program (`SIGKILL`: it gets no chance to tidy up, as in a crash or a power cut)
    /// and reaps it.
    pub fn kill(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        // An error is a program that has already ended; `wait` reaps it either way.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Looks with `look` until it gives a value, at most [`LOOKS`] times with a pause between two
/// looks. `look` answers `Err` with what it saw instead; the test fails with the last of those,
/// named by `what`, when no look gives a value.
pub fn poll<T>(what: &str, mut look: impl FnMut() -> Result<T, String>) -> T {
    let mut seen = String::from("nothing yet");
    for _ in 0..LOOKS {
        match look() {
            Ok(value) => return value,
            Err(now) => seen = now,
        }
        std::thread::sleep(PAUSE);
    }
    panic!("{what}: not reached in {LOOKS} looks; the last look saw: {seen}");
}
