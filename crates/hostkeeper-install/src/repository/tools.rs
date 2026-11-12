//! The tools a repository installer runs, by absolute path, and the port they run behind
//! (packages.md section 5.1). Every run is a transient unit (packages-repositories.md section
//! 4.6): offline (`PrivateNetwork=yes`) but for a backend's two fetch commands, which run
//! confined to the work directory (`ProtectSystem=strict`, `ReadWritePaths=`).

use std::path::PathBuf;

/// A tool of the host's own, run by its absolute path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tool {
    /// `/usr/bin/apt-get`: the index fetch, the simulation, the download and the install.
    AptGet,
    /// `/usr/bin/apt-cache`: the index's stanza of one version.
    AptCache,
    /// `/usr/bin/apt-mark`: the hold.
    AptMark,
    /// `/usr/bin/dpkg`: the architecture and the purge.
    Dpkg,
    /// `/usr/bin/dpkg-deb`: the control fields, the control members and the extraction.
    DpkgDeb,
    /// `/usr/bin/dpkg-query`: the installed version.
    DpkgQuery,
    /// `/usr/bin/systemctl`: the service manager's reload after the units changed.
    Systemctl,
}

impl Tool {
    /// Every tool.
    pub const ALL: [Tool; 7] = [
        Tool::AptGet,
        Tool::AptCache,
        Tool::AptMark,
        Tool::Dpkg,
        Tool::DpkgDeb,
        Tool::DpkgQuery,
        Tool::Systemctl,
    ];

    /// The tool's absolute path on a host.
    pub const fn path(self) -> &'static str {
        match self {
            Tool::AptGet => "/usr/bin/apt-get",
            Tool::AptCache => "/usr/bin/apt-cache",
            Tool::AptMark => "/usr/bin/apt-mark",
            Tool::Dpkg => "/usr/bin/dpkg",
            Tool::DpkgDeb => "/usr/bin/dpkg-deb",
            Tool::DpkgQuery => "/usr/bin/dpkg-query",
            Tool::Systemctl => "/usr/bin/systemctl",
        }
    }

    /// The tool's short name, as a unit name and a report spell it.
    pub const fn name(self) -> &'static str {
        match self {
            Tool::AptGet => "apt-get",
            Tool::AptCache => "apt-cache",
            Tool::AptMark => "apt-mark",
            Tool::Dpkg => "dpkg",
            Tool::DpkgDeb => "dpkg-deb",
            Tool::DpkgQuery => "dpkg-query",
            Tool::Systemctl => "systemctl",
        }
    }
}

/// Whether a run may reach the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    /// No network: every run but a fetch.
    Offline,
    /// The network, from the one repository the work directory names: a backend's two fetch
    /// commands only, which run no package script and are always confined.
    Fetch,
}

/// One run of a tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// The tool.
    pub tool: Tool,
    /// Its arguments, after the program.
    pub args: Vec<String>,
    /// Whether it may reach the network.
    pub network: Network,
    /// `Some`: the only paths it may write (`ProtectSystem=strict`); `None`: the host's system,
    /// as an install and a purge must.
    pub writable: Option<Vec<PathBuf>>,
    /// The directory it runs in, when it writes there (`apt-get download`).
    pub working_dir: Option<PathBuf>,
    /// Its environment, beside the service manager's own.
    pub env: Vec<(String, String)>,
    /// Its bound, in seconds.
    pub bound_secs: u64,
}

impl Run {
    /// An offline run of `tool` with `args`, unconfined, in no directory, with `LC_ALL=C` so its
    /// messages are the ones the backend reads.
    pub fn offline(tool: Tool, args: Vec<String>, bound_secs: u64) -> Run {
        Run {
            tool,
            args,
            network: Network::Offline,
            writable: None,
            working_dir: None,
            env: vec![("LC_ALL".to_owned(), "C".to_owned())],
            bound_secs,
        }
    }

    /// The run, confined to writing `paths`.
    pub fn writing(mut self, paths: Vec<PathBuf>) -> Run {
        self.writable = Some(paths);
        self
    }

    /// The run, with the network.
    pub fn fetching(mut self) -> Run {
        self.network = Network::Fetch;
        self
    }

    /// The run, in `dir`.
    pub fn in_dir(mut self, dir: PathBuf) -> Run {
        self.working_dir = Some(dir);
        self
    }

    /// The run, with `name` set to `value` too.
    pub fn with_env(mut self, name: &str, value: &str) -> Run {
        self.env.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// What a tool printed, and how it exited (`None`: killed by a signal or by its bound).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolOutput {
    /// The exit code.
    pub code: Option<i32>,
    /// What it wrote to its standard output, as far as it was kept.
    pub stdout: Vec<u8>,
    /// What it wrote to its standard error, as far as it was kept.
    pub stderr: Vec<u8>,
    /// Whether either stream lost its beginning, so a listing read from it is incomplete.
    pub cut: bool,
}

impl ToolOutput {
    /// An output that exited with `code` and printed `stdout`.
    pub fn exit(code: i32, stdout: &str) -> ToolOutput {
        ToolOutput {
            code: Some(code),
            stdout: stdout.as_bytes().to_vec(),
            ..ToolOutput::default()
        }
    }

    /// Whether the tool exited 0.
    pub fn succeeded(&self) -> bool {
        self.code == Some(0)
    }

    /// Both streams as text, standard error after standard output.
    pub fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.stdout).into_owned();
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&self.stderr));
        text
    }

    /// The last line of either stream that holds anything, from its standard error first, then
    /// its standard output: what a failure report quotes. An `E:` line of apt's wins over any
    /// other.
    pub fn last_line(&self) -> String {
        let lines = |bytes: &[u8]| -> Vec<String> {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let err = lines(&self.stderr);
        let out = lines(&self.stdout);
        err.iter()
            .chain(out.iter())
            .rfind(|line| line.starts_with("E:"))
            .or_else(|| err.last())
            .or_else(|| out.last())
            .cloned()
            .unwrap_or_default()
    }
}

/// Why a tool did not run to its end.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ToolError(pub String);

/// Runs a tool and waits for it to end within its bound.
pub trait Tools: Send + Sync {
    fn run(&self, run: &Run) -> Result<ToolOutput, ToolError>;
}

/// A wait for the package manager's lock, as it is reported while it lasts (`lock_wait`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockWait {
    /// The tool that found the lock held: `dpkg` (the purge) or `apt-mark` (the hold).
    pub tool: Tool,
    /// The seconds waited so far.
    pub waited_secs: u64,
    /// The seconds the step waits at most (`--package-lock-wait-secs`).
    pub bound_secs: u64,
    /// The tool's last line, which names the process holding the lock.
    pub holder: String,
}

/// Waits between two tries of a tool whose lock is held (dpkg and apt-mark have no lock timeout
/// of their own), and says that it waits.
pub trait Pause: Send + Sync {
    /// Waits `secs` seconds.
    fn pause(&self, secs: u64);

    /// Reports a wait for the package manager's lock that is going on.
    fn lock_wait(&self, wait: &LockWait);
}

/// The host's pause: the thread sleeps; a wait is reported nowhere.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sleep;

impl Pause for Sleep {
    fn pause(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }

    fn lock_wait(&self, _wait: &LockWait) {}
}
