//! The Remote Login probe: once at its start, a Mac's daemon runs, as itself (root under launchd,
//! with whatever privacy grants it has, none on a stock Mac), the commands that tell whether it can
//! turn on Remote Login, and reports what each said in every heartbeat after
//! (`HeartbeatRequest.remote_login_probe`), where `hkctl hosts get` shows it. It decides nothing:
//! it answered, from the Macs themselves, whether the daemon needs Full Disk Access to turn on
//! sshd (docs/design/ssh-through-coordinator.md, section 3, has the results), and it keeps
//! reporting the same steps at each start.
//!
//! The steps run in this order ([`run`]); each records the exact exit code, standard output and
//! standard error of its command, or why it could not be done:
//!
//! 1. `systemsetup -getremotelogin`, `launchctl print system/com.openssh.sshd` and a connect to
//!    `127.0.0.1:22` that reads the SSH banner: the state before the probe changes anything;
//! 2. `systemsetup -setremotelogin on`, then `systemsetup -getremotelogin`: the switch that the
//!    build Macs refuse without Full Disk Access;
//! 3. `launchctl enable system/com.openssh.sshd`, `launchctl bootstrap system
//!    /System/Library/LaunchDaemons/ssh.plist`, `launchctl print system/com.openssh.sshd`,
//!    `systemsetup -getremotelogin` and the banner again: the way that turns Remote Login on
//!    without it, and whether `systemsetup -getremotelogin` agrees after it;
//! 4. the `Include` lines of `/etc/ssh/sshd_config`, `dscl . -read
//!    /Groups/com.apple.access_ssh`, whether the privacy database
//!    (`/Library/Application Support/com.apple.TCC/TCC.db`) can be opened, its Full Disk Access
//!    rows (`sqlite3 -readonly`), and the code signature of the running binary (`codesign -dvv`)
//!    and the designated requirement of the installed one (`codesign -dr -`);
//! 5. why a Mac may sleep: `pmset -g custom`, `pmset -g`, `pmset -g assertions`, the newest
//!    `Sleep` and `Wake` lines of `pmset -g log` that start or end a system sleep (read as it is
//!    written, never whole, [`crate::line_process`]; the lines of a sleeping Mac's maintenance
//!    wakes are left out), and the newest `keep_awake:` lines of the daemon's
//!    own standard error log ([`DAEMON_ERR_LOG`], what [`crate::keep_awake`] said at its starts).
//!
//! Steps 2 and 3 change the host: they may turn Remote Login on, and nothing turns it off again.
//! The steps before them say what the host had, so the report shows whether the probe turned it
//! on. A command's output and every other text is cut to [`STEP_TEXT`] bytes from its start; the
//! two log steps keep their newest lines, to [`LOG_TEXT`] bytes. When the whole probe is still
//! more than [`PROBE_BUDGET`] bytes as JSON, the steps of [`SHRINK_FIRST`] are cut further, in that
//! order, so it stays within a heartbeat's bound (and is the first fact dropped when it does
//! not, [`hostkeeper_link::fit`]).
//! The probe runs on a thread of its own, so the daemon's start and its heartbeats do not wait
//! for it; a heartbeat carries it once it is done. Any host but a Mac runs none.

use std::fs::File;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess, PlatformKind};
use hostkeeper_link::{ProbeStep, RemoteLoginProbe, RemoteLoginProbeSource, UnixClock};

use crate::line_process::{LineProcess, LineTool};
use crate::unix_clock::SystemUnixClock;

/// The tool that reads and sets Remote Login.
pub const SYSTEMSETUP: &str = "/usr/sbin/systemsetup";
/// The tool that reads a directory service record.
pub const DSCL: &str = "/usr/bin/dscl";
/// The tool that reads the privacy database.
pub const SQLITE3: &str = "/usr/bin/sqlite3";
/// The launchd service of sshd.
pub const SSHD_SERVICE: &str = "system/com.openssh.sshd";
/// The launchd plist of sshd.
pub const SSHD_PLIST: &str = "/System/Library/LaunchDaemons/ssh.plist";
/// sshd's configuration file.
pub const SSHD_CONFIG: &str = "/etc/ssh/sshd_config";
/// The directory service record of the group that, when it exists, lists who may log in by SSH.
pub const ACCESS_SSH_GROUP: &str = "/Groups/com.apple.access_ssh";
/// The system's privacy database.
pub const TCC_DB: &str = "/Library/Application Support/com.apple.TCC/TCC.db";
/// The query of the privacy database's Full Disk Access rows.
pub const FULL_DISK_ACCESS_QUERY: &str = "SELECT client, client_type, auth_value, auth_reason \
     FROM access WHERE service = 'kTCCServiceSystemPolicyAllFiles'";
/// The installed binary, the one path launchd runs that no update replaces.
pub const INSTALLED_BINARY: &str = "/usr/local/bin/hostkeeper";
/// How long one command of the probe may run before it is killed.
pub const STEP_DEADLINE: Duration = Duration::from_secs(20);
/// How long the connect to sshd, and then the read of its banner, may each take.
pub const BANNER_DEADLINE: Duration = Duration::from_secs(5);
/// The most bytes of the banner read.
pub const BANNER_MAX: usize = 255;
/// The most bytes of a command's output, of an argument and of an error text, cut from the start.
pub const STEP_TEXT: usize = 1024;
/// The most bytes of a log step: its newest lines.
pub const LOG_TEXT: usize = 3072;
/// How long `pmset -g log` may run before it is killed.
pub const LOG_DEADLINE: Duration = Duration::from_secs(60);
/// The daemon's standard error log, the `StandardErrorPath` of its launchd plist.
pub const DAEMON_ERR_LOG: &str = "/var/log/hostkeeper/daemon.err.log";
/// The most bytes of the whole probe as JSON before [`SHRINK_FIRST`] is cut further.
pub const PROBE_BUDGET: usize = 10 * 1024;
/// The steps cut further, in this order, when the probe is over [`PROBE_BUDGET`].
pub const SHRINK_FIRST: [&str; 6] = [
    "power_log_sleep_wake",
    "power_assertions",
    "keep_awake_log",
    "sshd_print_after",
    "sshd_print_before",
    "codesign_running",
];
/// The `pmset -g log` events: the word after the line's time. A line of one is kept unless it is
/// a maintenance wake's (a `DarkWake`, `Wake Requests` or a sleep due to `'Maintenance Sleep'`).
pub const SLEEP_WAKE_EVENTS: [&str; 3] = ["Sleep", "Wake", "DarkWake"];

/// The programs the probe runs, one port each.
pub struct Tools {
    pub systemsetup: Box<dyn Launchctl>,
    pub launchctl: Box<dyn Launchctl>,
    pub dscl: Box<dyn Launchctl>,
    pub sqlite3: Box<dyn Launchctl>,
    pub codesign: Box<dyn Launchctl>,
    pub pmset: Box<dyn Launchctl>,
    pub pmset_log: Box<dyn LineTool>,
}

impl Tools {
    /// The host's own programs, each bounded by [`STEP_DEADLINE`].
    pub fn host() -> Tools {
        let tool = |program: &str| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(Path::new(program), STEP_DEADLINE))
        };
        Tools {
            systemsetup: tool(SYSTEMSETUP),
            launchctl: tool(hostkeeper_launchd::LAUNCHCTL),
            dscl: tool(DSCL),
            sqlite3: tool(SQLITE3),
            codesign: tool(crate::mac::CODESIGN),
            pmset: tool(crate::keep_awake::PMSET),
            pmset_log: Box::new(LineProcess::new(
                Path::new(crate::keep_awake::PMSET),
                LOG_DEADLINE,
            )),
        }
    }
}

/// What the probe reads besides the programs it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Places {
    /// Where sshd listens.
    pub ssh: SocketAddr,
    /// How long the connect, and then the read of the banner, may each take.
    pub banner_deadline: Duration,
    /// sshd's configuration file.
    pub sshd_config: PathBuf,
    /// The privacy database.
    pub tcc_db: PathBuf,
    /// The binary that runs the daemon.
    pub running: PathBuf,
    /// The installed binary.
    pub installed: PathBuf,
    /// The daemon's standard error log.
    pub daemon_log: PathBuf,
}

impl Places {
    /// The host's: `127.0.0.1:22`, [`SSHD_CONFIG`], [`TCC_DB`], this process's own binary (an
    /// empty path, which `codesign` then refuses, when it cannot be known) and
    /// [`INSTALLED_BINARY`] and [`DAEMON_ERR_LOG`].
    pub fn host() -> Places {
        Places {
            ssh: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 22)),
            banner_deadline: BANNER_DEADLINE,
            sshd_config: PathBuf::from(SSHD_CONFIG),
            tcc_db: PathBuf::from(TCC_DB),
            running: std::env::current_exe().unwrap_or_default(),
            installed: PathBuf::from(INSTALLED_BINARY),
            daemon_log: PathBuf::from(DAEMON_ERR_LOG),
        }
    }
}

/// Runs every step of the probe, in order, and returns what each said; the probe starts at
/// `clock`'s time.
pub fn run(tools: &Tools, places: &Places, clock: &dyn UnixClock) -> RemoteLoginProbe {
    let started_unix = clock.unix_secs();
    let get = |name: &str| command(name, &*tools.systemsetup, SYSTEMSETUP, &["-getremotelogin"]);
    let print = |name: &str| {
        command(
            name,
            &*tools.launchctl,
            hostkeeper_launchd::LAUNCHCTL,
            &["print", SSHD_SERVICE],
        )
    };
    let tcc_db = places.tcc_db.to_string_lossy();
    let running = places.running.to_string_lossy();
    let installed = places.installed.to_string_lossy();
    let steps = vec![
        get("remote_login_get_before"),
        print("sshd_print_before"),
        banner("ssh_banner_before", places.ssh, places.banner_deadline),
        command(
            "remote_login_set_on",
            &*tools.systemsetup,
            SYSTEMSETUP,
            &["-setremotelogin", "on"],
        ),
        get("remote_login_get_after_set"),
        command(
            "sshd_enable",
            &*tools.launchctl,
            hostkeeper_launchd::LAUNCHCTL,
            &["enable", SSHD_SERVICE],
        ),
        command(
            "sshd_bootstrap",
            &*tools.launchctl,
            hostkeeper_launchd::LAUNCHCTL,
            &["bootstrap", "system", SSHD_PLIST],
        ),
        print("sshd_print_after"),
        get("remote_login_get_after"),
        banner("ssh_banner_after", places.ssh, places.banner_deadline),
        include_lines("sshd_config_include", &places.sshd_config),
        command(
            "access_ssh_group",
            &*tools.dscl,
            DSCL,
            &[".", "-read", ACCESS_SSH_GROUP],
        ),
        opens("tcc_db_open", &places.tcc_db),
        command(
            "tcc_full_disk_access",
            &*tools.sqlite3,
            SQLITE3,
            &["-readonly", &tcc_db, FULL_DISK_ACCESS_QUERY],
        ),
        command(
            "codesign_running",
            &*tools.codesign,
            crate::mac::CODESIGN,
            &["-dvv", &running],
        ),
        command(
            "codesign_installed_requirement",
            &*tools.codesign,
            crate::mac::CODESIGN,
            &["-dr", "-", &installed],
        ),
        pmset("power_custom", &*tools.pmset, &["-g", "custom"]),
        pmset("power_settings", &*tools.pmset, &["-g"]),
        pmset("power_assertions", &*tools.pmset, &["-g", "assertions"]),
        sleep_wake_log("power_log_sleep_wake", &*tools.pmset_log),
        keep_awake_log("keep_awake_log", &places.daemon_log),
    ];
    within_budget(RemoteLoginProbe {
        started_unix,
        steps,
    })
}

/// The step `name` that runs `pmset` with `args`.
fn pmset(name: &str, tool: &dyn Launchctl, args: &[&str]) -> ProbeStep {
    command(name, tool, crate::keep_awake::PMSET, args)
}

/// Whether `line` of `pmset -g log` is a sleep or wake event: after its time
/// (`2026-11-16 04:57:12 -0800`), the word is one of [`SLEEP_WAKE_EVENTS`].
pub fn is_sleep_wake(line: &str) -> bool {
    let bytes = line.as_bytes();
    let timed = bytes.len() > 26 && bytes[4] == b'-' && bytes[10] == b' ' && bytes[19] == b' ';
    timed
        && line[26..]
            .split_whitespace()
            .next()
            .is_some_and(|word| SLEEP_WAKE_EVENTS.contains(&word))
}

/// Whether `event`, the text after the time of a sleep or wake line ([`is_sleep_wake`]), belongs to a maintenance wake of a
/// Mac that is asleep, and not to the start or the end of its sleep: a `DarkWake`, a `Wake
/// Requests` line (the wakes the system has scheduled) and a sleep `due to 'Maintenance Sleep'`
/// (the return to sleep after a dark wake). A sleeping Mac writes these every few minutes, so
/// over a night they would push the line that says why it went to sleep out of [`LOG_TEXT`].
fn is_maintenance(event: &str) -> bool {
    let mut words = event.split_whitespace();
    match (words.next(), words.next()) {
        (Some("DarkWake"), _) | (Some("Wake"), Some("Requests")) => true,
        (Some("Sleep"), _) => event.contains("due to 'Maintenance Sleep'"),
        _ => false,
    }
}

/// The newest of `lines` whose bytes, each with its line end, are at most `max` in all, oldest
/// first, each with its runs of blanks made one space; and whether any was left out.
pub fn newest(lines: &[String], max: usize) -> (String, bool) {
    let mut kept: Vec<String> = Vec::new();
    let mut total = 0;
    for line in lines.iter().rev() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if total + line.len() + 1 > max {
            break;
        }
        total += line.len() + 1;
        kept.push(line);
    }
    let left_out = kept.len() < lines.len();
    kept.reverse();
    (kept.join("\n"), left_out)
}

/// The step `name` that runs `pmset -g log` and keeps its newest sleep and wake lines that are not
/// a maintenance wake's ([`is_sleep_wake`], [`is_maintenance`], [`newest`]) to [`LOG_TEXT`] bytes.
fn sleep_wake_log(name: &str, tool: &dyn LineTool) -> ProbeStep {
    let args = vec!["-g".to_owned(), "log".to_owned()];
    let mut step = step(
        name,
        vec![
            crate::keep_awake::PMSET.to_owned(),
            "-g".to_owned(),
            "log".to_owned(),
        ],
    );
    let mut lines = Vec::new();
    match tool.run_lines(&args, &mut |line| {
        if is_sleep_wake(line) && !is_maintenance(&line[26..]) {
            lines.push(line.to_owned());
        }
    }) {
        Ok(code) => {
            step.exit_code = code;
            (step.stdout, step.truncated) = newest(&lines, LOG_TEXT);
        }
        Err(error) => step.error = cut(&error).0,
    }
    step
}

/// The step `name` that reads the daemon's log at `path` and keeps its newest `keep_awake:` lines
/// ([`newest`]) to [`STEP_TEXT`] bytes.
fn keep_awake_log(name: &str, path: &Path) -> ProbeStep {
    let mut step = step(
        name,
        vec!["read".to_owned(), path.to_string_lossy().into_owned()],
    );
    match std::fs::read(path) {
        Ok(bytes) => {
            let lines: Vec<String> = String::from_utf8_lossy(&bytes)
                .lines()
                .filter(|line| line.contains("keep_awake:"))
                .map(str::to_owned)
                .collect();
            (step.stdout, step.truncated) = newest(&lines, STEP_TEXT);
        }
        Err(error) => step.error = cut(&error.to_string()).0,
    }
    step
}

/// `probe`, with the standard output of the steps of [`SHRINK_FIRST`] cut further, in that order,
/// while it is more than [`PROBE_BUDGET`] bytes as JSON: a log step keeps its end (its newest
/// lines), any other its start.
pub fn within_budget(mut probe: RemoteLoginProbe) -> RemoteLoginProbe {
    for name in SHRINK_FIRST {
        let over = probe
            .to_json()
            .to_string()
            .len()
            .saturating_sub(PROBE_BUDGET);
        if over == 0 {
            break;
        }
        for step in probe.steps.iter_mut().filter(|step| step.name == name) {
            let keep = step.stdout.len().saturating_sub(over);
            let (text, cut_now) = if name.ends_with("_log") || name.contains("_log_") {
                cut_tail(&step.stdout, keep)
            } else {
                cut_to(&step.stdout, keep)
            };
            step.stdout = text;
            step.truncated |= cut_now;
        }
    }
    probe
}

/// `text` cut to [`STEP_TEXT`] bytes at a character boundary, and whether it was cut.
pub fn cut(text: &str) -> (String, bool) {
    cut_to(text, STEP_TEXT)
}

/// `text` cut to its first `max` bytes at a character boundary, and whether it was cut.
pub fn cut_to(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// `text` cut to its last `max` bytes at a character boundary, and whether it was cut.
pub fn cut_tail(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_owned(), true)
}

/// A step of nothing but `argv`.
fn step(name: &str, argv: Vec<String>) -> ProbeStep {
    ProbeStep {
        name: name.to_owned(),
        argv: argv.iter().map(|arg| cut(arg).0).collect(),
        ..ProbeStep::default()
    }
}

/// The step `name` that runs `program` with `args` through `tool`.
fn command(name: &str, tool: &dyn Launchctl, program: &str, args: &[&str]) -> ProbeStep {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let argv = std::iter::once(program.to_owned())
        .chain(args.iter().cloned())
        .collect();
    let mut step = step(name, argv);
    match tool.run(&args) {
        Ok(output) => {
            let (stdout, out_cut) = cut(&String::from_utf8_lossy(&output.stdout));
            let (stderr, err_cut) = cut(&String::from_utf8_lossy(&output.stderr));
            step.exit_code = output.code;
            step.stdout = stdout;
            step.stderr = stderr;
            step.truncated = out_cut || err_cut;
        }
        Err(error) => step.error = cut(&error.to_string()).0,
    }
    step
}

/// The step `name` that connects to `ssh` and reads what sshd writes first, its banner, at most
/// [`BANNER_MAX`] bytes in one read; the connect and the read are each bounded by `deadline`.
fn banner(name: &str, ssh: SocketAddr, deadline: Duration) -> ProbeStep {
    let mut step = step(name, vec!["connect".to_owned(), ssh.to_string()]);
    let read = TcpStream::connect_timeout(&ssh, deadline).and_then(|mut stream| {
        stream.set_read_timeout(Some(deadline))?;
        let mut buf = [0_u8; BANNER_MAX];
        let n = stream.read(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
    });
    match read {
        Ok(text) => step.stdout = text,
        Err(error) => step.error = cut(&error.to_string()).0,
    }
    step
}

/// The step `name` that reads `path` and keeps its `Include` lines (any case, leading space
/// dropped), one per line; none when it has none.
fn include_lines(name: &str, path: &Path) -> ProbeStep {
    let mut step = step(
        name,
        vec!["read".to_owned(), path.to_string_lossy().into_owned()],
    );
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let lines: Vec<&str> = text
                .lines()
                .map(str::trim_start)
                .filter(|line| {
                    line.get(..7)
                        .is_some_and(|word| word.eq_ignore_ascii_case("include"))
                })
                .collect();
            (step.stdout, step.truncated) = cut(&lines.join("\n"));
        }
        Err(error) => step.error = cut(&error.to_string()).0,
    }
    step
}

/// The step `name` that opens `path` for reading: `readable`, or why it cannot be opened.
fn opens(name: &str, path: &Path) -> ProbeStep {
    let mut step = step(
        name,
        vec!["open".to_owned(), path.to_string_lossy().into_owned()],
    );
    match File::open(path) {
        Ok(_) => step.stdout = "readable".to_owned(),
        Err(error) => step.error = cut(&error.to_string()).0,
    }
    step
}

/// Where the probe's result is kept for the heartbeats: empty until the probe is done.
#[derive(Clone, Debug, Default)]
pub struct ProbeSlot(Arc<OnceLock<RemoteLoginProbe>>);

impl RemoteLoginProbeSource for ProbeSlot {
    fn remote_login_probe(&self) -> Option<RemoteLoginProbe> {
        self.0.get().cloned()
    }
}

/// On a Mac (`kind`), starts `probe` on a thread of its own and returns the slot its result is
/// put in, with the thread; on any other host, runs nothing and returns none.
pub fn start(
    kind: PlatformKind,
    probe: impl FnOnce() -> RemoteLoginProbe + Send + 'static,
) -> Option<(ProbeSlot, JoinHandle<()>)> {
    if kind != PlatformKind::Macos {
        return None;
    }
    let slot = ProbeSlot::default();
    let filled = slot.clone();
    let thread = thread::spawn(move || {
        let _ = filled.0.set(probe());
    });
    Some((slot, thread))
}

/// [`start`] on the host's own programs and places ([`Tools::host`], [`Places::host`]): the
/// source the heartbeat loop reads, none on any host but a Mac.
pub fn at_host_start(kind: PlatformKind) -> Option<Arc<dyn RemoteLoginProbeSource>> {
    let tools = Tools::host();
    let places = Places::host();
    start(kind, move || run(&tools, &places, &SystemUnixClock))
        .map(|(slot, _)| Arc::new(slot) as Arc<dyn RemoteLoginProbeSource>)
}
