//! What launchd keeps about a job in the system domain, as the Mac device tests read it: its
//! override (the mark `launchctl disable` and `launchctl enable` write, which `launchctl
//! print-disabled system` lists) and whether the domain holds the job (`launchctl print
//! system/<label>`).
//!
//! The override decides whether `launchctl bootstrap`, and so a boot, loads a job whose plist is
//! in `/Library/LaunchDaemons`: a job marked disabled is not loaded after a reboot, though the
//! process that runs now keeps running. A test that proves a package's update leaves a job
//! usable at the next boot therefore reads the override, not only whether the job is loaded now.
//! [`override_of`] tells the three states apart (no mark, enabled, disabled), so that "not
//! disabled" is never read from a listing the test could not parse: a line it does not know is a
//! panic naming the listing.

use std::time::Duration;

use hostkeeper_launchd::launchctl::{JobStatus, print};
use hostkeeper_launchd::{Label, Launchctl, LaunchctlProcess};

/// The deadline of one `launchctl` command here.
const DEADLINE: Duration = Duration::from_secs(60);

/// What `launchctl print-disabled system` says of a label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Override {
    /// The listing does not name the label: no `disable` or `enable` was ever run for it.
    Absent,
    /// The listing names the label as enabled: an `enable` cleared an earlier `disable`, or was
    /// run first.
    Enabled,
    /// The listing names the label as disabled: launchd does not load the job.
    Disabled,
}

/// The override of `label` in launchd's database of the system domain.
///
/// # Panics
/// `launchctl print-disabled system` cannot be run, does not exit 0, or lists the label with a
/// value other than the ones this knows (`enabled` and `false`, `disabled` and `true`: the
/// words of macOS 15 and the booleans of the releases before it).
pub fn override_of(label: &Label) -> Override {
    let args = ["print-disabled".to_owned(), "system".to_owned()];
    let out = LaunchctlProcess::system(DEADLINE)
        .run(&args)
        .unwrap_or_else(|e| panic!("launchctl print-disabled system did not run to its end: {e}"));
    assert_eq!(
        out.code,
        Some(0),
        "launchctl print-disabled system: {}",
        out.stderr_text()
    );
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();
    let key = format!("\"{}\" =>", label.as_str());
    printed
        .lines()
        .find_map(|line| line.trim().strip_prefix(&key))
        .map_or(Override::Absent, |value| match value.trim() {
            "enabled" | "false" => Override::Enabled,
            "disabled" | "true" => Override::Disabled,
            other => panic!("the override of {label} reads {other:?} in:\n{printed}"),
        })
}

/// Asserts that launchd does not hold `label` disabled: it has no override or an enabling one,
/// which is what lets the next boot load the job. `when` says at what point of the test.
///
/// # Panics
/// The override is [`Override::Disabled`], or [`override_of`] panics.
pub fn assert_not_disabled(label: &Label, when: &str) {
    let found = override_of(label);
    assert!(
        matches!(found, Override::Absent | Override::Enabled),
        "{when}: launchd holds {label} disabled ({found:?}), so a reboot would not load it"
    );
}

/// Whether the system domain holds `label` (`launchctl print system/<label>` finds it).
///
/// # Panics
/// `launchctl print` cannot be run or read.
pub fn is_loaded(label: &Label) -> bool {
    match print(&LaunchctlProcess::system(DEADLINE), label) {
        Ok(JobStatus::Loaded(_)) => true,
        Ok(JobStatus::NotLoaded) => false,
        Err(e) => panic!("launchctl print system/{label}: {e}"),
    }
}
