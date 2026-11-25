//! The apply and the check of the `xcode` kind on the drained Mac
//! (docs/design/maintenance-updates.md, sections 2.4 steps 2 and 3, 5.6 and 6). The order
//! (`Maintain`) names the Xcodes the Mac is to have (`xcodes`: version, build, size, SHA-256),
//! the versions to remove (`xcode_remove`) and the version `xcode-select` is to name
//! (`xcode_default`, empty to leave it). An Xcode of version `<version>`
//! (`CFBundleShortVersionString`) is installed at `/Applications/Xcode-<version>.app`, beside the
//! others: an Xcode a build uses is never replaced under it.
//!
//! **`cutover`** ([`Apply::cutover`]), in order, each step only once the one before succeeded:
//!
//! 1. **The order** is checked before anything is read: each Xcode's SHA-256, version, build and
//!    size as the stage takes them, no version given twice, the default one of the order's Xcodes,
//!    and each version to remove digits and dots, older than every Xcode the order keeps, and
//!    given only with a default (so that what `xcode-select` names is never removed). Anything
//!    else fails `xcode_order_invalid`.
//! 2. **Where each Xcode is.** An Xcode installed at its path with its build is left as it is. Any
//!    other is taken only from the staging directory, `<staging>/<sha256>/Xcode.app`, where the
//!    stage puts an app only once `xip` checked Apple's signature on its archive, its version
//!    list named the order's version and build, and `codesign --verify` accepted it; the
//!    directory must still be one only root can read or write, and the app's version list must
//!    still name the order's version and build. An Xcode neither installed nor staged so fails
//!    `xcode_not_staged`, and a path of the order's version that holds anything else (another
//!    build, a link, a file) fails `xcode_install_taken` and is left alone. So does an installed
//!    Xcode whose own `xcodebuild`, or any directory from the app down to it, is a link, is not
//!    this process's user's (root's), or is writable by its group or others: an admin of the Mac
//!    (`/Applications` is theirs to write) must not have a program of theirs run as root. Every
//!    Xcode of the order is found before the first is moved.
//! 3. **The move.** Each staged Xcode is renamed to its path, one volume, so it appears whole or
//!    not at all (`xcode_move_failed`).
//! 4. **The licence and the first launch**, for each Xcode of the order in its order, by the
//!    Xcode's own `xcodebuild`: `-license accept` (`xcode_license_failed`), then `-runFirstLaunch`
//!    (`xcode_first_launch_failed`), which installs the packages every Xcode shares, the reason
//!    the Mac is drained. Both are safe to run again, so a cutover tried again after a failure
//!    runs them for an Xcode it moved before.
//! 5. **The default.** `xcode-select -s /Applications/Xcode-<default>.app` (`xcode_select_failed`);
//!    nothing with no default.
//! 6. **The removal** of each version to remove: `/Applications/Xcode-<version>.app` is removed
//!    when it is a directory whose own version list names that version: renamed first into the
//!    staging directory (made, mode 0700, when it is not there; it must be root's alone), so that
//!    it leaves its path whole, and then deleted there; what a deletion that fails or is cut off
//!    leaves there is removed by the next stage. One that is not there is
//!    gone already; one that is a link or a file, or whose version list is missing, unreadable or
//!    names another version, is foreign (a person, the App Store or device management put it
//!    there; the look reports it as such) and is never removed. Nothing but a directory named
//!    `Xcode-<version>.app` is ever removed, and nothing at all before every step above succeeded.
//!    A removal that fails is `xcode_remove_failed`.
//!
//! The kind is then applied, under the default's version (the newest of the order's when it names
//! none). Xcode needs no reboot. Simulator platforms are not imported: the Mac proof has not shown
//! `xcodebuild -importPlatform` working yet.
//!
//! **`verify`** ([`Apply::verify`]) checks the order again, then each Xcode of the order: its
//! path holds its version list (`xcode_missing`), readable and naming its version and build
//! (`xcode_build_mismatch`), its `xcodebuild` is root's alone as step 2 says
//! (`xcode_install_taken`), its `xcodebuild -version` exits 0 and names its build
//! (`xcode_version_failed`), and its `xcodebuild -license check` exits 0
//! (`xcode_license_not_accepted`); then that `xcode-select -p` names the default's developer
//! directory (`xcode_select_failed` when it does not run, `xcode_default_wrong` when it names
//! another).
//!
//! **Failure.** Every failure is [`KindError::Failed`]: the Mac is drained, and the run halts
//! there (section 5.4). Each program runs by its absolute path as root, with an empty
//! environment and no standard input, killed at its deadline ([`Programs`]); a detail names the
//! step, its exit code and the first line of its standard error.

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1::{Maintain, XcodeEntry};

use super::stage::{
    REASON_BUILD_MISMATCH, REASON_INSTALLED_UNREADABLE, REASON_ORDER_INVALID, STAGED_APP, STAGING,
    entry_host_dir, is_valid_entry, is_version, make_dir, text, trusted,
};
use crate::login_account::run_ok;
use crate::maintenance_collect::{APPLICATIONS, HostRoot, compare_versions};
use crate::maintenance_hook::kind::{Cutover, KindError};
use crate::maintenance_hook::macos::{HostPrograms, Programs};
use crate::maintenance_hook::params::MaintainParams;
use crate::maintenance_plist::{HostPlists, Plists};
use crate::maintenance_xcode::XCODE_SELECT;

/// Where an Xcode's own `xcodebuild` is in its app.
pub const XCODEBUILD: &str = "Contents/Developer/usr/bin/xcodebuild";
/// The developer directory of an app, as `xcode-select -p` names it.
pub const DEVELOPER: &str = "Contents/Developer";

/// How long `xcodebuild -license accept` may take.
pub const LICENSE_DEADLINE: Duration = Duration::from_secs(10 * 60);
/// How long `xcodebuild -runFirstLaunch` may take: it installs packages.
pub const FIRST_LAUNCH_DEADLINE: Duration = Duration::from_secs(60 * 60);
/// How long `xcode-select` may take.
pub const SELECT_DEADLINE: Duration = Duration::from_secs(60);
/// How long `xcodebuild -version` and `xcodebuild -license check` may take each.
pub const CHECK_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// An Xcode of the order neither installed with its build nor staged and verified.
pub const REASON_NOT_STAGED: &str = "xcode_not_staged";
/// The path of an Xcode of the order holds something else.
pub const REASON_INSTALL_TAKEN: &str = "xcode_install_taken";
/// A staged Xcode that could not be moved to its path.
pub const REASON_MOVE_FAILED: &str = "xcode_move_failed";
/// An `xcodebuild -license accept` that did not succeed.
pub const REASON_LICENSE_FAILED: &str = "xcode_license_failed";
/// An `xcodebuild -runFirstLaunch` that did not succeed.
pub const REASON_FIRST_LAUNCH_FAILED: &str = "xcode_first_launch_failed";
/// An `xcode-select` that did not succeed.
pub const REASON_SELECT_FAILED: &str = "xcode_select_failed";
/// An Xcode to remove that could not be removed.
pub const REASON_REMOVE_FAILED: &str = "xcode_remove_failed";
/// An Xcode of the order that is not at its path.
pub const REASON_MISSING: &str = "xcode_missing";
/// An `xcodebuild -version` that did not succeed or named another build.
pub const REASON_VERSION_FAILED: &str = "xcode_version_failed";
/// An Xcode whose licence `xcodebuild -license check` does not hold accepted.
pub const REASON_LICENSE_NOT_ACCEPTED: &str = "xcode_license_not_accepted";
/// An `xcode-select -p` that names another developer directory than the default's.
pub const REASON_DEFAULT_WRONG: &str = "xcode_default_wrong";

/// What a version list does not name, in a detail.
const NONE: &str = "<none>";
/// The prefix of the name, in the staging directory, of an Xcode being removed; its version
/// follows. It is no entry's name, so the next stage removes what a removal leaves there.
const REMOVING: &str = "removing-";

/// A failure for `reason`.
fn failed(reason: &'static str, detail: String) -> KindError {
    KindError::Failed { reason, detail }
}

/// The path of the Xcode of `version`, as the host names it.
fn app_path(version: &str) -> String {
    format!("{APPLICATIONS}/Xcode-{version}.app")
}

/// Where an Xcode of the order is before the move.
enum Place {
    /// At its path, with its build.
    Installed,
    /// Staged and verified, at the host's path given.
    Staged(String),
}

/// The apply and check of the `xcode` kind, over its ports.
pub struct Apply {
    /// The Mac's programs: each Xcode's `xcodebuild`, and `xcode-select`.
    pub(crate) programs: Box<dyn Programs>,
    /// The property lists of the host's files under `root`.
    pub(crate) plists: Box<dyn Plists>,
    pub(crate) root: HostRoot,
}

impl Apply {
    /// The apply of this Mac: its own programs and files.
    #[must_use]
    pub fn at_host() -> Apply {
        Apply::new(
            Box::new(HostPrograms),
            Box::new(HostPlists::at_host()),
            HostRoot::host(),
        )
    }

    /// The apply running the Mac's programs through `programs`, reading property lists through
    /// `plists` and finding each Xcode under `root`.
    #[must_use]
    pub fn new(programs: Box<dyn Programs>, plists: Box<dyn Plists>, root: HostRoot) -> Apply {
        Apply {
            programs,
            plists,
            root,
        }
    }

    /// Installs the order's Xcodes, selects its default and removes the old ones, as the module
    /// says.
    ///
    /// # Errors
    /// [`KindError::Failed`], naming the step and the Xcode.
    pub fn cutover(&self, params: &MaintainParams) -> Result<Cutover, KindError> {
        let order = params.maintain();
        check_order(order)?;
        let mut places = Vec::new();
        for entry in &order.xcodes {
            places.push(self.place(entry)?);
        }
        for (entry, place) in order.xcodes.iter().zip(places) {
            if let Place::Staged(staged) = place {
                self.move_into_place(entry, &staged)?;
            }
        }
        for entry in &order.xcodes {
            let app = app_path(&entry.version);
            self.xcodebuild(
                &app,
                &["-license", "accept"],
                LICENSE_DEADLINE,
                REASON_LICENSE_FAILED,
            )?;
            self.xcodebuild(
                &app,
                &["-runFirstLaunch"],
                FIRST_LAUNCH_DEADLINE,
                REASON_FIRST_LAUNCH_FAILED,
            )?;
        }
        if !order.xcode_default.is_empty() {
            let app = app_path(&order.xcode_default);
            self.xcode_select(&["-s", &app], REASON_SELECT_FAILED)?;
        }
        for version in &order.xcode_remove {
            self.remove(version)?;
        }
        let version = match order.xcode_default.as_str() {
            "" => newest(&order.xcodes).unwrap_or_default(),
            default => default.to_owned(),
        };
        Ok(Cutover::Applied { version })
    }

    /// Checks that each Xcode of the order is installed and usable and that `xcode-select` names
    /// the default, as the module says.
    ///
    /// # Errors
    /// [`KindError::Failed`], naming the first wrong fact.
    pub fn verify(&self, params: &MaintainParams) -> Result<(), KindError> {
        let order = params.maintain();
        check_order(order)?;
        for entry in &order.xcodes {
            let app = app_path(&entry.version);
            match self.names(&app) {
                Ok(Some((version, build))) if version == entry.version && build == entry.build => {}
                Ok(Some((version, build))) => {
                    return Err(failed(
                        REASON_BUILD_MISMATCH,
                        format!(
                            "{app} is Xcode {version} ({build}); the order names Xcode {} ({})",
                            entry.version, entry.build
                        ),
                    ));
                }
                Ok(None) => {
                    return Err(failed(
                        REASON_MISSING,
                        format!(
                            "Xcode {} ({}) is not at {app}: it has no version list",
                            entry.version, entry.build
                        ),
                    ));
                }
                Err(detail) => return Err(failed(REASON_BUILD_MISMATCH, detail)),
            }
            self.root_held(&app)
                .map_err(|detail| failed(REASON_INSTALL_TAKEN, detail))?;
            let said =
                self.xcodebuild(&app, &["-version"], CHECK_DEADLINE, REASON_VERSION_FAILED)?;
            let names_build = said
                .lines()
                .any(|line| line.trim() == format!("Build version {}", entry.build));
            if !names_build {
                return Err(failed(
                    REASON_VERSION_FAILED,
                    format!("{app}/{XCODEBUILD} -version names no build {}", entry.build),
                ));
            }
            self.xcodebuild(
                &app,
                &["-license", "check"],
                CHECK_DEADLINE,
                REASON_LICENSE_NOT_ACCEPTED,
            )?;
        }
        if order.xcode_default.is_empty() {
            return Ok(());
        }
        let app = app_path(&order.xcode_default);
        let said = self.xcode_select(&["-p"], REASON_SELECT_FAILED)?;
        let developer = said.lines().next().map_or("", str::trim);
        let selects = developer == app || developer.starts_with(&format!("{app}/"));
        if !selects {
            return Err(failed(
                REASON_DEFAULT_WRONG,
                format!(
                    "xcode-select -p names {}; the order's default is {app}/{DEVELOPER}",
                    match developer {
                        "" => NONE,
                        named => named,
                    }
                ),
            ));
        }
        Ok(())
    }

    /// The version and the build the version list of the app at the host's path `app` names
    /// ([`NONE`] for one it does not name); `None` when the app has no version list.
    fn names(&self, app: &str) -> Result<Option<(String, String)>, String> {
        let found = self
            .plists
            .dictionary(&format!("{app}/Contents/version.plist"))?;
        Ok(found.map(|plist| {
            let named = |key| text(&plist, key).map_or(NONE, String::as_str).to_owned();
            (
                named("CFBundleShortVersionString"),
                named("ProductBuildVersion"),
            )
        }))
    }

    /// Step 2 of the module's header for `entry`.
    fn place(&self, entry: &XcodeEntry) -> Result<Place, KindError> {
        let app = app_path(&entry.version);
        let wanted = || format!("Xcode {} ({})", entry.version, entry.build);
        match fs::symlink_metadata(self.root.path(&app)) {
            Ok(meta) if meta.is_dir() => {
                let names = self
                    .names(&app)
                    .map_err(|detail| failed(REASON_INSTALLED_UNREADABLE, detail))?;
                return match names {
                    Some((version, build)) if version == entry.version && build == entry.build => {
                        self.root_held(&app)
                            .map(|()| Place::Installed)
                            .map_err(|detail| {
                                failed(
                                    REASON_INSTALL_TAKEN,
                                    format!("{detail}; it is left as it is"),
                                )
                            })
                    }
                    Some((version, build)) => Err(failed(
                        REASON_INSTALL_TAKEN,
                        format!(
                            "{app} is Xcode {version} ({build}), not {}; it is left as it is",
                            wanted()
                        ),
                    )),
                    None => Err(failed(
                        REASON_INSTALL_TAKEN,
                        format!(
                            "{app} has no version list, so it is not {}; it is left as it is",
                            wanted()
                        ),
                    )),
                };
            }
            Ok(_) => {
                return Err(failed(
                    REASON_INSTALL_TAKEN,
                    format!("{app} is not a directory; it is left as it is"),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(failed(
                    REASON_INSTALLED_UNREADABLE,
                    format!("cannot read {app}: {error}"),
                ));
            }
        }
        if !trusted(&self.root.path(STAGING)) {
            return Err(failed(
                REASON_NOT_STAGED,
                format!(
                    "{} is not staged: {STAGING} is not a directory only this process's user can \
                     write",
                    wanted()
                ),
            ));
        }
        let staged = format!("{}/{STAGED_APP}", entry_host_dir(entry));
        let is_dir = fs::symlink_metadata(self.root.path(&staged)).is_ok_and(|meta| meta.is_dir());
        if !is_dir {
            return Err(failed(
                REASON_NOT_STAGED,
                format!("{} is not staged: {staged} is not a directory", wanted()),
            ));
        }
        match self.names(&staged) {
            Ok(Some((version, build))) if version == entry.version && build == entry.build => {
                Ok(Place::Staged(staged))
            }
            Ok(Some((version, build))) => Err(failed(
                REASON_NOT_STAGED,
                format!(
                    "{} is not staged: {staged} is Xcode {version} ({build})",
                    wanted()
                ),
            )),
            Ok(None) => Err(failed(
                REASON_NOT_STAGED,
                format!("{} is not staged: {staged} has no version list", wanted()),
            )),
            Err(detail) => Err(failed(REASON_NOT_STAGED, detail)),
        }
    }

    /// Step 3 of the module's header: the staged app at the host's path `staged` renamed to the
    /// path of `entry`.
    fn move_into_place(&self, entry: &XcodeEntry, staged: &str) -> Result<(), KindError> {
        let app = app_path(&entry.version);
        fs::rename(self.root.path(staged), self.root.path(&app)).map_err(|error| {
            failed(
                REASON_MOVE_FAILED,
                format!("cannot move {staged} to {app}: {error}"),
            )
        })?;
        // The entry's directory is empty now; one that cannot be removed is removed by the next
        // stage, which removes whatever is not an entry of its order.
        let _left = fs::remove_dir(self.root.path(&entry_host_dir(entry)));
        Ok(())
    }

    /// Step 6 of the module's header for `version`.
    fn remove(&self, version: &str) -> Result<(), KindError> {
        let app = app_path(version);
        let path = self.root.path(&app);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(failed(
                    REASON_REMOVE_FAILED,
                    format!("cannot read {app}: {error}"),
                ));
            }
        }
        let ours = matches!(self.names(&app), Ok(Some((named, _))) if named == version);
        if !ours {
            return Ok(());
        }
        let removing = format!("{STAGING}/{REMOVING}{version}");
        let staging = self.root.path(STAGING);
        let untrusted = || {
            failed(
                REASON_REMOVE_FAILED,
                format!(
                    "cannot remove {app}: {STAGING} is not a directory only this process's user \
                     can write"
                ),
            )
        };
        make_dir(&staging).map_err(|error| match error {
            KindError::Deferred { detail, .. } | KindError::Failed { detail, .. } => {
                failed(REASON_REMOVE_FAILED, detail)
            }
        })?;
        if !trusted(&staging) {
            return Err(untrusted());
        }
        let cannot = |what: String, error: io::Error| {
            failed(REASON_REMOVE_FAILED, format!("cannot {what}: {error}"))
        };
        let gone = self.root.path(&removing);
        if fs::symlink_metadata(&gone).is_ok() {
            fs::remove_dir_all(&gone)
                .map_err(|error| cannot(format!("remove {removing}"), error))?;
        }
        fs::rename(&path, &gone)
            .map_err(|error| cannot(format!("move {app} to {removing}"), error))?;
        fs::remove_dir_all(&gone).map_err(|error| cannot(format!("remove {removing}"), error))
    }

    /// Whether the app at the host's path `app`, each directory down to its `xcodebuild`, and
    /// that program are held by this process's user alone: not a link, this user's, and not
    /// writable by its group or others. Why not, naming the path, when they are not.
    fn root_held(&self, app: &str) -> Result<(), String> {
        let mut path = app.to_owned();
        for part in std::iter::once("").chain(XCODEBUILD.split('/')) {
            if !part.is_empty() {
                path = format!("{path}/{part}");
            }
            let meta = fs::symlink_metadata(self.root.path(&path))
                .map_err(|error| format!("cannot read {path}: {error}"))?;
            let held = !meta.file_type().is_symlink()
                && meta.uid() == rustix::process::geteuid().as_raw()
                && meta.mode() & 0o022 == 0;
            if !held {
                return Err(format!(
                    "{path} is a link, is not this process's user's, or is writable by others"
                ));
            }
        }
        Ok(())
    }

    /// The standard output of the `xcodebuild` of the app at the host's path `app` run with
    /// `args`, or the failure `reason` naming the step.
    fn xcodebuild(
        &self,
        app: &str,
        args: &[&str],
        deadline: Duration,
        reason: &'static str,
    ) -> Result<String, KindError> {
        let program = format!("{app}/{XCODEBUILD}");
        let tool = self.programs.tool(Path::new(&program), deadline);
        let what = format!("{program} {}", args.join(" "));
        run_ok(tool.as_ref(), &what, args).map_err(|detail| failed(reason, detail))
    }

    /// The standard output of `xcode-select` run with `args`, or the failure `reason`.
    fn xcode_select(&self, args: &[&str], reason: &'static str) -> Result<String, KindError> {
        let tool = self.programs.tool(Path::new(XCODE_SELECT), SELECT_DEADLINE);
        let what = format!("xcode-select {}", args.join(" "));
        run_ok(tool.as_ref(), &what, args).map_err(|detail| failed(reason, detail))
    }
}

/// The newest version of `entries`, none when there is none.
fn newest(entries: &[XcodeEntry]) -> Option<String> {
    entries
        .iter()
        .map(|entry| entry.version.as_str())
        .max_by(|left, right| compare_versions(left, right))
        .map(str::to_owned)
}

/// Step 1 of the module's header.
fn check_order(order: &Maintain) -> Result<(), KindError> {
    let invalid = |detail: String| Err(failed(REASON_ORDER_INVALID, detail));
    for (at, entry) in order.xcodes.iter().enumerate() {
        if !is_valid_entry(entry) {
            return invalid(
                "an Xcode of the order has a SHA-256, version, build or size the apply does not \
                 take"
                    .to_owned(),
            );
        }
        if order.xcodes[..at]
            .iter()
            .any(|seen| seen.version == entry.version)
        {
            return invalid(format!("the order names Xcode {} twice", entry.version));
        }
    }
    let keeps = |version: &str| order.xcodes.iter().any(|entry| entry.version == version);
    let default = order.xcode_default.as_str();
    if !default.is_empty() && !keeps(default) {
        return invalid(format!(
            "the order's default Xcode {default} is not one of the Xcodes it names"
        ));
    }
    for version in &order.xcode_remove {
        if !is_version(version) {
            return invalid("a version the order removes is not digits and dots".to_owned());
        }
        if default.is_empty() {
            return invalid(format!(
                "the order removes Xcode {version} and names no default"
            ));
        }
        let older = order
            .xcodes
            .iter()
            .all(|entry| compare_versions(version, &entry.version) == Ordering::Less);
        if !older {
            return invalid(format!(
                "the order removes Xcode {version}, which is not older than every Xcode it keeps"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
