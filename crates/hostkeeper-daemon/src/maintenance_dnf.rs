//! The collector `dnf` ([`crate::maintenance_collect`]): the updates `dnf` offers on a Red Hat
//! family host, from two listings: `dnf updateinfo list --security -q` names the packages a
//! security advisory covers, and `dnf check-update -q` lists every update (exit 100) or none
//! (exit 0). Each listing is parsed by one function ([`parse_dnf_security`],
//! [`parse_dnf_check_update`]), pinned by fixtures; a line that function does not know is counted
//! and never guessed at. `check-update` does not give the installed version, so a due item has no
//! `from`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};

use crate::maintenance_collect::{Collector, Look, Section, linux_item, listing};

/// The program that is run.
pub const DNF: &str = "/usr/bin/dnf";

/// How long each listing is waited for.
pub const DNF_DEADLINE: Duration = Duration::from_secs(300);

/// The packages that are a kernel: a newer one needs a reboot.
pub const DNF_KERNELS: [&str; 4] = ["kernel", "kernel-core", "kernel-uek", "kernel-uek-core"];

const SECURITY_ARGS: [&str; 4] = ["updateinfo", "list", "--security", "-q"];
const CHECK_ARGS: [&str; 2] = ["check-update", "-q"];
/// What `dnf check-update` exits with when it lists updates.
const UPDATES_AVAILABLE: i32 = 100;
const METADATA_CHECK: &str = "Last metadata expiration check:";
const OBSOLETING: &str = "Obsoleting Packages";
const SECURITY_NOTE: &str = "Security: ";
/// What ends the severity column of a security advisory row.
const SECURITY_SUFFIX: &str = "/Sec.";

/// The packages `dnf updateinfo list --security` names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityListing {
    /// The names (without version or architecture) of the packages a security advisory covers.
    pub packages: BTreeSet<String>,
    /// How many lines were not known.
    pub unparsed: u32,
}

/// One update `dnf check-update` lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DnfUpdate {
    /// The package's name, without its architecture.
    pub name: String,
    /// The version and release it would be updated to (`1:3.2.2-6.el9_5`).
    pub to: String,
}

/// What `dnf check-update` listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateListing {
    /// The updates, in the order listed.
    pub updates: Vec<DnfUpdate>,
    /// How many lines were not known.
    pub unparsed: u32,
}

/// The name of `package.arch`, or `None` when there is no architecture or no name.
fn without_arch(text: &str) -> Option<&str> {
    text.rsplit_once('.')
        .map(|(name, _)| name)
        .filter(|name| !name.is_empty())
}

/// The name of a `name-[epoch:]version-release.arch` string, or `None` when it has not that shape.
fn nevra_name(nevra: &str) -> Option<&str> {
    let mut parts = without_arch(nevra)?.rsplitn(3, '-');
    let (_release, _version, name) = (parts.next()?, parts.next()?, parts.next()?);
    Some(name).filter(|name| !name.is_empty())
}

/// What `dnf updateinfo list --security -q` wrote (dnf 4): rows `<advisory> <severity>/Sec.
/// <nevra>`, a `Last metadata expiration check:` line and blank lines are known; anything else is
/// counted.
#[must_use]
pub fn parse_dnf_security(text: &str) -> SecurityListing {
    let mut listing = SecurityListing::default();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.is_empty() || line.starts_with(METADATA_CHECK) {
            continue;
        }
        let name = match fields.as_slice() {
            [_, severity, nevra] if severity.ends_with(SECURITY_SUFFIX) => nevra_name(nevra),
            _ => None,
        };
        match name {
            Some(name) => {
                listing.packages.insert(name.to_owned());
            }
            None => listing.unparsed = listing.unparsed.saturating_add(1),
        }
    }
    listing
}

/// What `dnf check-update -q` wrote: rows `<name>.<arch> <version-release> <repo>`; a long name
/// alone on its line with the version and repository on the next, which starts with white space,
/// is one row; the `Obsoleting Packages` header, the indented lines under it, `Last metadata
/// expiration check:` and `Security: ` lines and blank lines are known; anything else is counted.
#[must_use]
pub fn parse_dnf_check_update(text: &str) -> UpdateListing {
    let mut listing = UpdateListing::default();
    let mut obsoleting = false;
    // A name that was alone on its line, waiting for the line that holds its version.
    let mut waiting: Option<&str> = None;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let indented = line.starts_with([' ', '\t']);
        if let Some(name) = waiting.take() {
            if indented && let [version, _repo] = fields.as_slice() {
                listing.updates.push(DnfUpdate {
                    name: name.to_owned(),
                    to: (*version).to_owned(),
                });
                continue;
            }
            listing.unparsed = listing.unparsed.saturating_add(1);
        }
        if fields.is_empty()
            || line.starts_with(METADATA_CHECK)
            || line.starts_with(SECURITY_NOTE)
            || (indented && obsoleting)
        {
            continue;
        }
        if line.trim_end() == OBSOLETING {
            obsoleting = true;
            continue;
        }
        let name = fields
            .first()
            .copied()
            .and_then(without_arch)
            .filter(|_| !indented);
        match (name, fields.as_slice()) {
            (Some(name), [_]) => waiting = Some(name),
            (Some(name), [_, version, _repo]) => listing.updates.push(DnfUpdate {
                name: name.to_owned(),
                to: (*version).to_owned(),
            }),
            _ => listing.unparsed = listing.unparsed.saturating_add(1),
        }
    }
    if waiting.is_some() {
        listing.unparsed = listing.unparsed.saturating_add(1);
    }
    listing
}

/// The collector of the updates `dnf` offers.
pub struct Dnf {
    dnf: Box<dyn Launchctl>,
}

impl Dnf {
    /// A collector that runs `dnf` through `dnf`.
    #[must_use]
    pub fn new(dnf: Box<dyn Launchctl>) -> Dnf {
        Dnf { dnf }
    }

    /// The collector of this host: [`DNF`], each listing waited for [`DNF_DEADLINE`].
    #[must_use]
    pub fn at_host() -> Dnf {
        Dnf::new(Box::new(LaunchctlProcess::new(
            Path::new(DNF),
            DNF_DEADLINE,
        )))
    }
}

impl Collector for Dnf {
    fn name(&self) -> &'static str {
        "dnf"
    }

    fn collect(&self, look: &Look<'_>) -> Result<Section, String> {
        let (_, advisories) = listing(&*self.dnf, "dnf updateinfo list", &SECURITY_ARGS, &[0])?;
        let security = parse_dnf_security(&advisories);
        let (_, updates) = listing(
            &*self.dnf,
            "dnf check-update",
            &CHECK_ARGS,
            &[0, UPDATES_AVAILABLE],
        )?;
        let updates = parse_dnf_check_update(&updates);
        let mut section = Section {
            unparsed: security.unparsed.saturating_add(updates.unparsed),
            ..Section::default()
        };
        for update in &updates.updates {
            let is_security = security.packages.contains(&update.name);
            let kernel = DNF_KERNELS.contains(&update.name.as_str());
            if look.due(&update.name, is_security, kernel) {
                section.due.push(linux_item(
                    &update.name,
                    "",
                    &update.to,
                    is_security,
                    kernel,
                ));
            }
        }
        Ok(section)
    }
}

#[cfg(test)]
#[path = "maintenance_dnf_tests.rs"]
mod tests;
