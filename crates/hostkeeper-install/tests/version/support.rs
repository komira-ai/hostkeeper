//! Shared by the tests of the version directory: scratch directories and a [`PackageFs`] in front
//! of the real one that records every call and can fail or crash at any.
#![allow(dead_code, reason = "each test module uses part of the shared support")]

use std::cell::{Cell, RefCell};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use hostkeeper_install::version::{HostFs, Owner, PackageFs, ServiceLock, StagedFile};
use sha2::{Digest, Sha256};

/// A directory under the target directory, unique to the test. Removed when dropped, read-only
/// trees included.
pub struct Scratch(hostkeeper_testkit::Scratch);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        Scratch(hostkeeper_testkit::Scratch::under(base, name).expect("create the scratch"))
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        open_up(self.path());
    }
}

/// Gives every directory below `path` mode 0700 so the tree can be removed.
pub fn open_up(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        for entry in fs::read_dir(path).into_iter().flatten().flatten() {
            open_up(&entry.path());
        }
    }
}

/// The service every test publishes.
pub const SERVICE: &str = "tierstore";
/// Its version.
pub const VERSION: &str = "1.5.0";

/// The manifest of the tree the format tests publish.
pub const MANIFEST: &str = "schema = 1
name = \"tierstore\"
version = \"1.5.0\"
platform = \"linux-x86_64\"
min_hostkeeper = \"0.1.0\"
[hook]
command = [\"bin/x\", \"update-hook\"]
verify_deadline_secs = 2
[hook.timeout_secs]
prepare = 5
cutover = 1
verify = 1
rollback = 1
cleanup = 1
[[files]]
source = \"files/c.conf\"
path = \"/etc/tierstore/c.conf\"
";

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A rig: a packages directory with its real port.
pub struct Rig {
    pub scratch: Scratch,
    pub packages: PathBuf,
}

impl Rig {
    pub fn new(name: &str) -> Rig {
        let scratch = Scratch::new(name);
        let packages = scratch.path().join("packages");
        fs::create_dir_all(&packages).unwrap();
        Rig { scratch, packages }
    }

    /// The real port over the packages directory, owned by the running user.
    pub fn host(&self) -> HostFs {
        HostFs::open(&self.packages, me()).unwrap()
    }

    /// `packages/tierstore`.
    pub fn service(&self) -> PathBuf {
        self.packages.join(SERVICE)
    }

    /// The names in `packages/tierstore`, sorted; empty when it is not there.
    pub fn listing(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.service())
            .into_iter()
            .flatten()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

pub fn me() -> Owner {
    Owner {
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    }
}

/// The owner of `path`.
pub fn owner_of(path: &Path) -> (u32, u32) {
    let meta = fs::symlink_metadata(path).unwrap();
    (meta.uid(), meta.gid())
}

/// One call on the port, as [`Faulty`] records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    ReadDir(PathBuf),
    Exists(PathBuf),
    CreateService(PathBuf),
    Lock(PathBuf),
    CreateStage(PathBuf),
    CreateFile(PathBuf),
    Write(PathBuf, usize),
    FileMode(PathBuf, u32),
    FileSync(PathBuf),
    SetMode(PathBuf, u32),
    SyncDir(PathBuf),
    Rename(PathBuf, PathBuf),
    RemoveTree(PathBuf),
}

impl Call {
    /// Whether the call writes: anything but a listing, a look, the lock, and the creation of
    /// the service directory, which changes nothing when it is there already (the tests that
    /// count writes run where it is).
    pub fn writes(&self) -> bool {
        !matches!(
            self,
            Call::ReadDir(_) | Call::Exists(_) | Call::Lock(_) | Call::CreateService(_)
        )
    }

    /// The paths the call names.
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Call::Rename(a, b) => vec![a, b],
            Call::ReadDir(p)
            | Call::Exists(p)
            | Call::CreateService(p)
            | Call::Lock(p)
            | Call::CreateStage(p)
            | Call::CreateFile(p)
            | Call::Write(p, _)
            | Call::FileMode(p, _)
            | Call::FileSync(p)
            | Call::SetMode(p, _)
            | Call::SyncDir(p)
            | Call::RemoveTree(p) => vec![p],
        }
    }
}

/// The errno of a refused permission.
pub const EACCES: i32 = 13;

/// When [`Faulty`] fails a call.
pub type Matcher = Box<dyn Fn(&Call) -> bool>;

/// What a hook runs.
type Hook = Rc<dyn Fn(&Call)>;

#[derive(Default)]
struct State {
    calls: RefCell<Vec<Call>>,
    /// Every call from this index on fails, as if the process had died.
    crash_at: Cell<Option<usize>>,
    dead: Cell<bool>,
    /// The `nth` call (from 0) that the matcher takes fails with the errno; with `every`,
    /// every call it takes from the `nth` on.
    fail: RefCell<Option<(Matcher, usize, i32)>>,
    every: Cell<bool>,
    taken: Cell<usize>,
    /// Writes to a file whose path ends with this reach the disk with their first byte changed.
    corrupt: RefCell<Option<String>>,
    /// A file or directory whose name ends with the first is given the second mode instead.
    dir_mode: RefCell<Option<(String, u32)>>,
    /// A write that fails writes the first half of its bytes before it fails.
    partial: Cell<bool>,
    /// Run once, after the first call the matcher takes is recorded and before it is made.
    hook: RefCell<Option<(Matcher, Hook)>>,
    hooked: Cell<bool>,
    /// A rename of a directory that its owner cannot write fails with `EACCES`, as macOS fails
    /// it for a caller that is not root.
    mac_rename: Cell<bool>,
}

/// The real port with every call recorded, and a failure or a crash where a test asks.
#[derive(Clone)]
pub struct Faulty {
    inner: Rc<HostFs>,
    state: Rc<State>,
}

impl Faulty {
    pub fn new(inner: HostFs) -> Faulty {
        Faulty {
            inner: Rc::new(inner),
            state: Rc::new(State::default()),
        }
    }

    /// Every call from index `at` on fails.
    pub fn crash_at(self, at: usize) -> Faulty {
        self.state.crash_at.set(Some(at));
        self
    }

    /// The `nth` call that `matcher` takes fails with `errno`.
    pub fn fail(self, matcher: Matcher, nth: usize, errno: i32) -> Faulty {
        *self.state.fail.borrow_mut() = Some((matcher, nth, errno));
        self
    }

    /// Every call that `matcher` takes fails with `errno`.
    pub fn fail_all(self, matcher: Matcher, errno: i32) -> Faulty {
        self.state.every.set(true);
        self.fail(matcher, 0, errno)
    }

    pub fn corrupt(self, suffix: &str) -> Faulty {
        *self.state.corrupt.borrow_mut() = Some(suffix.to_owned());
        self
    }

    pub fn dir_mode(self, suffix: &str, mode: u32) -> Faulty {
        *self.state.dir_mode.borrow_mut() = Some((suffix.to_owned(), mode));
        self
    }

    /// A failing write writes half its bytes first.
    pub fn partial(self) -> Faulty {
        self.state.partial.set(true);
        self
    }

    /// Runs `action` once, at the first call `matcher` takes, before the call is made.
    pub fn at(self, matcher: Matcher, action: impl Fn(&Call) + 'static) -> Faulty {
        *self.state.hook.borrow_mut() = Some((matcher, Rc::new(action)));
        self
    }

    /// Renames as macOS does for a caller that is not root: a directory without its owner's
    /// write bit is not renamed (`EACCES`), even to a new name in the same parent. Linux
    /// refuses it only when the parent changes, which an unpack never does.
    pub fn mac_rename(self) -> Faulty {
        self.state.mac_rename.set(true);
        self
    }

    pub fn calls(&self) -> Vec<Call> {
        self.state.calls.borrow().clone()
    }

    /// The bytes written through the port.
    pub fn written(&self) -> usize {
        self.calls()
            .iter()
            .map(|c| if let Call::Write(_, n) = c { *n } else { 0 })
            .sum()
    }

    fn call(&self, call: Call) -> io::Result<()> {
        record(&self.state, call)
    }
}

fn record(state: &State, call: Call) -> io::Result<()> {
    let index = state.calls.borrow().len();
    let failing = state
        .fail
        .borrow()
        .as_ref()
        .and_then(|(matcher, nth, errno)| matcher(&call).then_some((*nth, *errno)));
    let hook = state
        .hook
        .borrow()
        .as_ref()
        .filter(|(matcher, _)| !state.hooked.get() && matcher(&call))
        .map(|(_, action)| action.clone());
    state.calls.borrow_mut().push(call.clone());
    if let Some(action) = hook {
        state.hooked.set(true);
        action(&call);
    }
    if state.crash_at.get() == Some(index) {
        state.dead.set(true);
    }
    if state.dead.get() {
        return Err(io::Error::other("the process died here"));
    }
    if let Some((nth, errno)) = failing {
        let taken = state.taken.get();
        state.taken.set(taken + 1);
        if (taken == nth) | (state.every.get() & (taken >= nth)) {
            return Err(io::Error::from_raw_os_error(errno));
        }
    }
    Ok(())
}

impl PackageFs for Faulty {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<std::ffi::OsString>> {
        self.call(Call::ReadDir(dir.to_path_buf()))?;
        self.inner.read_dir(dir)
    }

    fn exists(&self, path: &Path) -> io::Result<bool> {
        self.call(Call::Exists(path.to_path_buf()))?;
        self.inner.exists(path)
    }

    fn create_service(&self, service: &Path) -> io::Result<()> {
        self.call(Call::CreateService(service.to_path_buf()))?;
        self.inner.create_service(service)
    }

    fn lock(&self, service: &Path) -> io::Result<ServiceLock> {
        self.call(Call::Lock(service.to_path_buf()))?;
        self.inner.lock(service)
    }

    fn create_stage(&self, service: &Path) -> io::Result<PathBuf> {
        self.call(Call::CreateStage(service.to_path_buf()))?;
        self.inner.create_stage(service)
    }

    fn create_file(&self, path: &Path) -> io::Result<Box<dyn StagedFile>> {
        self.call(Call::CreateFile(path.to_path_buf()))?;
        let inner = self.inner.create_file(path)?;
        Ok(Box::new(FaultyFile {
            path: path.to_path_buf(),
            inner,
            state: self.state.clone(),
        }))
    }

    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.call(Call::SetMode(path.to_path_buf(), mode))?;
        let swap = self.state.dir_mode.borrow().clone();
        let mode = match swap {
            Some((suffix, other)) if path.to_string_lossy().ends_with(&suffix) => other,
            _ => mode,
        };
        self.inner.set_mode(path, mode)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.call(Call::SyncDir(path.to_path_buf()))?;
        self.inner.sync_dir(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.call(Call::Rename(from.to_path_buf(), to.to_path_buf()))?;
        if self.state.mac_rename.get() {
            let meta = fs::symlink_metadata(self.inner.real_path(from))?;
            if meta.is_dir() & (meta.mode() & 0o200 == 0) {
                return Err(io::Error::from_raw_os_error(EACCES));
            }
        }
        self.inner.rename(from, to)
    }

    fn remove_tree(&self, path: &Path) -> io::Result<()> {
        self.call(Call::RemoveTree(path.to_path_buf()))?;
        self.inner.remove_tree(path)
    }

    fn real_path(&self, path: &Path) -> PathBuf {
        self.inner.real_path(path)
    }
}

struct FaultyFile {
    path: PathBuf,
    inner: Box<dyn StagedFile>,
    state: Rc<State>,
}

impl StagedFile for FaultyFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if let Err(e) = record(&self.state, Call::Write(self.path.clone(), bytes.len())) {
            if self.state.partial.get() {
                let _ = self.inner.write(&bytes[..bytes.len() / 2]);
            }
            return Err(e);
        }
        let suffix = self.state.corrupt.borrow().clone();
        let corrupt = suffix.is_some_and(|s| self.path.to_string_lossy().ends_with(&s));
        if corrupt && !bytes.is_empty() {
            let mut changed = bytes.to_vec();
            changed[0] ^= 0xff;
            return self.inner.write(&changed);
        }
        self.inner.write(bytes)
    }

    fn set_mode(&mut self, mode: u32) -> io::Result<()> {
        record(&self.state, Call::FileMode(self.path.clone(), mode))?;
        let swap = self.state.dir_mode.borrow().clone();
        let mode = match swap {
            Some((suffix, other)) if self.path.to_string_lossy().ends_with(&suffix) => other,
            _ => mode,
        };
        self.inner.set_mode(mode)
    }

    fn sync(&mut self) -> io::Result<()> {
        record(&self.state, Call::FileSync(self.path.clone()))?;
        self.inner.sync()
    }
}
