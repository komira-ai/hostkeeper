//! `hk releases wait`: waits, for at most `--wait-secs`, for the fleet to converge on a version of
//! a service, in held calls of `Releases.WaitConverged` of at most [`SLICE_SECS`] each, and says
//! what it waits on. It is a method of the generated service `releases` written by hand
//! ([`crate::hand`]); each call is the generated `releases wait-converged`'s POST, with a token of
//! its own.
//!
//! Why the calls are short: a wait held on one connection for up to half an hour sends no byte
//! until it ends, and anything on the path that drops a connection it has not seen a byte on for a
//! while (a stateful firewall, a NAT, a relay) cuts it with no answer. A call of [`SLICE_SECS`] is
//! well under the shortest such limit a hosted runner's path is known to have (four minutes), and
//! a call that is cut costs only itself: the same question is asked again.
//!
//! In order, until `--wait-secs` have passed on the clock of [`crate::tailnet_wait::Tailnet`]:
//!
//! 1. one held call of the time left, at most [`SLICE_SECS`];
//! 2. its answer: `DEADLINE` is not yet, and the next call is made; any other outcome
//!    (`CONVERGED`, `HALTED`, `SUPERSEDED`, `NO_DESIRED`) ends the wait with that answer. Each not
//!    yet whose table ([`table`]: each host the wait waits on, its state and since when, and why
//!    each plan waits) differs from the last one printed is printed on stderr;
//! 3. a call that got no answer (no connection, a connection cut in mid-call, an answer that is
//!    not the gateway's, or no token from the runner) is said on stderr, and asked again after
//!    [`RETRY`]; a refusal of the coordinator ends the wait as the generated command's would.
//!
//! When `--wait-secs` have passed, the last not yet is the answer, and its table is printed under
//! a line that says the fleet did not converge in time; when no call was answered at all, the
//! command fails with the last call's reason. The answer, whichever ended the wait, is written as
//! the generated command writes it (stdout, or `--response-file`), and the exit code is 0 for an
//! answer: what it says is the caller's to judge, as for `releases wait-converged`.
//!
//! [`wait_for`] is the order above against a [`Fleet`]; the tests run it against a fake one whose
//! clock, answers and cuts they script. [`System`] is the real one.

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::Write;
use std::time::Duration;

use serde_json::{Value, json};
use ureq::tls::Certificate;

use crate::args::{Globals, flag_of, value_of};
use crate::call::{Refused, credential, deliver, post, said_refused, target_of};
use crate::send::{Limits, certificates};
use crate::{Flow, PROGRAM, Stop, World};

/// The longest one call is held: well under four minutes, the shortest idle limit known on a
/// hosted runner's path; the table is at most this old.
pub(crate) const SLICE_SECS: u32 = 60;
/// The most `--wait-secs` may give: the coordinator's own bound of a held call, which
/// `set-desired.yml`'s `max_polls` of 180 gives.
pub(crate) const MAX_WAIT_SECS: u32 = 1800;
/// The pause before a call that got no answer is asked again.
pub(crate) const RETRY: Duration = Duration::from_secs(10);
/// The gateway's path of `Releases.WaitConverged`.
const PATH: &str = "/hostkeeper.operator.v1.Releases/WaitConverged";
/// The outcome that is not yet: the held call's time passed first.
const DEADLINE: &str = "WAIT_OUTCOME_DEADLINE";

/// What one held call got.
#[derive(Debug)]
pub(crate) enum Asked {
    /// The gateway's `200`: the answer's body.
    Answer(Vec<u8>),
    /// The coordinator refused the call.
    Refused(Refused),
    /// No answer, and why.
    Cut(String),
}

/// What the wait runs against: the coordinator and the clock.
pub(crate) trait Fleet {
    /// One call held for at most `secs`. An `Err` ends the wait (a usage error).
    fn ask(&mut self, secs: u32) -> Flow<Asked>;
    /// The time since an origin that does not change in the run.
    fn now(&mut self) -> Duration;
    /// Waits `time` before the next call.
    fn pause(&mut self, time: Duration);
    /// Where the wait says how it goes, line by line, as it goes.
    fn stderr(&mut self) -> &mut dyn Write;
}

/// What is waited for, and for how long.
pub(crate) struct Plan<'a> {
    pub(crate) service: &'a str,
    pub(crate) version: &'a str,
    pub(crate) host_group: Option<&'a str>,
    pub(crate) bound: Duration,
}

/// How the wait ended.
#[derive(Debug)]
pub(crate) enum Ended {
    /// An answer: the one that ended the wait, or the last not yet when the time ran out.
    Answered(Vec<u8>),
    /// A refusal.
    Refused(Refused),
    /// No call was answered; the last one's reason.
    NoAnswer(String),
}

/// `time` as `<m>m<ss>s`, or `<h>h<mm>m<ss>s` from an hour.
pub(crate) fn span(time: Duration) -> String {
    let secs = time.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else {
        format!("{m}m{s:02}s")
    }
}

/// `unix` seconds as `YYYY-MM-DDTHH:MM:SSZ`, in UTC.
pub(crate) fn utc(unix: u64) -> String {
    // The civil date of a count of days since 1970-01-01 (H. Hinnant's `civil_from_days`), on
    // unsigned numbers: every value here is at least 0.
    let (days, secs) = (unix / 86_400, unix % 86_400);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// The text of `value`'s `key`, when it is a string.
fn text<'v>(value: &'v Value, key: &str) -> Option<&'v str> {
    value.get(key).and_then(Value::as_str)
}

/// The number of `value`'s `key`: proto3 JSON writes a 64-bit one as a string, a 32-bit one as a
/// number; 0 when absent or neither.
fn number(value: &Value, key: &str) -> u64 {
    match value.get(key) {
        Some(Value::Number(n)) => n.as_u64().unwrap_or_default(),
        Some(Value::String(s)) => s.parse().unwrap_or_default(),
        _ => 0,
    }
}

/// The items of `value`'s array `key`; none when absent.
fn items<'v>(value: &'v Value, key: &str) -> &'v [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// An enum value's name less its `prefix`, in lower case, `_` as a space: `PLAN_WAIT_ROLLOUT_ELSEWHERE`
/// is `rollout elsewhere`. An absent one is `unspecified`, as proto3 JSON leaves out a 0.
fn words(name: Option<&str>, prefix: &str) -> String {
    let name = name.unwrap_or("UNSPECIFIED");
    name.strip_prefix(prefix)
        .unwrap_or(name)
        .to_ascii_lowercase()
        .replace('_', " ")
}

/// When this wait first saw each host in the state it is in now: the coordinator keeps no time of
/// when an update started running, so a host's since is, failing the coordinator's, the answer
/// that first showed it so.
#[derive(Default)]
pub(crate) struct Seen(BTreeMap<String, (String, u64)>);

impl Seen {
    /// When `node` was first seen in `state`, `answered` being the time of this answer.
    fn since(&mut self, node: &str, state: &str, answered: u64) -> u64 {
        let entry = self
            .0
            .entry(node.to_owned())
            .or_insert_with(|| (state.to_owned(), answered));
        if entry.0 != state {
            *entry = (state.to_owned(), answered);
        }
        entry.1
    }
}

/// One line of the table for the host `host` of a `notYet`, answered at `answered`.
fn host_line(host: &Value, answered: u64, seen: &mut Seen) -> String {
    let node = text(host, "nodeId").unwrap_or_default();
    let role = text(host, "role").unwrap_or_default();
    let state = text(host, "state");
    let reason = text(host, "reason").unwrap_or_default();
    let on = text(host, "installedVersion")
        .map_or_else(|| "nothing installed".to_owned(), |v| format!("on {v}"));
    let since_unix = number(host, "sinceUnix");
    let key = format!(
        "{}{}",
        state.unwrap_or_default(),
        text(host, "updatingTo").unwrap_or_default()
    );
    let first = seen.since(node, &key, answered);
    let seen_since = format!("seen so since {}", utc(first));
    let because = |what: &str| {
        if reason.is_empty() {
            what.to_owned()
        } else {
            format!("{what} ({reason})")
        }
    };
    let said = match state.unwrap_or_default() {
        "WAITING_STATE_UPDATING" => {
            let to = text(host, "updatingTo").unwrap_or_default();
            let step = text(host, "step").map_or_else(String::new, |step| {
                format!(", step {step} (try {})", number(host, "stepAttempt"))
            });
            format!("updating to {to}{step}, {on}, {seen_since}")
        }
        "WAITING_STATE_FAILED" => {
            let started = if since_unix == 0 {
                seen_since
            } else {
                format!("its update started {} by its clock", utc(since_unix))
            };
            format!("{}, {on}, {started}", because("failed"))
        }
        "WAITING_STATE_EXCLUDED" => format!("{}, {on}", because("excluded")),
        "WAITING_STATE_SILENT" => {
            let heard = if since_unix == 0 {
                "never heard since the coordinator started".to_owned()
            } else {
                format!("silent since {}", utc(since_unix))
            };
            format!("{heard}, {on}")
        }
        "WAITING_STATE_SETTLING" => format!("{}, {on}, {seen_since}", because("settling")),
        "WAITING_STATE_BEHIND" => format!("{}, {on}, {seen_since}", because("not ordered yet")),
        other => format!("{}, {on}", words(Some(other), "WAITING_STATE_")),
    };
    format!("{node} ({role}): {said}")
}

/// The table of the answer `answer`: the lines that say what the wait still waits on, which are
/// printed when they differ from the last ones printed.
///
/// With a `notYet` (from a coordinator that lists it): one line per host, the count of the hosts
/// it leaves out, and one line per plan. From a coordinator before it: one line per role of the
/// convergence, with its counts.
pub(crate) fn table(answer: &Value, seen: &mut Seen) -> Vec<String> {
    let convergence = answer.get("convergence").unwrap_or(&Value::Null);
    let state = words(text(convergence, "state"), "CONVERGENCE_STATE_");
    let mut lines = vec![format!("state: {state}")];
    let Some(not_yet) = answer.get("notYet") else {
        for role in items(convergence, "roles") {
            lines.push(format!(
                "{}: {} at the version, {} updating, {} failed, {} silent (this coordinator does \
                 not list its hosts)",
                text(role, "role").unwrap_or_default(),
                number(role, "atDesired"),
                number(role, "updating"),
                number(role, "failed"),
                number(role, "down")
            ));
        }
        return lines;
    };
    let answered = number(not_yet, "answeredUnix");
    let hosts = items(not_yet, "hosts");
    if hosts.is_empty() {
        lines.push("no host is listed".to_owned());
    }
    for host in hosts {
        lines.push(host_line(host, answered, seen));
    }
    let left_out = number(not_yet, "hostsLeftOut");
    if left_out > 0 {
        lines.push(format!("and {left_out} more hosts not listed"));
    }
    for plan in items(not_yet, "plans") {
        let group = text(plan, "hostGroup").map_or_else(
            || "the plan".to_owned(),
            |group| format!("the plan of {group}"),
        );
        let mut line = format!(
            "{group} waits: {}",
            words(text(plan, "waiting"), "PLAN_WAIT_")
        );
        let on: Vec<&str> = items(plan, "waitingOn")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        if !on.is_empty() {
            line.push_str(&format!(" on {}", on.join(", ")));
        }
        if let Some(why) = text(plan, "pauseReason") {
            line.push_str(&format!(" (paused: {why})"));
        }
        lines.push(line);
    }
    lines
}

/// Writes `head` and then each line of `lines` indented on `stderr`.
fn print(stderr: &mut dyn Write, head: &str, lines: &[String]) {
    // A stderr that cannot be written to does not stop the wait.
    let _ = writeln!(stderr, "{PROGRAM}: releases wait: {head}");
    for line in lines {
        let _ = writeln!(stderr, "  {line}");
    }
}

/// The wait of `plan` against `fleet`: see the module's order.
pub(crate) fn wait_for(fleet: &mut dyn Fleet, plan: &Plan<'_>) -> Flow<Ended> {
    let what = match plan.host_group {
        Some(group) => format!("{} {} in {group}", plan.service, plan.version),
        None => format!("{} {}", plan.service, plan.version),
    };
    let start = fleet.now();
    let mut seen = Seen::default();
    let mut shown: Option<Vec<String>> = None;
    let mut last: Option<(Vec<u8>, Vec<String>)> = None;
    let mut cut: Option<String> = None;
    loop {
        let left = plan.bound.saturating_sub(fleet.now().saturating_sub(start));
        let secs = u32::try_from(left.as_secs())
            .unwrap_or(u32::MAX)
            .min(SLICE_SECS);
        if secs == 0 {
            break;
        }
        match fleet.ask(secs)? {
            Asked::Answer(body) => {
                cut = None;
                let answer: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let lines = table(&answer, &mut seen);
                let elapsed = span(fleet.now().saturating_sub(start));
                if text(&answer, "outcome") != Some(DEADLINE) {
                    let outcome = words(text(&answer, "outcome"), "WAIT_OUTCOME_");
                    if outcome != "converged" {
                        print(
                            fleet.stderr(),
                            &format!("{what}: ended {outcome} after {elapsed}:"),
                            &lines,
                        );
                    }
                    return Ok(Ended::Answered(body));
                }
                if shown.as_ref() != Some(&lines) {
                    let bound = span(plan.bound);
                    print(
                        fleet.stderr(),
                        &format!("{what}: not converged after {elapsed} of {bound}:"),
                        &lines,
                    );
                    shown = Some(lines.clone());
                }
                last = Some((body, lines));
            }
            Asked::Refused(refused) => return Ok(Ended::Refused(refused)),
            Asked::Cut(why) => {
                let spent = fleet.now().saturating_sub(start);
                let elapsed = span(spent);
                let pause = RETRY.min(plan.bound.saturating_sub(spent));
                if pause.is_zero() {
                    let _ = writeln!(
                        fleet.stderr(),
                        "{PROGRAM}: releases wait: no answer after {elapsed} ({why})"
                    );
                } else {
                    let _ = writeln!(
                        fleet.stderr(),
                        "{PROGRAM}: releases wait: no answer after {elapsed} ({why}); asking \
                         again in {}s",
                        pause.as_secs()
                    );
                    fleet.pause(pause);
                }
                cut = Some(why);
            }
        }
    }
    let bound = span(plan.bound);
    match last {
        Some((body, lines)) => {
            let tail = cut.map_or_else(String::new, |why| {
                format!(" (the last call had no answer: {why})")
            });
            print(
                fleet.stderr(),
                &format!("{what}: did not converge within {bound}{tail}; it still waits on:"),
                &lines,
            );
            Ok(Ended::Answered(body))
        }
        None => Ok(Ended::NoAnswer(cut.unwrap_or_default())),
    }
}

/// The real [`Fleet`]: the coordinator's gateway, a token for each call, and the clock and the
/// wait of the world's [`crate::tailnet_wait::Tailnet`].
pub(crate) struct System<'s, 'w> {
    pub(crate) w: &'s mut World<'w>,
    pub(crate) globals: &'s Globals,
    pub(crate) endpoint: String,
    pub(crate) roots: Vec<Certificate<'static>>,
    /// The request without its `waitSecs`.
    pub(crate) request: Value,
}

/// A call that stopped before its answer: a failure (no token, no answer, an answer that is not
/// the gateway's) is a cut, asked again; any other stop (a usage error) ends the wait.
fn cut(stop: Stop) -> Flow<Asked> {
    match stop {
        Stop::Fail(why) => Ok(Asked::Cut(String::from_utf8_lossy(&why).into_owned())),
        stop => Err(stop),
    }
}

impl Fleet for System<'_, '_> {
    fn ask(&mut self, secs: u32) -> Flow<Asked> {
        let token = match credential(self.w, self.globals) {
            Ok(token) => token,
            Err(stop) => return cut(stop),
        };
        let mut request = self.request.clone();
        request["waitSecs"] = json!(secs);
        let body = request.to_string();
        let timeout = u64::from(self.globals.timeout_secs());
        let limits = Limits {
            connect: Duration::from_secs(timeout),
            total: Duration::from_secs(timeout + u64::from(secs)),
        };
        match post(
            &self.endpoint,
            PATH,
            body.as_bytes(),
            &token,
            &self.roots,
            limits,
        ) {
            Ok(Ok(answer)) => Ok(Asked::Answer(answer)),
            Ok(Err(refused)) => Ok(Asked::Refused(refused)),
            Err(stop) => cut(stop),
        }
    }

    fn now(&mut self) -> Duration {
        (self.w.tailnet.now)()
    }

    fn pause(&mut self, time: Duration) {
        (self.w.tailnet.wait)(time);
    }

    fn stderr(&mut self) -> &mut dyn Write {
        &mut *self.w.stderr
    }
}

/// The help of `hk releases wait`.
pub(crate) const HELP: &str = "\
hk releases wait: waits for the fleet to converge on a version, in held calls of
Releases.WaitConverged of at most 60 seconds each, and says what it waits on.

Usage: hk [global flags] releases wait --service NAME --version VERSION --wait-secs N
       [--host-group GROUP]

Each call is POST /hostkeeper.operator.v1.Releases/WaitConverged with a token of its own and
`waitSecs` the time left, at most 60. An answer DEADLINE is not yet: the next call is made, and
when what it waits on changed, it is printed on stderr: each host not settled on the version,
its state (updating, failed, excluded, silent, not ordered yet, settling) and since when, and
why each plan waits. Any other outcome ends the wait with that answer. A call with no answer
(a cut connection, no token, an answer that is not the gateway's) is said on stderr and asked
again 10 seconds later; a refusal ends the wait as the generated command's does. When N seconds
have passed, the last answer is the answer and what it still waits on is printed; with no
answer at all, hk exits 1.

The answer is written as `releases wait-converged` writes it, to stdout or --response-file;
the exit code of an answer is 0.

Flags:
  --service NAME       The service.
  --version VERSION    The version waited for; another desired version ends the wait.
  --wait-secs N        How long to wait in all, 1 to 1800 seconds.
  --host-group GROUP   The host group; none: every group that lists the service.

Run 'hk --help' for the global flags and the exit codes.";

/// `hk releases wait`: `args` are the words after `wait`, among which the global flags are
/// taken; `globals` are those given before it.
pub(crate) fn releases_wait(w: &mut World<'_>, mut globals: Globals, args: &[Vec<u8>]) -> Flow<()> {
    let mut service: Option<String> = None;
    let mut version: Option<String> = None;
    let mut wait: Option<String> = None;
    let mut host_group: Option<String> = None;
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        let rest = args.get(at..).unwrap_or_default();
        let flag = flag_of(arg);
        let slot = match flag {
            b"--help" | b"-h" => {
                let _ = writeln!(w.stdout, "{HELP}");
                return Ok(());
            }
            b"--service" => &mut service,
            b"--version" => &mut version,
            b"--wait-secs" => &mut wait,
            b"--host-group" => &mut host_group,
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
                    [format!("{PROGRAM} releases wait ").as_bytes(), what, flag].concat(),
                ));
            }
        };
        let (value, took) = value_of(rest)?;
        if slot.is_some() {
            return Err(Stop::usage([flag, b" is given twice"].concat()));
        }
        *slot = Some(String::from_utf8_lossy(value).into_owned());
        at += took;
    }
    let (Some(service), Some(version), Some(wait)) = (service, version, wait) else {
        return Err(Stop::usage(format!(
            "{PROGRAM} releases wait needs --service, --version and --wait-secs"
        )));
    };
    let digits = !wait.is_empty() && wait.bytes().all(|b| b.is_ascii_digit());
    let secs = wait
        .parse::<u32>()
        .ok()
        .filter(|secs| digits && (1..=MAX_WAIT_SECS).contains(secs));
    let Some(secs) = secs else {
        if digits {
            // A number out of the bound is refused as the coordinator refused a held call of it,
            // so that a caller reading `refused:` lines reads the same token.
            let _ = writeln!(w.stderr, "refused: invalid_wait");
        }
        return Err(Stop::usage(format!(
            "--wait-secs takes a whole number from 1 to {MAX_WAIT_SECS}"
        )));
    };
    crate::settings::fill(w, &mut globals)?;
    let (endpoint, ca) = target_of(&globals)?;
    if globals.response_file.as_ref().is_some_and(Vec::is_empty) {
        return Err(Stop::usage("--response-file needs a path"));
    }
    let mut request = json!({ "service": service, "version": version });
    let host_group = host_group.filter(|group| !group.is_empty());
    if let Some(group) = &host_group {
        request["hostGroup"] = json!(group);
    }
    let plan = Plan {
        service: &service,
        version: &version,
        host_group: host_group.as_deref(),
        bound: Duration::from_secs(u64::from(secs)),
    };
    let roots = certificates(&ca);
    let ended = wait_for(
        &mut System {
            w,
            globals: &globals,
            endpoint: endpoint.clone(),
            roots: roots.clone(),
            request,
        },
        &plan,
    )?;
    match ended {
        Ended::Answered(body) => deliver(w, &body, globals.response_file.as_deref()),
        Ended::Refused(refused) => Err(said_refused(
            w,
            &endpoint,
            &roots,
            globals.timeout_secs(),
            refused,
        )),
        Ended::NoAnswer(why) => Err(Stop::fail(format!(
            "no answer from {endpoint} in {}: the last call: {why}",
            span(plan.bound)
        ))),
    }
}
