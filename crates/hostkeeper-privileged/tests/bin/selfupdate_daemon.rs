//! `hostkeeper-selfupdate-daemon`: the hostkeeper binary of the privileged self-update tests
//! (`tests/self_update.rs` on Linux, `tests/device_mac_selfupdate.rs` on a Mac) and of the Mac's
//! hello test (`tests/device_mac_e2e.rs`), a program of the tests, not shipped.
//!
//! It parses the `hostkeeper` binary's command line (`hostkeeper_daemon::cli::Cli`) and runs its
//! `daemon` verb, the only one it takes; any other is a usage error.
//!
//! It is the shipped daemon (`hostkeeper_daemon::start`: the settle of a waiting
//! self-update, the boot pass, the readiness notice, the heartbeat loop, the executor and its
//! follow-on) on the
//! host's adapters (on Linux the step launcher's transient units and `systemctl`, on a Mac
//! `launchctl` and the launchd step jobs), started as
//! `hostkeeper daemon` starts: from the packaged binary, then the hand-over of its own update
//! (`hostkeeper_daemon::handover`, run first, so a binary that waits for a hand-over into another
//! version execs that binary while its start count lasts). Three things differ from the host's
//! own, that a test cannot have for real put in place:
//!
//! - on Linux, the host's repository entries are the fixture's (`repositories/` of the fixture's
//!   directory), where the test's signed apt repository of hostkeeper's `.deb`s is, and the root
//!   the installer writes the enablement links of a package's `run` units under is the fixture's
//!   (`root/`), so no link is made in the host's own unit directory (the package itself is the
//!   host's, in dpkg's database, as an update of hostkeeper installs it); on a Mac, the signers
//!   file of the `macos-pkg` installer is the fixture's (`macos-pkg.toml`), the `.pkg` files the
//!   daemon fetches by their pins are the fixture's (`pkgs/`, `shared/macos_pkg.rs`), and the
//!   installer's `pkgutil --check-signature` is answered as a signed package for a package the
//!   fixture serves (every other tool call, `installer` included, is the Mac's: the package is
//!   the host's, in its receipts and at its paths, as an update of hostkeeper installs it);
//! - the placement state (`placed.json`) and the unit directory are the fixture's (`placed/`,
//!   `units/`), never the host's;
//! - the heartbeat loop's link to the coordinator is files in the fixture's directory: each
//!   heartbeat is appended to `link/beats.jsonl` as one line of its JSON (the JSON the
//!   coordinator's heartbeat route reads), and the reply carries the order of `link/order.json`
//!   when that file is there, and the features and acknowledged verdicts of `link/answer.json`
//!   when that one is (no enrollment; a test may answer as the real coordinator in its process
//!   answers each heartbeat). An order carries the `entry` of a set by targets, in the JSON
//!   of `target.json` (`hostkeeper_daemon::target::to_json`). With `--fixture-link coordinator`
//!   the loops are the shipped daemon's instead (`hostkeeper_daemon::cli::host_links`): the
//!   daemon enrolls with the coordinator of its config and beats over mutual TLS, its pki
//!   directory the fixture's (`pki/`).
//!
//! Its own version is the name of the file it runs from, its links resolved (launchd starts
//! `current`, and macOS gives a program the path it was started by): the binary directory keeps
//! version `<v>` at `versions/<v>`, which is what `current` and `previous` name, so a copy staged
//! there runs as that version. (The shipped binary's version is the one it was built with; the
//! tests need two versions of one build.)
//!
//! Its own flags come before the verb, and only `daemon` reads them; it reads no environment
//! variable but the notify socket systemd sets for a `Type=notify` unit:
//!
//! - `--fixture-dir <dir>` (required by `daemon`): `link/`, `repositories/`, `root/`, `units/`
//!   and `placed/` (above), and on a Mac `macos-pkg.toml`, `pkgs/` and `pki/`. The host's own
//!   unit and state directories are never written.
//! - `--fixture-link <files|coordinator>`: the link to the coordinator, `files` (the default) or
//!   `coordinator` (above).
//! - `--fixture-fail-start <version>`: a `daemon` of that version writes one line to standard
//!   error ([`refused_line`]) and exits 1 before it does anything else: a new binary that cannot
//!   start. A `daemon` of any other version runs.
//! - `--fixture-own-unit <label>`: the launchd job (or unit) the daemon runs as, which its
//!   observer watches as the service `hostkeeper` in place of the platform's own
//!   (`dev.hostkeeper.hostkeeper`): the test's job has a label of its own.
//! - `--fixture-hold-exit <file>`: once the daemon has served and asks to exit with status 0 (its
//!   own update handed over), it creates `<file>` with its pid and blocks, unless `<file>` is
//!   already there: the moment between the follow-on's hand-over and the exit it asks for, held
//!   so that a test can kill it there.
//! - `--fixture-hold-start <version>=<file>`: a `daemon` of that version, once the hand-over has
//!   exec'd it (and before it does anything else), creates `<file>` with its pid and blocks,
//!   unless `<file>` is already there: the moment after the exec and before the settlement, held
//!   so that a test can kill it there.
//! - `--fixture-packaged <path>`: the file `<path>` (a copy of this program, named by its
//!   version) stands for the packaged binary (`hostkeeper_daemon::packaged::PACKAGED_BINARIES`):
//!   a `daemon` run from it, or from its canonical form (a Mac's `/var` links to `/private/var`),
//!   starts, before anything else, as the packaged binary does (`hostkeeper_daemon::packaged::
//!   start`): it seeds the binary directory of `--state-dir` with itself as its version when
//!   `current` starts nothing, and execs `current` with its own arguments, the fixture's flags
//!   among them, so the `current` it execs runs as the daemon (it runs from `versions/<v>`, not
//!   from `<path>`). A start that fails prints the error and exits with its status.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::adapters::Adapters;
use hostkeeper_daemon::cli::{self, Cli, Command, DaemonFlags, HostFiles, SystemdVars};
use hostkeeper_daemon::handover::{self, ProcessExec};
use hostkeeper_daemon::links::{DaemonLog, Links};
use hostkeeper_daemon::packaged;
use hostkeeper_daemon::start::{self, StartError};
use hostkeeper_daemon::target;
use hostkeeper_daemon::unix_clock::SystemUnixClock;
use hostkeeper_daemon::{Daemon, Ports, Watchdog};
use hostkeeper_launchd::Platform;
use hostkeeper_link::{
    Acknowledged, CertSource, CertState, ClientIdentity, ClockSleeper, CoordinatorLink,
    Entry as LinkEntry, Feature, Heartbeat, LinkError, Log, Order, OrderId, OrderRef,
    PackageDigest, Reply, ReplyFault, StopFlag, Target, ThreadDispatch, Verb,
};
use hostkeeper_placed::Roots;
use serde_json::Value;

/// The `.pkg` files and the signature answers of the fixture, on a Mac.
#[cfg(target_os = "macos")]
#[path = "../shared/macos_pkg.rs"]
mod macos_pkg;

/// The program's name, at the start of what it says on standard error.
const PROGRAM: &str = "hostkeeper-selfupdate-daemon";
/// The exit status of a command line this program refuses (`2`, a usage error, as clap's).
const EXIT_USAGE: u8 = 2;
/// The exit status of a `daemon` that `--fixture-fail-start` names (the daemon's `EXIT_FAILED`).
const EXIT_REFUSED: u8 = 1;

/// The fixture's own flags.
#[derive(Debug, Default)]
struct Fixture {
    dir: Option<PathBuf>,
    fail_start: Option<String>,
    hold_exit: Option<PathBuf>,
    /// The version that holds its start, and the file it holds it at.
    hold_start: Option<(String, PathBuf)>,
    own_unit: Option<String>,
    packaged: Option<PathBuf>,
    /// The link to the coordinator: the shipped daemon's, over mutual TLS, instead of files.
    coordinator_link: bool,
}

/// The line a `daemon` of `version` writes before it exits at `--fixture-fail-start`.
fn refused_line(version: &str) -> String {
    format!("{PROGRAM}: version {version} refuses to start (--fixture-fail-start)")
}

fn main() -> ExitCode {
    let (fixture, rest) = match take_fixture_flags(std::env::args_os().skip(1)) {
        Ok(taken) => taken,
        Err(why) => return usage(&why),
    };
    let cli = match Cli::try_parse_from(std::iter::once(OsString::from("hostkeeper")).chain(rest)) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    let Command::Daemon(flags) = cli.command else {
        return usage("`daemon` is the only verb of this program");
    };
    daemon_verb(&fixture, &flags)
}

fn usage(why: &str) -> ExitCode {
    eprintln!("{PROGRAM}: {why}");
    ExitCode::from(EXIT_USAGE)
}

/// Takes the fixture's flags from the front of `args`, and gives them and the rest: the verb and
/// its arguments.
fn take_fixture_flags(
    args: impl Iterator<Item = OsString>,
) -> Result<(Fixture, Vec<OsString>), String> {
    let mut fixture = Fixture::default();
    let mut args = args.peekable();
    while let Some(flag) = args.next_if(|arg| arg.to_string_lossy().starts_with("--fixture-")) {
        let flag = flag.to_string_lossy().into_owned();
        let value = args.next().ok_or_else(|| format!("{flag} takes a value"))?;
        match flag.as_str() {
            "--fixture-dir" => fixture.dir = Some(PathBuf::from(value)),
            "--fixture-fail-start" => {
                fixture.fail_start = Some(value.to_string_lossy().into_owned());
            }
            "--fixture-hold-exit" => fixture.hold_exit = Some(PathBuf::from(value)),
            "--fixture-hold-start" => {
                let value = value.to_string_lossy().into_owned();
                let (version, file) = value
                    .split_once('=')
                    .ok_or_else(|| format!("{flag} takes <version>=<file>"))?;
                fixture.hold_start = Some((version.to_owned(), PathBuf::from(file)));
            }
            "--fixture-own-unit" => fixture.own_unit = Some(value.to_string_lossy().into_owned()),
            "--fixture-packaged" => fixture.packaged = Some(PathBuf::from(value)),
            "--fixture-link" => {
                fixture.coordinator_link = match value.to_string_lossy().as_ref() {
                    "files" => false,
                    "coordinator" => true,
                    other => return Err(format!("{flag} takes files or coordinator, not {other}")),
                };
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok((fixture, args.collect()))
}

/// The version of a binary run from `exe`: its file name, a `major.minor.patch`.
fn own_version(exe: &Path) -> Result<HostkeeperVersion, String> {
    let name = exe
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    HostkeeperVersion::parse(&name).map_err(|_| {
        format!(
            "daemon runs from versions/<major.minor.patch> of a binary directory, not from {}",
            exe.display()
        )
    })
}

/// `daemon`, as the module says.
fn daemon_verb(fixture: &Fixture, flags: &DaemonFlags) -> ExitCode {
    let own = match std::env::current_exe()
        .map_err(|e| format!("its own executable cannot be read: {e}"))
        .and_then(|exe| fs::canonicalize(&exe).map_err(|e| format!("{}: {e}", exe.display())))
        .and_then(|exe| own_version(&exe))
    {
        Ok(own) => own,
        Err(why) => return usage(&why),
    };
    if let Some(code) = packaged_start(fixture, flags, &own) {
        return code;
    }
    // The hand-over, as `hostkeeper daemon` does it: first, with this process's arguments, so a
    // binary that waits for a hand-over into another version execs that binary.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    handover::at_daemon_start(&flags.state_dir, &own.to_string(), &ProcessExec::new(args));
    if let Some((version, file)) = &fixture.hold_start
        && *version == own.to_string()
        && hold(file)
    {
        loop {
            std::thread::park();
        }
    }
    if fixture.fail_start.as_deref() == Some(own.to_string().as_str()) {
        eprintln!("{}", refused_line(&own.to_string()));
        return ExitCode::from(EXIT_REFUSED);
    }
    let Some(dir) = fixture.dir.as_deref() else {
        return usage("daemon needs --fixture-dir");
    };
    let served = run_daemon(
        dir,
        fixture.own_unit.as_deref(),
        fixture.coordinator_link,
        flags,
        own,
    );
    if served.is_ok()
        && let Some(file) = &fixture.hold_exit
        && hold(file)
    {
        loop {
            std::thread::park();
        }
    }
    ExitCode::from(cli::exit_status(&served))
}

/// The start of a `daemon` run from the file `--fixture-packaged` names, as the packaged binary
/// starts (the module says how): `None` when the flag is absent or this is not that file, and
/// the daemon starts in this process; otherwise the exit status of the error that ended the
/// start (an exec that works does not return).
fn packaged_start(
    fixture: &Fixture,
    flags: &DaemonFlags,
    own: &HostkeeperVersion,
) -> Option<ExitCode> {
    let path = fixture.packaged.as_deref()?;
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let image = std::env::current_exe().ok()?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let error = packaged::start(
        &image,
        &[path, canonical.as_path()],
        &flags.state_dir,
        &own.to_string(),
        &args,
        &packaged::exec,
    )?;
    eprintln!("{PROGRAM}: {error}");
    Some(ExitCode::from(error.exit_code()))
}

/// Whether this process now holds its exit at `file`: it created it, with its pid. `false` when
/// the file is already there (an earlier start held), or cannot be made.
fn hold(file: &Path) -> bool {
    let made = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)
        .and_then(|mut out| {
            writeln!(out, "{}", std::process::id())?;
            out.sync_all()
        });
    match made {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "{PROGRAM}: the exit is not held at {}: {error}",
                file.display()
            );
            false
        }
    }
}

/// What systemd set for the process, the notify socket's address and the watchdog's timeout:
/// what `hostkeeper_daemon::cli` reads for `--notify-systemd`.
#[expect(
    clippy::disallowed_methods,
    reason = "NOTIFY_SOCKET and WATCHDOG_USEC are the platform-set markers of a Type=notify unit and of its watchdog, which AGENTS.md S2 allows; the shipped binary reads them the same way"
)]
fn systemd_vars() -> SystemdVars {
    SystemdVars {
        notify_socket: std::env::var_os(cli::NOTIFY_SOCKET),
        watchdog_usec: std::env::var_os(cli::WATCHDOG_USEC),
    }
}

/// Starts the shipped daemon with the fixture's repository entries (on a Mac, its `macos-pkg`
/// signers and packages) and link, and serves it until it asks to exit for its own update, as
/// `hostkeeper_daemon::cli` does with the host's; its observer watches `own_unit` as the daemon's
/// own job when one is given. `coordinator_link` has it enroll and beat as the shipped daemon
/// does, its pki directory the fixture's.
fn run_daemon(
    dir: &Path,
    own_unit: Option<&str>,
    coordinator_link: bool,
    flags: &DaemonFlags,
    own: HostkeeperVersion,
) -> Result<(), StartError> {
    let systemd = systemd_vars();
    let watchdog = cli::watchdog(flags, &systemd);
    let notifier = cli::notifier(flags, systemd.notify_socket)?;
    let settings = flags.settings();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(StartError::Runtime)?;
    runtime.block_on(async {
        let platform = Platform::host();
        let config = start::load_config(&settings)?;
        let mut adapters = Adapters::for_platform(&platform)?;
        if let Some(unit) = own_unit {
            adapters.own_unit = unit.to_owned();
        }
        let mut files = HostFiles::host();
        // A Mac has no repository entries and never reads the directory.
        #[cfg(not(target_os = "macos"))]
        {
            files.repositories = Some(dir.join("repositories"));
        }
        #[cfg(target_os = "macos")]
        {
            files.macos_pkg_signers = Some(dir.join(macos_pkg::SIGNERS_FILE));
        }
        if coordinator_link {
            files.pki = dir.join("pki");
        }
        let mut ports: Ports = cli::host_ports(&config, &settings, &files, own, adapters)?;
        let mut repository = (*ports.repository).clone();
        repository.dirs.root = dir.join("root");
        #[cfg(target_os = "macos")]
        {
            // The installer's tools and the package store are the fixture's; the signers are the
            // ones the daemon's loader read from the fixture's file, as `host_ports` did.
            let tools: Arc<dyn hostkeeper_install::Tools> = Arc::new(macos_pkg::SignedByStore {
                dir: dir.to_path_buf(),
            });
            let pkg = hostkeeper_daemon::macos_pkg::ports(
                true,
                hostkeeper_install::host::Host::mac(),
                cli::load_signers(&files, 0)?,
                Some(tools),
            );
            repository = repository.with_macos_pkg(pkg);
            ports.packages = Arc::new(macos_pkg::PkgStore {
                dir: dir.to_path_buf(),
                packages: settings.packages_dir.clone(),
            });
        }
        ports.repository = Arc::new(repository);
        ports.placement.roots = Roots::new(
            &dir.join("placed"),
            &dir.join("units"),
            &settings.packages_dir,
        );
        let started = start::startup_notifying(&settings, ports, own, notifier.as_ref())?;
        let links = if coordinator_link {
            cli::host_links(
                started.daemon(),
                &files,
                flags.link.heartbeat_secs,
                platform.kind,
                watchdog,
            )
        } else {
            file_links(
                started.daemon(),
                &dir.join("link"),
                flags.link.heartbeat_secs,
                watchdog,
            )
        };
        start::serve(started, links).await;
        Ok(())
    })
}

/// The loops of the daemon with the link of [`FileLink`]: no enrollment (the loop tries again on
/// the daemon's clock, as the shipped daemon's does), a certificate that the link never presents, the loop's own clock, sleeper and workers as the shipped daemon's, and the
/// shipped daemon's watchdog (`cli::watchdog`), which the unit's `WatchdogSec=` waits for.
fn file_links(
    daemon: &Arc<Daemon>,
    dir: &Path,
    period: NonZeroU64,
    watchdog: Box<dyn Watchdog>,
) -> Links {
    let stop = StopFlag::default();
    let log: Arc<dyn Log> = Arc::new(DaemonLog::new(Arc::clone(daemon)));
    Links {
        enroller: Box::new(|| {
            Err("the self-update fixture daemon does not enroll: its coordinator is files".into())
        }),
        link: Arc::new(FileLink {
            dir: dir.to_path_buf(),
        }),
        certs: Arc::new(NoCertificate),
        clock: Arc::new(SystemUnixClock),
        log,
        sleeper: Box::new(ClockSleeper::new(
            Arc::clone(&daemon.ctx().clock),
            stop.clone(),
        )),
        enroll_sleeper: Box::new(ClockSleeper::new(
            Arc::clone(&daemon.ctx().clock),
            stop.clone(),
        )),
        renewal: hostkeeper_daemon::links::RenewalState::default(),
        watchdog,
        console: None,
        period,
        dispatch: Arc::new(ThreadDispatch),
        stop,
    }
}

/// A pair the heartbeat loop reads as enrolled; [`FileLink`] presents it to no one.
struct NoCertificate;

impl CertSource for NoCertificate {
    fn current(&self) -> CertState {
        CertState::Enrolled(ClientIdentity::new(
            b"fixture".to_vec(),
            b"fixture".to_vec(),
        ))
    }
}

/// The coordinator as two files of a directory: `beats.jsonl`, to which each heartbeat is
/// appended, and `order.json`, the order of every reply while it is there:
/// `{"id": "...", "try": 1, "service": "...", "version": "...", "digest": "<64 hex>", "entry":
/// {...}}`, an `update`, or the same with `"verb": "install"`, an `install`; the `entry` is the
/// entry of the order, as an order set by targets carries it, in the JSON of `target.json`
/// (`hostkeeper_daemon::target::to_json`). A test replaces `order.json` by a rename, so it is read
/// whole or not at all.
struct FileLink {
    dir: PathBuf,
}

impl CoordinatorLink for FileLink {
    fn heartbeat(&self, _identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError> {
        let transport = |e: io::Error| LinkError::Transport(e.kind());
        let mut beats = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("beats.jsonl"))
            .map_err(transport)?;
        // One write of the whole line: `writeln!` writes a line in pieces, and a daemon stopped
        // between two of them (the old one at its hand-over) leaves half a line that the next
        // daemon's line is appended to. One append of a regular file is one piece.
        let line = format!("{}\n", beat.to_json());
        beats.write_all(line.as_bytes()).map_err(transport)?;
        let malformed = || LinkError::BadReply(ReplyFault::Malformed);
        let (features, acknowledged) = match read_if_there(&self.dir.join("answer.json")) {
            Ok(Some(text)) => read_answer(&text).ok_or_else(malformed)?,
            Ok(None) => (Vec::new(), Vec::new()),
            Err(e) => return Err(transport(e)),
        };
        let order = match read_if_there(&self.dir.join("order.json")) {
            Ok(Some(text)) => Some(read_order(&text).ok_or_else(malformed)?),
            Ok(None) => None,
            Err(e) => return Err(transport(e)),
        };
        Ok(Reply {
            order,
            features,
            acknowledged,
            ..Reply::default()
        })
    }
}

/// The text of `path`; `None` when there is no such file.
fn read_if_there(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The features and acknowledged verdicts of an `answer.json`:
/// `{"features": ["..."], "acks": [{"service": "...", "order_id": "...", "try": 1}]}`, either
/// key optional; a feature this daemon does not know is left out, as the loop leaves it out of
/// a real reply.
fn read_answer(text: &str) -> Option<(Vec<Feature>, Vec<Acknowledged>)> {
    let json: Value = serde_json::from_str(text).ok()?;
    let list = |key: &str| json.get(key).and_then(Value::as_array).cloned();
    let features = list("features")
        .unwrap_or_default()
        .iter()
        .filter_map(|name| Feature::parse(name.as_str()?))
        .collect();
    let acks = list("acks")
        .unwrap_or_default()
        .iter()
        .map(|ack| {
            let word = |key: &str| ack.get(key).and_then(Value::as_str);
            Some(Acknowledged {
                service: ServiceName::new(word("service")?).ok()?,
                order: OrderRef {
                    id: OrderId::new(word("order_id")?).ok()?,
                    order_try: ack.get("try").and_then(Value::as_u64)?,
                },
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((features, acks))
}

/// The order of an `order.json`: an `update`, or an `install` when its `verb` says so. Any other
/// `verb` is no order.
fn read_order(text: &str) -> Option<Order> {
    let json: Value = serde_json::from_str(text).ok()?;
    let word = |key: &str| json.get(key).and_then(Value::as_str);
    let target = Target {
        service: ServiceName::new(word("service")?).ok()?,
        version: Version::new(word("version")?).ok()?,
        digest: PackageDigest::new(word("digest")?).ok()?,
        index_digest: None,
        entry: Some(LinkEntry::new(
            target::from_json(json.get("entry")?.to_string().as_bytes()).ok()?,
        )),
    };
    let verb = match word("verb") {
        None | Some("update") => Verb::Update(target),
        Some("install") => Verb::Install(target),
        Some(_) => return None,
    };
    Some(Order {
        id: OrderId::new(word("id")?).ok()?,
        order_try: json.get("try").and_then(Value::as_u64)?,
        verb,
    })
}
