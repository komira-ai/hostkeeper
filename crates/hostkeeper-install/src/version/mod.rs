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
//! packages/<service>/<version>/        the tree the caller filled: dirs and executables 0555,
//!                                      other files 0444, root-owned
//! packages/<service>/<version>.seal    {"schema":1,"tree_digest":..,"package_digest":..}
//! packages/<service>/.stage-<random>/  a publish under way (removed when orphaned)
//! packages/<service>/current  -> <version>
//! packages/<service>/previous -> <version>
//! ```

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
