//! What this crate's integration tests need of its internals. It is built only with the feature
//! `test-support`, which only those tests turn on (the crate's dev-dependency on itself).

use std::path::Path;

use super::contract::Platform;
use super::package::{Request, package_dated};

/// Builds the linux-x86_64 hostkeeper package of `version` as `cargo xtask release package`
/// does, from `binary` and `unit`, into `out`, with every time in the archive `mtime` (the
/// verb takes it from the commit in git). Returns the verb's lines, or why it refused.
pub fn linux_package(
    version: &str,
    binary: &Path,
    unit: &Path,
    out: &Path,
    mtime: u64,
) -> Result<Vec<String>, String> {
    let request = Request {
        platform: Platform::LinuxX86_64,
        version: version.parse().map_err(|e| e.to_string())?,
        commit: "0".repeat(40).parse().map_err(|e| e.to_string())?,
        binary: binary.to_path_buf(),
        unit: Some(unit.to_path_buf()),
        out: out.to_path_buf(),
    };
    package_dated(&request, || Ok(mtime)).map_err(|e| e.to_string())
}
