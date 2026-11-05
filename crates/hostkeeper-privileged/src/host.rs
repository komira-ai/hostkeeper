//! The check a privileged test makes before it acts on the host: it runs as root, and systemd is
//! the service manager. `cargo test --features privileged` on a workstation therefore fails with
//! the reason instead of starting units there.
//!
//! The test reads the two facts (the effective uid, through `rustix`, and
//! [`SYSTEMD_RUNTIME_DIR`]) and gives them to [`check_host`]; the decision is here, tested
//! without root.

use std::path::{Path, PathBuf};

/// The directory systemd creates at boot when it is the service manager (what `sd_booted(3)`
/// tests).
pub const SYSTEMD_RUNTIME_DIR: &str = "/run/systemd/system";

/// Why a privileged test refused to start.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    #[error(
        "the privileged tests run only as root (effective uid 0): run them as \
         .github/workflows/privileged.yml does, in a throwaway VM, never on a workstation"
    )]
    NotRoot,
    #[error(
        "the privileged tests need systemd as the service manager, and {0:?} is not a directory"
    )]
    NoSystemd(PathBuf),
}

/// `Ok` when the test runs as root (`effective_root`) and `systemd_runtime_dir` is a directory.
pub fn check_host(effective_root: bool, systemd_runtime_dir: &Path) -> Result<(), HostError> {
    if !effective_root {
        return Err(HostError::NotRoot);
    }
    if !systemd_runtime_dir.is_dir() {
        return Err(HostError::NoSystemd(systemd_runtime_dir.to_path_buf()));
    }
    Ok(())
}
