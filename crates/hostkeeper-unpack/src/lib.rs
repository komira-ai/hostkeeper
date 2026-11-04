//! The unpack of a verified package (docs/design/hostkeeper-design.md, section 5.5, steps 3
//! and 4): a `.tar.zst` archive whose SHA-256 the fetch proved, made into a sealed version
//! directory `packages/<service>/<version>/` in one streaming pass under hard bounds, so that a
//! version directory is whole or absent.
//!
//! - [`unpack`] reads the archive once: the zstd decoder ([`MAX_WINDOW_BYTES`]), the tar
//!   reader and every rule of the archive run on each entry before any byte of it is written,
//!   into a staging directory, through the [`PackageFs`] port; then the manifest is loaded with
//!   the `hostkeeper-config` loader, the tree is sealed and renamed into place.
//! - [`remove_orphans`] removes what a crashed unpack left.
//! - [`UnpackError::outcome`] says how a step record reports each failure.
//!
//! This crate does no network and runs no service manager. The decoder is `ruzstd`, in pure
//! Rust, built without its default features.
//!
//! # Public formats
//!
//! The archive rules, the mode normalisation, the tree digest, the seal file and the names on
//! disk are formats other releases read; docs/package-format.md describes them and they change
//! additively only. The tree digest is [`hostkeeper_steps::tree_digest`] and the seal is
//! [`hostkeeper_steps::seal`], so that the step runner, which checks a version before it runs
//! anything from it, reads them with no dependency on this crate:
//!
//! ```text
//! packages/<service>/<version>/        manifest.toml, bin/, units/, files/: dirs and
//!                                      executables 0555, other files 0444, root-owned
//! packages/<service>/<version>.seal    {"schema":1,"tree_digest":..,"package_digest":..}
//! packages/<service>/.stage-<random>/  an unpack under way (removed when orphaned)
//! packages/<service>/current  -> <version>
//! packages/<service>/previous -> <version>
//! ```

mod archive;
mod error;
mod fs;
mod publish;
mod seal;
mod unpack;

pub use archive::{BUFFER, MAX_DEPTH, MAX_NAME_BYTES, MAX_WINDOW_BYTES};
pub use error::{
    REASON_ARTIFACT_CHANGED, REASON_BAD_ARCHIVE, REASON_DAEMON_TOO_OLD, REASON_DIGEST_MISMATCH,
    REASON_IO_ERROR, REASON_MANIFEST_MISMATCH, REASON_NO_SPACE, REASON_RESERVED_VERSION, Rule,
    UnpackError,
};
pub use fs::{HostFs, Owner, PackageFs, ServiceLock, StagedFile};
pub use seal::{SEAL_SCHEMA, Seal, SealError};
pub use unpack::{Limits, UnpackOrder, Unpacked, remove_orphans, running_hostkeeper, unpack};
