//! The record of a check a farm Mac made: the record of `hk xcode verify`
//! ([`crate::xcode_record::Verify`]) filled from the steps the Mac's probe reported, or from the
//! reason its turn was deferred. What no step gives is `null`; the key `farm` holds every step as
//! the Mac reported it.
//!
//! - `macos` is the `sw_vers` and `uname` steps, each with the step's exit code as `exit`, its
//!   output as `stdout` and nothing as `stderr`; a step the Mac did not report is no exit, no
//!   output and `not reported by <mac>` as `stderr`;
//! - `programs`, `signature`, `tamper` and `toc` are `null`;
//! - `expansion` is there when the `xip` step is: its exit code and output, the `.app` names of
//!   the `app` step, the version and the build of the `version` step when it gave two words that
//!   are an Xcode release's, and the `codesign` step as the app's `codesign`;
//! - `verdict` is the one the check came to.

use super::{Found, Subject};
use crate::xcode_push::archive::{is_build, is_version};
use crate::xcode_record::{Expansion, Farm, FarmStep, MacOs, Output, VERIFY, Verify};

use super::read::{Probe, Step};

/// What `step` wrote, as the record's [`Output`].
fn output_of(step: &Step) -> Output {
    Output {
        exit: step.exit_code,
        stdout: step.output.clone(),
        stderr: String::new(),
    }
}

/// The step `name` of the probe as an [`Output`], or that the Mac did not report it.
fn reported(probe: Option<&Probe>, name: &str, mac: &str) -> Output {
    probe.and_then(|probe| probe.step(name)).map_or_else(
        || Output {
            exit: None,
            stdout: String::new(),
            stderr: format!("not reported by {mac}"),
        },
        output_of,
    )
}

/// The version and the build the `version` step says: its two words, when they are an Xcode
/// release's, else neither.
fn release_of(step: &Step) -> (Option<String>, Option<String>) {
    let mut words = step.output.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some(version), Some(build), None)
            if is_version(version.as_bytes()) && is_build(build.as_bytes()) =>
        {
            (Some(version.to_owned()), Some(build.to_owned()))
        }
        _ => (None, None),
    }
}

/// The names of the apps the `app` step lists: its output split at `, `, none for `none`.
fn apps_of(step: &Step) -> Vec<String> {
    step.output
        .split(", ")
        .filter(|name| !name.is_empty() && *name != "none")
        .map(str::to_owned)
        .collect()
}

/// The expansion section: only when the probe has the `xip` step.
fn expansion_of(probe: Option<&Probe>) -> Option<Expansion> {
    let probe = probe?;
    let xip = probe.step("xip")?;
    let (version, build) = probe.step("version").map_or((None, None), release_of);
    Some(Expansion {
        exit: xip.exit_code,
        output: Some(xip.output.clone()),
        apps: probe.step("app").map(apps_of).unwrap_or_default(),
        version,
        build,
        codesign: probe.step("codesign").map(output_of),
        ..Expansion::default()
    })
}

/// The record of the check of `subject` in the run `run`, with what the Mac said as `found`;
/// `stamp` is the version of this `hk`.
pub(super) fn build(stamp: &str, subject: &Subject<'_>, run: &str, found: &Found) -> Verify {
    let probe = found.probe.as_ref();
    Verify {
        record: VERIFY,
        hk: stamp.to_owned(),
        macos: MacOs {
            sw_vers: reported(probe, "sw_vers", subject.mac),
            uname: reported(probe, "uname", subject.mac),
        },
        programs: None,
        archive: subject.archive.clone(),
        signature: None,
        tamper: None,
        toc: None,
        expansion: expansion_of(probe),
        farm: Some(Farm {
            host: subject.mac.to_owned(),
            run: run.to_owned(),
            probed_unix: probe.map(|probe| probe.probed_unix),
            steps: probe
                .map(|probe| {
                    probe
                        .steps
                        .iter()
                        .map(|step| FarmStep {
                            name: step.name.clone(),
                            exit: step.exit_code,
                            output: step.output.clone(),
                            cut: step.cut,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            reason: found.deferral.clone(),
        }),
        verdict: found.verdict.clone(),
    }
}
