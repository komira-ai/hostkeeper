//! Shared by the tests: scratch directories, an archive builder that writes tar headers and zstd
//! frames byte by byte (so a test can make what a tar tool refuses to make), and a
//! [`PackageFs`] in front of the real one that records every call and can fail or crash at any.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::cell::{Cell, RefCell};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_unpack::{HostFs, Limits, Owner, PackageFs, StagedFile, UnpackOrder};
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

/// The service every test unpacks.
pub const SERVICE: &str = "tierstore";
/// Its version.
pub const VERSION: &str = "1.5.0";

/// A manifest the loader takes for tierstore 1.5.0.
pub const MANIFEST: &str = "schema = 1
name = \"tierstore\"
version = \"1.5.0\"
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

/// One tar header's fields that a test may set.
#[derive(Clone, Debug)]
pub struct Head {
    pub name: Vec<u8>,
    pub flag: u8,
    pub mode: u64,
    pub size: u64,
    pub uid: u64,
    pub gid: u64,
    pub mtime: u64,
    /// `true` for a GNU header (`ustar  \0`), `false` for POSIX (`ustar\0` `00`).
    pub gnu: bool,
}

impl Head {
    pub fn new(name: &[u8], flag: u8, mode: u64, size: u64) -> Head {
        Head {
            name: name.to_vec(),
            flag,
            mode,
            size,
            uid: 1000,
            gid: 1000,
            mtime: 1_800_000_000,
            gnu: false,
        }
    }
}

/// A tar archive built header by header.
#[derive(Clone, Debug, Default)]
pub struct Tar {
    pub bytes: Vec<u8>,
}

/// An octal field of `width` bytes: digits, then NUL.
fn octal(value: u64, width: usize) -> Vec<u8> {
    let mut out = format!("{value:0w$o}", w = width - 1).into_bytes();
    out.push(0);
    out
}

impl Tar {
    pub fn new() -> Tar {
        Tar::default()
    }

    /// A header block for `head`. A name over 100 bytes goes to the POSIX prefix field at its
    /// last `/` that fits; the test must not ask for more than that holds.
    pub fn header_block(head: &Head) -> [u8; 512] {
        let mut block = [0u8; 512];
        let (prefix, name) = if head.name.len() <= 100 {
            (&[][..], &head.name[..])
        } else {
            let cut = head.name[..head.name.len().min(156)]
                .iter()
                .rposition(|&b| b == b'/')
                .expect("a long name has a / in its first 156 bytes");
            (&head.name[..cut], &head.name[cut + 1..])
        };
        assert!(
            name.len() <= 100 && prefix.len() <= 155,
            "name too long for ustar"
        );
        block[..name.len()].copy_from_slice(name);
        block[100..108].copy_from_slice(&octal(head.mode, 8));
        block[108..116].copy_from_slice(&octal(head.uid, 8));
        block[116..124].copy_from_slice(&octal(head.gid, 8));
        block[124..136].copy_from_slice(&octal(head.size, 12));
        block[136..148].copy_from_slice(&octal(head.mtime, 12));
        block[156] = head.flag;
        if head.gnu {
            block[257..265].copy_from_slice(b"ustar  \x00");
        } else {
            block[257..265].copy_from_slice(b"ustar\x0000");
            block[345..345 + prefix.len()].copy_from_slice(prefix);
        }
        block[148..156].copy_from_slice(b"        ");
        let sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
        let mut field = format!("{sum:06o}").into_bytes();
        field.extend_from_slice(b"\0 ");
        block[148..156].copy_from_slice(&field);
        block
    }

    /// A header, then `data` padded to the block.
    pub fn entry(&mut self, head: &Head, data: &[u8]) -> &mut Tar {
        self.bytes.extend_from_slice(&Tar::header_block(head));
        self.data(data)
    }

    /// `data`, padded with zeros to the block.
    pub fn data(&mut self, data: &[u8]) -> &mut Tar {
        self.bytes.extend_from_slice(data);
        let pad = (512 - data.len() % 512) % 512;
        self.bytes.extend(std::iter::repeat_n(0u8, pad));
        self
    }

    pub fn file(&mut self, name: &str, mode: u64, data: &[u8]) -> &mut Tar {
        let head = Head::new(name.as_bytes(), b'0', mode, data.len() as u64);
        self.entry(&head, data)
    }

    /// A directory entry, its name ending in `/` as tar tools write it.
    pub fn dir(&mut self, name: &str) -> &mut Tar {
        let head = Head::new(format!("{name}/").as_bytes(), b'5', 0o755, 0);
        self.entry(&head, &[])
    }

    /// A pax extended header holding `records` (key, value).
    pub fn pax(&mut self, records: &[(&str, &[u8])]) -> &mut Tar {
        let mut data = Vec::new();
        for (key, value) in records {
            data.extend_from_slice(&pax_record(key, value));
        }
        let head = Head::new(b"PaxHeaders/x", b'x', 0o644, data.len() as u64);
        self.entry(&head, &data)
    }

    /// A GNU long-name header for `name` (NUL-terminated, as GNU tar writes it).
    pub fn long_name(&mut self, name: &[u8]) -> &mut Tar {
        let mut data = name.to_vec();
        data.push(0);
        let mut head = Head::new(b"././@LongLink", b'L', 0o644, data.len() as u64);
        head.gnu = true;
        self.entry(&head, &data)
    }

    /// The end-of-archive marker: two zero blocks.
    pub fn end(&mut self) -> &mut Tar {
        self.bytes.extend_from_slice(&[0u8; 1024]);
        self
    }

    pub fn raw(&mut self, bytes: &[u8]) -> &mut Tar {
        self.bytes.extend_from_slice(bytes);
        self
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

/// One pax record, its length counting itself.
pub fn pax_record(key: &str, value: &[u8]) -> Vec<u8> {
    let body = key.len() + value.len() + 3;
    let mut len = body + body.to_string().len();
    if len.to_string().len() != body.to_string().len() {
        len += 1;
    }
    let mut out = format!("{len} {key}=").into_bytes();
    out.extend_from_slice(value);
    out.push(b'\n');
    assert_eq!(out.len(), len);
    out
}

/// The window descriptor of a 1 MiB window.
pub const WINDOW_1M: u8 = 10 << 3;

/// A zstd frame header: magic, a descriptor with no checksum, no dictionary and no content
/// size, and the window descriptor `window`.
pub fn frame_header(window: u8) -> Vec<u8> {
    vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, window]
}

/// A block header: last flag, type (0 raw, 1 RLE), size.
pub fn block_header(last: bool, kind: u32, size: u32) -> [u8; 3] {
    let value = (size << 3) | (kind << 1) | u32::from(last);
    let bytes = value.to_le_bytes();
    [bytes[0], bytes[1], bytes[2]]
}

/// One zstd frame of raw blocks holding `data` (the decoder decompresses it to `data`).
pub fn zstd(data: &[u8]) -> Vec<u8> {
    zstd_with(data, WINDOW_1M, &[])
}

/// A frame of window `window` holding `data` in raw blocks, then `extra` blocks (already
/// framed) before the last, empty block.
pub fn zstd_with(data: &[u8], window: u8, extra: &[u8]) -> Vec<u8> {
    let mut out = frame_header(window);
    for chunk in data.chunks(128 * 1024) {
        out.extend_from_slice(&block_header(false, 0, chunk.len() as u32));
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(extra);
    out.extend_from_slice(&block_header(true, 0, 0));
    out
}

/// `count` RLE blocks of 128 KiB of `byte`: four bytes each, 128 KiB each decompressed.
pub fn rle_blocks(byte: u8, count: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(count * 4);
    for _ in 0..count {
        out.extend_from_slice(&block_header(false, 1, 128 * 1024));
        out.push(byte);
    }
    out
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The tar of a valid package: the manifest, `bin/x`, `units/tierstore.service`,
/// `files/c.conf`.
pub fn valid_tar() -> Tar {
    let mut tar = Tar::new();
    tar.file("manifest.toml", 0o644, MANIFEST.as_bytes())
        .dir("bin")
        .file("bin/x", 0o755, b"#!x\n")
        .dir("units")
        .file("units/tierstore.service", 0o644, b"[Service]\n")
        .dir("files")
        .file("files/c.conf", 0o640, b"c\n");
    tar
}

/// A rig: a packages directory with its real port, and archives written beside it.
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

    /// Writes `archive` (the compressed bytes) and returns its path and the order for it.
    pub fn archive(&self, name: &str, archive: &[u8]) -> (PathBuf, UnpackOrder) {
        let path = self.scratch.path().join(name);
        fs::write(&path, archive).unwrap();
        (path, order(VERSION, &sha256_hex(archive)))
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

pub fn order(version: &str, digest: &str) -> UnpackOrder {
    UnpackOrder {
        service: ServiceName::new(SERVICE).unwrap(),
        version: Version::new(version).unwrap(),
        package_digest: Sha256Hex::parse(digest).unwrap(),
    }
}

pub fn me() -> Owner {
    Owner {
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    }
}

pub fn limits() -> Limits {
    Limits {
        max_unpacked_bytes: 1 << 30,
        max_entries: 1000,
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
    CreateStage(PathBuf),
    CreateDir(PathBuf),
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
    /// Whether the call writes (anything but a listing).
    pub fn writes(&self) -> bool {
        !matches!(self, Call::ReadDir(_))
    }

    /// The paths the call names.
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Call::Rename(a, b) => vec![a, b],
            Call::ReadDir(p)
            | Call::CreateStage(p)
            | Call::CreateDir(p)
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

/// When [`Faulty`] fails a call.
pub type Matcher = Box<dyn Fn(&Call) -> bool>;

#[derive(Default)]
struct State {
    calls: RefCell<Vec<Call>>,
    /// Every call from this index on fails, as if the process had died.
    crash_at: Cell<Option<usize>>,
    dead: Cell<bool>,
    /// The `nth` call (from 0) that the matcher takes fails with the errno.
    fail: RefCell<Option<(Matcher, usize, i32)>>,
    taken: Cell<usize>,
    /// Writes to a file whose path ends with this reach the disk with their first byte changed.
    corrupt: RefCell<Option<String>>,
    /// A file or directory whose name ends with the first is given the second mode instead.
    dir_mode: RefCell<Option<(String, u32)>>,
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

    pub fn corrupt(self, suffix: &str) -> Faulty {
        *self.state.corrupt.borrow_mut() = Some(suffix.to_owned());
        self
    }

    pub fn dir_mode(self, suffix: &str, mode: u32) -> Faulty {
        *self.state.dir_mode.borrow_mut() = Some((suffix.to_owned(), mode));
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
    state.calls.borrow_mut().push(call);
    if state.crash_at.get() == Some(index) {
        state.dead.set(true);
    }
    if state.dead.get() {
        return Err(io::Error::other("the process died here"));
    }
    if let Some((nth, errno)) = failing {
        let taken = state.taken.get();
        state.taken.set(taken + 1);
        if taken == nth {
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

    fn create_stage(&self, service: &Path) -> io::Result<PathBuf> {
        self.call(Call::CreateStage(service.to_path_buf()))?;
        self.inner.create_stage(service)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.call(Call::CreateDir(path.to_path_buf()))?;
        self.inner.create_dir(path)
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
        record(&self.state, Call::Write(self.path.clone(), bytes.len()))?;
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
