//! dnf's part of the stage ([`super::stage`]): on a Red Hat family host (dnf 4),
//!
//! 1. `dnf upgrade --assumeno <options>` resolves the upgrade and prints dnf's own estimate,
//!    `Total download size: <size>`, then answers no and exits 1 ("Operation aborted."); with
//!    nothing to upgrade it exits 0 and prints no total;
//! 2. `dnf upgrade --downloadonly -y -q <options>` downloads into dnf's cache (`/var/cache/dnf`)
//!    and installs nothing.
//!
//! The options are the same for both: `--setopt=exit_on_lock=True`, so a held lock ends the run
//! with exit 200 at once and the stage waits for it ([`super::stage::Calls`]) instead of dnf
//! waiting with no bound; `--security` under the scope `security` (the apply's `dnf upgrade
//! --security`); and `--exclude=<name>` for each of hostkeeper's own packages, which dnf cannot
//! hold.

use super::stage::{Calls, Download, arguments, listing_failed, scaled};
use crate::maintenance_collect::{LinuxScope, Look};
use crate::maintenance_dnf::{EXIT_ON_LOCK, lock_held};
use crate::maintenance_hook::kind::KindError;

/// Where dnf keeps what it downloads.
pub const CACHE: &str = "/var/cache/dnf";

/// What `--assumeno` exits with once it has printed the transaction.
const ABORTED: i32 = 1;

const SECURITY: &str = "--security";
const ESTIMATE: [&str; 2] = ["upgrade", "--assumeno"];
const DOWNLOAD: [&str; 4] = ["upgrade", "--downloadonly", "-y", "-q"];
const TOTAL: &str = "Total download size:";

/// Whether a `dnf` run found the lock held, as the maintenance look reads it ([`lock_held`]).
pub(super) fn busy(code: Option<i32>, text: &str) -> bool {
    lock_held(code, text)
}

/// The options of both runs under `look`.
fn options(look: &Look<'_>) -> Vec<String> {
    let mut options = vec![EXIT_ON_LOCK.to_owned()];
    if look.scope == LinuxScope::Security {
        options.push(SECURITY.to_owned());
    }
    options.extend(
        look.own_packages
            .iter()
            .map(|name| format!("--exclude={name}")),
    );
    options
}

/// What dnf will download for `look`, or `None` when there is nothing to upgrade or nothing left
/// to download.
pub(super) fn plan(calls: &Calls<'_>, look: &Look<'_>) -> Result<Option<Download>, KindError> {
    let options = options(look);
    let what = "dnf upgrade --assumeno";
    let (code, lines) =
        calls.listing(what, &arguments(&ESTIMATE, &options), busy, &[0, ABORTED])?;
    let total = lines
        .iter()
        .find_map(|line| line.trim().strip_prefix(TOTAL))
        .map(str::trim);
    let bytes = match (code, total) {
        (_, Some(size)) => parse_size(size)
            .ok_or_else(|| listing_failed(format!("{what} printed a size it does not know")))?,
        (0, None) => return Ok(None),
        (_, None) => {
            return Err(listing_failed(format!(
                "{what} exited {code} and printed no download size"
            )));
        }
    };
    if bytes == 0 {
        return Ok(None);
    }
    Ok(Some(Download {
        dir: CACHE,
        bytes,
        what: "dnf upgrade --downloadonly",
        args: arguments(&DOWNLOAD, &options),
    }))
}

/// A size as dnf prints it (`format_number`): a number, then nothing for bytes or ` k`, ` M`,
/// ` G`, ` T` for a power of 1024; rounded up to a byte.
pub fn parse_size(text: &str) -> Option<u64> {
    let (number, unit) = text.split_once(' ').unwrap_or((text, ""));
    let power = match unit.trim() {
        "" => 0,
        "k" => 1,
        "M" => 2,
        "G" => 3,
        "T" => 4,
        _ => return None,
    };
    scaled(number, 1024u64.pow(power))
}

#[cfg(test)]
#[path = "dnf_tests.rs"]
mod tests;
