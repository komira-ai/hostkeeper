//! The hostkeeper bootstrap of a Mac (docs/design/mac-bootstrap.md): a small program that a
//! one-time, signed pkg installs and a launchd job runs. It reads the farm's two values, fetches
//! hostkeeper's signed macOS pkg from the public repository, checks it, writes the daemon's
//! config when there is none, runs `installer` and exits. The daemon it installed enrolls through
//! the normal approval flow, and the coordinator installs everything else.
//!
//! - [`settings`]: the farm values (coordinator, CA hash) from the managed preferences of
//!   `dev.hostkeeper.config`, else a fallback file, and every tunable with its default.
//! - [`package`]: the descriptor and the pointer name the pkg; the download is hashed as written.
//! - [`signer`]: `pkgutil --check-signature` pinned to a Team ID.
//! - [`files`]: a config written only when it is absent.
//! - [`update_account`]: the update account's four keys and the update-account file's text.
//! - [`run`]: the steps, the bounded wait and the ports. [`cli`]: the flags and the host's ports.
//!
//! The network, the Mac's tools and the pause are ports, so Linux measures every step against
//! fakes; `main.rs` only forwards to [`cli::main`].

pub mod cli;
pub mod files;
pub mod package;
pub mod run;
pub mod settings;
pub mod signer;
pub mod update_account;

pub use cli::{execute, main};
