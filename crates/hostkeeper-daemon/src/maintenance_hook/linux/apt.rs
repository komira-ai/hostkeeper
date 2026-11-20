//! apt's part of the stage ([`super::stage`]): on a Debian or Ubuntu host,
//!
//! 1. `apt-get update -qq` refreshes the index (it takes the lists' lock);
//! 2. `apt-get --simulate -qq dist-upgrade` names what an upgrade would install, read by the
//!    maintenance look's parser ([`parse_apt_simulation`]), and the packages the scope makes due
//!    are kept ([`Look::due`]: security updates and kernels, or all), hostkeeper's own never;
//! 3. `apt-get --print-uris -qq install --only-upgrade <packages>` lists each archive still to
//!    download with its size, apt's own estimate (it takes no lock);
//! 4. `apt-get --download-only -y -qq -o Dpkg::Options::=--force-confold install --only-upgrade
//!    <packages>` downloads them into `/var/cache/apt/archives` and installs nothing.
//!
//! hostkeeper's own packages are never named, and stay held as the repository installer holds them
//! (`apt-mark hold`), so the download does not move them. No variable is set: `--download-only`
//! runs no package script, so no `DEBIAN_FRONTEND` is needed. apt-get's own lock timeout is not
//! used: a held lock (`Could not get lock`) is waited for by the stage ([`super::stage::Calls`]).

use hostkeeper_install::repository::apt::parse::BUSY;

use super::stage::{
    Calls, Download, LISTING_DEADLINE, REASON_LISTING_FAILED, arguments, listing_failed, never_busy,
};
use crate::maintenance_apt::parse_apt_simulation;
use crate::maintenance_collect::Look;
use crate::maintenance_hook::kind::KindError;

/// Where apt keeps the archives it downloads.
pub const ARCHIVES: &str = "/var/cache/apt/archives";

const UPDATE: [&str; 2] = ["update", "-qq"];
const SIMULATE: [&str; 3] = ["--simulate", "-qq", "dist-upgrade"];
const PRINT_URIS: [&str; 4] = ["--print-uris", "-qq", "install", "--only-upgrade"];
const DOWNLOAD: [&str; 7] = [
    "--download-only",
    "-y",
    "-qq",
    "-o",
    "Dpkg::Options::=--force-confold",
    "install",
    "--only-upgrade",
];

/// Whether an `apt-get` run that did not exit 0 found the lock held: it says so in one of the
/// forms the repository installer reads ([`BUSY`]).
pub(super) fn busy(code: Option<i32>, text: &str) -> bool {
    code != Some(0) && BUSY.iter().any(|needle| text.contains(needle))
}

/// What apt will download for `look`, or `None` when nothing is due or every archive is already
/// downloaded.
pub(super) fn plan(calls: &Calls<'_>, look: &Look<'_>) -> Result<Option<Download>, KindError> {
    calls.command(
        "apt-get update",
        &arguments(&UPDATE, &[]),
        busy,
        REASON_LISTING_FAILED,
        LISTING_DEADLINE,
    )?;
    let (_, simulation) = calls.listing(
        "apt-get dist-upgrade simulation",
        &arguments(&SIMULATE, &[]),
        never_busy,
        &[0],
    )?;
    let packages = due_packages(&simulation.join("\n"), look);
    if packages.is_empty() {
        return Ok(None);
    }
    let (_, uris) = calls.listing(
        "apt-get --print-uris",
        &arguments(&PRINT_URIS, &packages),
        never_busy,
        &[0],
    )?;
    let bytes = download_bytes(&uris).ok_or_else(|| {
        listing_failed("apt-get --print-uris wrote a line that is not an archive".to_owned())
    })?;
    if bytes == 0 {
        return Ok(None);
    }
    Ok(Some(Download {
        dir: ARCHIVES,
        bytes,
        what: "apt-get --download-only",
        args: arguments(&DOWNLOAD, &packages),
    }))
}

/// The packages of the simulation `text` that `look` makes due, each once, in its order.
fn due_packages(text: &str, look: &Look<'_>) -> Vec<String> {
    let mut packages: Vec<String> = Vec::new();
    for update in parse_apt_simulation(text).updates {
        if look.due(&update.package, update.security, update.kernel)
            && !packages.contains(&update.package)
        {
            packages.push(update.package);
        }
    }
    packages
}

/// The bytes of the archives `apt-get --print-uris` listed, one per line
/// (`'<uri>' <file> <size> <hash>`), blank lines passed over; `None` when a line is not of that
/// shape or the sum is more than `u64` holds.
pub fn download_bytes(lines: &[String]) -> Option<u64> {
    lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .try_fold(0u64, |sum, line| sum.checked_add(archive_size(line)?))
}

/// The size of one `--print-uris` line.
fn archive_size(line: &str) -> Option<u64> {
    let (_uri, rest) = line.strip_prefix('\'')?.split_once("' ")?;
    let mut fields = rest.split_whitespace();
    let _file = fields.next()?;
    fields.next()?.parse().ok()
}

#[cfg(test)]
#[path = "apt_tests.rs"]
mod tests;
