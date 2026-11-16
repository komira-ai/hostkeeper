//! The Remote Login probe: once at its start, a Mac's daemon runs, as itself (root under launchd,
//! with no Full Disk Access unless a profile granted it), the commands that tell whether it can
//! turn on Remote Login, and reports what each said in every heartbeat after
//! (`HeartbeatRequest.remote_login_probe`), where `hkctl hosts get` shows it. It decides nothing:
//! it is how the question "does the daemon need Full Disk Access to turn on sshd?"
//! (docs/design/ssh-through-coordinator.md, section 3) is answered from the Macs themselves.
//!
//! The steps run in this order ([`run`]); each records the exact exit code, standard output and
//! standard error of its command, or why it could not be done:
//!
//! 1. `systemsetup -getremotelogin`, `launchctl print system/com.openssh.sshd` and a connect to
//!    `127.0.0.1:22` that reads the SSH banner: the state before the probe changes anything;
//! 2. `systemsetup -setremotelogin on`, then `systemsetup -getremotelogin`: the supported switch,
//!    which is reported to need Full Disk Access;
//! 3. `launchctl enable system/com.openssh.sshd`, `launchctl bootstrap system
//!    /System/Library/LaunchDaemons/ssh.plist`, `launchctl print system/com.openssh.sshd`,
//!    `systemsetup -getremotelogin` and the banner again: the way that is not reported to need
//!    it, and whether System Settings' view agrees after it;
//! 4. the `Include` lines of `/etc/ssh/sshd_config`, `dscl . -read
//!    /Groups/com.apple.access_ssh`, whether the privacy database
//!    (`/Library/Application Support/com.apple.TCC/TCC.db`) can be opened, its Full Disk Access
//!    rows (`sqlite3 -readonly`), and the code signature of the running binary (`codesign -dvv`)
//!    and the designated requirement of the installed one (`codesign -dr -`).
//!
//! Steps 2 and 3 change the host: they may turn Remote Login on, and nothing turns it off again.
//! The steps before them say what the host had, so the report shows whether the probe turned it
//! on. Each text is cut to [`MAX_PROBE_TEXT`] bytes, so the probe stays well within a
//! heartbeat's bound (and is the first fact dropped when it does not, [`hostkeeper_link::fit`]).
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
use hostkeeper_proto::MAX_PROBE_TEXT;

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
/// The installed binary, the path a Full Disk Access profile names.
pub const INSTALLED_BINARY: &str = "/usr/local/bin/hostkeeper";
/// How long one command of the probe may run before it is killed.
pub const STEP_DEADLINE: Duration = Duration::from_secs(20);
/// How long the connect to sshd, and then the read of its banner, may each take.
pub const BANNER_DEADLINE: Duration = Duration::from_secs(5);
/// The most bytes of the banner read.
pub const BANNER_MAX: usize = 255;

/// The programs the probe runs, one port each.
pub struct Tools {
    pub systemsetup: Box<dyn Launchctl>,
    pub launchctl: Box<dyn Launchctl>,
    pub dscl: Box<dyn Launchctl>,
    pub sqlite3: Box<dyn Launchctl>,
    pub codesign: Box<dyn Launchctl>,
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
}

impl Places {
    /// The host's: `127.0.0.1:22`, [`SSHD_CONFIG`], [`TCC_DB`], this process's own binary (an
    /// empty path, which `codesign` then refuses, when it cannot be known) and
    /// [`INSTALLED_BINARY`].
    pub fn host() -> Places {
        Places {
            ssh: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 22)),
            banner_deadline: BANNER_DEADLINE,
            sshd_config: PathBuf::from(SSHD_CONFIG),
            tcc_db: PathBuf::from(TCC_DB),
            running: std::env::current_exe().unwrap_or_default(),
            installed: PathBuf::from(INSTALLED_BINARY),
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
    ];
    RemoteLoginProbe {
        started_unix,
        steps,
    }
}

/// `text` cut to [`MAX_PROBE_TEXT`] bytes at a character boundary, and whether it was cut.
pub fn cut(text: &str) -> (String, bool) {
    if text.len() <= MAX_PROBE_TEXT {
        return (text.to_owned(), false);
    }
    let mut end = MAX_PROBE_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
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
