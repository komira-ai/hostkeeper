//! The archive check of the `probe` kind (docs/design/maintenance-updates.md; `hk xcode add` and
//! `hk xcode verify` from a Linux box, where Apple's tools do not exist): the Mac checks an Xcode
//! archive the coordinator's store holds, and installs nothing.
//!
//! The order of a probe run names the archive in `Maintain.xcodes`: exactly one entry, valid as
//! the stage ([`super::stage`]) takes entries. Without an entry the probe is the plain probe
//! ([`crate::maintenance_hook::probe`]) and this module does nothing.
//!
//! **`prepare`**, while the Mac still serves ([`ArchiveCheck::prepare`]). Whatever is at
//! [`CHECK_DIR`] (on the volume of `/Applications`) is removed without being followed, and the
//! directory made again, mode 0700, empty; it is taken only when it is a directory of the process's
//! user that nobody else can read or write. The volume must have free the archive's size and
//! [`EXPANSION_FACTOR`] times its size (`free_space_short`, `free_space_unreadable`). The archive is
//! fetched from the store into `<CHECK_DIR>/Xcode.xip` as the stage fetches it (resumed within the
//! one prepare, up to [`MAX_FETCH_ATTEMPTS`](super::stage::MAX_FETCH_ATTEMPTS) connections), and its size, SHA-256 and format are
//! checked as the stage checks them (`xcode_not_in_store`, `xcode_fetch_failed`,
//! `xcode_archive_mismatch`, `xcode_archive_encrypted`, `xcode_staging_failed`). More than one
//! entry, or an entry the stage refuses, is `xcode_order_invalid`. Each failure is
//! [`KindError::Deferred`] and removes [`CHECK_DIR`].
//!
//! **`cutover`**, on the drained Mac ([`ArchiveCheck::steps`]): the probe's steps are these, each
//! program run as the stage runs its own (its runner, [`PROGRAM_ENV`](super::stage::PROGRAM_ENV), no standard input, an
//! absolute path, a deadline), each output the non-empty lines of its standard output then its
//! standard error, trimmed and joined by `; `, cut to the probe's bound:
//!
//! 1. `sw_vers` and 2. `uname -sm`, 60 s each: recorded, and never fail the check.
//! 3. `pkgutil --check-signature <archive>` in [`CHECK_DIR`]: must exit 0 and report Apple's own
//!    signature ([`super::signature`]); `xcode_archive_not_apple` otherwise. Its output is then
//!    `signed Apple Software, its chain ending at Apple Root CA`, or why the report is not Apple's.
//! 4. `xip --expand <archive>` in `<CHECK_DIR>/expand`: `xcode_expand_failed` on a non-zero exit.
//! 5. `app`, no program: the names of the `.app` directories xip made, or `none`; not exactly one
//!    is `xcode_expand_failed`.
//! 6. `version`, no program: `<CFBundleShortVersionString> <ProductBuildVersion>` of the app's
//!    `Contents/version.plist`; a file or key that is not there, or another version or build than
//!    the entry's, is `xcode_build_mismatch`.
//! 7. `codesign --verify --deep --strict <app>`: `xcode_signature_invalid` on a non-zero exit.
//! 8. `verdict`, always last: `pass` with exit 0; `<reason>: <detail>` with exit 1 for an archive a
//!    step refused (the steps after it do not run); with no exit code, `xcode_check_unfinished:
//!    <detail>` for a program that did not start, was killed at its deadline or was ended by a
//!    signal (its own step has no exit code and says which), `xcode_staging_failed: <detail>`
//!    for a file of the check that could not be read, written or made, or an archive that is gone
//!    (then no program of steps 3 to 7 runs), or `xcode_order_invalid: <detail>` for an order that
//!    does not name one valid archive (the same).
//!
//! The steps stop at the first refusal of steps 3 to 7. Whatever happened, [`CHECK_DIR`] is
//! removed after the steps, so the check leaves nothing in `/Applications`. It installs nothing: no
//! program of the check is given a password, and every file it makes is under [`CHECK_DIR`].

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use hostkeeper_package::gh::Ran;
use hostkeeper_proto::hostkeeper::agent::v1::{MaintenanceProbeStep, XcodeEntry};

use super::signature::{parse, verdict};
use super::stage::{
    ARCHIVE, CODESIGN, CODESIGN_ARGS, CODESIGN_SECS, EXPAND, EXPANSION_FACTOR, PKGUTIL,
    PKGUTIL_ARGS, PKGUTIL_SECS, REASON_ARCHIVE_NOT_APPLE, REASON_BUILD_MISMATCH,
    REASON_EXPAND_FAILED, REASON_FREE_SPACE_SHORT, REASON_FREE_SPACE_UNREADABLE,
    REASON_ORDER_INVALID, REASON_SIGNATURE_INVALID, REASON_STAGING_FAILED, Stage, XIP, XIP_SECS,
    apps_in, check_archive, check_entry, deferred, ended_detail, make_dir, not_one_app, remove,
    staging_failed, text, trusted,
};
use crate::maintenance_collect::APPLICATIONS;
use crate::maintenance_hook::KindError;
use crate::maintenance_hook::probe::{STEP_DEADLINE, bounded, joined};

/// The check directory, on the volume of `/Applications`: the archive and its expansion.
pub const CHECK_DIR: &str = "/Applications/.hostkeeper-check";
/// The program that names the macOS the check ran on.
pub const SW_VERS: &str = "/usr/bin/sw_vers";
/// The program that names the machine the check ran on.
pub const UNAME: &str = "/usr/bin/uname";
/// The arguments of `uname`.
pub const UNAME_ARGS: [&str; 1] = ["-sm"];
/// What step 3 says of an archive Apple signed.
pub const APPLE_SIGNED: &str = "signed Apple Software, its chain ending at Apple Root CA";
/// The verdict's output of an archive that passed every step.
pub const PASS: &str = "pass";
/// The verdict of a program that did not finish: it did not start, was killed at its deadline or
/// was ended by a signal.
pub const REASON_CHECK_UNFINISHED: &str = "xcode_check_unfinished";

/// The key of the app's version in `Contents/version.plist`.
const VERSION_KEY: &str = "CFBundleShortVersionString";
/// The key of the app's build in `Contents/version.plist`.
const BUILD_KEY: &str = "ProductBuildVersion";

/// Why the steps stopped before the last.
#[derive(Debug)]
enum Stop {
    /// The archive is refused: the verdict's exit code is 1.
    Refused {
        reason: &'static str,
        detail: String,
    },
    /// The archive was not checked: the verdict has no exit code.
    NotChecked {
        reason: &'static str,
        detail: String,
    },
}

/// A stop that did not check the archive, for the reason and detail of `error`.
fn not_checked(error: KindError) -> Stop {
    let (KindError::Deferred { reason, detail } | KindError::Failed { reason, detail }) = error;
    Stop::NotChecked { reason, detail }
}

/// A stop that refuses the archive, for the reason and detail of `error`.
fn refused(error: KindError) -> Stop {
    let (KindError::Deferred { reason, detail } | KindError::Failed { reason, detail }) = error;
    Stop::Refused { reason, detail }
}

/// The step `name`, its `text` cut to the probe's bound.
fn step(name: &str, exit_code: Option<i32>, text: String) -> MaintenanceProbeStep {
    let (output, cut) = bounded(text);
    MaintenanceProbeStep {
        name: name.to_owned(),
        exit_code,
        output,
        cut,
    }
}

/// `ran`, which exited 0, or why the steps stop: a program that did not finish did not check the
/// archive; one that exited non-zero refuses it for `reason`.
fn exited(ran: Result<Ran, String>, what: &str, reason: &'static str) -> Result<Ran, Stop> {
    match ran {
        Err(detail) => Err(Stop::NotChecked {
            reason: REASON_CHECK_UNFINISHED,
            detail,
        }),
        Ok(ran) if ran.code == Some(0) => Ok(ran),
        Ok(ran) => Err(Stop::Refused {
            reason,
            detail: ended_detail(&ran, what),
        }),
    }
}

/// The archive check of the `probe` kind, over the stage's ports.
pub struct ArchiveCheck {
    stage: Stage,
}

impl ArchiveCheck {
    /// The check reading the store and running programs through `stage`'s ports.
    #[must_use]
    pub fn new(stage: Stage) -> ArchiveCheck {
        ArchiveCheck { stage }
    }

    /// The check of this Mac, over the ports of the `xcode` kind's stage.
    #[must_use]
    pub fn at_host() -> ArchiveCheck {
        ArchiveCheck::new(super::host_stage())
    }

    /// Fetches and checks the archive of `entries` into [`CHECK_DIR`], as the module says; on any
    /// failure removes [`CHECK_DIR`]. Nothing is done for no entry.
    ///
    /// # Errors
    /// [`KindError::Deferred`], naming the step.
    pub fn prepare(&self, entries: &[XcodeEntry]) -> Result<(), KindError> {
        if entries.is_empty() {
            return Ok(());
        }
        let fetched = self.fetch(entries);
        if fetched.is_err() {
            // What cannot be removed now is removed by the next prepare or the cutover; the
            // deferral says what failed.
            let _ = remove(&self.stage.root.path(CHECK_DIR));
        }
        fetched
    }

    fn fetch(&self, entries: &[XcodeEntry]) -> Result<(), KindError> {
        let entry = one(entries)?;
        let dir = self.stage.root.path(CHECK_DIR);
        remove(&dir)?;
        self.check_space(entry)?;
        make_dir(&dir)?;
        if !trusted(&dir) {
            return Err(deferred(
                REASON_STAGING_FAILED,
                format!(
                    "{} is not a directory only this process's user can write",
                    dir.display()
                ),
            ));
        }
        let archive = dir.join(ARCHIVE);
        self.stage.fetch(entry, &archive, 0)?;
        check_archive(entry, &archive)
    }

    /// Fails `free_space_short` unless the volume has the archive and its expansion free.
    fn check_space(&self, entry: &XcodeEntry) -> Result<(), KindError> {
        let needed = u128::from(entry.size) + EXPANSION_FACTOR * u128::from(entry.size);
        let free = self
            .stage
            .space
            .free_bytes()
            .map_err(|detail| deferred(REASON_FREE_SPACE_UNREADABLE, detail))?;
        if u128::from(free) < needed {
            return Err(deferred(
                REASON_FREE_SPACE_SHORT,
                format!(
                    "{free} bytes are free on the volume of {APPLICATIONS}; the check needs {needed}"
                ),
            ));
        }
        Ok(())
    }

    /// The probe's steps for the archive of `entries`, as the module says, the verdict last; then
    /// removes [`CHECK_DIR`].
    pub fn steps(&self, entries: &[XcodeEntry]) -> Vec<MaintenanceProbeStep> {
        let cwd = self.stage.root.path("/");
        let secs = STEP_DEADLINE.as_secs();
        let mut steps = vec![
            self.execute("sw_vers", SW_VERS, &[], &cwd, secs).0,
            self.execute("uname", UNAME, &UNAME_ARGS.map(OsStr::new), &cwd, secs)
                .0,
        ];
        let stopped = self.checked(entries, &mut steps).err();
        steps.push(last(stopped));
        // The steps are the answer; what cannot be removed now is removed by the next prepare.
        let _ = remove(&self.stage.root.path(CHECK_DIR));
        steps
    }

    /// Steps 3 to 7 for the one archive of `entries`, each pushed on `steps` as it ends.
    fn checked(
        &self,
        entries: &[XcodeEntry],
        steps: &mut Vec<MaintenanceProbeStep>,
    ) -> Result<(), Stop> {
        let entry = one(entries).map_err(not_checked)?;
        let dir = self.stage.root.path(CHECK_DIR);
        let archive = dir.join(ARCHIVE);
        let expand = dir.join(EXPAND);
        present(&dir, &archive)?;
        let (pkgutil, result) = self.pkgutil(&archive, &dir);
        steps.push(pkgutil);
        result?;
        remove(&expand)
            .and_then(|()| make_dir(&expand))
            .map_err(not_checked)?;
        let (xip, ran) = self.execute(
            "xip",
            XIP,
            &[OsStr::new("--expand"), archive.as_os_str()],
            &expand,
            XIP_SECS,
        );
        steps.push(xip);
        exited(ran, "xip --expand", REASON_EXPAND_FAILED)?;
        let apps = apps_in(&expand).map_err(not_checked)?;
        let names = match apps.is_empty() {
            true => "none".to_owned(),
            false => apps.join(", "),
        };
        steps.push(step("app", None, names));
        let [app] = apps.as_slice() else {
            return Err(refused(not_one_app(apps.len())));
        };
        let (version, result) = self.version(entry, app);
        steps.push(version);
        result?;
        let app_path = expand.join(app);
        let mut args: Vec<&OsStr> = CODESIGN_ARGS.iter().map(|arg| OsStr::new(*arg)).collect();
        args.push(app_path.as_os_str());
        let (codesign, ran) = self.execute("codesign", CODESIGN, &args, &dir, CODESIGN_SECS);
        steps.push(codesign);
        exited(ran, "codesign --verify", REASON_SIGNATURE_INVALID).map(|_| ())
    }

    /// Step 3: `pkgutil --check-signature` on `archive`, run in `cwd`.
    fn pkgutil(&self, archive: &Path, cwd: &Path) -> (MaintenanceProbeStep, Result<(), Stop>) {
        let what = "pkgutil --check-signature";
        let mut args: Vec<&OsStr> = PKGUTIL_ARGS.iter().map(|arg| OsStr::new(*arg)).collect();
        args.push(archive.as_os_str());
        let (mut done, ran) = self.execute("pkgutil", PKGUTIL, &args, cwd, PKGUTIL_SECS);
        let ran = match exited(ran, what, REASON_ARCHIVE_NOT_APPLE) {
            Ok(ran) => ran,
            Err(stop) => return (done, Err(stop)),
        };
        match verdict(&parse(&String::from_utf8_lossy(&ran.stdout))) {
            Ok(()) => {
                APPLE_SIGNED.clone_into(&mut done.output);
                done.cut = false;
                (done, Ok(()))
            }
            Err(why) => {
                let stop = Stop::Refused {
                    reason: REASON_ARCHIVE_NOT_APPLE,
                    detail: format!("{what}: {why}"),
                };
                (done.output, done.cut) = bounded(why);
                (done, Err(stop))
            }
        }
    }

    /// Step 6: the version and build of `app` in the expansion, against those of `entry`.
    fn version(&self, entry: &XcodeEntry, app: &str) -> (MaintenanceProbeStep, Result<(), Stop>) {
        let host_path = format!("{CHECK_DIR}/{EXPAND}/{app}/Contents/version.plist");
        let mismatch = |detail: String| Stop::Refused {
            reason: REASON_BUILD_MISMATCH,
            detail,
        };
        let found = match self.stage.plists.dictionary(&host_path) {
            Ok(Some(found)) => found,
            Ok(None) => {
                let why = format!("{host_path} is missing");
                return (step("version", None, why.clone()), Err(mismatch(why)));
            }
            Err(why) => return (step("version", None, why.clone()), Err(mismatch(why))),
        };
        let version = text(&found, VERSION_KEY);
        let build = text(&found, BUILD_KEY);
        let output = match (version, build) {
            (Some(version), Some(build)) => format!("{version} {build}"),
            (None, _) => format!("version.plist has no {VERSION_KEY}"),
            (_, None) => format!("version.plist has no {BUILD_KEY}"),
        };
        let result = if version == Some(&entry.version) && build == Some(&entry.build) {
            Ok(())
        } else {
            Err(mismatch(format!(
                "the app is Xcode {} ({}); the order names Xcode {} ({})",
                version.map_or("<none>", String::as_str),
                build.map_or("<none>", String::as_str),
                entry.version,
                entry.build
            )))
        };
        (step("version", None, output), result)
    }

    /// Runs `program` as the stage runs its own and gives its step: its exit code and output; or,
    /// for a program that did not start, was killed at its deadline of `secs` or was ended by a
    /// signal, no exit code, what happened, and that as the error.
    fn execute(
        &self,
        name: &str,
        program: &str,
        args: &[&OsStr],
        cwd: &Path,
        secs: u64,
    ) -> (MaintenanceProbeStep, Result<Ran, String>) {
        let unfinished = |why: String| (step(name, None, why.clone()), Err(why));
        match self.stage.run_program(program, args, cwd, secs) {
            Err(error) => unfinished(format!("{program} could not start: {error}")),
            Ok(ran) if ran.timed_out => {
                unfinished(format!("{program} was killed at its deadline of {secs}s"))
            }
            Ok(ran) if ran.code.is_none() => unfinished(format!("{program} was ended by a signal")),
            Ok(ran) => {
                let text = joined(&[
                    &String::from_utf8_lossy(&ran.stdout),
                    &String::from_utf8_lossy(&ran.stderr),
                ]);
                (step(name, ran.code, text), Ok(ran))
            }
        }
    }
}

/// The one entry of the order, valid as the stage takes entries.
fn one(entries: &[XcodeEntry]) -> Result<&XcodeEntry, KindError> {
    match entries {
        [entry] => check_entry(entry).map(|()| entry),
        _ => Err(deferred(
            REASON_ORDER_INVALID,
            format!(
                "a probe checks one archive; the order names {}",
                entries.len()
            ),
        )),
    }
}

/// Fails when the check directory is not the process's user's alone, or holds no archive.
fn present(dir: &Path, archive: &Path) -> Result<(), Stop> {
    if !trusted(dir) {
        return Err(Stop::NotChecked {
            reason: REASON_STAGING_FAILED,
            detail: format!(
                "{} is not a directory only this process's user can write",
                dir.display()
            ),
        });
    }
    match fs::symlink_metadata(archive) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err(Stop::NotChecked {
            reason: REASON_STAGING_FAILED,
            detail: format!("{} is not a file", archive.display()),
        }),
        Err(error) => Err(not_checked(staging_failed(archive, "read", &error))),
    }
}

/// The last step, the verdict, of steps that `stopped` (or did not).
fn last(stopped: Option<Stop>) -> MaintenanceProbeStep {
    match stopped {
        None => step("verdict", Some(0), PASS.to_owned()),
        Some(Stop::Refused { reason, detail }) => {
            step("verdict", Some(1), format!("{reason}: {detail}"))
        }
        Some(Stop::NotChecked { reason, detail }) => {
            step("verdict", None, format!("{reason}: {detail}"))
        }
    }
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
