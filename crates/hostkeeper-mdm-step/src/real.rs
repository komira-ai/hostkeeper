//! The machine.
//!
//! Every change to the file system walks from the root directory one component at a time with
//! `openat(O_NOFOLLOW | O_DIRECTORY)`, so a symbolic link in any directory on the way (a
//! directory the service user owns can be made one at any time) is refused instead of followed.
//! Directories are made with `mkdirat`, and mode and owner are set on the open descriptor
//! (`fchmod`, `fchown`), never on a path. A file is written to a temporary name of its own in
//! its directory through the parent's descriptor and renamed over the target with `renameat`.
//!
//! Every directory on the way to a path must be owned by a user trusted for that path: a user
//! the host trusts everywhere (root and the effective user, by default), the owner a write or a
//! mkdir asks for, or a user granted the paths below a directory with
//! [`RealHost::trust_below`].

use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt, fchown};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{
    AtFlags, FileType, Mode as FileMode, OFlags, mkdirat, openat, renameat, statat, unlinkat,
};
use rustix::io::Errno;

use crate::{Cmd, Host, Kind, Meta, Output};

/// The end of every temporary file name a write uses: `NAME.PID-N.hostkeeper-mdm.tmp`.
pub const TEMP_SUFFIX: &str = ".hostkeeper-mdm.tmp";

/// How many temporary names a write tries before it gives up: a name is taken only by a writer
/// of another process with the same id (another pid namespace) or by a file a crashed process
/// left.
const TEMP_TRIES: usize = 16;

/// The number of the next temporary name this process uses.
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// The real file system and the real programs, under a root directory.
#[derive(Clone, Debug)]
pub struct RealHost {
    root: PathBuf,
    trusted: Vec<u32>,
    grants: Vec<(Vec<String>, u32)>,
}

impl RealHost {
    /// The machine itself: paths are the system's.
    pub fn new() -> RealHost {
        RealHost::under("/")
    }

    /// The machine with every path taken below `root` (a directory standing in for `/`).
    /// Programs still run on the machine. Directories owned by root or by the effective user
    /// are trusted on the way to any path.
    pub fn under(root: impl Into<PathBuf>) -> RealHost {
        let euid = rustix::process::geteuid().as_raw();
        RealHost::trusting(root, &[0, euid])
    }

    /// Like [`RealHost::under`], trusting directories owned by `uids` on the way to any path,
    /// and no other user but the owner a write or a mkdir asks for and the grants of
    /// [`RealHost::trust_below`].
    pub fn trusting(root: impl Into<PathBuf>, uids: &[u32]) -> RealHost {
        RealHost {
            root: root.into(),
            trusted: uids.to_vec(),
            grants: Vec::new(),
        }
    }

    /// This host, also trusting directories owned by `uid` on the way to `dir` and to every
    /// path below it, for every operation: the operator's user for the directory of the
    /// binaries it fetched, the service's user for its state directory. The grant is by
    /// component, so `/var/lib/svc` grants nothing on `/var/lib/svc2`. A path to any other
    /// place is walked as before. A `dir` with a `.` or `..` component grants nothing, as
    /// every operation refuses such a path.
    #[must_use]
    pub fn trust_below(mut self, dir: &str, uid: u32) -> RealHost {
        let names = dir
            .split('/')
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .collect();
        self.grants.push((names, uid));
        self
    }

    fn at(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    /// The users trusted on the way to the path of components `names`: the host's, `owner`
    /// when a step asks for one, and those granted a directory at or above the path.
    fn trusted_for(&self, names: &[&str], owner: Option<u32>) -> Vec<u32> {
        let granted = self
            .grants
            .iter()
            .filter(|(dir, _)| {
                dir.len() <= names.len() && dir.iter().zip(names).all(|(a, b)| a == b)
            })
            .map(|(_, uid)| *uid);
        self.trusted
            .iter()
            .copied()
            .chain(owner)
            .chain(granted)
            .collect()
    }

    /// The directory that holds `path` as a descriptor, and the name in it. The root and each
    /// directory on the way are opened without following links (the root too), made (0755) when
    /// `create` and missing, and must be owned by a user trusted for `path` (`owner` is the one
    /// a write or a mkdir asks for). A `.` or `..` component is refused here, whatever the
    /// caller checked.
    fn parent(&self, path: &str, create: bool, owner: Option<u32>) -> io::Result<(File, String)> {
        let all = components(path)?;
        let trusted = self.trusted_for(&all, owner);
        let mut names = all;
        let name = names.pop().ok_or_else(|| refused(path, "names no file"))?;
        let flags = OFlags::DIRECTORY | OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let anchor = openat(rustix::fs::CWD, &self.root, flags, FileMode::empty());
        let mut dir = File::from(anchor.map_err(|e| {
            let linked = is_link(rustix::fs::CWD, &self.root);
            walk_error(path, &self.root.display().to_string(), linked, e)
        })?);
        vet(&dir, &trusted, path, &self.root.display().to_string())?;
        for component in names {
            let opened = match openat(&dir, component, flags, FileMode::empty()) {
                Err(Errno::NOENT) if create => {
                    mkdirat(&dir, component, FileMode::from_raw_mode(0o755))?;
                    openat(&dir, component, flags, FileMode::empty())
                }
                other => other,
            };
            let next = File::from(
                opened.map_err(|e| walk_error(path, component, is_link(&dir, component), e))?,
            );
            vet(&next, &trusted, path, component)?;
            dir = next;
        }
        Ok((dir, name.to_owned()))
    }
}

impl Default for RealHost {
    fn default() -> RealHost {
        RealHost::new()
    }
}

/// Refuses a directory owned by a user that is not in `trusted`.
fn vet(dir: &File, trusted: &[u32], path: &str, label: &str) -> io::Result<()> {
    let uid = dir.metadata()?.uid();
    match trusted.contains(&uid) {
        true => Ok(()),
        false => Err(refused_at(
            path,
            &format!("{label} is owned by uid {uid}, which is not trusted for this path"),
        )),
    }
}

/// The temporary names of a write of `name` by this process: `NAME.PID-N` and the suffix, `N`
/// counting up across every write of the process, so no two writers (threads or processes)
/// ever pick the same name.
fn temp_names(name: &str) -> impl Iterator<Item = String> {
    let pid = std::process::id();
    std::iter::repeat_with(move || {
        let n = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        format!("{name}.{pid}-{n}{TEMP_SUFFIX}")
    })
}

/// Creates the first of `names` that is free in `dir`, 0600 so the bytes are never readable
/// wider than the mode asked for, and returns it with its name. A name that is taken is left
/// as it is: it is another writer's.
fn create_temp(
    dir: &File,
    path: &str,
    names: impl Iterator<Item = String>,
) -> io::Result<(File, String)> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    for temp in names.take(TEMP_TRIES) {
        match openat(dir, &temp, flags, FileMode::from_raw_mode(0o600)) {
            Ok(fd) => return Ok((File::from(fd), temp)),
            Err(Errno::EXIST) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("refused: {path}: no free temporary name in {TEMP_TRIES} tries"),
    ))
}

/// Writes `bytes` to the open temporary file `temp` in `dir`, sets mode and owner on the
/// descriptor and renames it to `name`.
fn put(
    dir: &File,
    mut file: File,
    temp: &str,
    name: &str,
    mode: u32,
    owner: (u32, u32),
    bytes: &[u8],
) -> io::Result<()> {
    file.write_all(bytes)?;
    file.set_permissions(Permissions::from_mode(mode))?;
    fchown(&file, Some(owner.0), Some(owner.1))?;
    file.sync_all()?;
    renameat(dir, temp, dir, name)?;
    // The new name is on disk only once the directory is.
    dir.sync_all()
}

/// The components of `path`, refusing `.` and `..`.
fn components(path: &str) -> io::Result<Vec<&str>> {
    let names: Vec<&str> = path.split('/').filter(|n| !n.is_empty()).collect();
    match names.iter().any(|n| *n == "." || *n == "..") {
        true => Err(refused(path, "has a . or .. component")),
        false => Ok(names),
    }
}

/// Whether `name` in `at` is a symbolic link (not followed). The system says "not a directory"
/// for a link opened as one, so this is how a link is told from a file.
fn is_link(at: impl AsFd, name: impl rustix::path::Arg) -> bool {
    statat(at, name, AtFlags::SYMLINK_NOFOLLOW)
        .is_ok_and(|st| FileType::from_raw_mode(st.st_mode).is_symlink())
}

/// The error for the directory `component` that could not be opened on the way to `path`;
/// `linked` is whether it is a symbolic link.
fn walk_error(path: &str, component: &str, linked: bool, e: Errno) -> io::Error {
    match (e, linked) {
        (Errno::LOOP | Errno::NOTDIR, true) => refused_at(
            path,
            &format!("{component} is a symbolic link; give the real path"),
        ),
        (Errno::NOTDIR, false) => refused_at(path, &format!("{component} is not a directory")),
        (other, _) => io::Error::from(other),
    }
}

/// A refusal that names the path and then what is wrong, as `refused: PATH: WHAT`.
fn refused_at(path: &str, what: &str) -> io::Error {
    io::Error::other(format!("refused: {path}: {what}"))
}

fn refused(path: &str, why: &str) -> io::Error {
    io::Error::other(format!("refused: {path} {why}"))
}

/// `result`, or `None` when the error is "not found".
fn found<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl Host for RealHost {
    fn euid(&self) -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn egid(&self) -> u32 {
        rustix::process::getegid().as_raw()
    }

    // Unlike every other operation this follows links on purpose: it is for a path the user
    // gave about their own account (`fetch --dir`), and what it is for is to see where the
    // links lead.
    fn canonical(&self, path: &str) -> io::Result<String> {
        let root = fs::canonicalize(&self.root)?;
        let mut rest: Vec<std::ffi::OsString> = Vec::new();
        let mut near = self.at(path);
        // The longest part that exists is resolved; the part that does not stays as given.
        let real = loop {
            match fs::canonicalize(&near) {
                Ok(real) => break real,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    rest.extend(near.file_name().map(ToOwned::to_owned));
                    near.pop();
                }
                Err(e) => return Err(e),
            }
        };
        let mut full = real;
        full.extend(rest.iter().rev());
        let below = full.strip_prefix(&root).unwrap_or(Path::new(""));
        Ok(format!("/{}", below.display()))
    }

    fn stat(&self, path: &str) -> io::Result<Option<Meta>> {
        let Some((dir, name)) = found(self.parent(path, false, None))? else {
            return Ok(None);
        };
        let seen = found(statat(&dir, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from))?;
        Ok(seen.map(|st| {
            let kind = FileType::from_raw_mode(st.st_mode);
            Meta {
                kind: match (kind.is_file(), kind.is_dir()) {
                    (true, _) => Kind::File,
                    (_, true) => Kind::Dir,
                    _ => Kind::Other,
                },
                mode: st.st_mode & 0o7777,
                uid: st.st_uid,
                gid: st.st_gid,
                size: u64::try_from(st.st_size).unwrap_or(0),
            }
        }))
    }

    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        self.read_limited(path, u64::MAX)
    }

    fn read_limited(&self, path: &str, limit: u64) -> io::Result<Option<Vec<u8>>> {
        let Some((dir, name)) = found(self.parent(path, false, None))? else {
            return Ok(None);
        };
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let opened = openat(&dir, &name, flags, FileMode::empty()).map_err(|e| match e {
            Errno::LOOP => refused_at(
                path,
                &format!("{name} is a symbolic link; give the real path"),
            ),
            other => io::Error::from(other),
        });
        let Some(fd) = found(opened)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        File::from(fd)
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)?;
        match bytes.len() as u64 > limit {
            true => Err(io::Error::from(io::ErrorKind::FileTooLarge)),
            false => Ok(Some(bytes)),
        }
    }

    fn list(&self, path: &str) -> io::Result<Option<Vec<String>>> {
        let Some((dir, name)) = found(self.parent(path, false, None))? else {
            return Ok(None);
        };
        let flags = OFlags::DIRECTORY | OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let opened = openat(&dir, &name, flags, FileMode::empty()).map_err(|e| match e {
            Errno::LOOP | Errno::NOTDIR => refused_at(
                path,
                &format!("{name} is a symbolic link or not a directory; give the real path"),
            ),
            other => io::Error::from(other),
        });
        let Some(fd) = found(opened)? else {
            return Ok(None);
        };
        let mut names = Vec::new();
        for entry in rustix::fs::Dir::read_from(&fd)? {
            let entry = entry?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let name = std::str::from_utf8(bytes)
                .map_err(|_| refused_at(path, "a name in it is not UTF-8"))?;
            names.push(name.to_owned());
        }
        names.sort();
        Ok(Some(names))
    }

    fn write(&self, path: &str, mode: u32, owner: (u32, u32), bytes: &[u8]) -> io::Result<()> {
        let (dir, name) = self.parent(path, false, Some(owner.0))?;
        let (file, temp) = create_temp(&dir, path, temp_names(&name))?;
        let done = put(&dir, file, &temp, &name, mode, owner, bytes);
        if done.is_err() {
            // A failed write leaves no temporary file behind; the name is this write's own.
            let _ = unlinkat(&dir, &temp, AtFlags::empty());
        }
        done
    }

    fn mkdir(&self, path: &str, mode: u32, owner: (u32, u32)) -> io::Result<()> {
        let (dir, name) = self.parent(path, true, Some(owner.0))?;
        match mkdirat(&dir, &name, FileMode::from_raw_mode(0o700)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(e) => return Err(e.into()),
        }
        let flags = OFlags::DIRECTORY | OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let opened = openat(&dir, &name, flags, FileMode::empty()).map_err(|_| {
            refused_at(
                path,
                &format!("{name} is a symbolic link or not a directory; give the real path"),
            )
        })?;
        let made = File::from(opened);
        made.set_permissions(Permissions::from_mode(mode))?;
        fchown(&made, Some(owner.0), Some(owner.1))?;
        // The directory's entry is on disk only once its parent is.
        dir.sync_all()
    }

    fn remove(&self, path: &str) -> io::Result<()> {
        let Some((dir, name)) = found(self.parent(path, false, None))? else {
            return Ok(());
        };
        found(unlinkat(&dir, &name, AtFlags::empty()).map_err(io::Error::from))?;
        // The removal is on disk only once the directory is.
        dir.sync_all()
    }

    fn run(&self, cmd: &Cmd) -> io::Result<Output> {
        let out = Command::new(&cmd.program)
            .args(&cmd.args)
            .env_clear()
            .stdin(Stdio::null())
            .output()?;
        Ok(Output {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches a writer that takes over a temporary name another writer holds, or that loops
    /// for ever when every name is taken: a taken name is skipped and left as it was, and
    /// after `TEMP_TRIES` taken names the write is refused.
    #[test]
    fn a_taken_temporary_name_is_skipped_and_left_alone() {
        let base = std::env::current_exe()
            .expect("the test binary")
            .parent()
            .expect("its directory")
            .join("hostkeeper-mdm-step-unit");
        let scratch = hostkeeper_testkit::Scratch::under(&base, "temp-names").expect("scratch");
        let dir = File::open(scratch.path()).expect("open the scratch directory");
        fs::write(scratch.path().join("t0"), b"another writer's").expect("plant t0");
        let names = (0..).map(|n| format!("t{n}"));
        let (_, temp) = create_temp(&dir, "/f", names).expect("a free name");
        assert_eq!(temp, "t1");
        assert_eq!(
            fs::read(scratch.path().join("t0")).expect("t0 is still there"),
            b"another writer's"
        );

        for n in 0..TEMP_TRIES {
            fs::write(scratch.path().join(format!("u{n}")), b"").expect("plant");
        }
        let names = (0..).map(|n| format!("u{n}"));
        let err = create_temp(&dir, "/f", names).expect_err("every name is taken");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            err.to_string(),
            "refused: /f: no free temporary name in 16 tries"
        );
        assert!(!scratch.path().join(format!("u{TEMP_TRIES}")).exists());
    }

    /// Catches temporary names that two writes of one process share, or that do not end with
    /// the suffix a leftover is recognised by.
    #[test]
    fn each_temporary_name_is_new_and_ends_with_the_suffix() {
        let names: Vec<String> = temp_names("f").take(3).collect();
        let prefix = format!("f.{}-", std::process::id());
        let numbers: Vec<Option<u64>> = names
            .iter()
            .map(|name| {
                let middle = name.strip_prefix(&prefix)?.strip_suffix(TEMP_SUFFIX)?;
                middle.parse().ok()
            })
            .collect();
        let distinct: std::collections::BTreeSet<_> = numbers.iter().flatten().collect();
        assert_eq!((numbers.len(), distinct.len()), (3, 3), "{names:?}");
    }
}
