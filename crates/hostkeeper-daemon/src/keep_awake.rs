//! A Mac that builds must never sleep: at its start a Mac's daemon sets the host's power
//! management so that it stays awake, as a built-in host setting with no per-host config
//! (`keep_awake` of the daemon config turns it off; the default is on).
//!
//! The start reads the host's power settings with `pmset -g custom` ([`read_args`]), which lists
//! the settings of each power source (`Battery Power:`, `AC Power:`; a desktop has the second
//! alone) as indented lines of a key and a value. [`plan`] compares them with [`WANTED`]:
//!
//! - `sleep`, `disksleep`, `standby`, `autopoweroff` and `powernap` are 0 (the Mac never sleeps,
//!   spins its disks down, goes to standby, powers off for the deep idle state or wakes only for
//!   Power Nap);
//! - `autorestart` is 1 (the Mac starts again after a power loss);
//! - `womp` is 1 (the Mac wakes for network access).
//!
//! `displaysleep` is left as it is: a display may sleep. A key the host does not list (a setting
//! this machine does not have) is never set, and a key every power source already has at its
//! value is not touched, so a Mac that is set up gets no command at all. What differs is changed
//! by one `pmset -a` command ([`Change::args`]: `-a` is every power source), and each change is
//! logged once, with the value it replaced. A `pmset` that cannot be run or that fails is logged
//! as an error and the start goes on: the host is as it was.
//!
//! The Mac's daemon runs as root, which is all `pmset` needs to write the settings; it reads no
//! protected data, so the daemon needs no Full Disk Access for it. The settings are made at each
//! start of the daemon (a restart for an update included), not watched in between.

use std::time::Duration;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_launchd::{
    Launchctl, LaunchctlError, LaunchctlOutput, LaunchctlProcess, PlatformKind,
};

/// The tool that reads and sets the power settings.
pub const PMSET: &str = "/usr/bin/pmset";
/// How long `pmset` may run before it is killed and the setting is not made.
pub const PMSET_DEADLINE: Duration = Duration::from_secs(30);

/// The settings a build Mac has, in the order they are set: the key and the value.
pub const WANTED: [(&str, u32); 7] = [
    ("sleep", 0),
    ("disksleep", 0),
    ("standby", 0),
    ("autopoweroff", 0),
    ("powernap", 0),
    ("autorestart", 1),
    ("womp", 1),
];

/// The arguments of `pmset` that list the settings of every power source.
pub fn read_args() -> Vec<String> {
    vec!["-g".to_owned(), "custom".to_owned()]
}

/// One setting that is not as a build Mac has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The key (`sleep`).
    pub key: &'static str,
    /// The value the host has, as `pmset -g custom` spelled it; when the power sources differ,
    /// the first that is not [`Change::to`].
    pub from: String,
    /// The value it is set to.
    pub to: u32,
}

impl Change {
    /// The log line of this change.
    pub fn line(&self) -> String {
        format!(
            "level=info keep_awake: pmset {} was {}, set to {}",
            self.key, self.from, self.to
        )
    }
}

/// The arguments of the one `pmset` command that makes `changes`: `-a`, then each key and value;
/// none when there is nothing to change.
pub fn args(changes: &[Change]) -> Option<Vec<String>> {
    if changes.is_empty() {
        return None;
    }
    let mut args = vec!["-a".to_owned()];
    for change in changes {
        args.push(change.key.to_owned());
        args.push(change.to.to_string());
    }
    Some(args)
}

/// What differs between the output of `pmset -g custom` and [`WANTED`], in the order of
/// [`WANTED`]. A key is read from the first word of an indented line and its value from the
/// second (a note after the value is not read), in every power source; a key that no line has is
/// not changed, and one that is there with a value other than the wanted one (or one that is not
/// a number) is, once, naming the first such value.
pub fn plan(custom: &str) -> Vec<Change> {
    WANTED
        .iter()
        .filter_map(|&(key, to)| {
            let wrong = values_of(custom, key).find(|value| value.parse() != Ok(to))?;
            Some(Change {
                key,
                from: wrong.to_owned(),
                to,
            })
        })
        .collect()
}

/// The values `key` has in `custom`, one per indented line that begins with it. A line with no
/// value gives an empty one.
fn values_of<'a>(custom: &'a str, key: &'a str) -> impl Iterator<Item = &'a str> {
    custom
        .lines()
        .filter(|line| line.starts_with(char::is_whitespace))
        .filter_map(move |line| {
            let mut words = line.split_whitespace();
            (words.next() == Some(key)).then(|| words.next().unwrap_or(""))
        })
}

/// Why `pmset` made no change, in words for the log.
fn failure(what: &str, output: &LaunchctlOutput) -> String {
    format!(
        "level=error keep_awake: pmset {what} ended with {:?}: {}",
        output.code,
        output.stderr_text().trim()
    )
}

/// Why `pmset` did not run to an exit, in words for the log.
fn not_run(what: &str, error: &LaunchctlError) -> String {
    let why = match error {
        LaunchctlError::Spawn { why, .. } => why.clone(),
        LaunchctlError::TimedOut { .. } => format!(
            "killed at its deadline of {} seconds",
            PMSET_DEADLINE.as_secs()
        ),
    };
    format!("level=error keep_awake: pmset {what} did not run: {why}")
}

/// The start's step: on a Mac whose config has `keep_awake` on, reads the power settings through
/// `pmset`, makes what differs by one command and says each change through `say`; on any other
/// host, or with the key off, does nothing (a Mac with it off says so once). Returns the changes
/// that were made, none when the host was already set, `pmset` failed, or the key is off.
pub fn at_start(
    config: &DaemonConfig,
    kind: PlatformKind,
    pmset: &dyn Launchctl,
    say: &dyn Fn(&str),
) -> Vec<Change> {
    if kind != PlatformKind::Macos {
        return Vec::new();
    }
    if !config.keep_awake {
        say("level=info keep_awake: is off in the config; the power settings are not touched");
        return Vec::new();
    }
    let read = match pmset.run(&read_args()) {
        Ok(output) if output.code == Some(0) => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        Ok(output) => {
            say(&failure("-g custom", &output));
            return Vec::new();
        }
        Err(error) => {
            say(&not_run("-g custom", &error));
            return Vec::new();
        }
    };
    let changes = plan(&read);
    let Some(command) = args(&changes) else {
        return Vec::new();
    };
    match pmset.run(&command) {
        Ok(output) if output.code == Some(0) => {
            changes.iter().for_each(|change| say(&change.line()));
            changes
        }
        Ok(output) => {
            say(&failure(&command.join(" "), &output));
            Vec::new()
        }
        Err(error) => {
            say(&not_run(&command.join(" "), &error));
            Vec::new()
        }
    }
}

/// [`at_start`] on the host's own `pmset`, saying through the daemon's stderr.
pub fn at_host_start(config: &DaemonConfig, kind: PlatformKind) -> Vec<Change> {
    let pmset = LaunchctlProcess::new(std::path::Path::new(PMSET), PMSET_DEADLINE);
    at_start(config, kind, &pmset, &|line| {
        eprintln!("hostkeeper daemon: {line}");
    })
}
