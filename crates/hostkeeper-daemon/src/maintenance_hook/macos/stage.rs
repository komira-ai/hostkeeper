//! The stage of the `macos` kind: the maintenance hook's `prepare` on a Mac
//! (docs/design/maintenance-updates.md, section 5.2 step 1, section 5.5, and section 1, "Only what
//! Apple offers now can be installed"). It runs while the Mac still serves, so it never fills the
//! disk and installs nothing:
//!
//! 1. What the Mac runs (`sw_vers`) and what Apple offers it now: `softwareupdate --list`, and,
//!    only when the order allows a major upgrade (`Maintain.macos_major`), `softwareupdate
//!    --list-full-installers`, both read by the maintenance look's parser
//!    ([`parse_softwareupdate_list`]). What is staged is named by these listings alone, never by
//!    the order: a minor update by the label `--list` gives it, a major upgrade by the version of
//!    its full installer.
//! 2. What to stage ([`plan`]): when the order allows a major upgrade and the Mac is offered a full
//!    installer of a higher major version, the newest such installer, alone (the upgrade replaces
//!    the system, so an update of the old major is not staged beside it); otherwise every macOS
//!    update `--list` offers within the running major version, as the look makes them due
//!    ([`crate::maintenance_softwareupdate::macos_due`]). When neither is offered, the update the
//!    run was ordered for is no longer offered, and the stage defers `macos_update_not_listed`.
//! 3. The free space of the data volume ([`FreeSpace`], `statvfs`): at least twice the bytes the
//!    listings give for what is staged, or the stage defers `free_space_short` and downloads
//!    nothing. A size the listing does not give in KiB defers `stage_listing_failed`: it is never
//!    guessed.
//! 4. The download alone: `softwareupdate --download <label>` for each update, in the listing's
//!    order, or `softwareupdate --fetch-full-installer --full-installer-version <version>` for the
//!    upgrade. An answer of `Update not found`, on either stream, means Apple no longer offers it (a
//!    newer one superseded it since the listing) and defers `macos_update_not_listed`.
//!
//! **Failure.** Every failure is [`KindError::Deferred`] (exit 75): the stage changed nothing the
//! Mac runs, the Mac never left service, and the run goes on. Its detail names the step, and for a
//! program that failed its exit code and the first line of its standard error; it never holds a
//! parameter.
//!
//! **Programs.** Each runs by its absolute path as the hook's user (root), with an empty
//! environment, no standard input, a deadline, and each output stream kept to its bound
//! ([`HostPrograms`]).

use std::cmp::Ordering;
use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlOutput, LaunchctlProcess};

use crate::login_account::{exit_failure, run};
use crate::maintenance_collect::{compare_versions, listing};
use crate::maintenance_hook::kind::KindError;
use crate::maintenance_hook::params::MaintainParams;
use crate::maintenance_mac::{DATA_VOLUME, FreeSpace, Statvfs};
use crate::maintenance_macos::parse_sw_vers;
use crate::maintenance_softwareupdate::{
    FullInstaller, ListedUpdate, MACOS_TITLE, SoftwareUpdateListing, major_of,
    parse_softwareupdate_list,
};
use crate::xcode_clt::{LIST_DEADLINE, LOOK_DEADLINE, SOFTWAREUPDATE, SW_VERS};

/// The reason of a listing (`sw_vers`, `softwareupdate --list` or `--list-full-installers`) that
/// failed, or that gave no running version or no size in KiB for what is staged.
pub const REASON_LISTING_FAILED: &str = "stage_listing_failed";
/// The reason of a Mac that is no longer offered what the run would stage: no macOS update of its
/// major version listed and no full installer the order allows, or a download that answered
/// `Update not found`.
pub const REASON_NOT_LISTED: &str = "macos_update_not_listed";
/// The reason of a data volume whose free space could not be read.
pub const REASON_SPACE_UNREADABLE: &str = "free_space_unreadable";
/// The reason of less free space than twice the download.
pub const REASON_SPACE_SHORT: &str = "free_space_short";
/// The reason of a download that failed.
pub const REASON_DOWNLOAD_FAILED: &str = "stage_download_failed";

/// How long one download may take: a full installer is some 16 GB. One stopped at its deadline is
/// deferred.
pub const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);

/// The arguments of the listing of updates.
const LIST_ARGS: [&str; 1] = ["--list"];
/// The arguments of the listing of full installers.
const FULL_ARGS: [&str; 1] = ["--list-full-installers"];
/// What `softwareupdate` answers for an update or installer Apple no longer offers.
const NOT_FOUND: &str = "Update not found";

/// Where the stage's programs run.
pub trait Programs: Send + Sync {
    /// The runner of `program`, bounded by `deadline`.
    fn tool(&self, program: &Path, deadline: Duration) -> Box<dyn Launchctl>;
}

/// The Mac's programs: each with an empty environment and no standard input, its output kept to
/// its bound, killed at its deadline ([`LaunchctlProcess`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct HostPrograms;

impl Programs for HostPrograms {
    fn tool(&self, program: &Path, deadline: Duration) -> Box<dyn Launchctl> {
        Box::new(LaunchctlProcess::new(program, deadline))
    }
}

/// One download of the stage.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Download {
    /// The arguments of `softwareupdate` that make it.
    args: Vec<String>,
    /// Its bytes, as the listing gives them.
    bytes: u64,
}

impl Download {
    /// The download of the update `--list` named.
    fn update(update: &ListedUpdate) -> Result<Download, KindError> {
        let bytes = update
            .size
            .ok_or_else(|| no_size("softwareupdate --list", &update.label))?;
        Ok(Download {
            args: vec!["--download".to_owned(), update.label.clone()],
            bytes,
        })
    }

    /// The download of the full installer `--list-full-installers` named.
    fn installer(installer: &FullInstaller) -> Result<Download, KindError> {
        let bytes = installer.size.ok_or_else(|| {
            no_size(
                "softwareupdate --list-full-installers",
                &format!("macOS {}", installer.version),
            )
        })?;
        Ok(Download {
            args: vec![
                "--fetch-full-installer".to_owned(),
                "--full-installer-version".to_owned(),
                installer.version.clone(),
            ],
            bytes,
        })
    }

    /// The step's name in a detail: the command line, which holds no parameter.
    fn what(&self) -> String {
        format!("softwareupdate {}", self.args.join(" "))
    }
}

/// The deferral of `reason` with `detail`.
fn deferred(reason: &'static str, detail: String) -> KindError {
    KindError::Deferred { reason, detail }
}

/// The deferral of a listing that failed: `stage_listing_failed`.
fn listing_failed(detail: String) -> KindError {
    deferred(REASON_LISTING_FAILED, detail)
}

/// The deferral of an update or installer `what` listed with no size in KiB.
fn no_size(what: &str, name: &str) -> KindError {
    listing_failed(format!("{what} gave no size in KiB for {name}"))
}

/// What to stage on a Mac of the major version `running`, from what `listing` offers, a major
/// upgrade only when `major_allowed`, as the module says.
///
/// # Errors
/// `macos_update_not_listed` when nothing is offered; `stage_listing_failed` when what would be
/// staged has no size.
fn plan(
    running: u64,
    listing: &SoftwareUpdateListing,
    major_allowed: bool,
) -> Result<Vec<Download>, KindError> {
    if major_allowed && let Some(installer) = newest_upgrade(running, &listing.installers) {
        return Ok(vec![Download::installer(installer)?]);
    }
    let downloads = listing
        .updates
        .iter()
        .filter(|update| {
            update.title.starts_with(MACOS_TITLE) && major_of(&update.version) == Some(running)
        })
        .map(Download::update)
        .collect::<Result<Vec<_>, _>>()?;
    if downloads.is_empty() {
        let detail = match major_allowed {
            true => format!(
                "softwareupdate offers no update of macOS {running} and no full installer of a \
                 later major version"
            ),
            false => format!("softwareupdate offers no update of macOS {running}"),
        };
        return Err(deferred(REASON_NOT_LISTED, detail));
    }
    Ok(downloads)
}

/// The newest of `installers` of macOS of a major version higher than `running`; of equal
/// versions, the first.
fn newest_upgrade(running: u64, installers: &[FullInstaller]) -> Option<&FullInstaller> {
    installers
        .iter()
        .filter(|installer| {
            installer.title.starts_with(MACOS_TITLE)
                && major_of(&installer.version).is_some_and(|major| major > running)
        })
        .reduce(
            |held, next| match compare_versions(&next.version, &held.version) {
                Ordering::Greater => next,
                Ordering::Equal | Ordering::Less => held,
            },
        )
}

/// Whether `output` says, on either stream, that Apple no longer offers what was asked for.
fn not_found(output: &LaunchctlOutput) -> bool {
    String::from_utf8_lossy(&output.stdout).contains(NOT_FOUND)
        || output.stderr_text().contains(NOT_FOUND)
}

/// The stage of a Mac.
pub struct Stage {
    programs: Box<dyn Programs>,
    space: Box<dyn FreeSpace>,
}

impl Stage {
    /// The stage running the Mac's programs through `programs` and measuring its data volume
    /// through `space`.
    pub fn new(programs: Box<dyn Programs>, space: Box<dyn FreeSpace>) -> Stage {
        Stage { programs, space }
    }

    /// The stage of this Mac: its own programs and its data volume ([`DATA_VOLUME`]).
    pub fn at_host() -> Stage {
        Stage::new(Box::new(HostPrograms), Box::new(Statvfs::data_volume()))
    }

    /// Stages what the run's `params` allow, as the module says.
    ///
    /// # Errors
    /// The deferral of the step that did not succeed.
    pub fn run(&self, params: &MaintainParams) -> Result<(), KindError> {
        let major_allowed = params.maintain().macos_major;
        let running = self.running_major()?;
        let mut offered = self.offered(&LIST_ARGS)?;
        if major_allowed {
            offered.installers = self.offered(&FULL_ARGS)?.installers;
        }
        let downloads = plan(running, &offered, major_allowed)?;
        // In `u128`, where no sum of sizes overflows, so the check never passes on a wrapped one.
        let bytes: u128 = downloads
            .iter()
            .map(|download| u128::from(download.bytes))
            .sum();
        let free = self
            .space
            .free_bytes()
            .map_err(|why| deferred(REASON_SPACE_UNREADABLE, why))?;
        if u128::from(free) < bytes * 2 {
            return Err(deferred(
                REASON_SPACE_SHORT,
                format!(
                    "{DATA_VOLUME} has {free} bytes free, less than twice the {bytes} bytes to \
                     download"
                ),
            ));
        }
        downloads
            .iter()
            .try_for_each(|download| self.download(download))
    }

    /// The major version of the macOS the Mac runs, from `sw_vers`.
    fn running_major(&self) -> Result<u64, KindError> {
        let tool = self.programs.tool(Path::new(SW_VERS), LOOK_DEADLINE);
        let (_, text) = listing(&*tool, "sw_vers", &[], &[0]).map_err(listing_failed)?;
        let (version, _) = parse_sw_vers(&text);
        major_of(&version).ok_or_else(|| {
            listing_failed(format!(
                "sw_vers named the version \"{version}\", which has no major number"
            ))
        })
    }

    /// What `softwareupdate` with `args` lists.
    fn offered(&self, args: &[&str]) -> Result<SoftwareUpdateListing, KindError> {
        let tool = self.programs.tool(Path::new(SOFTWAREUPDATE), LIST_DEADLINE);
        let what = format!("softwareupdate {}", args.join(" "));
        let (_, text) = listing(&*tool, &what, args, &[0]).map_err(listing_failed)?;
        Ok(parse_softwareupdate_list(&text))
    }

    /// Makes `download`.
    fn download(&self, download: &Download) -> Result<(), KindError> {
        let tool = self
            .programs
            .tool(Path::new(SOFTWAREUPDATE), DOWNLOAD_DEADLINE);
        let what = download.what();
        let args: Vec<&str> = download.args.iter().map(String::as_str).collect();
        let output =
            run(&*tool, &what, &args).map_err(|why| deferred(REASON_DOWNLOAD_FAILED, why))?;
        if not_found(&output) {
            return Err(deferred(
                REASON_NOT_LISTED,
                format!("{what} answered \"{NOT_FOUND}\": Apple no longer offers it"),
            ));
        }
        match output.code {
            Some(0) => Ok(()),
            _ => Err(deferred(
                REASON_DOWNLOAD_FAILED,
                exit_failure(&what, &output, true),
            )),
        }
    }
}

#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;
