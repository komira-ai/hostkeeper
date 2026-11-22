//! The version directory of a package on the host (docs/design/hostkeeper-design.md, section
//! 5.5, steps 3 and 4): `packages/<service>/<version>/`, sealed, in one piece or absent.
//!
//! - [`stage_filled`] publishes a staging directory the caller filled: the core makes the staging
//!   directory, the caller writes into it, and the core makes the tree durable, seals it and
//!   renames it into place through the [`PackageFs`] port ([`HostFs`] on a host).
//! - [`remove_orphans`] removes what a crashed publish left.
//! - [`UnpackError::reason`] says how a step record reports each failure.
//!
//! The tree digest and the seal are formats other releases read; docs/tree-digest.md describes
//! them and the names on disk, and they change additively only. The tree digest is
//! [`hostkeeper_steps::tree_digest`] and the seal is [`hostkeeper_steps::seal`], so that the step
//! runner, which checks a version before it runs anything from it, reads them with no dependency
//! on this crate:
//!
//! ```text
//! packages/                            0711, root-owned: passed through by name, listed by root alone
//! packages/<service>/                  0711, the same
//! packages/<service>/<version>/        0700, the tree the caller filled, root-owned: root's alone
//! packages/<service>/<version>.seal    0400, {"schema":1,"tree_digest":..,"package_digest":..}
//! packages/<service>/.stage-<random>/  0700, a publish under way (removed when orphaned)
//! packages/<service>/current  -> <version>
//! packages/<service>/previous -> <version>
//! ```
//!
//! What every other user may and may not do there is the layout's rule, and the reason for each
//! mode:
//!
//! - A version this crate publishes is root's alone ([`VERSION_MODE`]): it holds the package
//!   file itself (the kept `.deb` under `apt/`, the `.pkg` as `artifact`), which carries every
//!   file the package installs whatever mode the installed file is given on the host, and a copy
//!   of the package's tree (`tree/`) whose modes are the package's, not those its install scripts
//!   may set after. No service runs from it: the package manager installs the program at the
//!   package's own paths (`/usr/...`, `/Library/...`), and the hook runs as root.
//! - The packages directory and each service directory are passed through, not read
//!   ([`PACKAGES_MODE`], [`SERVICE_MODE`]): another user can reach a name in them it already
//!   knows, and no more than that name's own mode lets it, and cannot list them (it can read
//!   the links `current` and `previous`, so which versions of a service it names are not
//!   secret, and see that a name it guesses is there). So a version
//!   that a service of an earlier release runs from is reachable by the service's user, and
//!   nothing this crate publishes is: each version, `.stage-` directory and seal in a service
//!   directory is closed by its own mode (0700, 0700, 0400). An earlier release wrote seals
//!   0444; the daemon's start closes every file in a service directory to its owner.
//! - A version an earlier release unpacked from hostkeeper's own archive (`hostkeeper-archive`:
//!   a `manifest.toml` and no `target.json` at its top) is the tree its service runs from, as
//!   the service's own user (its unit's `ExecStart=` names `current/bin/...`), and is
//!   [`ARCHIVE_VERSION_MODE`], as that release made it. The archive's unpack gave every entry
//!   0555 or 0444, so no file of such a tree was ever any user's alone on the host, and no
//!   package file is kept in it. No installer makes such a version any more; the daemon's start
//!   gives the ones on a host this mode (`hostkeeper_daemon::package_dirs`).

/// The mode of the packages directory: passed through by every user, listed by root alone.
pub const PACKAGES_MODE: u32 = 0o711;

/// The mode of a service directory: passed through by every user, listed by root alone.
pub const SERVICE_MODE: u32 = 0o711;

/// The mode of a version directory this crate publishes: root's alone.
pub const VERSION_MODE: u32 = 0o700;

/// The mode of a version an earlier release unpacked from hostkeeper's own archive, which its
/// service runs from: read and passed through by every user, written by none.
pub const ARCHIVE_VERSION_MODE: u32 = 0o555;

mod error;
mod fill;
mod fs;
mod publish;
mod seal;
mod service;

pub use error::{
    REASON_ARTIFACT_CHANGED, REASON_DAEMON_TOO_OLD, REASON_IO_ERROR, REASON_NO_SPACE,
    REASON_RESERVED_VERSION, REASON_VERSION_TOO_LONG, UnpackError,
};
pub use fill::{FillError, FillOrder, Filled, stage_filled};
pub use fs::{HostFs, Owner, PackageFs, ServiceLock, StagedFile};
pub use service::remove_orphans;
