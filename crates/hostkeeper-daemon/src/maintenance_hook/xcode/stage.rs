//! The stage of the `xcode` kind (docs/design/maintenance-updates.md, section 2.4 step 1): while
//! the Mac still serves, each Xcode of the run's order that is not installed is fetched from the
//! coordinator's store, checked, expanded and verified in a staging directory on the volume of
//! `/Applications`, [`STAGING`]. Nothing is written to `/Applications/Xcode-<version>.app`: the
//! move there is the drained host's `cutover`.
//!
//! For each entry of the order (`Maintain.xcodes`: version, build, size, SHA-256), in order:
//!
//! 1. **Checked.** The SHA-256 must be 64 lower-case hex characters (it names the one path the
//!    store is read by, `/v1/xcode/<sha256>`, and the entry's staging directory), the version
//!    digits and dots and the build letters and digits, each 1 to 16 bytes, the size above 0.
//! 2. **Passed over** when `/Applications/Xcode-<version>.app` is installed with that version and
//!    build.
//! 3. **Re-verified** when it is staged already, `<staging>/<sha256>/Xcode.app` (only a verified
//!    app is put there): its build and its code signature are checked again (steps 9 and 10).
//! 4. **Space.** Before anything is fetched, the volume must have free, for each entry to fetch,
//!    what is left of its archive to fetch and [`EXPANSION_FACTOR`] times its size for the
//!    expansion; short is deferred `free_space_short`.
//! 5. **Fetched** into `<staging>/<sha256>/Xcode.xip` from the store, from the byte the file
//!    already holds: a copy an interrupted hook left there is resumed with a range, and so is a
//!    connection that broke (cut short, refused, or past its deadline), up to
//!    [`MAX_FETCH_ATTEMPTS`] connections in all. A 404 is `xcode_not_in_store`.
//! 6. **Size and SHA-256.** The file must be exactly the order's size with the order's SHA-256,
//!    or it is refused `xcode_archive_mismatch` before `xip` runs.
//! 7. **Format.** An Apple Encrypted Archive (`AEA1`) is deferred `xcode_archive_encrypted`: it
//!    is expanded by `archiveutil` [V], whose use the Mac proof has not shown yet.
//! 8. **Expanded** by `xip --expand <archive>` in `<staging>/<sha256>/expand`, which checks
//!    Apple's signature on the archive [V]; the archive is removed after, and the expansion must
//!    hold exactly one app.
//! 9. **Build.** The app's `Contents/version.plist` must name the order's version
//!    (`CFBundleShortVersionString`) and build (`ProductBuildVersion`).
//! 10. **Signature.** `codesign --verify --deep --strict <app>` must exit 0. The app is then
//!     renamed `<staging>/<sha256>/Xcode.app`.
//!
//! The staging directory is taken only when it is a directory of the process's user (root) that
//! nobody else can read or write; anything else in its place (a link an admin of the Mac put
//! there, since `/Applications` is theirs to write) is removed without being followed, and the
//! directory made again, mode 0700.
//!
//! Whatever is in the staging directory and is not an entry of the order is removed first. An
//! order naming one archive twice is deferred `xcode_order_invalid`. Any
//! step that does not succeed is [`KindError::Deferred`] (the host never left service) and the
//! whole staging directory is removed. Each program runs as root by absolute path, with no
//! standard input and only [`PROGRAM_ENV`] as its environment, killed at its deadline.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_link::{GetError, Got, Limit};
use hostkeeper_package::gh::{Ran, Runner, Spawn};
use hostkeeper_proto::hostkeeper::agent::v1::XcodeEntry;
use hostkeeper_release::grammar::is_sha256;
use hostkeeper_steps::{Clock, Stamp};
use sha2::{Digest, Sha256};

use super::store::{Store, StoreError};
use crate::maintenance_collect::{APPLICATIONS, HostRoot};
use crate::maintenance_hook::KindError;
use crate::maintenance_mac::FreeSpace;
use crate::maintenance_plist::Plists;

/// The staging directory, on the volume of `/Applications`, so that the move into place is a
/// rename.
pub const STAGING: &str = "/Applications/.hostkeeper-staging";
/// The prefix of the store's path of an archive; the SHA-256 follows.
pub const STORE_PREFIX: &str = "/v1/xcode/";
/// The name of the archive in an entry's staging directory.
pub const ARCHIVE: &str = "Xcode.xip";
/// The name of the directory `xip` expands into.
pub const EXPAND: &str = "expand";
/// The name of the verified app in an entry's staging directory.
pub const STAGED_APP: &str = "Xcode.app";
/// The program that expands an archive and checks Apple's signature on it.
pub const XIP: &str = "/usr/bin/xip";
/// The program that checks the expanded app's code signature.
pub const CODESIGN: &str = "/usr/bin/codesign";
/// The arguments of `codesign` before the app.
pub const CODESIGN_ARGS: [&str; 3] = ["--verify", "--deep", "--strict"];
/// The environment of each program: a fixed `PATH`, nothing inherited.
pub const PROGRAM_ENV: [(&str, &str); 1] = [("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")];
/// How many times its size an archive's expansion is taken to need free.
pub const EXPANSION_FACTOR: u128 = 3;
/// The most connections one archive is fetched over, in one stage.
pub const MAX_FETCH_ATTEMPTS: u32 = 8;
/// Seconds one connection to the store may take: tens of GB over the LAN.
pub const FETCH_SECS: u64 = 4 * 3600;
/// Seconds `xip --expand` may take.
pub const XIP_SECS: u64 = 2 * 3600;
/// Seconds `codesign --verify` may take.
pub const CODESIGN_SECS: u64 = 3600;
/// The first bytes of an Apple Encrypted Archive.
pub const AEA_MAGIC: &[u8; 4] = b"AEA1";

/// The most bytes of a version or a build.
const MAX_NAME: usize = 16;
/// The size of one read when the archive is hashed.
const HASH_CHUNK: usize = 1 << 20;

/// An entry of the order whose SHA-256, version, build or size the stage does not take.
pub const REASON_ORDER_INVALID: &str = "xcode_order_invalid";
/// The version list of an installed Xcode that could not be read.
pub const REASON_INSTALLED_UNREADABLE: &str = "xcode_installed_unreadable";
/// The free space of the volume of `/Applications` that could not be read.
pub const REASON_FREE_SPACE_UNREADABLE: &str = "free_space_unreadable";
/// Less free space than the archives to fetch and their expansions need.
pub const REASON_FREE_SPACE_SHORT: &str = "free_space_short";
/// An archive the store does not hold (404).
pub const REASON_NOT_IN_STORE: &str = "xcode_not_in_store";
/// A read of the store that failed, and was not resumed or could be resumed no more.
pub const REASON_FETCH_FAILED: &str = "xcode_fetch_failed";
/// An archive whose size or SHA-256 is not the order's.
pub const REASON_ARCHIVE_MISMATCH: &str = "xcode_archive_mismatch";
/// An Apple Encrypted Archive, which the stage does not expand.
pub const REASON_ARCHIVE_ENCRYPTED: &str = "xcode_archive_encrypted";
/// An `xip --expand` that failed, or made other than one app.
pub const REASON_EXPAND_FAILED: &str = "xcode_expand_failed";
/// An app whose version or build is not the order's, or whose version list cannot be read.
pub const REASON_BUILD_MISMATCH: &str = "xcode_build_mismatch";
/// An app `codesign --verify` refuses.
pub const REASON_SIGNATURE_INVALID: &str = "xcode_signature_invalid";
/// A file of the staging directory that could not be read, written or removed.
pub const REASON_STAGING_FAILED: &str = "xcode_staging_failed";

/// A deferral for `reason`.
fn deferred(reason: &'static str, detail: String) -> KindError {
    KindError::Deferred { reason, detail }
}

/// The stage of the `xcode` kind, over its ports.
pub struct Stage {
    pub store: Box<dyn Store>,
    pub runner: Box<dyn Runner>,
    /// The free space of the volume of `/Applications`.
    pub space: Box<dyn FreeSpace>,
    /// The property lists of the host's files under `root`.
    pub plists: Box<dyn Plists>,
    pub root: HostRoot,
    pub clock: Arc<dyn Clock>,
}

/// What the stage does with one entry.
enum Plan {
    /// Installed with its build: nothing.
    Installed,
    /// Staged and verified by an earlier stage: checked again.
    Staged,
    /// Fetched, `have` bytes of it already on disk.
    Fetch { have: u64 },
}

impl Stage {
    /// Stages each Xcode of `entries`; on any failure removes the staging directory.
    ///
    /// # Errors
    /// [`KindError::Deferred`], naming the step and the entry.
    pub fn run(&self, entries: &[XcodeEntry]) -> Result<(), KindError> {
        if entries.is_empty() {
            return Ok(());
        }
        let staged = self.stage(entries);
        if staged.is_err() {
            // What cannot be removed now is removed by the next stage, which removes whatever
            // is not an entry of its order; the deferral says what failed.
            let _ = remove(&self.root.path(STAGING));
        }
        staged
    }

    fn stage(&self, entries: &[XcodeEntry]) -> Result<(), KindError> {
        for (at, entry) in entries.iter().enumerate() {
            check_entry(entry)?;
            if entries[..at].iter().any(|seen| seen.sha256 == entry.sha256) {
                return Err(deferred(
                    REASON_ORDER_INVALID,
                    format!("the order names the archive {} twice", entry.sha256),
                ));
            }
        }
        let staging = self.root.path(STAGING);
        match trusted(&staging) {
            true => self.remove_others(entries)?,
            false => remove(&staging)?,
        }
        let mut plans = Vec::new();
        for entry in entries {
            plans.push(self.plan(entry)?);
        }
        self.check_space(entries, &plans)?;
        for (entry, plan) in entries.iter().zip(plans) {
            match plan {
                Plan::Installed => {}
                Plan::Staged => {
                    self.verify(entry, &format!("{}/{STAGED_APP}", entry_host_dir(entry)))?;
                }
                Plan::Fetch { have } => self.fetch_and_expand(entry, have)?,
            }
        }
        Ok(())
    }

    /// The staging directory of `entry`, under the root.
    fn entry_dir(&self, entry: &XcodeEntry) -> PathBuf {
        self.root.path(&entry_host_dir(entry))
    }

    /// Removes from the (trusted) staging directory what is not the directory of an entry.
    fn remove_others(&self, entries: &[XcodeEntry]) -> Result<(), KindError> {
        let staging = self.root.path(STAGING);
        let listed =
            fs::read_dir(&staging).map_err(|error| staging_failed(&staging, "read", &error))?;
        for item in listed {
            let item = item.map_err(|error| staging_failed(&staging, "read", &error))?;
            let name = item.file_name();
            if entries.iter().any(|entry| name == entry.sha256.as_str()) {
                continue;
            }
            remove(&item.path())?;
        }
        Ok(())
    }

    /// What to do with `entry`: installed, staged, or fetched from what is on disk.
    fn plan(&self, entry: &XcodeEntry) -> Result<Plan, KindError> {
        let installed = format!(
            "{APPLICATIONS}/Xcode-{}.app/Contents/version.plist",
            entry.version
        );
        let found = self
            .plists
            .dictionary(&installed)
            .map_err(|detail| deferred(REASON_INSTALLED_UNREADABLE, detail))?;
        let installed = |plist: &plist::Dictionary| {
            text(plist, "CFBundleShortVersionString") == Some(&entry.version)
                && text(plist, "ProductBuildVersion") == Some(&entry.build)
        };
        if found.as_ref().is_some_and(installed) {
            return Ok(Plan::Installed);
        }
        let dir = self.entry_dir(entry);
        if dir.join(STAGED_APP).is_dir() {
            return Ok(Plan::Staged);
        }
        // A file an interrupted stage left is resumed; one longer than the archive is not it.
        let archive = dir.join(ARCHIVE);
        let have = match fs::symlink_metadata(archive) {
            Ok(meta) if meta.is_file() && meta.len() <= entry.size => meta.len(),
            _ => 0,
        };
        Ok(Plan::Fetch { have })
    }

    /// Fails `free_space_short` unless the volume has what the entries to fetch need.
    fn check_space(&self, entries: &[XcodeEntry], plans: &[Plan]) -> Result<(), KindError> {
        let needed: u128 = entries
            .iter()
            .zip(plans)
            .map(|(entry, plan)| match plan {
                Plan::Fetch { have } => {
                    u128::from(entry.size - have) + EXPANSION_FACTOR * u128::from(entry.size)
                }
                Plan::Installed | Plan::Staged => 0,
            })
            .sum();
        if needed == 0 {
            return Ok(());
        }
        let free = self
            .space
            .free_bytes()
            .map_err(|detail| deferred(REASON_FREE_SPACE_UNREADABLE, detail))?;
        if u128::from(free) < needed {
            return Err(deferred(
                REASON_FREE_SPACE_SHORT,
                format!(
                    "{free} bytes are free on the volume of {APPLICATIONS}; staging needs {needed}"
                ),
            ));
        }
        Ok(())
    }

    /// Steps 5 to 10 of the module's header for `entry`, `have` bytes of whose archive are on
    /// disk.
    fn fetch_and_expand(&self, entry: &XcodeEntry, have: u64) -> Result<(), KindError> {
        let dir = self.entry_dir(entry);
        let staging = self.root.path(STAGING);
        make_dir(&staging)?;
        if !trusted(&staging) {
            return Err(deferred(
                REASON_STAGING_FAILED,
                format!(
                    "{} is not a directory only this process's user can write",
                    staging.display()
                ),
            ));
        }
        make_dir(&dir)?;
        let expand = dir.join(EXPAND);
        remove(&expand)?;
        let archive = dir.join(ARCHIVE);
        self.fetch(entry, &archive, have)?;
        check_archive(entry, &archive)?;
        make_dir(&expand)?;
        let ran = self.run_program(
            XIP,
            &[OsStr::new("--expand"), archive.as_os_str()],
            &expand,
            XIP_SECS,
        );
        program_ok(&ran, "xip --expand", REASON_EXPAND_FAILED)?;
        remove(&archive)?;
        let name = only_app(&expand)?;
        let app = expand.join(&name);
        self.verify(entry, &format!("{}/{EXPAND}/{name}", entry_host_dir(entry)))?;
        let staged = dir.join(STAGED_APP);
        fs::rename(&app, staged).map_err(|error| staging_failed(&app, "rename", &error))?;
        remove(&expand)
    }

    /// Fetches the archive of `entry` into `archive`, from its byte `have` on: what the file
    /// holds past `have` (none, or all of a file that was not a part of the archive) is cut.
    fn fetch(&self, entry: &XcodeEntry, archive: &Path, mut have: u64) -> Result<(), KindError> {
        let path = format!("{STORE_PREFIX}{}", entry.sha256);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(archive)
            .and_then(|file| file.set_len(have).map(|()| file))
            .map_err(|error| staging_failed(archive, "open", &error))?;
        let mut attempts = 0;
        while have < entry.size {
            attempts += 1;
            let limit = Limit {
                max_bytes: entry.size - have,
                deadline_secs: FETCH_SECS,
            };
            let mut given = 0_u64;
            let got = self.store.get_from(&path, have, limit, &mut |chunk| {
                file.write_all(chunk)?;
                given += chunk.len() as u64;
                Ok(())
            });
            have += given;
            match got {
                Ok(Got::Body(_)) if have == entry.size => {}
                Ok(Got::Body(_)) => {
                    return Err(mismatch(
                        entry,
                        &format!("the store's archive ended at {have} bytes"),
                    ));
                }
                Ok(Got::NotFound) => {
                    return Err(deferred(
                        REASON_NOT_IN_STORE,
                        format!("the store holds no archive {path}"),
                    ));
                }
                Err(StoreError::Get(GetError::TooLong { length, .. })) => {
                    return Err(mismatch(
                        entry,
                        &format!("the store's archive is {} bytes", have + length),
                    ));
                }
                Err(StoreError::Get(GetError::Sink(kind))) => {
                    return Err(staging_failed(archive, "write", &io::Error::from(kind)));
                }
                Err(StoreError::Get(
                    GetError::Truncated | GetError::Deadline | GetError::Link(_),
                )) if attempts < MAX_FETCH_ATTEMPTS => {}
                Err(error) => {
                    return Err(deferred(
                        REASON_FETCH_FAILED,
                        format!("{path} from byte {have}, connection {attempts}: {error}"),
                    ));
                }
            }
        }
        file.sync_all()
            .map_err(|error| staging_failed(archive, "sync", &error))
    }

    /// Steps 9 and 10 for the app at the host's path `app`.
    fn verify(&self, entry: &XcodeEntry, app: &str) -> Result<(), KindError> {
        let host_path = format!("{app}/Contents/version.plist");
        let found = self
            .plists
            .dictionary(&host_path)
            .map_err(|detail| deferred(REASON_BUILD_MISMATCH, detail))?
            .ok_or_else(|| deferred(REASON_BUILD_MISMATCH, format!("{host_path} is missing")))?;
        let version = text(&found, "CFBundleShortVersionString");
        let build = text(&found, "ProductBuildVersion");
        if version != Some(&entry.version) || build != Some(&entry.build) {
            return Err(deferred(
                REASON_BUILD_MISMATCH,
                format!(
                    "the app is Xcode {} ({}); the order names Xcode {} ({})",
                    version.map_or("<none>", String::as_str),
                    build.map_or("<none>", String::as_str),
                    entry.version,
                    entry.build
                ),
            ));
        }
        let app = self.root.path(app);
        let mut args: Vec<&OsStr> = CODESIGN_ARGS.iter().map(|arg| OsStr::new(*arg)).collect();
        args.push(app.as_os_str());
        let cwd = self.root.path(STAGING);
        let ran = self.run_program(CODESIGN, &args, &cwd, CODESIGN_SECS);
        program_ok(&ran, "codesign --verify", REASON_SIGNATURE_INVALID)
    }

    /// Runs `program` with `args` in `cwd`, killed after `secs`.
    fn run_program(
        &self,
        program: &str,
        args: &[&OsStr],
        cwd: &Path,
        secs: u64,
    ) -> io::Result<Ran> {
        let now = Stamp::now(&*self.clock);
        let spawn = Spawn {
            program: PathBuf::from(program),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            env: PROGRAM_ENV
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            cwd: cwd.to_owned(),
            deadline: Stamp {
                boot_id: now.boot_id,
                mono_secs: now.mono_secs.saturating_add(secs),
            },
        };
        self.runner.run(&spawn)
    }
}

/// The staging directory of `entry`, as the host names it.
pub(super) fn entry_host_dir(entry: &XcodeEntry) -> String {
    format!("{STAGING}/{}", entry.sha256)
}

/// The text of `key` in `plist`.
pub(super) fn text<'a>(plist: &'a plist::Dictionary, key: &str) -> Option<&'a String> {
    match plist.get(key)? {
        plist::Value::String(text) => Some(text),
        _ => None,
    }
}

/// Whether `text` is 1 to 16 bytes, each one `allowed` takes.
fn is_name(text: &str, allowed: fn(u8) -> bool) -> bool {
    (1..=MAX_NAME).contains(&text.len()) && text.bytes().all(allowed)
}

/// Whether `text` is a version the stage and the apply take: 1 to 16 digits and dots.
pub(super) fn is_version(text: &str) -> bool {
    is_name(text, |b| b.is_ascii_digit() || b == b'.')
}

/// Whether `entry` is one step 1 of the module's header takes.
pub(super) fn is_valid_entry(entry: &XcodeEntry) -> bool {
    is_sha256(&entry.sha256)
        && is_version(&entry.version)
        && is_name(&entry.build, |b| b.is_ascii_alphanumeric())
        && entry.size > 0
}

/// Step 1 of the module's header.
fn check_entry(entry: &XcodeEntry) -> Result<(), KindError> {
    if is_valid_entry(entry) {
        return Ok(());
    }
    Err(deferred(
        REASON_ORDER_INVALID,
        "an Xcode of the order has a SHA-256, version, build or size the stage does not take"
            .to_owned(),
    ))
}

/// The refusal of the archive of `entry` before it is expanded.
fn mismatch(entry: &XcodeEntry, found: &str) -> KindError {
    deferred(
        REASON_ARCHIVE_MISMATCH,
        format!(
            "{found}; the order names {} bytes with SHA-256 {}",
            entry.size, entry.sha256
        ),
    )
}

/// Steps 6 and 7: the archive at `path` is the order's size and SHA-256, and not encrypted.
fn check_archive(entry: &XcodeEntry, path: &Path) -> Result<(), KindError> {
    let mut file = File::open(path).map_err(|error| staging_failed(path, "open", &error))?;
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut chunk = vec![0_u8; HASH_CHUNK];
    loop {
        let n = file
            .read(&mut chunk)
            .map_err(|error| staging_failed(path, "read", &error))?;
        if n == 0 {
            break;
        }
        hash.update(&chunk[..n]);
        size += n as u64;
    }
    let digest = hex::encode(hash.finalize());
    // The order's SHA-256 names the order's bytes, so a match is the order's size too.
    if digest != entry.sha256 {
        return Err(mismatch(
            entry,
            &format!("the archive is {size} bytes with SHA-256 {digest}"),
        ));
    }
    let mut head = Vec::new();
    File::open(path)
        .and_then(|file| file.take(AEA_MAGIC.len() as u64).read_to_end(&mut head))
        .map_err(|error| staging_failed(path, "read", &error))?;
    if head == AEA_MAGIC {
        return Err(deferred(
            REASON_ARCHIVE_ENCRYPTED,
            format!(
                "Xcode {} ({}) is an Apple Encrypted Archive, which this stage does not expand",
                entry.version, entry.build
            ),
        ));
    }
    Ok(())
}

/// The name of the one app `xip` made in `expand`.
fn only_app(expand: &Path) -> Result<String, KindError> {
    let apps: Vec<String> = fs::read_dir(expand)
        .map_err(|error| staging_failed(expand, "read", &error))?
        .filter_map(Result::ok)
        .filter(|item| item.path().is_dir())
        .filter_map(|item| item.file_name().into_string().ok())
        .filter(|name| name.ends_with(".app"))
        .collect();
    match apps.as_slice() {
        [app] => Ok(app.clone()),
        _ => Err(deferred(
            REASON_EXPAND_FAILED,
            format!("xip --expand made {} apps, not one Xcode", apps.len()),
        )),
    }
}

/// Fails `reason` unless the program that `ran` exited 0.
fn program_ok(ran: &io::Result<Ran>, what: &str, reason: &'static str) -> Result<(), KindError> {
    let ran = match ran {
        Ok(ran) => ran,
        Err(error) => return Err(deferred(reason, format!("{what} could not start: {error}"))),
    };
    let ended = match (ran.timed_out, ran.code) {
        (false, Some(0)) => return Ok(()),
        (true, _) => "was killed at its deadline".to_owned(),
        (false, Some(code)) => format!("exited {code}"),
        (false, None) => "was ended by a signal".to_owned(),
    };
    let stderr = String::from_utf8_lossy(&ran.stderr);
    match stderr.lines().next().map(str::trim) {
        Some(first) if !first.is_empty() => {
            Err(deferred(reason, format!("{what} {ended}: {first}")))
        }
        _ => Err(deferred(reason, format!("{what} {ended}"))),
    }
}

/// Whether `path` is a directory (not a link to one) that this process's user owns and nobody
/// else can read or write: what an admin of the Mac (`/Applications` is theirs to write) cannot
/// have put in its place. A path that is not there is not trusted.
pub(super) fn trusted(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.is_dir()
            && meta.uid() == rustix::process::geteuid().as_raw()
            && meta.mode() & 0o077 == 0
    })
}

/// Removes the file or the directory `path`, when it is there.
pub(super) fn remove(path: &Path) -> Result<(), KindError> {
    let removed = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(_) => Ok(()),
    };
    removed.map_err(|error| staging_failed(path, "remove", &error))
}

/// Makes the directory `path`, mode 0700, when it is not there.
pub(super) fn make_dir(path: &Path) -> Result<(), KindError> {
    match DirBuilder::new().mode(0o700).create(path) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {
            Err(staging_failed(path, "make", &error))
        }
        _ => Ok(()),
    }
}

/// The deferral of a file of the staging directory that could not be read or written.
fn staging_failed(path: &Path, what: &str, error: &io::Error) -> KindError {
    deferred(
        REASON_STAGING_FAILED,
        format!("cannot {what} {}: {error}", path.display()),
    )
}

#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;
