//! The console's issue file of the enrollment (docs/design/network-install.md, section 7, "Show
//! the key hash on the machine's console"): after each step of the enrollment loop, the daemon
//! writes the request's state, the coordinator it posts to and the key hash of the request to
//! [`ENROLLMENT_ISSUE`], and asks the gettys to draw their login prompt again
//! (`agetty --reload`). The deb and the boot-image overlay link [`ISSUE_LINK`] to that file, so
//! the login prompt of the machine's console shows it. The person who approves the request
//! compares the key hash on the console with the `key_sha256` the coordinator lists: the one
//! check someone elsewhere on the network cannot fake.
//!
//! The text is [`step_text`] of the step and the enrollment it left, or [`unopened_text`] while
//! the enroller cannot be made. It is written only when it differs from the text written last
//! by this process, through a temporary file renamed over the issue file, so a getty never reads
//! half of it; the gettys are asked to redraw after each write. A write that fails is logged
//! ([`NO_ISSUE`]) and tried again after the next step; a redraw that fails is logged
//! ([`NO_REDRAW`]) and not tried again until the text changes. Each failure is logged once, and
//! again only when its line changes or after a write and redraw that succeeded.
//!
//! Whatever agetty does with the file, the text holds: every character that is not printable
//! ASCII, and every backslash (agetty's escape character in an issue file), is written as `?`.
//! Whether agetty of the installed system reads `/etc/issue.d`, follows the link, and shows a
//! file written after its prompt was drawn only once asked to redraw is checked on the real
//! system (the design's **[V]**); asking it to redraw when it would have read the file anyway
//! changes nothing.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hostkeeper_enroll::{Enrollment, Error, Outcome};
use hostkeeper_link::Log;
use hostkeeper_placed::CommandLauncher;

/// The issue file the daemon writes on a Linux host: in its runtime directory, which only root
/// reads, as a getty does.
pub const ENROLLMENT_ISSUE: &str = "/run/hostkeeper/enrollment.issue";

/// The link the deb, the rpm and the boot-image overlay install to [`ENROLLMENT_ISSUE`]: agetty
/// shows each `*.issue` file of `/etc/issue.d` above its login prompt.
pub const ISSUE_LINK: &str = "/etc/issue.d/hostkeeper.issue";

/// The agetty the daemon asks to redraw, on a Linux host: `/sbin`, which is `/usr/sbin` on a
/// system with a merged `/usr` and agetty's own directory on one without.
pub const AGETTY: &str = "/sbin/agetty";

/// The argument that makes agetty ask every running agetty to draw its prompt again.
pub const RELOAD: &str = "--reload";

/// The line logged when the issue file is not written, before the path and the error.
pub const NO_ISSUE: &str = "error: the console's issue file is not written: ";

/// The line logged when the gettys were not asked to redraw, before the error.
pub const NO_REDRAW: &str = "error: the gettys were not asked to redraw their prompt: ";

/// The first line of every text: what the lines below it are about.
const HEADING: &str = "hostkeeper enrollment";

/// The issue file of a host's console and the command that has the gettys redraw.
pub struct ConsoleIssue {
    path: PathBuf,
    coordinator: String,
    reload: Vec<String>,
    launcher: Box<dyn CommandLauncher + Send>,
    /// The text this process wrote last.
    shown: Option<String>,
    /// The failure logged last, until a write and a redraw succeed.
    failed: Option<String>,
}

impl ConsoleIssue {
    /// The issue file at `path`, whose texts name `coordinator` (the daemon config's), redrawn by
    /// `<agetty> --reload` run through `launcher`. Nothing is written or run here.
    pub fn new(
        path: PathBuf,
        coordinator: String,
        agetty: &Path,
        launcher: Box<dyn CommandLauncher + Send>,
    ) -> ConsoleIssue {
        ConsoleIssue {
            path,
            coordinator,
            reload: vec![agetty.to_string_lossy().into_owned(), RELOAD.to_owned()],
            launcher,
            shown: None,
            failed: None,
        }
    }

    /// Shows what a step of the enrollment loop came to, `step`, with the enrollment it left
    /// ([`step_text`]); failures go to `log`.
    pub fn after_step(
        &mut self,
        step: &Result<Outcome, Error>,
        enrollment: &Enrollment,
        log: &dyn Log,
    ) {
        let text = step_text(&self.coordinator, step, enrollment);
        self.show(text, log);
    }

    /// Shows that the enroller cannot be made, for `why` ([`unopened_text`]); failures go to
    /// `log`.
    pub fn unopened(&mut self, why: &str, log: &dyn Log) {
        let text = unopened_text(&self.coordinator, why);
        self.show(text, log);
    }

    /// Writes `text` when it differs from the text written last, then asks the gettys to redraw.
    fn show(&mut self, text: String, log: &dyn Log) {
        if self.shown.as_ref() == Some(&text) {
            return;
        }
        if let Err(error) = write_issue(&self.path, &text) {
            let line = format!("{NO_ISSUE}{}: {error}", self.path.display());
            self.fail(line, log);
            return;
        }
        self.shown = Some(text);
        let argv: Vec<&str> = self.reload.iter().map(String::as_str).collect();
        match self.launcher.run(&argv) {
            Ok(()) => self.failed = None,
            Err(why) => self.fail(format!("{NO_REDRAW}{}: {why}", argv.join(" ")), log),
        }
    }

    /// Logs `line` unless it is the failure logged last.
    fn fail(&mut self, line: String, log: &dyn Log) {
        if self.failed.as_ref() != Some(&line) {
            log.line(&line);
            self.failed = Some(line);
        }
    }
}

/// Writes `text` to a temporary file beside `path`, mode 0600, and renames it over `path`.
fn write_issue(path: &Path, text: &str) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(text.as_bytes())?;
    fs::rename(&tmp, path)
}

/// The text of the issue file after a step that came to `step` and left the host's enrollment
/// at `enrollment`, the requests going to `coordinator`:
///
/// ```text
/// hostkeeper enrollment: <state>
///   coordinator: <coordinator>
///   key sha256:  <key hash>
///
/// ```
///
/// The state, by the first that holds:
///
/// - the host is enrolled: `approved, enrolled as host:<id>`, and the text has no key line;
/// - the coordinator's CA is not the pinned one: `nothing sent: the coordinator's CA is not the
///   pinned ca_sha256` (the hash it presents is in the daemon's log, not here, so that no second
///   hash stands beside the key's);
/// - the request was denied: `denied: <reason>`;
/// - another error: `error: <error>`, as the daemon logs it;
/// - the request is posted and not yet approved: `pending approval; approve only the request
///   whose key_sha256 is the key sha256 below`;
/// - the pki directory cannot be read: `error: <why>`;
/// - no request was made yet: `no request yet`.
///
/// The key line is the key hash of the pending key (the request's `key_sha256`), or `none yet`
/// while there is no pending key. Every character that is not printable ASCII, and every
/// backslash, is `?`.
pub fn step_text(
    coordinator: &str,
    step: &Result<Outcome, Error>,
    enrollment: &Enrollment,
) -> String {
    let key = match enrollment {
        Enrollment::Enrolled { host_id, .. } => {
            let state = format!("approved, enrolled as {}", host_id.subject());
            return text(&state, coordinator, None);
        }
        Enrollment::Pending { key_sha256 } => key_sha256.to_string(),
        Enrollment::Unenrolled | Enrollment::Unreadable(_) => "none yet".to_owned(),
    };
    let state = match (step, enrollment) {
        (Err(Error::UntrustedCoordinator { .. }), _) => {
            "nothing sent: the coordinator's CA is not the pinned ca_sha256".to_owned()
        }
        (Err(Error::Denied(reason)), _) => format!("denied: {reason}"),
        (Err(error), _) => format!("error: {error}"),
        (Ok(_), Enrollment::Pending { .. }) => {
            "pending approval; approve only the request whose key_sha256 is the key sha256 below"
                .to_owned()
        }
        (Ok(_), Enrollment::Unreadable(why)) => format!("error: {why}"),
        (Ok(_), _) => "no request yet".to_owned(),
    };
    text(&state, coordinator, Some(&key))
}

/// The text of the issue file while the enroller cannot be made, for `why`: the state is
/// `error: the enroller cannot be opened: <why>`, and the key line `unknown`, since the pki
/// directory was not read. Written as [`step_text`] writes.
pub fn unopened_text(coordinator: &str, why: &str) -> String {
    let state = format!("error: the enroller cannot be opened: {why}");
    text(&state, coordinator, Some("unknown"))
}

/// The lines of a text, each made printable.
fn text(state: &str, coordinator: &str, key: Option<&str>) -> String {
    let mut out = format!(
        "{HEADING}: {}\n  coordinator: {}\n",
        printable(state),
        printable(coordinator)
    );
    if let Some(key) = key {
        out.push_str(&format!("  key sha256:  {}\n", printable(key)));
    }
    out.push('\n');
    out
}

/// `text` with every character that is not printable ASCII, and every backslash, as `?`.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\\' => '?',
            ' '..='~' => c,
            _ => '?',
        })
        .collect()
}
