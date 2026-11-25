//! The tests of the stage of the `xcode` kind, over a scratch directory standing for the Mac's
//! files, a fake store that serves one archive by its SHA-256 and can break a read part way, a
//! fake `xip` that makes an app from a fixture version list, a scripted `codesign`, and a fake
//! free space. Each deferral is asserted by equality of its reason and its whole detail, each run
//! of a program and each read of the store by equality of the whole list.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_link::LinkError;
use hostkeeper_testkit::Scratch;

use super::*;
use crate::maintenance_plist::HostPlists;
use crate::testing::FakeClock;

const V27: &[u8] = include_bytes!("../../../tests/fixtures/maintenance/xcode/version-27.2.plist");
const V26: &[u8] = include_bytes!("../../../tests/fixtures/maintenance/xcode/version-26.3.plist");
const NO_BUILD: &[u8] =
    include_bytes!("../../../tests/fixtures/maintenance/xcode/version-no-build.plist");
const INTEGER: &[u8] =
    include_bytes!("../../../tests/fixtures/maintenance/xcode/version-integer.plist");

/// The archive the store serves: an `xar` header and some bytes.
const ARCHIVE_BYTES: &[u8] = b"xar!\x00\x1c\x00\x01 the bytes of an Xcode archive";

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The order's entry of `bytes`: Xcode 27.2 (27B52).
fn entry_of(bytes: &[u8]) -> XcodeEntry {
    XcodeEntry {
        version: "27.2".to_owned(),
        build: "27B52".to_owned(),
        size: bytes.len() as u64,
        sha256: sha256(bytes),
    }
}

/// What one read of the fake store does after it gives the bytes it gives.
#[derive(Clone, Debug)]
enum Read {
    /// Gives everything from `first` on.
    Whole,
    /// Gives `count` bytes from `first`, then fails with the error.
    Breaks(u64, GetError),
    /// Gives nothing and fails.
    Fails(StoreError),
}

/// A store that holds `bytes` at `/v1/xcode/<sha256 of held>` and plays `reads` in order (then
/// [`Read::Whole`]), recording each `(path, first)` it is asked.
#[derive(Clone)]
struct FakeStore {
    path: String,
    bytes: Vec<u8>,
    reads: Arc<Mutex<Vec<Read>>>,
    asked: Arc<Mutex<Vec<(String, u64)>>>,
}

impl FakeStore {
    fn holding(held: &[u8], bytes: &[u8]) -> FakeStore {
        FakeStore {
            path: format!("/v1/xcode/{}", sha256(held)),
            bytes: bytes.to_vec(),
            reads: Arc::default(),
            asked: Arc::default(),
        }
    }

    fn playing(self, reads: Vec<Read>) -> FakeStore {
        *lock(&self.reads) = reads;
        self
    }

    fn asked(&self) -> Vec<(String, u64)> {
        lock(&self.asked).clone()
    }
}

impl Store for FakeStore {
    fn get_from(
        &self,
        path: &str,
        first: u64,
        limit: Limit,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Got, StoreError> {
        lock(&self.asked).push((path.to_owned(), first));
        if path != self.path {
            return Ok(Got::NotFound);
        }
        let read = {
            let mut reads = lock(&self.reads);
            match reads.is_empty() {
                true => Read::Whole,
                false => reads.remove(0),
            }
        };
        let rest = &self.bytes[usize::try_from(first).expect("small")..];
        let length = rest.len() as u64;
        if length > limit.max_bytes {
            return Err(StoreError::Get(GetError::TooLong {
                length,
                max: limit.max_bytes,
            }));
        }
        match read {
            Read::Whole => {
                sink(rest).map_err(|e| StoreError::Get(GetError::Sink(e.kind())))?;
                Ok(Got::Body(length))
            }
            Read::Breaks(count, error) => {
                let count = usize::try_from(count).expect("small");
                sink(&rest[..count]).map_err(|e| StoreError::Get(GetError::Sink(e.kind())))?;
                Err(StoreError::Get(error))
            }
            Read::Fails(error) => Err(error),
        }
    }
}

/// What a fake program does when it is run.
type Program = Box<dyn Fn(&Spawn) -> io::Result<Ran> + Send>;

/// A runner of fake programs by path, recording each run and, for `xip`, the archive's bytes
/// when it ran.
#[derive(Clone, Default)]
struct FakeRunner {
    programs: Arc<Mutex<BTreeMap<PathBuf, Program>>>,
    ran: Arc<Mutex<Vec<Spawn>>>,
    archives: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeRunner {
    fn answers(&self, program: &str, answer: Program) {
        lock(&self.programs).insert(PathBuf::from(program), answer);
    }

    fn ran(&self) -> Vec<Spawn> {
        lock(&self.ran).clone()
    }
}

impl Runner for FakeRunner {
    fn run(&self, spawn: &Spawn) -> io::Result<Ran> {
        lock(&self.ran).push(spawn.clone());
        if spawn.program == Path::new(XIP) {
            let archive = fs::read(&spawn.args[1]).unwrap_or_default();
            lock(&self.archives).push(archive);
        }
        let programs = lock(&self.programs);
        let program = programs
            .get(&spawn.program)
            .expect("a program the test answers");
        program(spawn)
    }
}

/// A program that exits `code` saying `stderr`.
fn exits(code: i32, stderr: &str) -> Program {
    let stderr = stderr.as_bytes().to_vec();
    Box::new(move |_| {
        Ok(Ran {
            code: Some(code),
            timed_out: false,
            stderr: stderr.clone(),
        })
    })
}

/// An `xip` that makes `<cwd>/<name>` with `plist` as its version list, and exits 0.
fn xip_making(name: &'static str, plist: &'static [u8]) -> Program {
    Box::new(move |spawn| {
        let contents = spawn.cwd.join(name).join("Contents");
        fs::create_dir_all(&contents)?;
        fs::write(contents.join("version.plist"), plist)?;
        Ok(Ran {
            code: Some(0),
            ..Ran::default()
        })
    })
}

/// A free space of `bytes`, or one that cannot be read; counts its reads.
#[derive(Clone)]
struct FakeSpace {
    free: Result<u64, String>,
    reads: Arc<Mutex<u32>>,
    /// A directory made, mode 0755, when the space is read: one put in the staging directory's
    /// place between the stage's first look at it and the fetch.
    swap_in: Option<PathBuf>,
}

impl FreeSpace for FakeSpace {
    fn free_bytes(&self) -> Result<u64, String> {
        *lock(&self.reads) += 1;
        if let Some(dir) = &self.swap_in {
            fs::create_dir(dir).expect("the swapped-in directory");
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
        }
        self.free.clone()
    }
}

/// A Mac with its files under a scratch directory.
struct Mac {
    root: Scratch,
    store: FakeStore,
    runner: FakeRunner,
    space: FakeSpace,
    clock: Arc<dyn Clock>,
}

/// The free space the archive [`ARCHIVE_BYTES`] needs to be staged: itself and its expansion.
fn needed() -> u64 {
    ARCHIVE_BYTES.len() as u64 * 4
}

impl Mac {
    /// A Mac whose store holds [`ARCHIVE_BYTES`], whose `xip` makes Xcode 27.2 (27B52), whose
    /// `codesign` accepts it, with exactly the space staging needs free.
    fn new(name: &str) -> Mac {
        let runner = FakeRunner::default();
        runner.answers(XIP, xip_making("Xcode.app", V27));
        runner.answers(CODESIGN, exits(0, ""));
        let clock: Arc<dyn Clock> = FakeClock::new();
        let root = scratch(name);
        fs::create_dir(root.path().join("Applications")).expect("the Applications directory");
        Mac {
            root,
            store: FakeStore::holding(ARCHIVE_BYTES, ARCHIVE_BYTES),
            runner,
            space: FakeSpace {
                free: Ok(needed()),
                reads: Arc::default(),
                swap_in: None,
            },
            clock,
        }
    }

    fn stage(&self) -> Stage {
        Stage {
            store: Box::new(self.store.clone()),
            runner: Box::new(self.runner.clone()),
            space: Box::new(self.space.clone()),
            plists: Box::new(HostPlists::new(HostRoot::at(self.root.path()))),
            root: HostRoot::at(self.root.path()),
            clock: Arc::clone(&self.clock),
        }
    }

    fn run(&self, entries: &[XcodeEntry]) -> Result<(), KindError> {
        self.stage().run(entries)
    }

    /// The host's `path` under the scratch root.
    fn path(&self, path: &str) -> PathBuf {
        self.root.path().join(path.trim_start_matches('/'))
    }

    /// The names in the host's directory `path`, sorted; none when it is not there.
    fn names(&self, path: &str) -> Vec<String> {
        let Ok(listed) = fs::read_dir(self.path(path)) else {
            return Vec::new();
        };
        let mut names: Vec<String> = listed
            .map(|item| {
                item.expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// The staging directory as the stage makes it: the test's user's, mode 0700.
    fn trusted_staging(&self) {
        DirBuilder::new()
            .mode(0o700)
            .create(self.path(STAGING))
            .expect("the staging directory");
    }

    /// The host's file `path`, with `bytes`.
    fn file(&self, path: &str, bytes: &[u8]) {
        let path = self.path(path);
        fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
        fs::write(path, bytes).expect("the file");
    }

    /// The deadline of a program run now for `secs`.
    fn deadline(&self, secs: u64) -> Stamp {
        let now = Stamp::now(&*self.clock);
        Stamp {
            boot_id: now.boot_id,
            mono_secs: now.mono_secs + secs,
        }
    }

    /// The run of `xip` on the archive of `entry`.
    fn xip_run(&self, entry: &XcodeEntry) -> Spawn {
        let dir = self.path(&format!("{STAGING}/{}", entry.sha256));
        Spawn {
            program: PathBuf::from(XIP),
            args: vec![
                OsString::from("--expand"),
                dir.join(ARCHIVE).into_os_string(),
            ],
            env: vec![(
                "PATH".to_owned(),
                "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
            )],
            cwd: dir.join(EXPAND),
            deadline: self.deadline(XIP_SECS),
        }
    }

    /// The run of `codesign` on the app at the host's `app`.
    fn codesign_run(&self, app: &str) -> Spawn {
        Spawn {
            program: PathBuf::from(CODESIGN),
            args: vec![
                OsString::from("--verify"),
                OsString::from("--deep"),
                OsString::from("--strict"),
                self.path(app).into_os_string(),
            ],
            env: vec![(
                "PATH".to_owned(),
                "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
            )],
            cwd: self.path(STAGING),
            deadline: self.deadline(CODESIGN_SECS),
        }
    }

    /// Nothing of a stage is left: no staging directory, nothing in `/Applications`.
    fn assert_clean(&self) {
        assert!(
            !self.path(STAGING).exists(),
            "the staging directory is removed"
        );
        assert_eq!(self.names(APPLICATIONS), Vec::<String>::new());
    }
}

fn deferral(reason: &'static str, detail: &str) -> Result<(), KindError> {
    Err(KindError::Deferred {
        reason,
        detail: detail.to_owned(),
    })
}

/// Proves: a good archive, with exactly the space it needs free, is read once from the store by
/// its SHA-256 from byte 0, `xip --expand` runs on the archive when it holds exactly the store's
/// bytes, `codesign --verify --deep --strict` runs on the expanded app, and the stage ends with
/// the verified app at `<staging>/<sha256>/Xcode.app` alone (the archive and the expansion
/// removed) and nothing else in `/Applications`.
#[test]
fn a_good_archive_ends_with_a_verified_app_in_staging_and_nothing_in_applications() {
    let mac = Mac::new("xcs-good");
    let entry = entry_of(ARCHIVE_BYTES);
    assert_eq!(mac.run(std::slice::from_ref(&entry)), Ok(()));
    let dir = format!("{STAGING}/{}", entry.sha256);
    assert_eq!(
        mac.store.asked(),
        [(format!("/v1/xcode/{}", entry.sha256), 0)]
    );
    assert_eq!(
        mac.runner.ran(),
        [
            mac.xip_run(&entry),
            mac.codesign_run(&format!("{dir}/expand/Xcode.app")),
        ]
    );
    assert_eq!(*lock(&mac.runner.archives), [ARCHIVE_BYTES.to_vec()]);
    assert_eq!(mac.names(APPLICATIONS), [".hostkeeper-staging"]);
    assert_eq!(mac.names(STAGING), [entry.sha256.as_str()]);
    assert_eq!(mac.names(&dir), ["Xcode.app"]);
    assert_eq!(
        fs::read(mac.path(&format!("{dir}/Xcode.app/Contents/version.plist"))).expect("read"),
        V27
    );
}

/// Proves: one byte less free than the archive and its expansion need defers
/// `free_space_short` before the store is read or any program runs, and leaves nothing; a free
/// space that cannot be read defers `free_space_unreadable` and fetches nothing.
#[test]
fn a_short_disk_defers_and_fetches_nothing() {
    let mut mac = Mac::new("xcs-short");
    mac.space.free = Ok(needed() - 1);
    let entry = entry_of(ARCHIVE_BYTES);
    assert_eq!(
        mac.run(&[entry]),
        deferral(
            REASON_FREE_SPACE_SHORT,
            &format!(
                "{} bytes are free on the volume of /Applications; staging needs {}",
                needed() - 1,
                needed()
            )
        )
    );
    assert!(mac.store.asked().is_empty());
    assert!(mac.runner.ran().is_empty());
    mac.assert_clean();

    let mut mac = Mac::new("xcs-space-unreadable");
    mac.space.free = Err("statvfs of /Applications failed: denied".to_owned());
    assert_eq!(
        mac.run(&[entry_of(ARCHIVE_BYTES)]),
        deferral(
            REASON_FREE_SPACE_UNREADABLE,
            "statvfs of /Applications failed: denied"
        )
    );
    assert!(mac.store.asked().is_empty());
}

/// Proves: an archive of another size or another SHA-256 than the order's is refused
/// `xcode_archive_mismatch` before `xip` runs, and the staging directory is removed: a store
/// whose archive ends early, one whose archive is longer (refused by the read's bound), and one
/// of the order's size with other bytes.
#[test]
fn a_size_or_sha256_mismatch_is_refused_before_xip_runs() {
    let entry = entry_of(ARCHIVE_BYTES);
    let short = &ARCHIVE_BYTES[..ARCHIVE_BYTES.len() - 1];
    let mut long = ARCHIVE_BYTES.to_vec();
    long.push(b'!');
    let mut other = ARCHIVE_BYTES.to_vec();
    other[5] = b'X';
    let size = ARCHIVE_BYTES.len();
    let cases = [
        (
            short.to_vec(),
            format!("the store's archive ended at {} bytes", size - 1),
        ),
        (long, format!("the store's archive is {} bytes", size + 1)),
        (
            other.clone(),
            format!(
                "the archive is {size} bytes with SHA-256 {}",
                sha256(&other)
            ),
        ),
    ];
    for (served, found) in cases {
        let mut mac = Mac::new("xcs-mismatch");
        mac.store = FakeStore::holding(ARCHIVE_BYTES, &served);
        assert_eq!(
            mac.run(std::slice::from_ref(&entry)),
            deferral(
                REASON_ARCHIVE_MISMATCH,
                &format!(
                    "{found}; the order names {size} bytes with SHA-256 {}",
                    entry.sha256
                )
            ),
            "{found}"
        );
        assert!(mac.runner.ran().is_empty(), "{found}");
        mac.assert_clean();
    }
}

/// Proves: a read that breaks part way (cut short, a failed connection, past its deadline) is
/// resumed from the byte the copy holds, by the same path, and the resumed copy is the archive:
/// the reads are from 0, 5, 8 and 8, and the app is staged.
#[test]
fn a_broken_download_resumes_by_range() {
    let mut mac = Mac::new("xcs-resume");
    mac.store = mac.store.clone().playing(vec![
        Read::Breaks(5, GetError::Truncated),
        Read::Breaks(
            3,
            GetError::Link(LinkError::Transport(io::ErrorKind::ConnectionReset)),
        ),
        Read::Breaks(0, GetError::Deadline),
    ]);
    let entry = entry_of(ARCHIVE_BYTES);
    assert_eq!(mac.run(std::slice::from_ref(&entry)), Ok(()));
    let path = format!("/v1/xcode/{}", entry.sha256);
    assert_eq!(
        mac.store.asked(),
        [
            (path.clone(), 0),
            (path.clone(), 5),
            (path.clone(), 8),
            (path, 8)
        ]
    );
    assert_eq!(*lock(&mac.runner.archives), [ARCHIVE_BYTES.to_vec()]);
    assert_eq!(
        mac.names(&format!("{STAGING}/{}", entry.sha256)),
        ["Xcode.app"]
    );
}

/// Proves: a read that keeps breaking is given up after [`MAX_FETCH_ATTEMPTS`] connections,
/// deferred `xcode_fetch_failed` naming the byte and the connection, and nothing is left.
#[test]
fn a_download_that_keeps_breaking_defers_after_its_attempts() {
    let mut mac = Mac::new("xcs-breaking");
    mac.store = mac.store.clone().playing(vec![
        Read::Breaks(1, GetError::Truncated);
        MAX_FETCH_ATTEMPTS as usize
    ]);
    let entry = entry_of(ARCHIVE_BYTES);
    let path = format!("/v1/xcode/{}", entry.sha256);
    assert_eq!(
        mac.run(&[entry]),
        deferral(
            REASON_FETCH_FAILED,
            &format!(
                "{path} from byte 8, connection 8: the connection ended before the whole body came"
            )
        )
    );
    let firsts: Vec<u64> = mac
        .store
        .asked()
        .into_iter()
        .map(|(_, first)| first)
        .collect();
    assert_eq!(firsts, [0, 1, 2, 3, 4, 5, 6, 7]);
    assert!(mac.runner.ran().is_empty());
    mac.assert_clean();
}

/// Proves: the order's SHA-256 is the only path the store is read by. An entry whose SHA-256 is
/// not 64 lower-case hex characters (a path, upper case, too short), whose version or build is
/// not one the stage takes, or whose size is 0 defers `xcode_order_invalid` before the store is
/// read, the space measured or any program run; with a valid order every read is of
/// `/v1/xcode/<the order's sha256>`; an order naming one archive twice defers too.
#[test]
fn the_orders_sha256_is_the_only_fetch_path_used() {
    let good = entry_of(ARCHIVE_BYTES);
    let with = |change: &dyn Fn(&mut XcodeEntry)| {
        let mut entry = good.clone();
        change(&mut entry);
        entry
    };
    let invalid = [
        with(&|e| e.sha256 = "../../../etc/passwd".to_owned()),
        with(&|e| e.sha256 = e.sha256.to_uppercase()),
        with(&|e| e.sha256 = e.sha256[..63].to_owned()),
        with(&|e| e.version = String::new()),
        with(&|e| e.version = "27.2/../x".to_owned()),
        with(&|e| e.version = "1".repeat(17)),
        with(&|e| e.build = "27B 52".to_owned()),
        with(&|e| e.size = 0),
    ];
    for entry in invalid {
        let mac = Mac::new("xcs-invalid");
        assert_eq!(
            mac.run(&[good.clone(), entry.clone()]),
            deferral(
                REASON_ORDER_INVALID,
                "an Xcode of the order has a SHA-256, version, build or size the stage does not take"
            ),
            "{entry:?}"
        );
        assert!(mac.store.asked().is_empty(), "{entry:?}");
        assert_eq!(*lock(&mac.space.reads), 0, "{entry:?}");
        assert!(mac.runner.ran().is_empty(), "{entry:?}");
    }
    let mac = Mac::new("xcs-one-path");
    mac.store
        .clone()
        .playing(vec![Read::Breaks(2, GetError::Truncated)]);
    assert_eq!(mac.run(std::slice::from_ref(&good)), Ok(()));
    let paths: Vec<String> = mac
        .store
        .asked()
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(paths, vec![format!("/v1/xcode/{}", good.sha256); 2]);
    let mac = Mac::new("xcs-twice");
    assert_eq!(
        mac.run(&[good.clone(), good.clone()]),
        deferral(
            REASON_ORDER_INVALID,
            &format!("the order names the archive {} twice", good.sha256)
        )
    );
    assert!(mac.store.asked().is_empty());
}

/// Proves: an expanded app whose build or version is not the order's, or whose version list
/// names none, defers `xcode_build_mismatch` without running `codesign`, and removes the
/// staging directory.
#[test]
fn a_wrong_product_build_version_defers_and_removes_staging() {
    let cases: [(&'static [u8], &str); 3] = [
        (V26, "the app is Xcode 26.3 (17C52)"),
        (NO_BUILD, "the app is Xcode 27.2 (<none>)"),
        (INTEGER, "the app is Xcode <none> (27B52)"),
    ];
    for (plist, found) in cases {
        let mac = Mac::new("xcs-build");
        mac.runner.answers(XIP, xip_making("Xcode.app", plist));
        let entry = entry_of(ARCHIVE_BYTES);
        assert_eq!(
            mac.run(std::slice::from_ref(&entry)),
            deferral(
                REASON_BUILD_MISMATCH,
                &format!("{found}; the order names Xcode 27.2 (27B52)")
            )
        );
        assert_eq!(mac.runner.ran(), [mac.xip_run(&entry)], "{found}");
        mac.assert_clean();
    }
}

/// Proves: an app `codesign --verify` refuses defers `xcode_signature_invalid`, naming its exit
/// and the first line of its standard error, and removes the staging directory.
#[test]
fn a_failing_codesign_verify_defers_and_removes_staging() {
    let mac = Mac::new("xcs-codesign");
    mac.runner.answers(
        CODESIGN,
        exits(
            1,
            "Xcode.app: a sealed resource is missing or invalid\nmore\n",
        ),
    );
    assert_eq!(
        mac.run(&[entry_of(ARCHIVE_BYTES)]),
        deferral(
            REASON_SIGNATURE_INVALID,
            "codesign --verify exited 1: Xcode.app: a sealed resource is missing or invalid"
        )
    );
    assert_eq!(mac.runner.ran().len(), 2);
    mac.assert_clean();
}

/// Proves: an `xip --expand` that exits non-zero, is killed at its deadline, is ended by a
/// signal, cannot start, or makes no app or two defers `xcode_expand_failed`, runs no
/// `codesign`, and removes the staging directory.
#[test]
fn a_failed_expansion_defers_and_removes_staging() {
    let killed: Program = Box::new(|_| {
        Ok(Ran {
            code: None,
            timed_out: true,
            stderr: Vec::new(),
        })
    });
    let signalled: Program = Box::new(|_| {
        Ok(Ran {
            code: None,
            timed_out: false,
            stderr: b"terminated".to_vec(),
        })
    });
    let unstartable: Program = Box::new(|_| Err(io::Error::other("no such program")));
    let none: Program = exits(0, "");
    let two: Program = Box::new(|spawn| {
        for name in ["Xcode.app", "Xcode-beta.app"] {
            fs::create_dir_all(spawn.cwd.join(name))?;
        }
        Ok(Ran {
            code: Some(0),
            ..Ran::default()
        })
    });
    let cases: [(Program, &str); 7] = [
        (
            exits(1, "xip: error: The archive is damaged\n"),
            "xip --expand exited 1: xip: error: The archive is damaged",
        ),
        (exits(2, "\nxip: the second line"), "xip --expand exited 2"),
        (killed, "xip --expand was killed at its deadline"),
        (signalled, "xip --expand was ended by a signal: terminated"),
        (unstartable, "xip --expand could not start: no such program"),
        (none, "xip --expand made 0 apps, not one Xcode"),
        (two, "xip --expand made 2 apps, not one Xcode"),
    ];
    for (xip, detail) in cases {
        let mac = Mac::new("xcs-expand");
        mac.runner.answers(XIP, xip);
        assert_eq!(
            mac.run(&[entry_of(ARCHIVE_BYTES)]),
            deferral(REASON_EXPAND_FAILED, detail)
        );
        assert_eq!(mac.runner.ran().len(), 1, "{detail}");
        mac.assert_clean();
    }
}

/// Proves: an Apple Encrypted Archive of the order's size and SHA-256 defers
/// `xcode_archive_encrypted` and is not given to `xip`.
#[test]
fn an_encrypted_archive_defers_and_is_not_expanded() {
    let encrypted: &[u8] = b"AEA1 an encrypted Xcode archive";
    let mut mac = Mac::new("xcs-encrypted");
    mac.store = FakeStore::holding(encrypted, encrypted);
    mac.space.free = Ok(encrypted.len() as u64 * 4);
    assert_eq!(
        mac.run(&[entry_of(encrypted)]),
        deferral(
            REASON_ARCHIVE_ENCRYPTED,
            "Xcode 27.2 (27B52) is an Apple Encrypted Archive, which this stage does not expand"
        )
    );
    assert!(mac.runner.ran().is_empty());
    mac.assert_clean();
}

/// Proves: an archive the store does not hold defers `xcode_not_in_store`, a refusal by the
/// coordinator (a status) or a store that cannot be reached defers `xcode_fetch_failed` after
/// one read and is not resumed, and a copy that cannot be written defers
/// `xcode_staging_failed`; each leaves nothing.
#[test]
fn a_store_that_gives_no_archive_defers() {
    let entry = entry_of(ARCHIVE_BYTES);
    let path = format!("/v1/xcode/{}", entry.sha256);
    let missing = FakeStore::holding(b"another archive", ARCHIVE_BYTES);
    let cases = [
        (
            missing,
            REASON_NOT_IN_STORE,
            format!("the store holds no archive {path}"),
        ),
        (
            FakeStore::holding(ARCHIVE_BYTES, ARCHIVE_BYTES)
                .playing(vec![Read::Fails(StoreError::Get(GetError::Status(403)))]),
            REASON_FETCH_FAILED,
            format!("{path} from byte 0, connection 1: the coordinator answered 403"),
        ),
        (
            FakeStore::holding(ARCHIVE_BYTES, ARCHIVE_BYTES).playing(vec![Read::Fails(
                StoreError::Unusable("no config".to_owned()),
            )]),
            REASON_FETCH_FAILED,
            format!(
                "{path} from byte 0, connection 1: the coordinator cannot be reached: no config"
            ),
        ),
    ];
    for (store, reason, detail) in cases {
        let mut mac = Mac::new("xcs-no-archive");
        mac.store = store;
        assert_eq!(
            mac.run(std::slice::from_ref(&entry)),
            deferral(reason, &detail)
        );
        assert_eq!(mac.store.asked().len(), 1, "{detail}");
        mac.assert_clean();
    }
    let mut mac = Mac::new("xcs-sink");
    mac.store = FakeStore::holding(ARCHIVE_BYTES, ARCHIVE_BYTES).playing(vec![Read::Fails(
        StoreError::Get(GetError::Sink(io::ErrorKind::StorageFull)),
    )]);
    let refused = mac.run(std::slice::from_ref(&entry));
    assert!(
        matches!(
            refused,
            Err(KindError::Deferred {
                reason: REASON_STAGING_FAILED,
                ..
            })
        ),
        "{refused:?}"
    );
    mac.assert_clean();
}

/// Proves: an Xcode installed as `/Applications/Xcode-<version>.app` with the order's build is
/// passed over (no read of the store, the space or a program), and one under that name with
/// another build, or with the order's build under another version, is staged.
#[test]
fn an_installed_xcode_is_passed_over_and_another_build_is_staged() {
    let entry = entry_of(ARCHIVE_BYTES);
    let installed = "/Applications/Xcode-27.2.app/Contents/version.plist";
    let mut mac = Mac::new("xcs-installed");
    mac.file(installed, V27);
    mac.space.free = Err("not to be read".to_owned());
    assert_eq!(mac.run(std::slice::from_ref(&entry)), Ok(()));
    assert!(mac.store.asked().is_empty());
    assert!(mac.runner.ran().is_empty());
    assert!(!mac.path(STAGING).exists());

    let other_version = String::from_utf8_lossy(V27).replace(">27.2<", ">27.2.1<");
    for (name, plist) in [("other", V26), ("other-version", other_version.as_bytes())] {
        let mac = Mac::new(&format!("xcs-installed-{name}"));
        mac.file(installed, plist);
        assert_eq!(mac.run(std::slice::from_ref(&entry)), Ok(()), "{name}");
        assert_eq!(mac.store.asked().len(), 1, "{name}");
    }

    let mac = Mac::new("xcs-installed-garbage");
    mac.file(installed, b"not a property list");
    assert_eq!(
        mac.run(&[entry]),
        deferral(
            REASON_INSTALLED_UNREADABLE,
            &format!("{installed} is not a property list")
        )
    );
    assert!(mac.store.asked().is_empty());
}

/// Proves: an app an earlier stage verified is checked again (its build and `codesign`) and not
/// fetched, with no space needed; and a staged app that is no longer the order's build defers
/// `xcode_build_mismatch` and is removed.
#[test]
fn a_staged_app_is_verified_again_and_not_fetched() {
    let entry = entry_of(ARCHIVE_BYTES);
    let app = format!("{STAGING}/{}/Xcode.app", entry.sha256);
    let mut mac = Mac::new("xcs-staged");
    mac.trusted_staging();
    mac.file(&format!("{app}/Contents/version.plist"), V27);
    mac.space.free = Err("not to be read".to_owned());
    assert_eq!(mac.run(std::slice::from_ref(&entry)), Ok(()));
    assert!(mac.store.asked().is_empty());
    assert_eq!(mac.runner.ran(), [mac.codesign_run(&app)]);

    let mac = Mac::new("xcs-staged-gone");
    mac.trusted_staging();
    fs::create_dir_all(mac.path(&app)).expect("the app");
    assert_eq!(
        mac.run(&[entry]),
        deferral(
            REASON_BUILD_MISMATCH,
            &format!("{app}/Contents/version.plist is missing")
        )
    );
    assert!(mac.runner.ran().is_empty());
    mac.assert_clean();
}

/// Proves: what the staging directory holds that is not an entry of the order (a directory and a
/// file an older run left) is removed, and the entry's own directory is kept.
#[test]
fn what_is_not_an_entry_of_the_order_is_removed_from_staging() {
    let mac = Mac::new("xcs-leftovers");
    mac.trusted_staging();
    mac.file(&format!("{STAGING}/{}/Xcode.app/x", "a".repeat(64)), b"old");
    mac.file(&format!("{STAGING}/stray"), b"stray");
    let entry = entry_of(ARCHIVE_BYTES);
    assert_eq!(mac.run(std::slice::from_ref(&entry)), Ok(()));
    assert_eq!(mac.names(STAGING), [entry.sha256]);
}

/// Proves: an order with no Xcode stages nothing and reads nothing.
#[test]
fn an_order_with_no_xcode_stages_nothing() {
    let mut mac = Mac::new("xcs-none");
    mac.space.free = Err("not to be read".to_owned());
    assert_eq!(mac.run(&[]), Ok(()));
    assert!(mac.store.asked().is_empty());
    assert!(mac.runner.ran().is_empty());
}

/// Proves: what stands at the staging directory's path and is not a directory only the stage's
/// user can use (a link to another directory, a directory others can read, a file) is removed
/// without being followed, the stage makes its own and stages the app; the link's target and
/// what it holds are left as they were.
#[test]
fn a_staging_directory_the_stage_did_not_make_is_replaced_and_not_followed() {
    let entry = entry_of(ARCHIVE_BYTES);
    for case in ["link", "open", "file"] {
        let mac = Mac::new("xcs-untrusted");
        let target = mac.path("/elsewhere");
        fs::create_dir(&target).expect("the target");
        fs::write(target.join("kept"), b"kept").expect("a file in the target");
        match case {
            "link" => std::os::unix::fs::symlink(&target, mac.path(STAGING)).expect("the link"),
            "open" => {
                fs::create_dir(mac.path(STAGING)).expect("the directory");
                fs::set_permissions(mac.path(STAGING), fs::Permissions::from_mode(0o755))
                    .expect("its mode");
                fs::write(mac.path(&format!("{STAGING}/stray")), b"x").expect("a file");
            }
            _ => mac.file(STAGING, b"a file"),
        }
        assert_eq!(mac.run(std::slice::from_ref(&entry)), Ok(()), "{case}");
        assert_eq!(mac.names(STAGING), [entry.sha256.as_str()], "{case}");
        let meta = fs::symlink_metadata(mac.path(STAGING)).expect("the staging directory");
        assert!(meta.is_dir(), "{case}");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700, "{case}");
        assert_eq!(
            fs::read(target.join("kept")).expect("kept"),
            b"kept",
            "{case}"
        );
        assert_eq!(mac.names("/elsewhere"), ["kept"], "{case}");
    }
}

/// Proves: a staging directory that cannot be made (no `/Applications` to make it in), and an
/// archive path that holds a directory, each defer `xcode_staging_failed` before anything is
/// expanded.
#[test]
fn a_staging_directory_that_cannot_be_written_defers() {
    let entry = entry_of(ARCHIVE_BYTES);
    let mac = Mac::new("xcs-no-applications");
    fs::remove_dir(mac.path(APPLICATIONS)).expect("no Applications");
    let refused = mac.run(std::slice::from_ref(&entry));
    assert_eq!(
        refused,
        deferral(
            REASON_STAGING_FAILED,
            &format!(
                "cannot make {}: No such file or directory (os error 2)",
                mac.path(STAGING).display()
            )
        )
    );
    assert!(mac.store.asked().is_empty());

    let mac = Mac::new("xcs-archive-dir");
    mac.trusted_staging();
    let archive = mac.path(&format!("{STAGING}/{}/{ARCHIVE}", entry.sha256));
    fs::create_dir_all(&archive).expect("a directory in the archive's place");
    let refused = mac.run(std::slice::from_ref(&entry));
    assert!(
        matches!(
            refused,
            Err(KindError::Deferred {
                reason: REASON_STAGING_FAILED,
                ..
            })
        ),
        "{refused:?}"
    );
    assert!(mac.store.asked().is_empty());
    assert!(mac.runner.ran().is_empty());
    mac.assert_clean();
}

/// Proves: a directory put in the staging directory's place after the stage first looked at it
/// (here, while the free space is read) and before the fetch is not written through: the stage
/// defers `xcode_staging_failed` naming it, and nothing is fetched.
#[test]
fn a_staging_directory_swapped_in_before_the_fetch_is_refused() {
    let mut mac = Mac::new("xcs-swapped");
    mac.space.swap_in = Some(mac.path(STAGING));
    assert_eq!(
        mac.run(&[entry_of(ARCHIVE_BYTES)]),
        deferral(
            REASON_STAGING_FAILED,
            &format!(
                "{} is not a directory only this process's user can write",
                mac.path(STAGING).display()
            )
        )
    );
    assert!(mac.store.asked().is_empty());
    assert!(mac.runner.ran().is_empty());
    mac.assert_clean();
}

// The stage as the maintenance hook runs it, and the stage over an archive a stage left.
#[path = "stage_hook_tests.rs"]
mod hook;
