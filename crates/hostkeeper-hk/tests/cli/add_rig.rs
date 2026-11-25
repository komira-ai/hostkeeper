//! What the tests of `hk xcode add` share: stand-ins for the Mac's programs (the test binary
//! `hk-fake-mac-tool` under each name), a clock that moves 1.5 seconds a reading, and a fake
//! Xcode store on a loopback port of 0 that answers `Xcode.List` from what the test says it holds
//! and takes the upload like the real one: a body whose SHA-256 is not its path's is refused
//! `digest_mismatch`.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use hostkeeper_hk::xcode_push::Mac;
use sha2::{Digest as _, Sha256};

use crate::rig::{Opts, Ran, Reply, Request, Rig, Seen, Server, args, call};
use crate::verify_rig::{SIGNED_BY_APPLE, clock, df};

/// The bytes of an archive: the four bytes of an xip, then `tag`.
pub fn xip(tag: &str) -> Vec<u8> {
    format!("xar!{tag}").into_bytes()
}

/// The archive of most tests.
pub fn archive_bytes() -> Vec<u8> {
    xip(", the bytes of an Xcode archive as Apple ships them")
}

/// The lower-case hex SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The path `Xcode.List` is called on.
pub const LIST: &str = "/hostkeeper.operator.v1.Xcode/List";

/// The paths the summary of `hk xcode add` reads the Macs from: a store of this file answers each
/// with an empty list, a coordinator that knows no host, group or run.
pub const FLEET: [&str; 3] = [
    "/hostkeeper.operator.v1.Hosts/List",
    "/hostkeeper.operator.v1.HostGroups/List",
    "/hostkeeper.operator.v1.Maintenance/List",
];

/// What the fake store holds and how it answers an upload.
#[derive(Clone, Default)]
pub struct Store {
    /// `(sha256, version, build)` of each archive held, newest first.
    pub held: Vec<(String, String, String)>,
    /// The release waiting: version and build.
    pub waiting: Option<(String, String)>,
    /// `configured` is false (no `--xcode-dir`).
    pub unconfigured: bool,
    /// The answer to every upload, in place of the real one.
    pub upload: Option<(u16, String)>,
}

impl Store {
    /// A store holding the archive `bytes` as `version` (`build`) too.
    pub fn holding(mut self, bytes: &[u8], version: &str, build: &str) -> Store {
        self.held
            .push((sha256(bytes), version.to_owned(), build.to_owned()));
        self
    }

    /// A store holding an archive of the SHA-256 `sha256`.
    pub fn holding_sha(mut self, sha256: &str, version: &str, build: &str) -> Store {
        self.held
            .push((sha256.to_owned(), version.to_owned(), build.to_owned()));
        self
    }

    pub fn waiting(mut self, version: &str, build: &str) -> Store {
        self.waiting = Some((version.to_owned(), build.to_owned()));
        self
    }

    pub fn answering(mut self, status: u16, body: &str) -> Store {
        self.upload = Some((status, body.to_owned()));
        self
    }

    /// The JSON `Xcode.List` answers.
    fn list(&self) -> String {
        let archives: Vec<String> = self
            .held
            .iter()
            .map(|(sha, version, build)| {
                format!(
                    "{{\"sha256\":\"{sha}\",\"version\":\"{version}\",\"build\":\"{build}\",\
                     \"sizeBytes\":\"1\"}}"
                )
            })
            .collect();
        let waiting = self
            .waiting
            .as_ref()
            .map_or(String::new(), |(version, build)| {
                format!(",\"waiting\":{{\"version\":\"{version}\",\"build\":\"{build}\"}}")
            });
        let configured = if self.unconfigured { "false" } else { "true" };
        format!(
            "{{\"configured\":{configured},\"archives\":[{}]{waiting},\"keep\":2}}",
            archives.join(",")
        )
    }

    fn answer(&self, request: &Request) -> Reply {
        if request.target == LIST {
            return Reply::new(200, self.list());
        }
        if FLEET.contains(&request.target.as_str()) {
            return Reply::new(200, "{}");
        }
        if let Some((status, body)) = &self.upload {
            return Reply::new(*status, body.clone());
        }
        let named = request
            .target
            .strip_prefix("/v1/xcode/")
            .and_then(|rest| rest.split('?').next())
            .unwrap_or_default();
        if request.method != "PUT" || named != sha256(&request.body) {
            return Reply::new(400, "{\"error\":\"digest_mismatch\"}");
        }
        Reply::new(
            201,
            format!(
                "{{\"sha256\":\"{named}\",\"version\":\"27.2\",\"build\":\"27B52\",\"size\":{}}}",
                request.body.len()
            ),
        )
    }

    /// The server.
    pub fn serve(self, rig: &Rig) -> Server {
        Server::serve(&rig.pki, || (), move |(), request| self.answer(request))
    }

    /// The server, where `other` answers first: the requests it has an answer to are not the
    /// store's.
    pub fn serve_with(
        self,
        rig: &Rig,
        other: impl Fn(&Request) -> Option<Reply> + Send + 'static,
    ) -> Server {
        Server::serve(
            &rig.pki,
            || (),
            move |(), request| other(request).unwrap_or_else(|| self.answer(request)),
        )
    }

    /// The server, and what the file `lock` held at each request it answered: its text and its
    /// mode, or `None` when there was no such file. The requests are made while a run holds its
    /// lock, so this is what the lock was like during the run.
    pub fn serve_watching(self, rig: &Rig, lock: &Path) -> (Server, Watched) {
        let watched: Watched = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&watched);
        let lock = lock.to_path_buf();
        let server = Server::serve(
            &rig.pki,
            || (),
            move |(), request| {
                let held = std::fs::read_to_string(&lock).ok().zip(
                    std::fs::metadata(&lock)
                        .ok()
                        .map(|meta| meta.permissions().mode() & 0o777),
                );
                record.lock().expect("the record").push(held);
                self.answer(request)
            },
        );
        (server, watched)
    }
}

/// What a watched lock file held at each request: its text and mode.
pub type Watched = Arc<Mutex<Vec<Option<(String, u32)>>>>;

/// The uploads a server saw.
pub fn puts(server: &Server) -> Vec<Seen> {
    server
        .seen()
        .into_iter()
        .filter(|seen| seen.method == "PUT")
        .collect()
}

/// The `Xcode.List` calls a server saw.
pub fn lists(server: &Server) -> Vec<Seen> {
    server
        .seen()
        .into_iter()
        .filter(|seen| seen.target == LIST)
        .collect()
}

/// A test's directory with the stand-ins in `bin`, set up for an archive Apple signed whose app is
/// Xcode 27.2 (27B52) on a Mac with room, and a `Downloads` directory in its home.
pub struct Add {
    pub rig: Rig,
    pub mac: Mac,
}

/// The programs a run uses.
pub const TOOLS: [&str; 6] = ["xcodes", "pkgutil", "xip", "plutil", "codesign", "df"];

impl Add {
    pub fn new(name: &str) -> Add {
        let rig = Rig::new(&format!("xcode-add-{name}"));
        let bin = rig.path("bin");
        std::fs::create_dir(&bin).expect("the bin directory");
        let fake = Path::new(env!("CARGO_BIN_EXE_hk-fake-mac-tool"));
        for tool in TOOLS {
            // A link, not a copy: a copy is a file open for writing, which a process another test
            // forks meanwhile could hold when this one runs it.
            std::fs::hard_link(fake, bin.join(tool)).expect("a stand-in");
        }
        std::fs::create_dir_all(rig.path("home/Downloads")).expect("a downloads directory");
        let mac = Mac {
            os: "macos",
            xcodes: bin.join("xcodes"),
            pkgutil: bin.join("pkgutil"),
            xip: bin.join("xip"),
            plutil: bin.join("plutil"),
            codesign: bin.join("codesign"),
            df: bin.join("df"),
            clock,
            ..Mac::system()
        };
        let add = Add { rig, mac };
        add.tool("pkgutil.out", SIGNED_BY_APPLE.as_bytes());
        add.tool("xip.version", b"27.2\n");
        add.tool("xip.product-build", b"27B52\n");
        add.tool("df.out", df(900_000_000).as_bytes());
        add
    }

    /// Writes `name` in `bin`, beside the stand-ins.
    pub fn tool(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.rig.path("bin").join(name), bytes).expect("written");
    }

    /// What the stand-in wrote to `name` in `bin`; empty when it wrote nothing.
    pub fn record(&self, name: &str) -> String {
        std::fs::read_to_string(self.rig.path("bin").join(name)).unwrap_or_default()
    }

    /// How many times the program `tool` ran.
    pub fn runs(&self, tool: &str) -> usize {
        self.record(&format!("{tool}.args")).lines().count()
    }

    /// Writes `bytes` as `name` under `~/Downloads` (a path inside it) whose modification time is
    /// `modified` seconds after the epoch; its path.
    pub fn file(&self, name: &str, bytes: &[u8], modified: u64) -> String {
        let path = self.rig.path("home/Downloads").join(name);
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("directories");
        std::fs::write(&path, bytes).expect("written");
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open")
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(modified))
            .expect("a modification time");
        path.display().to_string()
    }

    /// `~/Downloads`.
    pub fn downloads(&self) -> std::path::PathBuf {
        self.rig.path("home/Downloads")
    }

    /// `~/.hostkeeper/xcode/<name>`.
    pub fn kept(&self, name: &str) -> std::path::PathBuf {
        self.rig.path("home/.hostkeeper/xcode").join(name)
    }

    /// `hk` with the global flags of a call on `port`, then `words`, on the stand-ins.
    pub fn run(&self, port: u16, words: &[&str]) -> Ran {
        self.run_with(Opts::real(), port, words)
    }

    /// [`Add::run`] with `opts`.
    pub fn run_with<'a>(&'a self, opts: Opts<'a>, port: u16, words: &[&str]) -> Ran {
        self.rig
            .run(opts.mac(&self.mac), &call(&self.rig, port, words))
    }

    /// `hk` with `words` as they are, no flags added: what the settings file may fill.
    pub fn bare(&self, words: &[&str]) -> Ran {
        self.rig.run(Opts::real().mac(&self.mac), &args(words))
    }

    /// `hk xcode add <words>` against a store.
    pub fn add(&self, server: &Server, words: &[&str]) -> Ran {
        let mut all = vec!["xcode", "add"];
        all.extend_from_slice(words);
        self.run(server.port, &all)
    }

    /// The entries of `~/Downloads`: what a run left behind.
    pub fn beside(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.downloads())
            .expect("the directory")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}
