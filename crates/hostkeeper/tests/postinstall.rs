//! The macOS pkg's `postinstall` (`hostkeeper_launchd::postinstall::Postinstall`), run as the
//! installer runs it: an executable file in its own directory, started with the path of the pkg,
//! the install location, the target volume and the root of the volume as arguments, and an empty
//! environment. It runs `launchctl` twice, a boot-out of the daemon's job and the bootstrap of its
//! plist, and nothing else: the binary the pkg installed seeds the binary directory and writes the
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

/// The job the script boots out.
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

    /// The two calls of a load.
    fn load_calls(&self) -> Vec<String> {
        vec![
            format!("bootout {JOB}"),
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
    /// does.
    fn install(&self) -> Output {
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
/// writes anything (a state directory, a config), says anything, or does not boot out and load
/// the job; and one that, run again (the pkg installed again), does not load again.
#[test]
fn the_postinstall_loads_the_job_writes_nothing_and_a_second_run_loads_again() {
    let mac = Mac::new("postinstall-first");
    let before = mac.entries();
    let out = mac.install();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stderr(&out), "");
    assert_eq!(out.stdout, b"");
    assert_eq!(mac.calls(), mac.load_calls());
    let mut written = before.clone();
    written.push(PathBuf::from("scripts"));
    written.sort();
    assert_eq!(mac.entries(), written);
    assert!(!mac.paths.plist.exists());

    let again = mac.install();
    assert!(again.status.success(), "{again:?}");
    assert_eq!(stderr(&again), "");
    let twice: Vec<String> = mac.load_calls().into_iter().cycle().take(4).collect();
    assert_eq!(mac.calls(), twice);
    assert_eq!(mac.entries(), written);
}

/// Catches a postinstall that stops at the boot-out's failure (which is how a first install
/// looks, nothing being loaded) or shows its message: the bootstrap still runs, nothing is said
/// and the install succeeds.
#[test]
fn a_failing_bootout_does_not_stop_the_bootstrap() {
    let mac = Mac::new("postinstall-bootout-fails");
    mac.fail("bootout", 3);
    let out = mac.install();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stderr(&out), "");
    assert_eq!(mac.calls(), mac.load_calls());
}

/// Catches a postinstall that reports success when `launchctl bootstrap` fails: the install fails
/// with its status and exactly the message it wrote.
#[test]
fn a_failing_bootstrap_fails_the_postinstall_with_its_status_and_message() {
    let mac = Mac::new("postinstall-bootstrap-fails");
    mac.fail("bootstrap", 5);
    let out = mac.install();
    assert_eq!(out.status.code(), Some(5), "{out:?}");
    assert_eq!(stderr(&out), "launchctl fixture: bootstrap failed\n");
    assert_eq!(mac.calls(), mac.load_calls());
}
