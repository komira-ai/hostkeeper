//! `hk tailnet-wait`: waits, after a runner joined the tailnet, until the coordinator answers at
//! the address the next call talks to, and rejoins the tailnet once when it does not. It is not a
//! gateway call and takes no token. In order:
//!
//! 1. round 1: up to `--attempts` requests `GET <endpoint>/v1/version` (the coordinator's version
//!    route, which reads no token), trusting the certificates of `--ca-file` alone, each limited
//!    by `--timeout-secs`, with `--interval-secs` between two of them. Any HTTP status is an
//!    answer: the coordinator is reached, and what it says is the next call's to judge. A
//!    request that ends without one (no connection, no TLS, no status) is no answer, and says
//!    why on stderr, one line per attempt;
//! 2. when round 1 had no answer, the rejoin: `tailscale down`, then `tailscale up` with no flag
//!    (which brings the node back with the settings of its join), each stopped when it runs
//!    longer than [`Tailnet::limit`]. The join made the runner's user the node's operator, so
//!    neither runs as root;
//! 3. round 2: as round 1.
//!
//! The command ends 0 at the first answer, with one line on stdout saying which attempt of which
//! round it was; and 1 when neither round had one, or the rejoin failed, with one line saying how
//! each round, and the rejoin, ended.
//!
//! [`wait_for`] is the order above against a [`Net`]; the tests run it with a fake one, whose
//! clock, probe and rejoin they script. [`System`] is the real one.

#[cfg(test)]
mod tests;

use std::io::{Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use ureq::tls::Certificate;

use crate::args::{Globals, flag_of, value_of};
use crate::body::read_path;
use crate::call::is_endpoint;
use crate::help::TAILNET_WAIT_HELP;
use crate::send::{Limits, Method, certificates, one_line, send};
use crate::{Flow, PROGRAM, Stop, World};

/// The attempts of a round when `--attempts` is not given.
pub(crate) const DEFAULT_ATTEMPTS: u32 = 6;
/// The most attempts a round may have.
pub(crate) const MAX_ATTEMPTS: u32 = 100;
/// The seconds between two attempts when `--interval-secs` is not given.
pub(crate) const DEFAULT_INTERVAL_SECS: u32 = 10;
/// The most seconds `--interval-secs` may give.
pub(crate) const MAX_INTERVAL_SECS: u32 = 600;
/// How long `tailscale down` or `tailscale up` may run before it is stopped.
pub const REJOIN_LIMIT: Duration = Duration::from_secs(120);
/// How often a running `tailscale` is asked whether it ended.
const POLL: Duration = Duration::from_millis(50);
/// The most bytes of a `tailscale` command's stderr that a diagnostic quotes.
const SAID_BYTES: usize = 300;

/// What `hk tailnet-wait` rejoins the tailnet with, and what it waits between attempts with.
pub struct Tailnet<'a> {
    /// The `tailscale` program the rejoin runs as `tailscale down` and `tailscale up`.
    pub tailscale: PathBuf,
    /// How long each of those may run before it is stopped.
    pub limit: Duration,
    /// Waits the time between two attempts.
    pub wait: &'a dyn Fn(Duration),
    /// The time since an origin that does not change in the run, by which `hk releases wait`
    /// spends its `--wait-secs`.
    pub now: &'a dyn Fn() -> Duration,
}

impl Tailnet<'static> {
    /// `tailscale` from the `PATH`, [`REJOIN_LIMIT`], the thread's own sleep and the system's
    /// monotonic clock.
    pub fn system() -> Tailnet<'static> {
        Tailnet {
            tailscale: PathBuf::from("tailscale"),
            limit: REJOIN_LIMIT,
            wait: &std::thread::sleep,
            now: &crate::xcode_push::monotonic,
        }
    }
}

/// What the wait runs against: the coordinator's route, the clock and the tailnet.
pub(crate) trait Net {
    /// One request to the coordinator: the HTTP status of its answer, or why there is none.
    fn probe(&mut self) -> Result<u16, String>;
    /// Waits `time` before the next attempt.
    fn wait(&mut self, time: Duration);
    /// Leaves the tailnet and joins it again; why not, when it could not.
    fn rejoin(&mut self) -> Result<(), String>;
}

/// The attempts of each round and the time between two of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) attempts: u32,
    pub(crate) interval: Duration,
}

/// How a round with no answer ended: its last attempt's reason.
struct Silent {
    last: String,
}

/// Round `round` of `plan` against `net`: the attempt and the status of the first answer, or the
/// reason of the last attempt when none answered. Each attempt with no answer is one line on
/// `stderr`.
fn round(
    net: &mut dyn Net,
    plan: Plan,
    round: u8,
    stderr: &mut dyn Write,
) -> Result<(u32, u16), Silent> {
    let mut last = String::new();
    for attempt in 1..=plan.attempts {
        if attempt > 1 {
            net.wait(plan.interval);
        }
        match net.probe() {
            Ok(status) => return Ok((attempt, status)),
            Err(why) => {
                // A stderr that cannot be written to does not stop the wait.
                let _ = writeln!(
                    stderr,
                    "{PROGRAM}: tailnet-wait: round {round}, attempt {attempt} of {}: no answer: {why}",
                    plan.attempts
                );
                last = why;
            }
        }
    }
    Err(Silent { last })
}

/// The wait of `plan` against `net`: the line that says which attempt answered, or the failure
/// that says how both rounds (or round 1 and the rejoin) ended.
pub(crate) fn wait_for(
    net: &mut dyn Net,
    plan: Plan,
    stderr: &mut dyn Write,
) -> Result<String, String> {
    let attempts = plan.attempts;
    let first = match round(net, plan, 1, stderr) {
        Ok((attempt, status)) => {
            return Ok(format!(
                "the coordinator answered (HTTP {status}) on attempt {attempt} of {attempts} of round 1, after the join"
            ));
        }
        Err(silent) => silent.last,
    };
    let _ = writeln!(
        stderr,
        "{PROGRAM}: tailnet-wait: no answer in the {attempts} attempts of round 1; rejoining the tailnet (tailscale down, then tailscale up)"
    );
    if let Err(why) = net.rejoin() {
        return Err(format!(
            "the coordinator did not answer: round 1, after the join: no answer in {attempts} attempts, the last: {first}; the rejoin failed: {why}; round 2 did not run"
        ));
    }
    match round(net, plan, 2, stderr) {
        Ok((attempt, status)) => Ok(format!(
            "the coordinator answered (HTTP {status}) on attempt {attempt} of {attempts} of round 2, after the rejoin (round 1 had no answer in {attempts} attempts)"
        )),
        Err(silent) => Err(format!(
            "the coordinator did not answer: round 1, after the join: no answer in {attempts} attempts, the last: {first}; round 2, after the rejoin: no answer in {attempts} attempts, the last: {}",
            silent.last
        )),
    }
}

/// The real [`Net`]: the coordinator's version route over HTTPS, the wait and the `tailscale`
/// of a [`Tailnet`].
pub(crate) struct System<'t, 'a> {
    /// `<endpoint>/v1/version`.
    pub(crate) url: String,
    /// The certificates of `--ca-file`, the only ones trusted.
    pub(crate) roots: Vec<Certificate<'static>>,
    /// The limits of one request.
    pub(crate) limits: Limits,
    pub(crate) tailnet: &'t Tailnet<'a>,
}

impl Net for System<'_, '_> {
    fn probe(&mut self) -> Result<u16, String> {
        send(&self.url, &Method::Get(None), &self.roots, self.limits).map(|answer| answer.status)
    }

    fn wait(&mut self, time: Duration) {
        (self.tailnet.wait)(time);
    }

    fn rejoin(&mut self) -> Result<(), String> {
        for verb in ["down", "up"] {
            run_limited(&self.tailnet.tailscale, verb, self.tailnet.limit)?;
        }
        Ok(())
    }
}

/// Runs `<program> <verb>` with no input and no output but its stderr, and stops it when it runs
/// longer than `limit`. The error says how it ended, with the last line of its stderr.
fn run_limited(program: &Path, verb: &str, limit: Duration) -> Result<(), String> {
    let mut command = crate::xcode_push::command(program);
    command
        .arg(verb)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let what = format!("{} {verb}", program.display());
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot run {what} ({error})"))?;
    let stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        stderr
            .map(|mut stderr| {
                let mut said = Vec::new();
                // What it said is for the diagnostic alone; a read that fails leaves it shorter.
                let _ = stderr.read_to_end(&mut said);
                said
            })
            .unwrap_or_default()
    });
    let started = Instant::now();
    let ended: Option<ExitStatus> = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break Some(status);
        }
        if started.elapsed() >= limit {
            // It may have ended since it was asked; either way it is reaped here.
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(POLL);
    };
    let said = reader.join().unwrap_or_default();
    let said = last_line(&said);
    let tail = if said.is_empty() {
        String::new()
    } else {
        format!(": {said}")
    };
    match ended {
        Some(status) if status.success() => Ok(()),
        Some(status) => Err(format!("{what} ended with {status}{tail}")),
        None => Err(format!(
            "{what} did not end within {} seconds and was stopped{tail}",
            limit.as_secs_f64()
        )),
    }
}

/// The last line of `text` that is not blank, as one line, at most [`SAID_BYTES`] bytes of it.
fn last_line(text: &[u8]) -> String {
    let text = String::from_utf8_lossy(text);
    let line = one_line(
        text.lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default(),
    );
    let mut end = line.len().min(SAID_BYTES);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line.get(..end).unwrap_or_default().to_owned()
}

/// The whole number of `value`, when it is one of decimal digits from `min` to `max`.
fn number(value: &[u8], min: u32, max: u32) -> Option<u32> {
    if value.is_empty() || value.len() > 6 || !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    // At most six digits: no overflow.
    let n = value
        .iter()
        .fold(0_u32, |n, digit| n * 10 + u32::from(digit - b'0'));
    (min..=max).contains(&n).then_some(n)
}

/// `hk tailnet-wait`: `args` are the words after `tailnet-wait`, among which the global flags
/// are taken; `globals` are those given before it.
pub(crate) fn tailnet_wait(w: &mut World<'_>, mut globals: Globals, args: &[Vec<u8>]) -> Flow<()> {
    let mut attempts: Option<u32> = None;
    let mut interval: Option<u32> = None;
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        let rest = args.get(at..).unwrap_or_default();
        let flag = flag_of(arg);
        let (slot, min, max) = match flag {
            b"--help" | b"-h" => {
                let _ = writeln!(w.stdout, "{TAILNET_WAIT_HELP}");
                return Ok(());
            }
            b"--attempts" => (&mut attempts, 1, MAX_ATTEMPTS),
            b"--interval-secs" => (&mut interval, 0, MAX_INTERVAL_SECS),
            _ => {
                if let Some(took) = globals.take(rest)? {
                    at += took;
                    continue;
                }
                let what: &[u8] = if arg.starts_with(b"-") {
                    b"has no flag "
                } else {
                    b"takes no argument: "
                };
                return Err(Stop::usage(
                    [format!("{PROGRAM} tailnet-wait ").as_bytes(), what, flag].concat(),
                ));
            }
        };
        let (value, took) = value_of(rest)?;
        if slot.is_some() {
            return Err(Stop::usage([flag, b" is given twice"].concat()));
        }
        let Some(n) = number(value, min, max) else {
            return Err(Stop::usage(format!(
                "{} takes a whole number from {min} to {max}",
                String::from_utf8_lossy(flag)
            )));
        };
        *slot = Some(n);
        at += took;
    }
    let plan = Plan {
        attempts: attempts.unwrap_or(DEFAULT_ATTEMPTS),
        interval: Duration::from_secs(u64::from(interval.unwrap_or(DEFAULT_INTERVAL_SECS))),
    };
    if globals.token_file.is_some()
        || globals.token_stdin
        || globals.audience.is_some()
        || globals.response_file.is_some()
    {
        return Err(Stop::usage(format!(
            "{PROGRAM} tailnet-wait takes no token and writes no answer: --token-file, \
             --token-stdin, --github-oidc-audience and --response-file are not for it"
        )));
    }
    let (Some(endpoint), Some(ca_file)) = (&globals.endpoint, &globals.ca_file) else {
        return Err(Stop::usage(format!(
            "{PROGRAM} tailnet-wait needs --endpoint and --ca-file"
        )));
    };
    if !is_endpoint(endpoint) {
        return Err(Stop::usage("--endpoint takes https://<host>[:<port>]"));
    }
    let Some(ca) = read_path(ca_file) else {
        return Err(Stop::usage(
            [b"cannot read --ca-file ".as_slice(), ca_file].concat(),
        ));
    };
    let roots = certificates(&ca);
    if roots.is_empty() {
        return Err(Stop::usage(
            [b"--ca-file ".as_slice(), ca_file, b" holds no certificate"].concat(),
        ));
    }
    let secs = Duration::from_secs(u64::from(globals.timeout_secs()));
    let endpoint = String::from_utf8_lossy(endpoint.strip_suffix(b"/").unwrap_or(endpoint));
    let mut net = System {
        url: format!("{endpoint}/v1/version"),
        roots,
        limits: Limits {
            connect: secs,
            total: secs,
        },
        tailnet: w.tailnet,
    };
    match wait_for(&mut net, plan, w.stderr) {
        Ok(line) => {
            let _ = writeln!(w.stdout, "{line}");
            Ok(())
        }
        Err(failure) => Err(Stop::fail(failure)),
    }
}
