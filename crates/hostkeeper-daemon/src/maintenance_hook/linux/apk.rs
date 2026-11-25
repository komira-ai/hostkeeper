//! apk's part of the stage ([`super::stage`]): on an Alpine host,
//!
//! 1. `apk update -q` refreshes the index (it takes apk's database lock);
//! 2. `apk version -l <` lists the installed packages that have a newer version, read by the
//!    maintenance look's parser ([`parse_apk_version`]); the packages the scope makes due are
//!    kept ([`Look::due`]): apk has no security origin, so under `security` only a kernel is, and
//!    hostkeeper's own never are;
//! 3. `apk info --size <packages>` prints apk's size of each version it knows of those packages
//!    (`<name>-<version> installed size:`, then `<n> <unit>`); a package's estimate is the larger
//!    of its installed and its new version's, the unpacked size, which is more than the archive;
//! 4. `apk fetch --quiet --output /var/cache/hostkeeper/maintenance/apk <packages>` downloads the
//!    new versions into hostkeeper's own staging directory, and installs nothing.
//!
//! hostkeeper's own packages are never named, so nothing of theirs is downloaded; the pin that
//! keeps them where they are during the upgrade (`name=version` in `/etc/apk/world`) is the
//! apply's, since the stage changes nothing apk reads. A held lock (`Unable to lock database`) is
//! waited for by the stage ([`super::stage::Calls`]).

use std::collections::BTreeMap;

use super::stage::{
    Calls, Download, LISTING_DEADLINE, REASON_LISTING_FAILED, arguments, listing_failed,
    never_busy, scaled,
};
use crate::maintenance_apk::{APK_KERNELS, ApkUpdate, lock_held, parse_apk_version};
use crate::maintenance_collect::Look;
use crate::maintenance_hook::kind::KindError;

/// Where the stage puts the packages `apk fetch` downloads.
pub const STAGE_DIR: &str = "/var/cache/hostkeeper/maintenance/apk";

const UPDATE: [&str; 2] = ["update", "-q"];
const VERSION: [&str; 3] = ["version", "-l", "<"];
const INFO: [&str; 2] = ["info", "--size"];
const FETCH: [&str; 4] = ["fetch", "--quiet", "--output", STAGE_DIR];
const SIZE_HEADER: &str = " installed size:";

/// Whether an `apk` run found the lock held, as the maintenance look reads it ([`lock_held`]).
pub(super) fn busy(code: Option<i32>, text: &str) -> bool {
    lock_held(code, text)
}

/// What apk will download for `look`, or `None` when nothing is due.
pub(super) fn plan(calls: &Calls<'_>, look: &Look<'_>) -> Result<Option<Download>, KindError> {
    calls.command(
        "apk update",
        &arguments(&UPDATE, &[]),
        busy,
        REASON_LISTING_FAILED,
        LISTING_DEADLINE,
    )?;
    let (_, listed) = calls.listing("apk version", &arguments(&VERSION, &[]), never_busy, &[0])?;
    let updates: Vec<ApkUpdate> = parse_apk_version(&listed.join("\n"))
        .updates
        .into_iter()
        .filter(|update| {
            let kernel = APK_KERNELS.contains(&update.name.as_str());
            look.due(&update.name, false, kernel)
        })
        .collect();
    if updates.is_empty() {
        return Ok(None);
    }
    let names: Vec<String> = updates.iter().map(|update| update.name.clone()).collect();
    let (_, info) = calls.listing(
        "apk info --size",
        &arguments(&INFO, &names),
        never_busy,
        &[0],
    )?;
    let sizes = parse_apk_sizes(&info).ok_or_else(|| {
        listing_failed("apk info --size wrote a line it does not know".to_owned())
    })?;
    let mut bytes: u64 = 0;
    for update in &updates {
        let size = [&update.from, &update.to]
            .into_iter()
            .filter_map(|version| sizes.get(&format!("{}-{version}", update.name)))
            .max()
            .ok_or_else(|| {
                listing_failed(format!("apk info --size gave no size of {}", update.name))
            })?;
        bytes = bytes.saturating_add(*size);
    }
    Ok(Some(Download {
        dir: STAGE_DIR,
        bytes,
        what: "apk fetch",
        args: arguments(&FETCH, &names),
    }))
}

/// The sizes `apk info --size` printed, by `<name>-<version>`: each header
/// `<name>-<version> installed size:` followed by its size line, blank lines passed over; `None`
/// when a line is neither, or a header has no size after it.
pub fn parse_apk_sizes(lines: &[String]) -> Option<BTreeMap<String, u64>> {
    let mut sizes = BTreeMap::new();
    let mut header: Option<&str> = None;
    for line in lines.iter().map(|line| line.trim()) {
        if line.is_empty() {
            continue;
        }
        match (header.take(), line.strip_suffix(SIZE_HEADER)) {
            (None, Some(package)) => header = Some(package),
            (Some(package), None) => {
                sizes.insert(package.to_owned(), parse_apk_size(line)?);
            }
            _ => return None,
        }
    }
    header.is_none().then_some(sizes)
}

/// A size as apk prints it: a whole number, then `B` or a power of 1024 (`KiB`, `MiB`, `GiB`,
/// `TiB`). apk rounds down, so a size in a unit larger than a byte is taken one unit up.
pub fn parse_apk_size(text: &str) -> Option<u64> {
    let (number, unit) = text.split_once(' ')?;
    let (power, rounded) = match unit.trim() {
        "B" => (0, 0),
        "KiB" => (1, 1),
        "MiB" => (2, 1),
        "GiB" => (3, 1),
        "TiB" => (4, 1),
        _ => return None,
    };
    let whole: u64 = number.parse().ok()?;
    scaled(&whole.checked_add(rounded)?.to_string(), 1024u64.pow(power))
}

#[cfg(test)]
#[path = "apk_tests.rs"]
mod tests;
