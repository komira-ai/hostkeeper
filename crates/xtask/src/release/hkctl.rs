//! `release hkctl --version V --commit C --out D`: the release asset `hkctl-<V>`, the command
//! line of hostkeeper's coordinator (`docs/design/two-programs.md` section 2.4): one bash script
//! for every platform, generated from the protos' command table by `hostkeeper_hkctl::script`,
//! whose `--version` prints `hkctl <V>+<first 12 hex digits of C>`. The artifacts workflow writes
//! it beside the other assets before it sums them, and attests it with a bundle of its own. No
//! host package carries it.
//!
//! [`problems`] is the step of `release check` that holds the asset to that shape: a bash
//! script whose one version line is the release's. The commit it was built from is what its
//! attestation names, which `release check` verifies against the pin.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use hostkeeper_proto::ctl::Command;

use super::Error;
use super::contract::{Commit, HKCTL, Version, hkctl_name};

/// The first line of the script.
const SHEBANG: &str = "#!/usr/bin/env bash";

/// What starts the script's version line; the stamp follows it, quoted.
const VERSION_LINE: &str = "_hk_version='";

/// Writes `hkctl-<version>` (0755) into `out`, made if missing, from `commands`.
pub(crate) fn write(
    commands: &[Command],
    version: &Version,
    commit: &Commit,
    out: &Path,
) -> Result<Vec<String>, Error> {
    let stamp = format!("{version}+{}", commit.short());
    let text =
        hostkeeper_hkctl::script(commands, &stamp).map_err(|e| Error::Failed(e.to_string()))?;
    std::fs::create_dir_all(out).map_err(Error::io(format!("create {}", out.display())))?;
    let path = out.join(hkctl_name(version));
    std::fs::write(&path, text).map_err(Error::io(format!("write {}", path.display())))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .map_err(Error::io(format!("chmod {}", path.display())))?;
    Ok(vec![format!("{}: {HKCTL} {stamp}", path.display())])
}

/// What is wrong with `text` as the asset `hkctl-<version>`: it must start with the bash
/// shebang, and hold exactly one version line, `_hk_version='hkctl <version>+<12 hex>'`.
pub(crate) fn problems(text: &str, version: &Version) -> Vec<String> {
    let name = hkctl_name(version);
    let mut problems = Vec::new();
    if text.lines().next() != Some(SHEBANG) {
        problems.push(format!("{name} does not start with `{SHEBANG}`"));
    }
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with(VERSION_LINE))
        .collect();
    let prefix = format!("{VERSION_LINE}{HKCTL} {version}+");
    let stamped = |line: &&str| {
        line.strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix('\''))
            .is_some_and(|hex| {
                hex.len() == 12 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
    };
    if !matches!(lines.as_slice(), [one] if stamped(one)) {
        problems.push(format!(
            "{name} does not hold exactly one version line `{prefix}<12 hex>'`"
        ));
    }
    problems
}

#[cfg(test)]
mod tests;
