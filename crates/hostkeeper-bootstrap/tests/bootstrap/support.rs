//! The fakes and the fixtures of the tests.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use hostkeeper_bootstrap::run::{Paths, Pause, Ports};
use hostkeeper_bootstrap::settings::Sources;
use hostkeeper_install::tools::{Tool, ToolError, ToolOutput, Tools};
use hostkeeper_package::net::{Response, Timeouts, Transport};
use hostkeeper_package::ports::NetError;
use hostkeeper_testkit::Scratch;
use sha2::{Digest, Sha256};

pub const BASE: &str = "https://repo.example";
pub const TEAM: &str = "U7RTX4XD3X";
pub const CA: &str = "3d2a0000000000000000000000000000000000000000000000000000000000aa";
pub const VERSION: &str = "1.2.3";
pub const PKG_BYTES: &[u8] = b"a signed pkg, as far as the transport knows";

pub fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// The settings text of a manual install; `extra` is appended (more keys or tables).
pub fn settings_text(extra: &str) -> String {
    format!(
        "coordinator = \"https://coordinator.example\"\nca_sha256 = \"{CA}\"\nrelease_channel = \"{BASE}\"\n{extra}"
    )
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn write_mode(path: &Path, text: &str, mode: u32) {
    fs::create_dir_all(path.parent().expect("a parent")).expect("parent");
    fs::write(path, text).expect("write");
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Paths in `dir`, with a fallback settings file holding `settings_text(extra)`.
pub fn paths_in(dir: &Path, extra: &str) -> Paths {
    let owner = fs::metadata(dir).expect("scratch").uid();
    let paths = Paths {
        sources: Sources {
            managed: dir.join("managed.plist"),
            fallback: dir.join("bootstrap.toml"),
            trusted_uid: owner,
        },
        binary: dir.join("usr/local/bin/hostkeeper"),
        pki_current: dir.join("etc/updater/pki/current"),
        config_dir: dir.join("etc/hostkeeper"),
        staging_dir: dir.join("var/bootstrap"),
        update_account: dir.join("Library/hostkeeper/update-account"),
        daemon_plist: dir.join("Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist"),
    };
    write_mode(&paths.sources.fallback, &settings_text(extra), 0o600);
    paths
}

/// The descriptor of a repository with the macos-pkg format.
pub fn descriptor() -> String {
    serde_json::json!({
        "version": 1,
        "channels": ["testing", "stable"],
        "key": {"keyring": "apt/k.gpg", "fingerprints": ["CB09CF205649494A92DFC22C70D1F2A25906CB22"]},
        "formats": {
            "apt": {"path": "apt", "components": ["main"]},
            "macos-pkg": {
                "path": "pkg",
                "platforms": ["macos-arm64"],
                "packages": {"hostkeeper": {"dir": "hostkeeper"}}
            }
        }
    })
    .to_string()
}

/// The pointer of version `VERSION` naming the file with `sha256` and `size`.
pub fn pointer(sha256: &str, size: u64) -> String {
    serde_json::json!({
        "version": VERSION,
        "files": {format!("hostkeeper-{VERSION}-macos-arm64.pkg"): {"sha256": sha256, "size": size}}
    })
    .to_string()
}

pub const DESCRIPTOR_URL: &str = "https://repo.example/hostkeeper-repository.json";
pub const POINTER_URL: &str = "https://repo.example/pkg/hostkeeper/stable.json";
pub const PKG_URL: &str =
    "https://repo.example/pkg/hostkeeper/1.2.3/hostkeeper-1.2.3-macos-arm64.pkg";

/// A repository that serves each URL's `(status, body)`, and remembers what was asked.
#[derive(Default)]
pub struct Repo {
    pub answers: BTreeMap<String, (u16, Vec<u8>)>,
    pub asked: RefCell<Vec<String>>,
    /// The `content_length` each answer announces: the body's own unless set.
    pub announced: BTreeMap<String, u64>,
    pub down: Cell<bool>,
}

impl Repo {
    /// A repository serving a good pkg.
    pub fn good() -> Repo {
        let mut repo = Repo::default();
        repo.serve(DESCRIPTOR_URL, &descriptor());
        repo.serve(
            POINTER_URL,
            &pointer(&sha256_hex(PKG_BYTES), PKG_BYTES.len() as u64),
        );
        repo.answers
            .insert(PKG_URL.to_owned(), (200, PKG_BYTES.to_vec()));
        repo
    }

    pub fn serve(&mut self, url: &str, body: &str) {
        self.answers
            .insert(url.to_owned(), (200, body.as_bytes().to_vec()));
    }
}

impl Transport for Repo {
    fn open(&self, url: &str, _timeouts: Timeouts) -> Result<Response, NetError> {
        self.asked.borrow_mut().push(url.to_owned());
        if self.down.get() {
            return Err(NetError::Connect);
        }
        let (status, body) = self.answers.get(url).cloned().unwrap_or((404, Vec::new()));
        Ok(Response {
            status,
            content_length: Some(
                self.announced
                    .get(url)
                    .copied()
                    .unwrap_or(body.len() as u64),
            ),
            location: None,
            www_authenticate: None,
            body: Box::new(Cursor::new(body)),
        })
    }
}

/// The leaf's fingerprint, as `pkgutil` prints it (two lines of 16 bytes) and as the signers file
/// holds it.
pub fn leaf_hex() -> String {
    format!("3d2a{}aa{}bb", "00".repeat(13), "00".repeat(15))
}

/// What `pkgutil --check-signature` prints for a pkg signed as `status` and `leaf` say.
pub fn pkgutil_output(status: &str, leaf: &str) -> String {
    let first = format!("3D 2A {} AA", ["00"; 13].join(" "));
    let second = format!("{} BB", ["00"; 15].join(" "));
    format!(
        "Package \"hostkeeper.pkg\":\n   Status: {status}\n   Signed with a trusted timestamp on: 2026-11-14 10:00:00 +0000\n   Certificate Chain:\n    1. {leaf}\n       Expires: 2031-11-14 10:00:00 +0000\n       SHA256 Fingerprint:\n           {first}\n           {second}\n       ------------------------------------------------------------------------\n    2. Developer ID Certification Authority\n       Expires: 2031-11-14 10:00:00 +0000\n"
    )
}

pub fn good_pkgutil() -> String {
    pkgutil_output(
        "signed by a developer certificate issued by Apple for distribution",
        &format!("Developer ID Installer: Komira AI, Inc. ({TEAM})"),
    )
}

/// What a scripted tool answers with: an output, or the reason it could not be started.
pub type Answer = Result<ToolOutput, String>;

/// `pkgutil`, `installer` and `launchctl`, scripted; every call is recorded.
pub struct Tool_ {
    pub pkgutil: Mutex<Result<ToolOutput, String>>,
    pub installer_code: Mutex<Option<i32>>,
    pub installer_spawn_fails: Mutex<bool>,
    /// The answers of `launchctl print`, one for each call in order; once they are used up, the
    /// job is loaded (exit 0).
    pub launchctl_prints: Mutex<VecDeque<Answer>>,
    /// The answer of `launchctl enable`.
    pub launchctl_enable: Mutex<Answer>,
    /// The answer of `launchctl bootstrap`.
    pub launchctl_bootstrap: Mutex<Answer>,
    pub calls: Mutex<Vec<(Tool, Vec<String>)>>,
    /// The bytes of the staged pkg when `pkgutil` was run, to prove what it was asked about.
    pub seen_pkg: Mutex<Vec<Vec<u8>>>,
    /// Files whose bytes `installer` records when it is run: what its postinstall would find.
    pub watch: Mutex<Vec<PathBuf>>,
    /// For each run of `installer`, the bytes of each watched file, `None` when it is not there.
    pub at_installer: Mutex<Vec<Vec<Option<Vec<u8>>>>>,
}

impl Tool_ {
    pub fn new(pkgutil_stdout: &str) -> Tool_ {
        Tool_ {
            pkgutil: Mutex::new(Ok(ToolOutput::success(pkgutil_stdout))),
            installer_code: Mutex::new(Some(0)),
            installer_spawn_fails: Mutex::new(false),
            launchctl_prints: Mutex::new(VecDeque::new()),
            launchctl_enable: Mutex::new(Ok(ToolOutput::success(""))),
            launchctl_bootstrap: Mutex::new(Ok(ToolOutput::success(""))),
            calls: Mutex::new(Vec::new()),
            seen_pkg: Mutex::new(Vec::new()),
            watch: Mutex::new(Vec::new()),
            at_installer: Mutex::new(Vec::new()),
        }
    }

    pub fn good() -> Tool_ {
        Tool_::new(&good_pkgutil())
    }

    /// Scripts the answers of the `launchctl print` calls, in order.
    pub fn print_answers(&self, answers: impl IntoIterator<Item = Answer>) {
        *self.launchctl_prints.lock().expect("prints") = answers.into_iter().collect();
    }

    /// The arguments of each `launchctl` call, in order.
    pub fn launchctl_calls(&self) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .filter(|call| call.0 == Tool::Launchctl)
            .map(|call| call.1.clone())
            .collect()
    }

    pub fn ran(&self) -> Vec<Tool> {
        self.calls
            .lock()
            .expect("calls")
            .iter()
            .map(|c| c.0)
            .collect()
    }
}

impl Tools for Tool_ {
    fn run(
        &self,
        tool: Tool,
        args: &[String],
        _deadline: Duration,
    ) -> Result<ToolOutput, ToolError> {
        self.calls
            .lock()
            .expect("calls")
            .push((tool, args.to_vec()));
        if tool == Tool::Installer {
            let seen = self
                .watch
                .lock()
                .expect("watch")
                .iter()
                .map(|path| fs::read(path).ok())
                .collect();
            self.at_installer.lock().expect("seen").push(seen);
        }
        let spawn = |why: &str| ToolError::Spawn {
            args: args.join(" "),
            why: why.to_owned(),
        };
        match tool {
            Tool::Launchctl => {
                let answer = match args.first().map(String::as_str) {
                    Some("print") => self
                        .launchctl_prints
                        .lock()
                        .expect("prints")
                        .pop_front()
                        .unwrap_or_else(|| {
                            Ok(ToolOutput::success("system/x = { state = running }"))
                        }),
                    Some("enable") => self.launchctl_enable.lock().expect("enable").clone(),
                    Some("bootstrap") => {
                        self.launchctl_bootstrap.lock().expect("bootstrap").clone()
                    }
                    other => panic!("a launchctl verb the bootstrap does not run: {other:?}"),
                };
                answer.map_err(|why| spawn(&why))
            }
            Tool::Lsbom => panic!("the bootstrap does not run lsbom"),
            Tool::Pkgutil => {
                let pkg = args.last().expect("a path");
                self.seen_pkg
                    .lock()
                    .expect("seen")
                    .push(fs::read(pkg).unwrap_or_default());
                self.pkgutil
                    .lock()
                    .expect("pkgutil")
                    .clone()
                    .map_err(|why| spawn(&why))
            }
            Tool::Installer if *self.installer_spawn_fails.lock().expect("flag") => {
                Err(spawn("no such file"))
            }
            Tool::Installer => Ok(ToolOutput {
                code: *self.installer_code.lock().expect("code"),
                stdout: Vec::new(),
                stderr: b"installer: the install failed\n".to_vec(),
            }),
        }
    }
}

/// A pause that returns at once and remembers its lengths.
#[derive(Default)]
pub struct Pauses {
    pub secs: Mutex<Vec<u64>>,
    /// Runs at each pause: what happens in the world while the bootstrap waits.
    pub hook: Option<Box<dyn Fn()>>,
}

impl Pause for Pauses {
    fn pause(&self, secs: u64) {
        self.secs.lock().expect("pauses").push(secs);
        if let Some(hook) = &self.hook {
            hook();
        }
    }
}

/// A run's ports and what they recorded.
pub struct World {
    pub repo: Repo,
    pub tools: Tool_,
    pub pauses: Pauses,
    pub log: RefCell<Vec<String>>,
}

impl World {
    pub fn good() -> World {
        World {
            repo: Repo::good(),
            tools: Tool_::good(),
            pauses: Pauses::default(),
            log: RefCell::new(Vec::new()),
        }
    }

    pub fn run(
        &self,
        paths: &Paths,
    ) -> Result<hostkeeper_bootstrap::run::Outcome, hostkeeper_bootstrap::run::GaveUp> {
        let log = |line: &str| self.log.borrow_mut().push(line.to_owned());
        let ports = Ports {
            transport: &self.repo,
            tools: &self.tools,
            pause: &self.pauses,
            log: &log,
        };
        hostkeeper_bootstrap::run::run(paths, &ports)
    }

    pub fn pauses(&self) -> Vec<u64> {
        self.pauses.secs.lock().expect("pauses").clone()
    }
}

pub fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

pub fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("a file")
}

pub fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("a file").mode() & 0o7777
}
