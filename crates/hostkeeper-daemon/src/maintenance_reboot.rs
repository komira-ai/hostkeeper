//! The collector `reboot` of the maintenance look ([`crate::maintenance_collect`]): the reboots
//! the host itself says it needs, by the file `/run/reboot-required` (Debian and Ubuntu),
//! `needrestart -b` and `needs-restarting -r` (docs/design/maintenance-updates.md, section 1). A
//! service that needs a restart is not a reboot: only a kernel is.

use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};

use crate::maintenance_collect::{Collector, HostRoot, Look, Section, listing, reboot_item};

/// The program of Debian's and Ubuntu's `needrestart`.
pub const NEEDRESTART: &str = "/usr/sbin/needrestart";

/// The program of the Red Hat family's `needs-restarting`.
pub const NEEDS_RESTARTING: &str = "/usr/bin/needs-restarting";

/// How long each of the two programs is given.
pub const DEADLINE: Duration = Duration::from_secs(60);

/// The file a package leaves behind when it needs a reboot.
const REBOOT_REQUIRED: &str = "/run/reboot-required";

/// The lines `needrestart -b` prints that name something other than the kernel's state: known,
/// and not a reason to reboot.
const NEEDRESTART_KNOWN: [&str; 9] = [
    "NEEDRESTART-VER",
    "NEEDRESTART-KCUR",
    "NEEDRESTART-KEXP",
    "NEEDRESTART-SVC",
    "NEEDRESTART-CONT",
    "NEEDRESTART-SESS",
    "NEEDRESTART-UCSTA",
    "NEEDRESTART-UCCUR",
    "NEEDRESTART-UCEXP",
];

/// The collector `reboot`: the reboots the host says it needs.
pub struct RebootNeed {
    needrestart: Box<dyn Launchctl>,
    needs_restarting: Box<dyn Launchctl>,
    root: HostRoot,
}

impl RebootNeed {
    /// The collector over `needrestart` and `needs-restarting` (each run only when its program is
    /// a file under `root`, at [`NEEDRESTART`] and [`NEEDS_RESTARTING`]) and the files under
    /// `root`.
    #[must_use]
    pub fn new(
        needrestart: Box<dyn Launchctl>,
        needs_restarting: Box<dyn Launchctl>,
        root: HostRoot,
    ) -> RebootNeed {
        RebootNeed {
            needrestart,
            needs_restarting,
            root,
        }
    }

    /// The collector over the host's own programs, each bounded by [`DEADLINE`], and its own
    /// files.
    #[must_use]
    pub fn at_host() -> RebootNeed {
        let tool = |program: &str| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(Path::new(program), DEADLINE))
        };
        RebootNeed::new(tool(NEEDRESTART), tool(NEEDS_RESTARTING), HostRoot::host())
    }
}

impl Collector for RebootNeed {
    fn name(&self) -> &'static str {
        "reboot"
    }

    fn collect(&self, _look: &Look<'_>) -> Result<Section, String> {
        let mut section = Section::default();
        if self.root.exists(REBOOT_REQUIRED) {
            section.due.push(reboot_item("reboot-required", ""));
        }
        if self.root.is_file(NEEDRESTART) {
            let (_, text) = listing(self.needrestart.as_ref(), "needrestart -b", &["-b"], &[0])?;
            let found = parse_needrestart(&text);
            section.unparsed = found.unparsed;
            if found.reboot {
                section.due.push(reboot_item("needrestart", ""));
            }
        }
        if self.root.is_file(NEEDS_RESTARTING) {
            let (code, _) = listing(
                self.needs_restarting.as_ref(),
                "needs-restarting -r",
                &["-r"],
                &[0, 1],
            )?;
            if code == 1 {
                section.due.push(reboot_item("needs-restarting", ""));
            }
        }
        Ok(section)
    }
}

/// What `needrestart -b` said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeedrestartListing {
    /// Whether the kernel's state (`NEEDRESTART-KSTA`) says a reboot is needed: 2 (the kernel's
    /// ABI changed) or 3 (a newer version is installed).
    pub reboot: bool,
    /// How many lines were none that `needrestart -b` is known to print, or a `NEEDRESTART-KSTA`
    /// that is not a number.
    pub unparsed: u32,
}

/// The kernel's state and the unparsed lines of the output of `needrestart -b`.
#[must_use]
pub fn parse_needrestart(text: &str) -> NeedrestartListing {
    let mut found = NeedrestartListing {
        reboot: false,
        unparsed: 0,
    };
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        match line.split_once(':') {
            Some(("NEEDRESTART-KSTA", state)) => match state.trim().parse::<u8>() {
                Ok(state) => found.reboot |= state == 2 || state == 3,
                Err(_) => found.unparsed += 1,
            },
            Some((key, _)) if NEEDRESTART_KNOWN.contains(&key) => {}
            _ => found.unparsed += 1,
        }
    }
    found
}

#[cfg(test)]
#[path = "maintenance_reboot_tests.rs"]
mod tests;
