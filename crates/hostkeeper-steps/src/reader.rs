//! The oldest reader of a release of hostkeeper (docs/design/two-programs.md, section 4.2): the
//! one compiled-in version both the daemon and the release builder read.
//!
//! - **Forward.** The release builder writes it as `min_hostkeeper` of hostkeeper's own package
//!   (`crates/xtask/src/release/package.rs`), so a daemon older than it defers the release
//!   (`daemon_too_old`) before anything of it runs: the daemon that runs is the release's
//!   fallback, and it must read every file the release writes.
//! - **Backward.** A daemon refuses a release of hostkeeper older than its own
//!   [`OLDEST_READER`] before the commit, since that release could not read this daemon's files
//!   (`hostkeeper_daemon::selfupdate`).
//!
//! The rule: a release that writes a new format sets [`OLDEST_READER`] to the release that first
//! read it. Stage 3 of the design sets it to R2, release 0.1.194, the first release that holds
//! every pull request of stage 2: it hands over to a new binary and writes no fallback unit or
//! job, so a stage-3 release, which has neither the fallback verbs, the start log nor
//! `--admin-socket`, can fall back to it. A daemon older than R2 defers a release of this line
//! `daemon_too_old` before anything of it runs.

use hostkeeper_config::manifest::HostkeeperVersion;

/// The oldest release of hostkeeper that reads every file this release writes.
pub const OLDEST_READER: HostkeeperVersion = HostkeeperVersion {
    major: 0,
    minor: 1,
    patch: 194,
};
