//! `hk xcode add` off macOS: what differs from the run on a Mac (the header of [`super`]), which
//! has Apple's programs. No program of Apple's runs here; the check of the archive is made by a
//! farm Mac ([`crate::xcode_farm`]).
//!
//! - [`browser`]: the failure of a version that has no archive in `~/Downloads`, where `xcodes`
//!   does not run;
//! - [`release`]: the version and the build of an archive the store does not hold, which the
//!   archive cannot be read for: from a passing record of these bytes, else the version the run
//!   was given or the file's name carries and the build of the coordinator's waiting release of
//!   that version;
//! - [`Check`]: the check on the farm Mac and what each end of it makes of the run: a pass is the
//!   line `checked on <mac> (run <id>): pass`; an archive that failed is removed from the store
//!   and refused (`refused: <verdict>`, exit 1); a check that gave no verdict is exit 1 with the
//!   archive stored and the line that checks it again.

use std::path::Path;

use super::store::Listing;
use super::{find, stored};
use crate::args::Globals;
use crate::home::Home;
use crate::xcode_farm::{self, Outcome, Subject, Verdict, shell_word};
use crate::xcode_push::{failed, say};
use crate::xcode_record::Archive;
use crate::{Flow, PROGRAM, Stop, World};

/// What a version with no archive in `~/Downloads` is told off macOS, where `xcodes` does not run.
pub(super) fn browser(version: &str) -> String {
    format!(
        "Xcode {version} has no archive in ~/Downloads, and {PROGRAM} xcode add does not download \
         off macOS (xcodes is a macOS program): download Xcode_{version}.xip from Apple's \
         developer downloads page (https://developer.apple.com/download/all/) in a browser into \
         ~/Downloads, and run {PROGRAM} xcode add {version} again"
    )
}

/// The version and the build of an archive the store does not hold, with no program of Apple's to
/// read them from the app. `saved` is the release a passing record of these bytes gives. Else the
/// version is `wanted` (the one given, or the one the store waits for) or the one the file's
/// `name` carries (`Xcode_<version>.xip`), and the build is that of the coordinator's waiting
/// release of that version, which `listing` names.
pub(super) fn release(
    w: &mut World<'_>,
    listing: &Listing,
    saved: Option<(String, String)>,
    wanted: Option<&str>,
    name: &str,
    record: &Path,
) -> Flow<(String, String)> {
    if let Some(found) = saved {
        say(
            w,
            &format!(
                "{}, read from the record kept in {}; no program of Apple's runs here",
                stored(&found.0, &found.1),
                record.display()
            ),
        );
        return Ok(found);
    }
    let Some(version) = wanted.or_else(|| find::name_version(name)) else {
        return Err(failed(&format!(
            "{name} names no Xcode version, and off macOS {PROGRAM} cannot read it from the \
             archive: its name must be Xcode_<version>.xip (Xcode-<version>.xip too), as \
             Xcode_27.2.xip is"
        )));
    };
    let Some(waiting) = listing
        .waiting
        .as_ref()
        .filter(|waiting| find::parts(&waiting.version) == find::parts(version))
    else {
        return Err(failed(&format!(
            "the coordinator names no waiting release of Xcode {version}, and off macOS \
             {PROGRAM} cannot read the build from the archive: the build is that of the release \
             the daily list names and the store does not hold yet ({PROGRAM} xcode list shows \
             it)"
        )));
    };
    say(
        w,
        &format!(
            "{}, the release the coordinator waits for; no program of Apple's runs here",
            stored(&waiting.version, &waiting.build)
        ),
    );
    Ok((waiting.version.clone(), waiting.build.clone()))
}

/// The check of one archive on the farm Mac `mac`.
pub(super) struct Check<'a> {
    pub(super) mac: &'a str,
    /// The local file, as the record names it.
    pub(super) archive: Archive,
    pub(super) version: &'a str,
    pub(super) build: &'a str,
    /// The file, which the line that checks it again names.
    pub(super) path: &'a Path,
}

impl Check<'_> {
    /// Runs the check. On a pass, the line for stdout that says so; the record is written. On any
    /// other end, the run's failure: the archive that failed is removed from the store.
    pub(super) fn run(&self, w: &mut World<'_>, globals: &Globals, home: &Home) -> Flow<String> {
        let subject = Subject {
            verb: "xcode add",
            mac: self.mac,
            archive: &self.archive,
            version: self.version,
            build: self.build,
        };
        match xcode_farm::check(w, globals, home, &subject) {
            Err(stop) => {
                self.again(w);
                Err(stop)
            }
            Ok(Outcome::Unchecked(why)) => {
                say(w, &format!("not checked: {why}"));
                self.again(w);
                Err(Stop::Exit(1))
            }
            Ok(Outcome::Verdict(verdict)) if verdict.pass => Ok(format!(
                "checked on {} (run {}): pass",
                self.mac, verdict.run
            )),
            Ok(Outcome::Verdict(verdict)) => Err(self.refuse(w, globals, &verdict)),
        }
    }

    /// Says that the archive is stored, unchecked, and how to check it again.
    fn again(&self, w: &mut World<'_>) {
        say(
            w,
            &format!(
                "the archive stays in the store; to check it again: {PROGRAM} xcode verify --mac \
                 {} {}",
                self.mac,
                shell_word(&self.path.display().to_string())
            ),
        );
    }

    /// An archive that failed the check: `refused: <verdict>` on stdout, and the removal from the
    /// store; the run's failure.
    fn refuse(&self, w: &mut World<'_>, globals: &Globals, verdict: &Verdict) -> Stop {
        let _ = writeln!(w.stdout, "refused: {}", verdict.text);
        let sha256 = &self.archive.sha256;
        let reason = xcode_farm::removal_reason(self.mac, &verdict.run, &verdict.reason);
        match xcode_farm::remove(w, globals, sha256, &reason) {
            Ok(()) => say(
                w,
                &format!(
                    "the store no longer holds the archive: it failed the check on {} (run {}): \
                     {}",
                    self.mac, verdict.run, verdict.reason
                ),
            ),
            Err(why) => say(
                w,
                &format!(
                    "the archive failed the check on {} (run {}), but it could not be removed \
                     from the store ({why}); remove it with: {PROGRAM} xcode remove --sha256 \
                     {sha256} --reason <why>",
                    self.mac, verdict.run
                ),
            ),
        }
        Stop::Exit(1)
    }
}
