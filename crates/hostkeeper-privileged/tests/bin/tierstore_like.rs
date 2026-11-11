//! `tierstore-like`, the service of the tierstore-shaped end-to-end test: a program that is both
//! the workload and its own update hook, as the production service `tierstore` is.
//!
//! It is built by cargo with the crate's tests and put by the test into a real `.deb`
//! (`tests/e2e/package.rs`), as the program of its run units and as its hook; nothing of it is
//! checked in as a binary. It depends on std and on `hostkeeper_hook` (the params file reader and
//! the step line) only.
//!
//! ```text
//! tierstore-like serve <port> [<release file>]
//! tierstore-like [update-hook] <step> [<staged release>] --params <file>
//! ```
//!
//! `serve` is the run unit's `ExecStart`: a tiny HTTP server on `127.0.0.1:<port>` that answers
//! `GET /healthz` with `version=<version>` and everything else with 404. The version is the one
//! in the release file of the package installed on the host ([`RELEASE_FILE`], or the file the
//! second argument names), so two releases answer differently. It never exits on its own:
//! systemd's `SIGTERM` (whose default action ends the process) stops it.
//!
//! The hook is this program run from the tree of the staged version
//! (`<version directory>/tree/usr/lib/hostkeeper-ci-tierstore/hostkeeper-hook`), which reads the
//! release file of the staged and of the previous version directory ([`TREE_RELEASE`]). The five
//! steps behave as the production hook does in outline, and leave files in the record directory
//! (the entry's `tierstore_like_record_dir` param) for the test to read:
//!
//! - `prepare` waits while `drain-gate` holds `busy` (writing `prepare-<v>.waiting` once), then
//!   writes `staged-<v>`.
//! - `cutover` starts the new run units, waits until each port answers `version=<v>`, then stops
//!   the previous release's run units that the new release does not run.
//! - `verify` fails when the param `tierstore_like_fail_verify` is true, else asks every port once.
//! - `rollback` starts the previous run units and stops the new ones the previous does not run.
//! - `cleanup` removes `staged-<v>`.
//!
//! Each step ends by writing `<step>-<v>` (`ok` or `failed`) and a last stdout line, the JSON
//! report of the hook contract. The exit code is `hostkeeper_hook`'s: 0 ok, 1 failure.

use std::convert::Infallible;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

use hostkeeper_hook::{Invocation, Outcome, Params, Scalar, Step};

type Fallible<T> = Result<T, Box<dyn Error>>;

/// The release file of the package installed on the host, which `serve` reads.
const RELEASE_FILE: &str = "/usr/share/hostkeeper-ci-tierstore/release.toml";
/// The release file in a version directory, which the hook reads of the staged and of the previous
/// version.
const TREE_RELEASE: &str = "tree/usr/share/hostkeeper-ci-tierstore/release.toml";
/// The param naming the directory the hook records into (an absolute path).
const RECORD_DIR_KEY: &str = "tierstore_like_record_dir";
/// The param that makes `verify` fail.
const FAIL_VERIFY_KEY: &str = "tierstore_like_fail_verify";
/// The gate file the test writes: `busy` holds `prepare`, `idle` or absent lets it go.
const GATE_FILE: &str = "drain-gate";
/// Where `systemctl` is on the runner.
const SYSTEMCTL: &str = "/usr/bin/systemctl";
/// How long a poll sleeps between two looks.
const POLL: Duration = Duration::from_millis(200);
/// The longest wait of one socket operation, so a stuck peer cannot hold a thread or a poll.
const SOCKET_WAIT: Duration = Duration::from_secs(5);
/// The most bytes of a request head the server reads.
const MAX_HEAD: usize = 8 * 1024;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().and_then(|a| a.to_str()) == Some("serve") {
        return match serve(&args[1..]) {
            Ok(never) => match never {},
            Err(e) => {
                eprintln!("tierstore-like serve: {e}");
                Outcome::Failure.into()
            }
        };
    }
    // The daemon passes the program's own `hook_args` (`update-hook`) before the step, or not.
    let rest = match args.first().and_then(|a| a.to_str()) {
        Some("update-hook") => &args[1..],
        _ => &args[..],
    };
    match hook(rest) {
        Ok(outcome) => outcome.into(),
        Err(e) => {
            eprintln!("tierstore-like: {e}");
            println!(r#"{{"class":"release","reason":"fixture_error"}}"#);
            Outcome::Failure.into()
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The release: a release file, read the way this fixture needs it.

/// One release of the service: its version and its run units.
struct Release {
    version: String,
    /// The `run` entries, `hostkeeper-ci-tierstore@<port>.service`.
    units: Vec<String>,
}

impl Release {
    /// Reads the release file `path`: the line `version = "<v>"` and the line `run = [...]`. The
    /// file is the fixture's own template, so a line reader is enough.
    fn load(path: &Path) -> Fallible<Release> {
        let text =
            fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut version = None;
        let mut units = None;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "version" if version.is_none() => {
                    version = quoted(value).into_iter().next().map(str::to_owned);
                }
                "run" if units.is_none() => {
                    units = Some(quoted(value).into_iter().map(str::to_owned).collect());
                }
                _ => {}
            }
        }
        Ok(Release {
            version: version.ok_or_else(|| format!("{} has no version", path.display()))?,
            units: units.ok_or_else(|| format!("{} has no run units", path.display()))?,
        })
    }

    /// The ports of the run units: the digits between `@` and `.service`.
    fn ports(&self) -> Fallible<Vec<u16>> {
        let mut ports = Vec::new();
        for unit in &self.units {
            let port = unit
                .rsplit_once('@')
                .and_then(|(_, instance)| instance.strip_suffix(".service"))
                .and_then(|port| port.parse::<u16>().ok())
                .ok_or_else(|| format!("run unit {unit:?} is not an instance with a port"))?;
            ports.push(port);
        }
        Ok(ports)
    }
}

/// The double-quoted strings of a TOML line (the odd pieces between the quotes).
fn quoted(text: &str) -> Vec<&str> {
    text.split('"').skip(1).step_by(2).collect()
}

// ---------------------------------------------------------------------------------------------
// serve

/// Runs the HTTP server of one run unit: `serve <port> [<release file>]`.
fn serve(args: &[OsString]) -> Fallible<Infallible> {
    let (port, release) = match args {
        [port] => (port, Path::new(RELEASE_FILE)),
        [port, release] => (port, Path::new(release)),
        _ => return Err("usage: tierstore-like serve <port> [<release file>]".into()),
    };
    let port: u16 = port
        .to_str()
        .and_then(|port| port.parse().ok())
        .ok_or("the port is not a number")?;
    let body = format!("version={}", Release::load(release)?.version);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    loop {
        let (stream, _) = listener.accept()?;
        let body = body.clone();
        // A connection that fails is the client's; the server goes on.
        thread::spawn(move || {
            let _ = answer(stream, &body);
        });
    }
}

/// Reads one request head and answers it.
fn answer(mut stream: TcpStream, body: &str) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_WAIT))?;
    stream.set_write_timeout(Some(SOCKET_WAIT))?;
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < MAX_HEAD {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&head);
    let mut line = head.lines().next().unwrap_or("").split(' ');
    let found = line.next() == Some("GET") && line.next() == Some("/healthz");
    let (status, body) = if found {
        ("200 OK", body)
    } else {
        ("404 Not Found", "")
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// `GET /healthz` of `127.0.0.1:<port>`: the status and the body, `None` when nothing answers.
fn healthz(port: u16) -> Option<(u16, String)> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, SOCKET_WAIT).ok()?;
    stream.set_read_timeout(Some(SOCKET_WAIT)).ok()?;
    stream.set_write_timeout(Some(SOCKET_WAIT)).ok()?;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply).ok()?;
    let (head, body) = reply.split_once("\r\n\r\n")?;
    let status = head.lines().next()?.split(' ').nth(1)?.parse().ok()?;
    Some((status, body.to_owned()))
}

/// Whether the server on `port` answers with `version`.
fn serves(port: u16, version: &str) -> bool {
    healthz(port)
        .is_some_and(|(status, body)| status == 200 && body == format!("version={version}"))
}

// ---------------------------------------------------------------------------------------------
// the update hook

/// What a step knows: its release, the previous one, and where it records.
struct Ctx {
    params: Params,
    record_dir: PathBuf,
    own: Release,
    previous: Option<Release>,
}

impl Ctx {
    /// Reads the params file and the release files of the staged and the previous version, and
    /// refuses a params file whose versions do not match the release files it points at.
    fn read(params_file: &Path) -> Fallible<Ctx> {
        let params = Params::read(params_file)?;
        let record_dir = match params.get(RECORD_DIR_KEY) {
            Some(Scalar::Text(dir)) if Path::new(dir).is_absolute() => PathBuf::from(dir),
            _ => return Err(format!("{RECORD_DIR_KEY} is not an absolute path").into()),
        };
        let staged = params.staged_release().ok_or("no updater.staged_release")?;
        let own = Release::load(&staged.join(TREE_RELEASE))?;
        if params.target_version() != Some(own.version.as_str()) {
            return Err("updater.target_version is not the staged release's version".into());
        }
        let previous = match params.previous_release() {
            Some(dir) => {
                let previous = Release::load(&dir.join(TREE_RELEASE))?;
                if params.previous_version() != Some(previous.version.as_str()) {
                    return Err("updater.previous_version is not the previous release's".into());
                }
                Some(previous)
            }
            None => None,
        };
        Ok(Ctx {
            params,
            record_dir,
            own,
            previous,
        })
    }

    /// Writes `<name>-<version>` into the record directory, whole or not at all.
    fn record(&self, name: &str, content: &str) -> io::Result<()> {
        let path = self.record_dir.join(format!("{name}-{}", self.own.version));
        write_whole(&path, content)
    }

    /// The run units of the previous release that the new one does not run.
    fn previous_only(&self) -> Vec<&str> {
        let own = &self.own.units;
        let previous = self.previous.iter().flat_map(|p| &p.units);
        previous
            .filter(|u| !own.contains(u))
            .map(String::as_str)
            .collect()
    }

    /// The run units of the new release that the previous one does not run.
    fn own_only(&self) -> Vec<&str> {
        let none = Vec::new();
        let previous = self.previous.as_ref().map_or(&none, |p| &p.units);
        self.own
            .units
            .iter()
            .filter(|u| !previous.contains(u))
            .map(String::as_str)
            .collect()
    }
}

/// Writes `path` through a temporary file and a rename, so a reader never sees half of it.
fn write_whole(path: &Path, content: &str) -> io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.tmp"));
    fs::write(&temporary, content)?;
    fs::rename(&temporary, path)
}

/// Runs one step: parses the command line, reads the params, runs the step, records how it ended.
fn hook(args: &[OsString]) -> Fallible<Outcome> {
    let invocation = Invocation::parse(args.iter().cloned())?;
    let params_file = invocation.params.ok_or("--params is required")?;
    let ctx = Ctx::read(&params_file)?;
    let step = invocation.step;
    let result = match step {
        Step::Prepare => prepare(&ctx),
        Step::Cutover => cutover(&ctx),
        Step::Verify => verify(&ctx),
        Step::Rollback => rollback(&ctx),
        Step::Cleanup => cleanup(&ctx),
        Step::Uninstall => Err("the fixture has no uninstall step".into()),
    };
    // A step that cannot finish says so in its record, for the test to read, before it fails.
    if result.is_err() {
        let _ = ctx.record(step.name(), "failed");
    }
    result
}

/// The last stdout line of a step that ends well, and its outcome.
fn done(line: &str) -> Fallible<Outcome> {
    println!("{line}");
    Ok(Outcome::Success)
}

/// The last stdout line of a step that ends badly, and its outcome.
fn failed(reason: &str) -> Fallible<Outcome> {
    println!(r#"{{"class":"release","reason":"{reason}"}}"#);
    Ok(Outcome::Failure)
}

/// `prepare`: waits for the drain gate, then stages.
fn prepare(ctx: &Ctx) -> Fallible<Outcome> {
    let gate = ctx.record_dir.join(GATE_FILE);
    let waiting = ctx
        .record_dir
        .join(format!("prepare-{}.waiting", ctx.own.version));
    let mut announced = false;
    loop {
        match fs::read_to_string(&gate) {
            Ok(text) if text.trim() == "busy" => {
                // Said once, for the test to know the hook is parked on the gate.
                if !announced {
                    write_whole(&waiting, "waiting")?;
                    announced = true;
                }
                thread::sleep(POLL);
            }
            Ok(text) if text.trim() == "idle" => break,
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Ok(other) => return Err(format!("the drain gate holds {:?}", other.trim()).into()),
            Err(e) => return Err(e.into()),
        }
    }
    ctx.record("staged", "staged")?;
    ctx.record("prepare", "ok")?;
    done("{}")
}

/// `cutover`: starts the new run units, waits until each serves the new version, then stops the
/// previous release's units that the new one does not run. The wait ends only by the step's own
/// bound (the daemon kills the step at `hook.timeout_secs.cutover`).
fn cutover(ctx: &Ctx) -> Fallible<Outcome> {
    for unit in &ctx.own.units {
        systemctl("start", unit)?;
    }
    for port in ctx.own.ports()? {
        while !serves(port, &ctx.own.version) {
            thread::sleep(POLL);
        }
    }
    for unit in ctx.previous_only() {
        systemctl("stop", unit)?;
    }
    ctx.record("cutover", "ok")?;
    done(r#"{"reversible":true}"#)
}

/// `verify`: fails on request, else asks every port of the new release once.
fn verify(ctx: &Ctx) -> Fallible<Outcome> {
    let refused = ctx.params.get_bool(FAIL_VERIFY_KEY) == Some(true);
    let serving = !refused
        && ctx
            .own
            .ports()?
            .iter()
            .all(|port| serves(*port, &ctx.own.version));
    if serving {
        ctx.record("verify", "ok")?;
        done("{}")
    } else {
        ctx.record("verify", "failed")?;
        failed("fixture_verify_failed")
    }
}

/// `rollback`: starts the previous release's units, stops the new ones it does not run.
fn rollback(ctx: &Ctx) -> Fallible<Outcome> {
    for unit in ctx.previous.iter().flat_map(|p| &p.units) {
        systemctl("start", unit)?;
    }
    for unit in ctx.own_only() {
        systemctl("stop", unit)?;
    }
    ctx.record("rollback", "ok")?;
    done("{}")
}

/// `cleanup`: drops what `prepare` staged.
fn cleanup(ctx: &Ctx) -> Fallible<Outcome> {
    match fs::remove_file(ctx.record_dir.join(format!("staged-{}", ctx.own.version))) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    ctx.record("cleanup", "ok")?;
    done("{}")
}

/// `systemctl --system <action> <unit>`, which must exit 0.
fn systemctl(action: &str, unit: &str) -> Fallible<()> {
    let status = Command::new(SYSTEMCTL)
        .args(["--system", action, unit])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("systemctl {action} {unit} failed: {status}").into())
    }
}
