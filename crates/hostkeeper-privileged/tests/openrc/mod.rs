//! The Alpine host of one test of `tests/openrc_device.rs`: a container of the image
//! `crates/hostkeeper-privileged/openrc/Dockerfile` (Alpine 3.22, OpenRC its init), which
//! privileged.yml's job `openrc-device` builds before the tests as [`IMAGE`], booted for the test
//! and removed after it.
//!
//! **The tree.** The test's scratch directory is the container's `/hk` ([`HK_ROOT`]): what the
//! test puts there the container sees, and what the daemon writes there (its step record, the
//! fixture's heartbeats) the test reads from the host. Every directory and file in it is root's,
//! made by the test (which runs as root), so the release trees under it pass the daemon's trust
//! checks inside the container, whose `/` is root's.
//!
//! **The programs.** What runs in the container is built for musl, static, as hostkeeper's Alpine
//! package carries the daemon: the job's step `cargo build --target x86_64-unknown-linux-musl`
//! builds `hostkeeper` into the musl directory beside the test's own target directory
//! ([`musl_program`]); a program missing there fails the test by name. The Alpine package is built from the committed
//! `packaging/alpine/` by the pinned nfpm the job installed ([`nfpm`]).
//!
//! **Waits.** Every wait reads a state a bounded number of times ([`wait`]) and fails naming
//! what it waited for; nothing asserts how long anything took.
//!
//! Dropping the host prints, when the test is failing, the daemon's log and OpenRC's view of the
//! service, then removes the container.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use hostkeeper_testkit::Scratch;

/// The image of the tests' Alpine hosts.
pub const IMAGE: &str = "hostkeeper-openrc-device";
/// The target the container's programs are built for.
pub const MUSL: &str = "x86_64-unknown-linux-musl";
/// Where the test's tree is in the container.
pub const HK_ROOT: &str = "/hk";
/// The service of the daemon under OpenRC, its init script `/etc/init.d/hostkeeper`.
pub const SERVICE: &str = "hostkeeper";
/// The daemon's log, as the init script sends its output there.
pub const LOG: &str = "/var/log/hostkeeper.log";
/// The readiness notice of the init script's daemon.
pub const READY: &str = "/run/hostkeeper/ready";
/// Where supervise-daemon keeps the pid of the daemon it runs.
pub const CHILD_PID: &str = "/run/openrc/options/hostkeeper/child_pid";
/// How many times a wait reads its state, and the pause between two reads.
const READS: u32 = 1500;
const PAUSE: Duration = Duration::from_millis(200);

/// Reads `done` until it holds, [`READS`] times at most; fails naming `what` when it never did.
pub fn wait(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..READS {
        if done() {
            return;
        }
        std::thread::sleep(PAUSE);
    }
    panic!("{what}: not reached after {READS} reads");
}

/// The target directory of this build: the parent of cargo's scratch directory for tests.
fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("the target directory")
        .to_path_buf()
}

/// The program `name` built for musl beside this build, which the job's musl step builds.
pub fn musl_program(name: &str) -> PathBuf {
    let path = target_dir().join(MUSL).join("debug").join(name);
    assert!(
        path.is_file(),
        "{} is not there: build it as privileged.yml's job openrc-device does \
         (cargo build --locked --target {MUSL} ...)",
        path.display()
    );
    path
}

/// The pinned nfpm the job installed (its step `Install nfpm`).
pub fn nfpm() -> PathBuf {
    let path = target_dir().join("privileged-tools").join("nfpm");
    assert!(
        path.is_file(),
        "the pinned nfpm is not at {}: install it as privileged.yml's job openrc-device does",
        path.display()
    );
    path
}

/// The repository's root.
pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository")
}

/// `path` made a directory of mode `mode`, with its parents.
pub fn dir(path: &Path, mode: u32) {
    fs::create_dir_all(path).unwrap_or_else(|e| panic!("make {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("its mode");
}

/// `bytes` written at `path`, mode `mode`.
pub fn file(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("its mode");
}

/// The file `from` copied to `to`, byte for byte, mode `mode`.
pub fn copy(from: &Path, to: &Path, mode: u32) {
    let bytes = fs::read(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display()));
    file(to, &bytes, mode);
}

/// What a command printed, as text.
pub fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// One test's Alpine host (the module says what it is).
pub struct Alpine {
    name: String,
    scratch: Scratch,
}

impl Drop for Alpine {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("{}", self.logs());
        }
        let _ = docker(&["rm", "--force", &self.name]);
    }
}

/// `docker` with `args`, to its end.
fn docker<S: AsRef<OsStr>>(args: &[S]) -> Output {
    Command::new("docker")
        .args(args)
        .output()
        .expect("docker runs")
}

impl Alpine {
    /// Boots a container of [`IMAGE`] named for `tag` and this process, the test's tree its
    /// `/hk`, its `/run` a tmpfs as a host's is, and waits until OpenRC has entered its default
    /// runlevel.
    pub fn boot(tag: &str) -> Alpine {
        assert!(
            rustix::process::geteuid().is_root(),
            "the OpenRC device tests run as root"
        );
        let name = format!("hk-openrc-{tag}-{}", std::process::id());
        let scratch =
            Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), &name).expect("the test's tree");
        dir(scratch.path(), 0o755);
        let _ = docker(&["rm", "--force", &name]);
        let volume = format!("{}:{HK_ROOT}", scratch.path().display());
        let run = docker(&[
            "run", "--detach", "--name", &name, "--tmpfs", "/run", "--volume", &volume, IMAGE,
        ]);
        assert!(
            run.status.success(),
            "the container of {IMAGE} did not start (build it as privileged.yml's job \
             openrc-device does): {}",
            text(&run)
        );
        let host = Alpine { name, scratch };
        host.booted();
        host
    }

    /// Waits until OpenRC has entered its default runlevel.
    fn booted(&self) {
        wait("OpenRC enters its default runlevel", || {
            let runlevel = self.exec(&["rc-status", "--runlevel"]);
            String::from_utf8_lossy(&runlevel.stdout).trim() == "default"
        });
    }

    /// Restarts the container, as a host is rebooted, and waits for its default runlevel.
    pub fn reboot(&self) {
        let restart = docker(&["restart", &self.name]);
        assert!(restart.status.success(), "{}", text(&restart));
        self.booted();
    }

    /// The test's tree on the host, the container's `/hk`.
    pub fn base(&self) -> &Path {
        self.scratch.path()
    }

    /// `args` run in the container, as root, to their end.
    pub fn exec(&self, args: &[&str]) -> Output {
        let mut all = vec!["exec", self.name.as_str()];
        all.extend(args);
        docker(&all)
    }

    /// `args` run in the container; fails with their output unless they exit 0.
    pub fn exec_ok(&self, args: &[&str]) -> String {
        let output = self.exec(args);
        assert!(
            output.status.success(),
            "{args:?} failed: {}\n{}",
            text(&output),
            self.logs()
        );
        text(&output)
    }

    /// The exit code of `rc-service hostkeeper status`: 0 started, 3 stopped.
    pub fn status(&self) -> Option<i32> {
        self.exec(&["rc-service", SERVICE, "status"]).status.code()
    }

    /// The pid of the daemon supervise-daemon runs, while it runs one.
    pub fn child(&self) -> Option<u32> {
        let output = self.exec(&["cat", CHILD_PID]);
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }

    /// The file process `pid` of the container runs, while it runs.
    pub fn exe_of(&self, pid: u32) -> Option<String> {
        let output = self.exec(&["readlink", &format!("/proc/{pid}/exe")]);
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Waits until supervise-daemon runs a daemon whose file is `exe` (a path in the
    /// container), other than `not` when given, and gives its pid.
    pub fn runs(&self, exe: &str, not: Option<u32>) -> u32 {
        let mut found = None;
        wait(&format!("supervise-daemon runs {exe}"), || {
            found = self
                .child()
                .filter(|pid| Some(*pid) != not)
                .filter(|pid| self.exe_of(*pid).as_deref() == Some(exe));
            found.is_some()
        });
        found.expect("a pid")
    }

    /// The readiness notice's text, when it is there.
    pub fn ready(&self) -> Option<String> {
        let output = self.exec(&["cat", READY]);
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// The daemon's log so far.
    pub fn log(&self) -> String {
        String::from_utf8_lossy(&self.exec(&["cat", LOG]).stdout).into_owned()
    }

    /// The daemon's log and OpenRC's view of its services, for a failing test.
    pub fn logs(&self) -> String {
        format!(
            "--- {LOG}\n{}\n--- rc-status\n{}",
            self.log(),
            text(&self.exec(&["rc-status", "--all"]))
        )
    }
}
