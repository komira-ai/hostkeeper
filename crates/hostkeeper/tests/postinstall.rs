//! The macOS pkg's `postinstall` (`hostkeeper_launchd::postinstall::Postinstall`), run as the
//! installer runs it: an executable file in its own directory, started with the path of the pkg,
//! the install location, the target volume and the root of the volume as arguments, and an empty
//! environment. It runs `launchctl` to ask whether the daemon's job is loaded (`print`) and, only
//! when it is not, to bootstrap its plist, and nothing else: it never boots the job out, as an
//! `installer` the daemon itself runs must not stop the daemon, and the binary the pkg installed seeds the binary directory and writes the
//! config at each start of the job (docs/design/two-programs.md, section 4.1), not here. Every
//! path is in a scratch directory and holds a space, so the script's quoting is exercised by the
//! real shell, and `launchctl` is the test program `hostkeeper-launchctl-fixture`, which logs its
//! calls. On a Mac the pkg names `/bin/launchctl` and the plist of `Postinstall::pkg`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hostkeeper_launchd::postinstall::Postinstall;
use hostkeeper_testkit::Scratch;

/// Held by each write and run of a script ([`Mac::install`]), the only fork of these tests.
static EXECS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The job the script asks about.
const JOB: &str = "system/dev.hostkeeper.hostkeeper";

/// A Mac in a scratch directory: the `launchctl` fixture in a directory with a space in its name,
/// and the path of the plist, which the script only names.
struct Mac {
    scratch: Scratch,
    paths: Postinstall,
}

impl Mac {
    fn new(name: &str) -> Mac {
        let scratch =
            Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch dir");
        let root = scratch.path();
        let bin_dir = root.join("usr local");
        fs::create_dir(&bin_dir).expect("the launchctl's directory");
        let launchctl = bin_dir.join("launchctl");
        fs::copy(
            env!("CARGO_BIN_EXE_hostkeeper-launchctl-fixture"),
            &launchctl,
        )
        .expect("install the fake launchctl");
        let paths = Postinstall {
            launchctl,
            plist: root
                .join("Launch Daemons")
                .join("dev.hostkeeper.hostkeeper.plist"),
        };
        Mac { scratch, paths }
    }

    /// The lines the fake `launchctl` logged, none when it never ran.
    fn calls(&self) -> Vec<String> {
        let log = self.paths.launchctl.with_file_name("launchctl.calls");
        fs::read_to_string(log)
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Makes the fake `launchctl` fail its `verb` with `code`.
    fn fail(&self, verb: &str, code: i32) {
        let file = self
            .paths
            .launchctl
            .with_file_name(format!("launchctl.{verb}.exit"));
        fs::write(file, code.to_string()).expect("the exit file");
    }

    /// Makes launchd have the job loaded, as a daemon that is running has it.
    fn load_job(&self) {
        fs::write(self.paths.launchctl.with_file_name("launchctl.loaded"), "")
            .expect("the loaded marker");
    }

    /// Whether the fake launchd has the job loaded.
    fn is_loaded(&self) -> bool {
        self.paths
            .launchctl
            .with_file_name("launchctl.loaded")
            .exists()
    }

    /// The call that asks whether the job is loaded.
    fn print_call() -> String {
        format!("print {JOB}")
    }

    /// The calls of a load: the question, then the bootstrap.
    fn load_calls(&self) -> Vec<String> {
        vec![
            Self::print_call(),
            format!("bootstrap system {}", self.paths.plist.display()),
        ]
    }

    /// The names in the scratch directory, sorted.
    fn entries(&self) -> Vec<PathBuf> {
        let mut names: Vec<PathBuf> = fs::read_dir(self.scratch.path())
            .expect("list the scratch directory")
            .map(|entry| PathBuf::from(entry.expect("an entry").file_name()))
            .collect();
        names.sort();
        names
    }

    /// Writes the script into a directory of its own, mode 0755, and runs it as the installer
    /// does. The write and the run hold [`EXECS`]: a test of this binary that forks while
    /// another holds its script open for writing hands the child that descriptor until the
    /// child's own exec, and an exec of the script in that moment fails `ETXTBSY`.
    fn install(&self) -> Output {
        let _one = EXECS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let scripts = self.scratch.path().join("scripts");
        fs::create_dir_all(&scripts).expect("the scripts directory");
        let script = scripts.join("postinstall");
        fs::write(&script, self.paths.script()).expect("the script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("its mode");
        let pkg = self.scratch.path().join("hostkeeper.pkg");
        Command::new(&script)
            .args([pkg.as_os_str(), "/".as_ref(), "/".as_ref(), "/".as_ref()])
            .current_dir(&scripts)
            .env_clear()
            .output()
            .expect("run the postinstall")
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Catches a postinstall that does not run, exits other than 0 on a first install with no
/// config and no managed preferences (the daemon's start, not the install, refuses that host),
/// writes anything (a state directory, a config), says anything, or does not ask whether the job
/// is loaded and then load it; it also holds that the job is loaded afterwards.
#[test]
fn the_postinstall_loads_a_job_that_is_not_loaded_and_writes_nothing() {
    let mac = Mac::new("postinstall-first");
    let before = mac.entries();
    let out = mac.install();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stderr(&out), "");
    assert_eq!(out.stdout, b"");
    assert_eq!(mac.calls(), mac.load_calls());
    assert!(mac.is_loaded());
    let mut written = before.clone();
    written.push(PathBuf::from("scripts"));
    written.sort();
    assert_eq!(mac.entries(), written);
    assert!(!mac.paths.plist.exists());
}

/// Catches a postinstall that touches a job launchd has loaded (a boot-out of the running daemon
/// by an installer the daemon itself runs, or a bootstrap over it): it asks once, exits 0, says
/// nothing (the answer's output is discarded) and runs no other `launchctl` verb.
#[test]
fn the_postinstall_leaves_a_loaded_job_alone() {
    let mac = Mac::new("postinstall-loaded");
    mac.load_job();
    let out = mac.install();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stderr(&out), "");
    assert_eq!(out.stdout, b"");
    assert_eq!(mac.calls(), vec![Mac::print_call()]);
    assert!(mac.is_loaded());
}

/// Catches a second run (the pkg installed again) that does more than the first left to do: the
/// first loads the job, the second finds it loaded and asks only; no boot-out in either.
#[test]
fn a_second_run_on_the_loaded_job_does_nothing_more() {
    let mac = Mac::new("postinstall-twice");
    assert!(mac.install().status.success());
    assert_eq!(mac.calls(), mac.load_calls());
    let again = mac.install();
    assert!(again.status.success(), "{again:?}");
    assert_eq!(stderr(&again), "");
    let mut want = mac.load_calls();
    want.push(Mac::print_call());
    assert_eq!(mac.calls(), want);
    assert!(mac.calls().iter().all(|call| !call.starts_with("bootout")));
}

/// Catches a postinstall that reads a failing `print` as anything but "not loaded", or shows its
/// complaint: whatever status `print` ends with (here 3, not launchd's 113), the job is
/// bootstrapped and nothing is said.
#[test]
fn a_print_that_fails_for_any_reason_loads_the_job_quietly() {
    let mac = Mac::new("postinstall-print-fails");
    mac.fail("print", 3);
    let out = mac.install();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stderr(&out), "");
    assert_eq!(mac.calls(), mac.load_calls());
}

/// Catches a postinstall that reports success when `launchctl bootstrap` fails: the install fails
/// with its status and exactly the message it wrote, after the one question and the one try.
#[test]
fn a_failing_bootstrap_fails_the_postinstall_with_its_status_and_message() {
    let mac = Mac::new("postinstall-bootstrap-fails");
    mac.fail("bootstrap", 5);
    let out = mac.install();
    assert_eq!(out.status.code(), Some(5), "{out:?}");
    assert_eq!(stderr(&out), "launchctl fixture: bootstrap failed\n");
    assert_eq!(mac.calls(), mac.load_calls());
    assert!(!mac.is_loaded());
}
