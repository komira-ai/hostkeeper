//! The registry of this build's installers and the order a native installer's `stage` is given
//! (packages.md section 5.1): the code that turns a verified package into installed software,
//! and takes it away again.
//!
//! | Step       | Does                                                               | Must not |
//! |------------|--------------------------------------------------------------------|----------|
//! | `stage`    | Makes the version directory from the verified package              | Change anything outside the version's staging directory, or run a script of the package |
//! | `activate` | Makes the version the installed one; a rollback activates the kept previous version | Fetch anything; start, stop or restart the service's `run` units; touch another service |
//! | `retire`   | What a version that is neither `current` nor `previous` leaves once the core deleted its directory and seal | Call the format's tool on the host's installed package |
//! | `remove`   | Takes the whole service off the host: what its `activate` recorded | Remove anything `activate` did not record, or anything changed since |
//!
//! The deletion of a version's directory and seal is the same for every installer and the core's
//! (the prune, section 5.2); `retire` is what an installer does beyond it. A format's own tool is
//! run through the step launcher ([`hostkeeper_steps::StepLauncher::run_tool`]), by absolute
//! path, bounded, in a unit with no network.
//!
//! The registry ([`backend_of`]) is the closed list of installers the daemon is built with, which
//! no manifest can extend (section 5.1, "Why built in, and not delivered"). This build holds one:
//!
//! - **`apt`** ([`crate::repository::apt::Apt`]): the repository installer's backend
//!   (packages-repositories.md section 4). Nothing is fetched as an artifact, and each step works
//!   on the version directory and the order's entry ([`crate::repository::RepositoryInstaller`]),
//!   so the daemon runs them itself.
//!
//! `macos-pkg` (A9) has its module and is not registered: the daemon does not run it yet.

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_release::Installer;

use crate::repository::Backend;
use crate::repository::apt::Apt;

/// What `stage` is given: the order's service, version and package digest, and its entry.
#[derive(Clone, Copy, Debug)]
pub struct StageOrder<'a> {
    /// The service.
    pub service: &'a ServiceName,
    /// The version.
    pub version: &'a Version,
    /// The SHA-256 the package was verified to have.
    pub package_digest: &'a Sha256Hex,
    /// The entry the order carries, already judged on receipt.
    pub entry: Option<&'a Target>,
}

/// The installers this build has the backend of, in the order of their spellings.
pub const REGISTERED: [Installer; 1] = [Installer::Apt];

/// The backend of the repository installer `installer`, `None` for one this build has no
/// backend of.
pub fn backend_of(installer: Installer) -> Option<&'static dyn Backend> {
    match installer {
        Installer::Apt => Some(&Apt),
        Installer::MacosPkg => None,
    }
}
