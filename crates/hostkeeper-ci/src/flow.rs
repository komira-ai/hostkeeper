//! The flow: set the version, then read the convergence until it ends.
//!
//! Every end is an [`Outcome`] with a reason token: `converged`, `not_waited`, a coordinator's
//! refusal token, or one of `no_answer`, `not_stored`, `superseded`, `halted`, `not_converged`.
//! Every failure prints one `::error::` line that says whether the version is set.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Write};

use rustls_pki_types::CertificateDer;

use crate::call::{Answer, call};
use crate::flags::{SetDesiredFlags, Settings, is_version, validate};
use crate::observe::Convergence;
use crate::ports::Ports;

/// The request file of `releases set-desired`, in the work directory.
pub(crate) const REQUEST_FILE: &str = "set-desired.json";

/// How the flow ended; these are the job's outputs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// The version that is set, or empty when none was.
    pub desired_version: String,
    pub converged: bool,
    pub reason: String,
}

impl Outcome {
    /// Whether the job succeeded: converged, or not waited for.
    pub(crate) fn succeeded(&self) -> bool {
        self.reason == "converged" || self.reason == "not_waited"
    }

    /// The text appended to the job's output file.
    fn outputs(&self) -> String {
        format!(
            "desired_version={}\nconverged={}\nreason={}\n",
            self.desired_version, self.converged, self.reason
        )
    }
}

/// Runs `hostkeeper ci set-desired` to its exit status, with the ports `ports` makes from the
/// root certificates of `--web-roots`. A usage error is printed on `stderr`.
///
/// This one function is generic over the ports; it has no branch of its own, so that every
/// branch of the flow is in a function compiled once, for `&mut dyn Ports`.
pub(crate) fn set_desired<P: Ports>(
    flags: &SetDesiredFlags,
    stderr: &mut dyn Write,
    ports: impl FnOnce(&[CertificateDer<'static>]) -> P,
) -> u8 {
    check(flags, stderr).map_or(2, |(settings, roots)| finish(&settings, &mut ports(&roots)))
}

/// The checked flags and the roots of `--web-roots`, or `None` after printing the usage error
/// on `stderr`.
fn check(
    flags: &SetDesiredFlags,
    stderr: &mut dyn Write,
) -> Option<(Settings, Vec<CertificateDer<'static>>)> {
    match validate(flags) {
        Ok(checked) => Some(checked),
        Err(e) => {
            drop(writeln!(stderr, "hostkeeper ci set-desired: {e}"));
            None
        }
    }
}

/// Runs the flow on checked `settings` and writes the job's outputs; the exit status.
fn finish(settings: &Settings, ports: &mut dyn Ports) -> u8 {
    let outcome = execute(settings, ports);
    if let Some(path) = &settings.github_output
        && let Err(e) = append(path, &outcome.outputs())
    {
        ports.say(&format!(
            "::error::cannot write the job outputs to {}: {e}",
            path.display()
        ));
        return 1;
    }
    u8::from(!outcome.succeeded())
}

/// Appends `text` to the file at `path`, made when it is missing.
fn append(path: &std::path::Path, text: &str) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(text.as_bytes())
}

/// Prints the `::error::` line `message` and returns the failed outcome.
fn failed(ports: &mut dyn Ports, desired_version: &str, reason: &str, message: &str) -> Outcome {
    ports.say(&format!("::error::{message}"));
    Outcome {
        desired_version: desired_version.to_owned(),
        converged: false,
        reason: reason.to_owned(),
    }
}

/// The flow, from the first token to the outcome.
pub(crate) fn execute(settings: &Settings, ports: &mut dyn Ports) -> Outcome {
    if let Err(outcome) = set_version(settings, ports) {
        return outcome;
    }
    ports.say(&format!(
        "The desired version of {} is {}",
        settings.service, settings.version
    ));
    if !settings.wait {
        return Outcome {
            desired_version: settings.version.clone(),
            converged: false,
            reason: "not_waited".to_owned(),
        };
    }
    poll(settings, ports)
}

/// `releases set-desired`: the version is set, or the outcome says why not.
fn set_version(settings: &Settings, ports: &mut dyn Ports) -> Result<(), Outcome> {
    let (service, version) = (&settings.service, &settings.version);
    let file = settings.work_dir.join(REQUEST_FILE);
    let request = serde_json::json!({ "desired": { "version": version } }).to_string();
    if let Err(e) = std::fs::write(&file, request) {
        return Err(failed(
            ports,
            "",
            "no_answer",
            &format!(
                "cannot write the request file {}: {e}; the version {version} of {service} is not set",
                file.display()
            ),
        ));
    }
    let rpc: Vec<OsString> = vec![
        "set-desired".into(),
        "--service".into(),
        service.into(),
        "--package-digest".into(),
        settings.package_digest.clone().into(),
        "--request".into(),
        file.into_os_string(),
    ];
    match call(ports, settings, rpc) {
        Answer::Reply(reply) => {
            let stored = reply["desired"]["version"].as_str();
            if stored == Some(version.as_str()) {
                return Ok(());
            }
            let kept = stored.filter(|stored| is_version(stored)).unwrap_or("");
            Err(failed(
                ports,
                kept,
                "not_stored",
                &format!(
                    "the coordinator did not store the version {version} of {service}; it is not set"
                ),
            ))
        }
        Answer::Refused(token) => Err(failed(
            ports,
            "",
            &token,
            &format!(
                "the coordinator refused the version {version} of {service}: {token}; it is not set"
            ),
        )),
        Answer::NotSent(why) => Err(failed(
            ports,
            "",
            "no_answer",
            &format!("{why}; the version {version} of {service} is not set"),
        )),
        Answer::Lost(why) => Err(failed(
            ports,
            "",
            "no_answer",
            &format!(
                "no answer to the request to set the version {version} of {service} ({why}); whether it is set is not known"
            ),
        )),
    }
}

/// `releases converge`, up to `max_polls` reads at a fixed pause.
fn poll(settings: &Settings, ports: &mut dyn Ports) -> Outcome {
    let (service, version) = (&settings.service, &settings.version);
    let mut last: Option<String> = None;
    for n in 1..=settings.max_polls {
        if n > 1 {
            ports.pause(settings.pause_secs);
        }
        let rpc: Vec<OsString> = vec!["converge".into(), "--service".into(), service.into()];
        let answer = call(ports, settings, rpc);
        let line = match &answer {
            Answer::Reply(reply) => Convergence::of(reply).line(),
            Answer::Refused(token) => format!("refused: {token}"),
            Answer::NotSent(_) | Answer::Lost(_) => "no answer".to_owned(),
        };
        if last.as_deref() != Some(line.as_str()) {
            ports.say(&format!("poll {n}: {line}"));
            last = Some(line);
        }
        match answer {
            Answer::Reply(reply) => {
                let convergence = Convergence::of(&reply);
                if convergence.desired.as_deref() != Some(version.as_str()) {
                    return failed(
                        ports,
                        version,
                        "superseded",
                        &format!(
                            "the version {version} of {service} was set, but another desired version replaced it"
                        ),
                    );
                }
                match convergence.state.as_str() {
                    "converged" => {
                        ports.say(&format!("The hosts of {service} run {version}"));
                        return Outcome {
                            desired_version: version.clone(),
                            converged: true,
                            reason: "converged".to_owned(),
                        };
                    }
                    "halted" => {
                        return failed(
                            ports,
                            version,
                            "halted",
                            &format!(
                                "the version {version} of {service} was set, but the service is halted"
                            ),
                        );
                    }
                    _ => {}
                }
            }
            Answer::Refused(token) => {
                return failed(
                    ports,
                    version,
                    &token,
                    &format!(
                        "the version {version} of {service} was set, but the coordinator refused to report its convergence: {token}"
                    ),
                );
            }
            Answer::NotSent(_) | Answer::Lost(_) => {}
        }
    }
    failed(
        ports,
        version,
        "not_converged",
        &format!(
            "the version {version} of {service} was set but did not converge in {} reads",
            settings.max_polls
        ),
    )
}
