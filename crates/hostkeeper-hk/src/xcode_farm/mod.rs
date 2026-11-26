//! The check of an Xcode archive on a farm Mac, for `hk xcode add` and `hk xcode verify` on a
//! system that has none of Apple's programs (`pkgutil`, `xip`, `codesign`): which Mac, and the
//! check itself, which is a maintenance run of the kind `probe` that names the archive in the
//! coordinator's Xcode store (`Maintenance.Schedule`'s `xcode_sha256`).
//!
//! [`mac_of`] says which Mac: the verb's `--mac`, else the settings file's `xcode_mac`, both a
//! node id, and a usage error when neither is given off macOS, or when `--mac` is given on macOS,
//! where the checks run on the Mac itself. [`check`] then, in order:
//!
//! 1. `Maintenance.Schedule` of the kinds `[probe]` on that one host, `take_last_host` and
//!    `confirm`, with the archive's SHA-256 and a reason that names the release and the first 12
//!    hex digits of the SHA-256. A refusal is said in words with the coordinator's code and ends
//!    the run with the exit the gateway's call path gives it. A run whose `xcode_sha256` is not
//!    the archive's is a plain probe scheduled by a coordinator that cannot order an archive
//!    check: it is cancelled and the run fails;
//! 2. stderr says the run's id, the Mac and the command that stops the run;
//! 3. `Maintenance.Get` every [`POLL_SECS`] seconds, through the world's wait
//!    ([`crate::World::tailnet`]), until the Mac's turn has ended (done, deferred, halted, not back,
//!    not reached, skipped) or the run is done, cancelled or past its window; each step the turn
//!    reaches is said once on stderr. There is no limit of time: the Mac's turn ends by itself, and
//!    the command of step 2 stops it;
//! 4. what the Mac says of the archive. A turn that is done is read from the probe the Mac reports
//!    (`Hosts.Get`): it must be this run's and have the step `verdict`, whose exit code is 0 with
//!    the output `pass` for a pass, 1 for an archive failure (the output is `<reason>: <detail>`),
//!    and none when the check did not finish. A turn that is deferred is cancelled, so that the Mac
//!    is not offered it again, and is an archive failure when the Mac's reason is
//!    `xcode_archive_mismatch` or `xcode_archive_encrypted` (the token after `exit 75, `), and
//!    not checked for any other. A turn that is halted, not back, not reached or skipped, and a
//!    run that was cancelled or ended, are not checked;
//! 5. when the Mac gave a verdict, the record ([`record`]) is written to
//!    `~/.hostkeeper/xcode/<sha256>.json`.
//!
//! The result is [`Outcome::Verdict`], or [`Outcome::Unchecked`] with the words that say why not
//! (the Mac gave no verdict; nothing is written), or a [`Stop`] for a refusal or a call that got
//! no answer. The token is the bearer of each call and is in no output.

mod read;
mod record;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use crate::args::Globals;
use crate::call::{Refused, is_reason};
use crate::home::Home;
use crate::xcode_push::say;
use crate::xcode_record::{self, Archive};
use crate::{Flow, PROGRAM, Stop, World};

pub(crate) use self::read::remove;

/// The seconds between two reads of the run.
pub(crate) const POLL_SECS: u64 = 15;

/// The longest reason the coordinator takes, in bytes.
const MAX_REASON_BYTES: usize = 256;

/// What `exit 75, ` precedes in the reason of a turn a Mac deferred: the daemon's own word for the
/// failure of the stage.
const AFTER_EXIT: &str = "exit 75, ";

/// The two failures of the archive that a Mac's stage reports by deferring its turn.
const ARCHIVE_FAILURES: [&str; 2] = ["xcode_archive_mismatch", "xcode_archive_encrypted"];

/// The archive to check, and where.
pub(crate) struct Subject<'a> {
    /// The verb that asks, `xcode add` or `xcode verify`: the reason of the run names it.
    pub(crate) verb: &'a str,
    /// The farm Mac's node id.
    pub(crate) mac: &'a str,
    /// The local file: its name, size, SHA-256 and kind, which the record holds.
    pub(crate) archive: &'a Archive,
    /// The release the archive is: its version and build, which the reason of the run names.
    pub(crate) version: &'a str,
    pub(crate) build: &'a str,
}

/// What a check came to.
pub(crate) enum Outcome {
    /// The Mac gave a verdict, and the record of it is written.
    Verdict(Verdict),
    /// The Mac gave none: why, in words. Nothing is written.
    Unchecked(String),
}

/// A farm Mac's verdict on the archive.
pub(crate) struct Verdict {
    /// The run the Mac gave it in.
    pub(crate) run: String,
    /// Whether the archive passed.
    pub(crate) pass: bool,
    /// What failed, as the token the Mac named it with (`xcode_signature_invalid`); empty for a
    /// pass.
    pub(crate) reason: String,
    /// The record's verdict: `pass`, or `fail: <what the Mac said>`.
    pub(crate) text: String,
    /// The record, as the JSON that is written and printed.
    pub(crate) json: String,
    /// Where the record is kept.
    pub(crate) path: PathBuf,
}

/// What the Mac said of the archive, before the record is made of it.
struct Found {
    /// The Mac's probe; none for a turn that was deferred.
    probe: Option<read::Probe>,
    pass: bool,
    reason: String,
    verdict: String,
    /// The reason of a deferred turn, as the Mac worded it; empty otherwise.
    deferral: String,
}

/// How a Mac's turn ended, as far as the coordinator says.
enum Ended {
    Done,
    Deferred(String),
    Halted(String),
    Unchecked(String),
}

/// `text` as one line the coordinator takes as a reason: no control character, at most
/// [`MAX_REASON_BYTES`] bytes, trimmed.
fn clean(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_REASON_BYTES {
            break;
        }
        out.push(c);
    }
    out.trim().to_owned()
}

/// `text` as one word of a command line that is typed in a shell: as it is when it is plain, else
/// in single quotes.
pub(crate) fn shell_word(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+'));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// The farm Mac the verb `word` checks on: `None` on macOS, where the checks run on this Mac and
/// `--mac` is a usage error; off macOS the node id of `flag` (the verb's `--mac`), else of the
/// settings file's `xcode_mac`, which also fills the global flags `globals` lack. Neither is a
/// usage error, and so is an id that is not a host id.
pub(crate) fn mac_of(
    w: &mut World<'_>,
    globals: &mut Globals,
    word: &str,
    mut flag: Option<Vec<u8>>,
) -> Flow<Option<String>> {
    if w.mac.os == "macos" {
        if flag.is_some() {
            return Err(Stop::usage(format!(
                "{PROGRAM} {word} runs its checks on this Mac, so it takes no --mac: --mac names \
                 the farm Mac that checks the archive for a system without macOS's programs"
            )));
        }
        return Ok(None);
    }
    let from_flag = flag.is_some();
    crate::settings::fill_mac(w, globals, &mut flag)?;
    let Some(id) = flag else {
        return Err(Stop::usage(format!(
            "{PROGRAM} {word} checks the archive on a farm Mac, because this system is {}, which \
             has no pkgutil, xip or codesign of Apple's: name the Mac with --mac <node-id>, or \
             with xcode_mac in ~/.hostkeeper/hk.toml",
            w.mac.os
        )));
    };
    let id = String::from_utf8_lossy(&id).into_owned();
    crate::ssh::check_host_id(&id).map_err(|error| {
        let source = if from_flag {
            "--mac"
        } else {
            "xcode_mac of ~/.hostkeeper/hk.toml"
        };
        Stop::usage(format!("{source}: {error}"))
    })?;
    Ok(Some(id))
}

/// The line that cancels the run `run`.
fn cancel_line(run: &str) -> String {
    format!("{PROGRAM} maintenance cancel --run-id {run} --reason <why>")
}

/// The reason of the run that checks `subject`: the release and the first 12 hex digits of the
/// archive's SHA-256.
fn reason_of(subject: &Subject<'_>) -> String {
    let digits: String = subject.archive.sha256.chars().take(12).collect();
    clean(&format!(
        "{PROGRAM} {}: check Xcode {} ({}), sha256 {digits}",
        subject.verb, subject.version, subject.build
    ))
}

/// What a refusal of `Maintenance.Schedule` means for `mac`, said in words; `None` for a reason
/// this does not know.
fn refusal_words(reason: &[u8], mac: &str) -> Option<String> {
    let text = match reason {
        b"nothing_due" => format!(
            "{mac} cannot check the archive: it must be a Mac whose hostkeeper lists probe on \
             demand, in a host group that keeps Xcode (xcode_keep 1 or more), so that it can read \
             the store"
        ),
        b"host_in_maintenance" => format!(
            "{mac} is in another maintenance run ({PROGRAM} maintenance list shows the open \
             ones); run this again when it ends"
        ),
        b"unknown_host" => format!(
            "the coordinator has not enrolled a host {mac}; {PROGRAM} hosts list names the hosts"
        ),
        b"invalid_request" => "the coordinator holds no archive of these bytes, or refused the \
                              request: this coordinator may be older than this hk, which asks it \
                              for a check of one archive"
            .to_owned(),
        _ => return None,
    };
    Some(text)
}

/// The stop for a refusal of `Maintenance.Schedule`: the words, the coordinator's code, the reason
/// when it is a token, and the exit the gateway's call path gives the refusal.
fn refused(w: &mut World<'_>, mac: &str, refusal: &Refused) -> Stop {
    if let Some(words) = refusal_words(&refusal.reason, mac) {
        say(w, &words);
    }
    let _ = writeln!(
        w.stderr,
        "{PROGRAM}: the coordinator answered {}",
        String::from_utf8_lossy(&refusal.code)
    );
    if is_reason(&refusal.reason) {
        let _ = writeln!(
            w.stderr,
            "refused: {}",
            String::from_utf8_lossy(&refusal.reason)
        );
    }
    Stop::Exit(refusal.status)
}

/// Schedules the run that checks `subject`; its id. The stops: the refusal, a coordinator that
/// scheduled a run that does not name the archive (cancelled here), no run.
fn schedule(w: &mut World<'_>, globals: &Globals, subject: &Subject<'_>) -> Flow<String> {
    let body = json!({
        "kinds": ["MAINTENANCE_KIND_PROBE"],
        "hostIds": [subject.mac],
        "reason": reason_of(subject),
        "takeLastHost": true,
        "confirm": true,
        "xcodeSha256": subject.archive.sha256,
    });
    let answered = match read::call::<read::Scheduled>(
        w,
        globals,
        "Maintenance.Schedule",
        read::SCHEDULE,
        &body,
    )? {
        Ok(answered) => answered,
        Err(refusal) => return Err(refused(w, subject.mac, &refusal)),
    };
    let run = answered.run;
    if run.id.is_empty() {
        return Err(Stop::fail(
            "the answer of Maintenance.Schedule names no run; nothing was checked",
        ));
    }
    if run.xcode_sha256 != subject.archive.sha256 {
        let cancelled = read::cancel(
            w,
            globals,
            &run.id,
            &clean(&format!(
                "{PROGRAM} {}: this coordinator cannot order an archive check",
                subject.verb
            )),
        );
        let ended = match cancelled {
            Ok(()) => "it is cancelled".to_owned(),
            Err(why) => format!(
                "it could not be cancelled ({why}); stop it with {}",
                cancel_line(&run.id)
            ),
        };
        return Err(Stop::fail(format!(
            "this coordinator cannot order an archive check: its run {} on {} does not name the \
             archive, so the Mac makes its ordinary reads; {ended}; nothing was checked",
            run.id, subject.mac
        )));
    }
    Ok(run.id)
}

/// What a step of a turn that has not ended is said as, once.
const STEP_WORDS: [(&str, &str); 7] = [
    ("MAINTENANCE_HOST_STEP_WAITING", "waiting"),
    (
        "MAINTENANCE_HOST_STEP_STAGING",
        "staging: fetching the archive from the store",
    ),
    ("MAINTENANCE_HOST_STEP_DRAINING", "draining"),
    (
        "MAINTENANCE_HOST_STEP_APPLYING",
        "applying: checking the archive",
    ),
    ("MAINTENANCE_HOST_STEP_REBOOTING", "rebooting"),
    ("MAINTENANCE_HOST_STEP_CHECKING", "checking"),
    ("MAINTENANCE_HOST_STEP_PUTTING_BACK", "putting back"),
];

/// A skip's name as the gateway writes it, in lower case without its prefix: `mdm_conflict`.
fn skip_token(name: &str) -> String {
    name.strip_prefix("MAINTENANCE_SKIP_")
        .unwrap_or(name)
        .to_ascii_lowercase()
}

/// How the turn `turn` of the run `run` ended, when it has, as the coordinator says it.
fn ended_of(mac: &str, turn: &read::Turn, run: &read::Run) -> Option<Ended> {
    let ended = match turn.step.as_str() {
        "MAINTENANCE_HOST_STEP_DONE" => Ended::Done,
        "MAINTENANCE_HOST_STEP_DEFERRED" => Ended::Deferred(turn.reason.clone()),
        "MAINTENANCE_HOST_STEP_HALTED" => Ended::Halted(turn.reason.clone()),
        "MAINTENANCE_HOST_STEP_NOT_BACK" => Ended::Unchecked(format!(
            "{mac} did not report back, so its turn of run {} has not ended",
            run.id
        )),
        "MAINTENANCE_HOST_STEP_NOT_REACHED" => {
            Ended::Unchecked(format!("run {} ended before {mac}'s turn started", run.id))
        }
        "MAINTENANCE_HOST_STEP_SKIPPED" => Ended::Unchecked(format!(
            "run {} skipped {mac}: {}",
            run.id,
            skip_token(&turn.skipped_because)
        )),
        _ => match run.state.as_str() {
            read::RUN_DONE => Ended::Unchecked(format!(
                "run {} is done, with {mac}'s turn at {}",
                run.id, turn.step
            )),
            read::RUN_CANCELLED => Ended::Unchecked(format!("run {} was cancelled", run.id)),
            read::RUN_WINDOW_ENDED => Ended::Unchecked(format!(
                "the window of run {} ended before {mac}'s turn did",
                run.id
            )),
            _ => return None,
        },
    };
    Some(ended)
}

/// Reads the run `run` until the Mac's turn of it has ended, waiting [`POLL_SECS`] seconds between
/// two reads and saying each step the turn reaches once.
fn watch(w: &mut World<'_>, globals: &Globals, subject: &Subject<'_>, run: &str) -> Ended {
    let mac = subject.mac;
    let mut said = String::new();
    loop {
        let got = match read::call::<read::Got>(
            w,
            globals,
            "Maintenance.Get",
            read::GET,
            &json!({"runId": run}),
        ) {
            Ok(Ok(got)) => got,
            Ok(Err(refusal)) => {
                return Ended::Unchecked(format!(
                    "Maintenance.Get of run {run} was refused: {}; the run goes on",
                    read::said(&refusal)
                ));
            }
            Err(stop) => {
                return Ended::Unchecked(format!("{}; run {run} goes on", read::why(&stop)));
            }
        };
        let Some(turn) = got.hosts.iter().find(|turn| turn.host_id == mac) else {
            return Ended::Unchecked(format!("run {run} holds no turn of {mac}"));
        };
        if turn.step != said {
            if let Some((_, words)) = STEP_WORDS.iter().find(|(step, _)| *step == turn.step) {
                say(w, &format!("{mac}: {words}"));
            }
            said.clone_from(&turn.step);
        }
        if let Some(ended) = ended_of(mac, turn, &got.run) {
            return ended;
        }
        (w.tailnet.wait)(Duration::from_secs(POLL_SECS));
    }
}

/// The token and the detail of the failure of the archive that a deferred turn's `reason` names:
/// `stage: exit 75, xcode_archive_mismatch: <detail>`, the token being what follows `exit 75, ` up
/// to a `:` or a space; `None` for any other reason.
fn archive_failure(reason: &str) -> Option<(String, String)> {
    let (_, rest) = reason.split_once(AFTER_EXIT)?;
    let token = rest.split([':', ' ']).next().unwrap_or_default();
    if !ARCHIVE_FAILURES.contains(&token) {
        return None;
    }
    let detail = rest
        .get(token.len()..)
        .and_then(|tail| tail.strip_prefix(": "))
        .unwrap_or_default();
    Some((token.to_owned(), detail.to_owned()))
}

/// A turn that is deferred: the run is cancelled, so that the Mac is not offered the turn again,
/// and the Mac's reason is the verdict when it names the archive, else why it was not checked.
fn deferred(
    w: &mut World<'_>,
    globals: &Globals,
    subject: &Subject<'_>,
    run: &str,
    reason: &str,
) -> Result<Found, String> {
    let why = clean(&format!(
        "{PROGRAM} {}: {} deferred its turn",
        subject.verb, subject.mac
    ));
    if let Err(error) = read::cancel(w, globals, run, &why) {
        say(
            w,
            &format!(
                "{error}; stop run {run} with {}, or {} is offered the turn again",
                cancel_line(run),
                subject.mac
            ),
        );
    }
    let Some((token, detail)) = archive_failure(reason) else {
        return Err(format!(
            "{} deferred its turn of run {run} (the archive was not checked): {reason}",
            subject.mac
        ));
    };
    let verdict = if detail.is_empty() {
        format!("fail: {token}")
    } else {
        format!("fail: {token}: {detail}")
    };
    Ok(Found {
        probe: None,
        pass: false,
        reason: token,
        verdict,
        deferral: reason.to_owned(),
    })
}

/// A turn that is halted: not checked, and the Mac stays drained until an operator acts, which
/// the lines say how.
fn halted(w: &mut World<'_>, subject: &Subject<'_>, run: &str, reason: &str) -> String {
    let mac = subject.mac;
    say(
        w,
        &format!(
            "{mac} stays drained until an operator acts: {PROGRAM} maintenance retry-host \
             --run-id {run} --host-id {mac} --reason <why> tries its turn again from the step that \
             failed, and {PROGRAM} maintenance put-back --run-id {run} --host-id {mac} --reason \
             <why> puts it back in service"
        ),
    );
    format!("{mac}'s turn of run {run} halted: {reason}")
}

/// What the probe `probe` of the run `run` says of the archive: the verdict step's exit code and
/// output. The words say why when it says none.
fn judged(probe: read::Probe, run: &str, mac: &str) -> Result<Found, String> {
    let Some(step) = probe.step("verdict").filter(|_| probe.run_id == run) else {
        return Err(format!(
            "{mac}'s probe is not an archive check of run {run}: its hostkeeper made its reads, \
             which means it is older than this hk, or the store no longer held the archive when \
             its turn started"
        ));
    };
    let output = step.output.clone();
    let (pass, reason, verdict) = match step.exit_code {
        Some(0) if output == "pass" => (true, String::new(), "pass".to_owned()),
        Some(1) => {
            let token = output.split_once(": ").map_or(output.as_str(), |(t, _)| t);
            (false, token.to_owned(), format!("fail: {output}"))
        }
        None => {
            return Err(format!(
                "{mac}'s check did not finish (run {run}): {output}"
            ));
        }
        Some(code) => {
            return Err(format!(
                "{mac}'s verdict of run {run} is exit {code}, {output}, which this hk does not read"
            ));
        }
    };
    Ok(Found {
        probe: Some(probe),
        pass,
        reason,
        verdict,
        deferral: String::new(),
    })
}

/// A turn that is done: the verdict of the probe the Mac reports (`Hosts.Get`).
fn probed(
    w: &mut World<'_>,
    globals: &Globals,
    subject: &Subject<'_>,
    run: &str,
) -> Result<Found, String> {
    let mac = subject.mac;
    let hosted = match read::call::<read::Hosted>(
        w,
        globals,
        "Hosts.Get",
        read::HOST,
        &json!({"nodeId": mac}),
    ) {
        Ok(Ok(hosted)) => hosted,
        Ok(Err(refusal)) => {
            return Err(format!(
                "Hosts.Get of {mac} was refused: {}",
                read::said(&refusal)
            ));
        }
        Err(stop) => return Err(read::why(&stop)),
    };
    let Some(probe) = hosted.into_probe() else {
        return Err(format!("{mac} reports no probe of run {run}"));
    };
    judged(probe, run, mac)
}

/// Checks the archive of `subject` on its farm Mac (the steps of this module's header), and writes
/// the record in `home` when the Mac gave a verdict.
pub(crate) fn check(
    w: &mut World<'_>,
    globals: &Globals,
    home: &Home,
    subject: &Subject<'_>,
) -> Flow<Outcome> {
    let run = schedule(w, globals, subject)?;
    say(
        w,
        &format!(
            "run {run} checks the archive on {}: the Mac is drained for it, and its turn ends by \
             itself",
            subject.mac
        ),
    );
    say(w, &format!("to stop it: {}", cancel_line(&run)));
    let found = match watch(w, globals, subject, &run) {
        Ended::Unchecked(why) => return Ok(Outcome::Unchecked(why)),
        Ended::Halted(reason) => return Ok(Outcome::Unchecked(halted(w, subject, &run, &reason))),
        Ended::Deferred(reason) => deferred(w, globals, subject, &run, &reason),
        Ended::Done => probed(w, globals, subject, &run),
    };
    let found = match found {
        Ok(found) => found,
        Err(why) => return Ok(Outcome::Unchecked(why)),
    };
    let rec = record::build(w.stamp, subject, &run, &found);
    let json = xcode_record::json(&rec)?;
    let path = xcode_record::write(home, &subject.archive.sha256, &json)?;
    Ok(Outcome::Verdict(Verdict {
        run,
        pass: found.pass,
        reason: found.reason,
        text: found.verdict,
        json,
        path,
    }))
}

/// The reason `hk xcode add` gives for the removal of an archive that failed the check of the run
/// `run` on `mac`; `why` is what failed.
pub(crate) fn removal_reason(mac: &str, run: &str, why: &str) -> String {
    clean(&format!(
        "{PROGRAM} xcode add: the check on {mac} (run {run}) failed: {why}"
    ))
}
