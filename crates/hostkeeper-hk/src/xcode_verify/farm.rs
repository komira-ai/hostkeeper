//! `hk xcode verify --mac <node-id> <archive.xip>` off macOS: the archive is checked by a farm Mac
//! ([`crate::xcode_farm`]) in place of the checks of this module's other parts, which run Apple's
//! programs. It needs the archive in the coordinator's store, where the farm Mac fetches it from:
//! `Xcode.List` must hold these bytes, else the run fails saying `hk xcode add <path>` uploads and
//! checks it.
//!
//! The record the farm check writes is printed on stdout as on macOS, with `verdict <verdict>;
//! record kept in <path>` on stderr; the exit is 0 on a pass and 1 otherwise. An archive that
//! failed is not removed from the store (only `hk xcode add` removes): the line that removes it is
//! said on stderr. A check that gave no verdict is exit 1, says why on stderr and writes no record.

use std::path::Path;

use crate::args::Globals;
use crate::home::Home;
use crate::xcode_add::store;
use crate::xcode_farm::{self, Outcome, Subject, shell_word};
use crate::xcode_push::say;
use crate::xcode_record::Archive;
use crate::{Flow, PROGRAM, Stop, World};

/// The archive on disk, as `hk xcode verify` read it.
pub(super) struct Local<'a> {
    pub(super) path: &'a Path,
    pub(super) size: u64,
    pub(super) sha256: &'a str,
    /// The kind, from the first four bytes.
    pub(super) kind: &'a str,
}

/// Checks `local` on the farm Mac `mac`.
pub(super) fn verify(
    w: &mut World<'_>,
    globals: &Globals,
    home: &Home,
    mac: &str,
    local: &Local<'_>,
) -> Flow<()> {
    let listing = store::list(w, globals)?;
    let Some(held) = listing.of_bytes(local.sha256) else {
        return Err(Stop::fail(format!(
            "the coordinator's store holds no archive of these bytes (sha256 {}), and {mac} reads \
             the archive from there: {PROGRAM} xcode add {} uploads it and checks it",
            local.sha256,
            shell_word(&local.path.display().to_string())
        )));
    };
    let archive = Archive {
        name: local
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size: local.size,
        sha256: local.sha256.to_owned(),
        kind: local.kind.to_owned(),
    };
    let subject = Subject {
        verb: "xcode verify",
        mac,
        archive: &archive,
        version: &held.version,
        build: &held.build,
    };
    let verdict = match xcode_farm::check(w, globals, home, &subject)? {
        Outcome::Verdict(verdict) => verdict,
        Outcome::Unchecked(why) => {
            say(w, &format!("not checked: {why}"));
            return Err(Stop::Exit(1));
        }
    };
    let _ = write!(w.stdout, "{}", verdict.json);
    let Some(path) = &verdict.path else {
        say(
            w,
            &format!("verdict {}; the record was not kept", verdict.text),
        );
        return Err(Stop::Exit(1));
    };
    say(
        w,
        &format!(
            "verdict {}; record kept in {}",
            verdict.text,
            path.display()
        ),
    );
    if verdict.pass {
        return Ok(());
    }
    say(
        w,
        &format!(
            "the archive stays in the store; to remove it: {PROGRAM} xcode remove --sha256 {} \
             --reason <why>",
            local.sha256
        ),
    );
    Err(Stop::Exit(1))
}
