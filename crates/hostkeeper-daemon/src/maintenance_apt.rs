//! The `apt` collector of the maintenance look (docs/design/maintenance-updates.md section 1): on a
//! Debian or Ubuntu host, which package updates are due, from a refreshed index and a simulated
//! `dist-upgrade`. Nothing is installed: the simulation prints what the upgrade would do and
//! changes nothing. A package held back (`apt-mark hold`, or kept back by the resolver) is not in
//! the simulation's `Inst` lines, so it is never due.

use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};

use crate::maintenance_collect::{Collector, Look, Section, linux_item, listing};

/// The program that refreshes the index and simulates the upgrade.
pub const APT_GET: &str = "/usr/bin/apt-get";

/// How long `apt-get update` may take: it fetches the index of every source.
const UPDATE_DEADLINE: Duration = Duration::from_secs(600);

/// How long the simulation may take.
const SIMULATE_DEADLINE: Duration = Duration::from_secs(300);

/// The arguments that refresh the index.
const UPDATE_ARGS: [&str; 2] = ["update", "-qq"];

/// The arguments that simulate the upgrade.
const SIMULATE_ARGS: [&str; 3] = ["--simulate", "-qq", "dist-upgrade"];

/// The progress lines of `apt-get` that carry no update (each may end with ` Done`).
const PROGRESS: [&str; 4] = [
    "Reading package lists...",
    "Building dependency tree...",
    "Reading state information...",
    "Calculating upgrade...",
];

/// The line `apt-get` ends its simulation with, its counts replaced by nothing.
const SUMMARY_SHAPE: &str = " upgraded,  newly installed,  to remove and  not upgraded.";

/// The collector `apt`.
pub struct Apt {
    update: Box<dyn Launchctl>,
    simulate: Box<dyn Launchctl>,
}

impl Apt {
    /// A collector refreshing the index with `update` and simulating the upgrade with `simulate`
    /// (each runs `apt-get` with the arguments it is given).
    #[must_use]
    pub fn new(update: Box<dyn Launchctl>, simulate: Box<dyn Launchctl>) -> Apt {
        Apt { update, simulate }
    }

    /// The collector of this host: [`APT_GET`] as root, the index refresh bounded at 600 seconds
    /// and the simulation at 300.
    #[must_use]
    pub fn at_host() -> Apt {
        let program = Path::new(APT_GET);
        Apt::new(
            Box::new(LaunchctlProcess::new(program, UPDATE_DEADLINE)),
            Box::new(LaunchctlProcess::new(program, SIMULATE_DEADLINE)),
        )
    }
}

impl Collector for Apt {
    fn name(&self) -> &'static str {
        "apt"
    }

    fn collect(&self, look: &Look<'_>) -> Result<Section, String> {
        listing(&*self.update, "apt-get update", &UPDATE_ARGS, &[0])?;
        let (_, text) = listing(
            &*self.simulate,
            "apt-get dist-upgrade simulation",
            &SIMULATE_ARGS,
            &[0],
        )?;
        let parsed = parse_apt_simulation(&text);
        let due = parsed
            .updates
            .iter()
            .filter(|update| look.due(&update.package, update.security, update.kernel))
            .map(|update| {
                linux_item(
                    &update.package,
                    &update.from,
                    &update.to,
                    update.security,
                    update.kernel,
                )
            })
            .collect();
        Ok(Section {
            due,
            unparsed: parsed.unparsed,
            ..Section::default()
        })
    }
}

/// One package the simulation installs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AptUpdate {
    /// The package's name, as `apt-get` prints it (with `:arch` for a foreign architecture).
    pub package: String,
    /// The installed version; empty for a package not yet installed.
    pub from: String,
    /// The version the upgrade installs.
    pub to: String,
    /// Whether one of the origins of the new version is a security archive.
    pub security: bool,
    /// Whether it is a kernel image (`linux-image-`), which needs a reboot.
    pub kernel: bool,
}

/// What [`parse_apt_simulation`] read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AptListing {
    /// The packages the simulation installs, in its order.
    pub updates: Vec<AptUpdate>,
    /// How many lines it did not know.
    pub unparsed: u32,
}

/// Reads what `apt-get --simulate dist-upgrade` wrote: its `Inst` lines are the updates; `Conf`,
/// `Remv` and `Purg` lines, indented lines, blank lines, progress lines, the lists headed
/// `The following ...:`, the summary and the simulation note are known and give nothing; any other
/// line, and an `Inst` line of another shape, is counted and never guessed at.
#[must_use]
pub fn parse_apt_simulation(text: &str) -> AptListing {
    let mut updates = Vec::new();
    let mut unparsed: u32 = 0;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Inst ") {
            match parse_inst(rest) {
                Some(update) => updates.push(update),
                None => unparsed = unparsed.saturating_add(1),
            }
        } else if !is_known(line) {
            unparsed = unparsed.saturating_add(1);
        }
    }
    AptListing { updates, unparsed }
}

/// `name [old] (new origin, origin [arch])`, the old version absent for a new package.
fn parse_inst(rest: &str) -> Option<AptUpdate> {
    let (package, rest) = rest.split_once(' ')?;
    let (from, rest) = match rest.strip_prefix('[') {
        Some(after) => after.split_once("] ")?,
        None => ("", rest),
    };
    let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
    let (to, origins) = inner.split_once(' ')?;
    let (origins, _arch) = origins.strip_suffix(']')?.rsplit_once(" [")?;
    Some(AptUpdate {
        package: package.to_owned(),
        from: from.to_owned(),
        to: to.to_owned(),
        security: origins.split(", ").any(is_security),
        kernel: package.starts_with("linux-image-"),
    })
}

/// Whether an origin (`Ubuntu:24.04/noble-security`, `Debian-Security:12/stable-security`) is a
/// security archive: its suite, after the first `/`, ends with `-security`, or its label, before
/// the first `:`, is `Debian-Security`.
fn is_security(origin: &str) -> bool {
    let label = origin.split(':').next();
    let suite = origin.split_once('/').map(|(_, suite)| suite);
    label == Some("Debian-Security") || suite.is_some_and(|suite| suite.ends_with("-security"))
}

/// Whether `line` is one `apt-get` writes that holds no `Inst` update.
fn is_known(line: &str) -> bool {
    line.is_empty()
        || line.starts_with(char::is_whitespace)
        || ["Conf ", "Remv ", "Purg "]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        || line == "NOTE: This is only a simulation!"
        || (line.starts_with("The following ") && line.ends_with(':'))
        || PROGRESS.contains(&line.strip_suffix(" Done").unwrap_or(line))
        || is_summary(line)
}

/// `N upgraded, N newly installed, N to remove and N not upgraded.`
fn is_summary(line: &str) -> bool {
    let words_only: String = line.chars().filter(|c| !c.is_ascii_digit()).collect();
    words_only == SUMMARY_SHAPE
}

#[cfg(test)]
#[path = "maintenance_apt_tests.rs"]
mod tests;
