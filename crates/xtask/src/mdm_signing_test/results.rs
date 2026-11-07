//! `results.toml`, beside the kit: one entry per command `send --apply` queued, in order, each
//! with what the operator recorded for it (`record`), and the report of the run (`report`).
//!
//! `InstallEnterpriseApplication` is answered `Acknowledged` before the Mac downloads the pkg,
//! and errors of the install are not reported to the server (design section 7.2), so a pkg's
//! outcome is read on the Mac: whether it holds the pkg's receipt. [`verdict`] reads the run as
//! section 7.2 does, and names a run that cannot decide as inconclusive rather than reading
//! it as a refusal of the signature.
//!
//! The order of the run is enforced at the send: pkg 3 and pkg 4 are sent only once the root's
//! profile was recorded `Acknowledged` and has not been removed since ([`Results::may_send`]).

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Error, PKGS, Step};

/// The file of the run's sends, beside `kit.toml`.
pub(crate) const RESULTS_FILE: &str = "results.toml";

/// The device's answer to a command, as NanoHUB logs it (`status=` of the line with the
/// command's UUID).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
pub(crate) enum Answer {
    #[value(name = "Acknowledged")]
    Acknowledged,
    #[value(name = "Error")]
    Error,
    #[value(name = "CommandFormatError")]
    CommandFormatError,
    #[value(name = "NotNow")]
    NotNow,
    /// No line yet: the Mac has not answered.
    #[value(name = "none")]
    #[serde(rename = "none")]
    None,
}

impl Answer {
    /// The answer as the `--answer` flag and NanoHUB's log name it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Answer::Acknowledged => "Acknowledged",
            Answer::Error => "Error",
            Answer::CommandFormatError => "CommandFormatError",
            Answer::NotNow => "NotNow",
            Answer::None => "none",
        }
    }
}

/// What the operator recorded for one send.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Outcome {
    pub(crate) answer: Answer,
    /// For a pkg: the Mac holds its receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) installed: Option<bool>,
    /// For pkg 3: `pkgutil --check-signature` on the Mac reads its signature as trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) trusted: Option<bool>,
    /// The install log's line about the pkg.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
}

/// One queued command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QueuedSend {
    pub(crate) step: Step,
    pub(crate) command_uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) push_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) outcome: Option<Outcome>,
}

/// Every send of the run, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Results {
    #[serde(default, rename = "send")]
    pub(crate) sends: Vec<QueuedSend>,
}

impl Results {
    /// The run of the kit in `dir`; none sent when the file is not there yet.
    pub(crate) fn read(dir: &Path) -> Result<Results, Error> {
        let path = dir.join(RESULTS_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Results::default()),
            Err(e) => return Err(Error::io(format!("read {}", path.display()))(e)),
        };
        toml::from_str(&text)
            .map_err(|e| Error::Refused(format!("{}: {}", path.display(), e.message())))
    }

    /// Writes the run to `dir`, replacing the file whole.
    pub(crate) fn write(&self, dir: &Path) -> Result<(), Error> {
        let path = dir.join(RESULTS_FILE);
        let partial = dir.join(format!("{RESULTS_FILE}.partial"));
        let text = toml::to_string(self)
            .map_err(|e| Error::Refused(format!("write {}: {e}", path.display())))?;
        std::fs::write(&partial, text)
            .map_err(Error::io(format!("write {}", partial.display())))?;
        std::fs::rename(&partial, &path).map_err(Error::io(format!("write {}", path.display())))
    }

    /// The number of sends of `step` so far.
    pub(crate) fn sends_of(&self, step: Step) -> usize {
        self.sends.iter().filter(|s| s.step == step).count()
    }

    /// The last send of `step`.
    fn last(&self, step: Step) -> Option<&QueuedSend> {
        self.sends.iter().rev().find(|s| s.step == step)
    }

    /// Refuses `step` when the run is not ready for it: pkg 3 and pkg 4 need the root's profile
    /// recorded `Acknowledged`, with no removal sent after it.
    pub(crate) fn may_send(&self, step: Step) -> Result<(), Error> {
        if !matches!(step, Step::Pkg3 | Step::Pkg4) {
            return Ok(());
        }
        let installed_at = self.sends.iter().rposition(|s| {
            s.step == Step::RootProfile
                && s.outcome
                    .as_ref()
                    .is_some_and(|o| o.answer == Answer::Acknowledged)
        });
        let removed_at = self
            .sends
            .iter()
            .rposition(|s| s.step == Step::RemoveRootProfile);
        match (installed_at, removed_at) {
            (Some(i), Some(r)) if r < i => Ok(()),
            (Some(_), None) => Ok(()),
            _ => Err(Error::Refused(format!(
                "{} needs the root's profile on the Mac: send root-profile and record it \
                 Acknowledged first, with no remove-root-profile after it",
                step.name()
            ))),
        }
    }

    /// Adds a send of `step`.
    pub(crate) fn add_send(&mut self, step: Step, uuid: &str, push_error: Option<String>) {
        self.sends.push(QueuedSend {
            step,
            command_uuid: uuid.to_owned(),
            push_error,
            outcome: None,
        });
    }
}

/// Records `outcome` for the last send of `step` of the kit in `dir`, replacing what was
/// recorded for it before.
pub(crate) fn record(dir: &Path, step: Step, outcome: Outcome) -> Result<Vec<String>, Error> {
    let is_pkg = step.pkg().is_some();
    if is_pkg && outcome.installed.is_none() {
        return Err(Error::Refused(format!(
            "{} needs --installed: whether the Mac holds the pkg's receipt",
            step.name()
        )));
    }
    if !is_pkg && outcome.installed.is_some() {
        return Err(Error::Refused(format!(
            "{} installs no pkg: --installed is for pkg-1 to pkg-4",
            step.name()
        )));
    }
    if step != Step::Pkg3 && outcome.trusted.is_some() {
        return Err(Error::Refused(
            "--trusted is for pkg-3 alone: the Mac's reading of its chain with the root installed"
                .to_owned(),
        ));
    }
    let mut results = Results::read(dir)?;
    let send = results
        .sends
        .iter_mut()
        .rev()
        .find(|s| s.step == step)
        .ok_or_else(|| {
            Error::Refused(format!(
                "{} has not been sent: `send --apply` it first",
                step.name()
            ))
        })?;
    let uuid = send.command_uuid.clone();
    send.outcome = Some(outcome);
    results.write(dir)?;
    Ok(vec![format!("recorded {} (command {uuid})", step.name())])
}

/// The table of the run of the kit in `dir` and its verdict.
pub(crate) fn report(dir: &Path) -> Result<Vec<String>, Error> {
    let results = Results::read(dir)?;
    let mut lines = vec![
        "| Step | Command | Answer | Installed | Note |".to_owned(),
        "|---|---|---|---|---|".to_owned(),
    ];
    for send in &results.sends {
        let outcome = send.outcome.as_ref();
        let answer = outcome.map_or("not recorded", |o| o.answer.name());
        let installed = match outcome.and_then(|o| o.installed) {
            Some(true) => "yes",
            Some(false) => "no",
            None => "",
        };
        let mut note = outcome.and_then(|o| o.note.clone()).unwrap_or_default();
        if let Some(trusted) = outcome.and_then(|o| o.trusted) {
            let read = if trusted { "trusted" } else { "not trusted" };
            note = format!("chain {read} on the Mac. {note}")
                .trim_end()
                .to_owned();
        }
        if let Some(why) = &send.push_error {
            note = format!("push failed: {why}. {note}").trim_end().to_owned();
        }
        lines.push(format!(
            "| {} | {} | {answer} | {installed} | {} |",
            send.step.name(),
            send.command_uuid,
            note.replace('|', "/")
        ));
    }
    lines.push(String::new());
    lines.extend(verdict(&results));
    lines.extend(cleanup(&results));
    Ok(lines)
}

/// What the run decides, as design section 7.2 reads it, from the last recorded send of each
/// pkg: one line for the identity, then a line for each unsigned pkg that installed.
pub(crate) fn verdict(results: &Results) -> Vec<String> {
    let mut outcomes = Vec::new();
    for spec in &PKGS {
        match results.last(spec.step).and_then(|s| s.outcome.as_ref()) {
            Some(outcome) => outcomes.push(outcome),
            None => {
                return vec![format!(
                    "Verdict: incomplete: {} has no recorded outcome",
                    spec.step.name()
                )];
            }
        }
    }
    if let Some((spec, o)) = PKGS
        .iter()
        .zip(&outcomes)
        .find(|(_, o)| o.answer != Answer::Acknowledged)
    {
        return vec![format!(
            "Verdict: inconclusive: the Mac answered {} pkg {} with {}: the command, not the \
             signature, was refused; read its answer and send it again",
            spec.step.name(),
            spec.number,
            o.answer.name()
        )];
    }
    // By the pkg's number, 1 to 4.
    let installed = |number: usize| outcomes[number - 1].installed == Some(true);
    let mut lines = vec![if installed(4) {
        "Verdict: inconclusive: pkg 4 installed although its root is installed nowhere, so the \
         Mac did not judge the chain and pkg 3 proves nothing"
            .to_owned()
    } else if installed(3) {
        "Verdict: farm identity: pkg 3 installs and pkg 4 does not; the root's payload is what \
         makes it pass. Task M15 builds `hostkeeper-mdm pkg add`"
            .to_owned()
    } else if outcomes[2].trusted == Some(true) {
        "Verdict: Developer ID Installer: the Mac trusts pkg 3's chain and still does not install \
         it. Task M15 builds the Developer ID job; question Q4 asks for the identity"
            .to_owned()
    } else {
        "Verdict: inconclusive: pkg 3 did not install and the Mac was not shown to trust its \
         chain with the root installed (record --trusted from pkgutil --check-signature); the \
         identity, not the MDM, may be what is refused"
            .to_owned()
    }];
    for spec in &PKGS[..2] {
        if installed(usize::from(spec.number)) {
            lines.push(format!(
                "Also: pkg {}, an {}, installed: this Mac installs an unsigned pkg by MDM",
                spec.number, spec.what
            ));
        }
    }
    lines
}

/// Whether the run leaves root A trusted on the Mac: a line when the root's profile was
/// recorded `Acknowledged` and no `remove-root-profile` after it was.
pub(crate) fn cleanup(results: &Results) -> Option<String> {
    let acknowledged = |s: &QueuedSend| {
        s.outcome
            .as_ref()
            .is_some_and(|o| o.answer == Answer::Acknowledged)
    };
    let installed = results
        .sends
        .iter()
        .rposition(|s| s.step == Step::RootProfile && acknowledged(s))?;
    let removed = results.sends[installed..]
        .iter()
        .any(|s| s.step == Step::RemoveRootProfile && acknowledged(s));
    (!removed).then(|| {
        "Cleanup: root A's profile is still on the Mac: send remove-root-profile and record it \
         Acknowledged"
            .to_owned()
    })
}
