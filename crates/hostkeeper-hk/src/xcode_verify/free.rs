//! The free space of the archive's volume, which `df -Pk <directory>` says, and the margin an
//! expansion needs: three times the archive's size, as the farm Mac's stage asks.
//! `hk` reads it through the Mac's `df` so that no system call is made here and a test can give
//! any answer.

use std::path::Path;

use super::run::{exit_text, tool};
use crate::xcode_push::Mac;

/// The most the free space may be short of the archive's size, as a multiple: the expansion needs
/// this many times the archive.
const MARGIN: u64 = 3;

/// The bytes free on the volume of `dir`: the `Available` column of the one line `df -Pk` prints
/// for it, in KiB. The error says why that could not be read.
pub(super) fn free_bytes(mac: &Mac, dir: &Path) -> Result<u64, String> {
    let out = tool(&mac.df, &[&"-Pk", &dir], None);
    if out.exit != Some(0) {
        return Err(format!(
            "{} -Pk {} {}",
            mac.df.display(),
            dir.display(),
            exit_text(&out)
        ));
    }
    available(&out.stdout).ok_or_else(|| {
        format!(
            "{} -Pk {} does not print a line with an Available column",
            mac.df.display(),
            dir.display()
        )
    })
}

/// The Available column of the last line of a report of `df -Pk`, in bytes: the number before the
/// first word that ends with `%` (the capacity), the first word being a file system's name.
pub(super) fn available(report: &str) -> Option<u64> {
    let line = report.lines().rfind(|line| !line.trim().is_empty())?;
    let words: Vec<&str> = line.split_whitespace().collect();
    // Counting from the second word, the capacity's place is the Available column's index.
    let at = words.iter().skip(1).position(|word| word.ends_with('%'))?;
    words
        .get(at)
        .and_then(|word| word.parse::<u64>().ok())
        .and_then(|kib| kib.checked_mul(1024))
}

/// Whether `free` bytes are enough to expand an archive of `size` bytes: the failure, naming both
/// numbers, when they are not.
pub(super) fn enough(free: u64, size: u64, dir: &Path) -> Result<(), String> {
    let needed = size.saturating_mul(MARGIN);
    if free >= needed {
        return Ok(());
    }
    Err(format!(
        "free_space_short: the volume of {} has {free} bytes free and the expansion needs \
         {needed} ({MARGIN} times the archive's {size})",
        dir.display()
    ))
}
