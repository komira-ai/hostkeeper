//! What the coordinator's Xcode store holds, asked with the gateway call `Xcode.List` through the
//! transport every call of `hk` uses ([`crate::call::exchange`]): the archives held, newest
//! release first, and the release waiting (`xcode_release_waiting`). The answer is the gateway's
//! JSON; a field this reads that is absent is empty (the gateway leaves a default out), and a
//! field it does not read is ignored.

use serde::Deserialize;

use crate::args::{Given, Globals};
use crate::call::exchange;
use crate::settings::LIST_PATH;
use crate::{Flow, PROGRAM, Stop, World};

/// An archive the store holds.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Held {
    pub(crate) sha256: String,
    pub(crate) version: String,
    pub(crate) build: String,
}

/// The release the daily list names that the store does not hold.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Waiting {
    pub(crate) version: String,
    pub(crate) build: String,
}

/// The answer of `Xcode.List`, as far as `hk xcode add` reads it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Listing {
    configured: bool,
    /// The archives held, newest release first.
    pub(crate) archives: Vec<Held>,
    pub(crate) waiting: Option<Waiting>,
}

impl Listing {
    /// The archive of these bytes, when the store holds it.
    pub(crate) fn of_bytes(&self, sha256: &str) -> Option<&Held> {
        self.archives.iter().find(|held| held.sha256 == sha256)
    }
}

/// Asks the store what it holds. A coordinator with no store (`configured` false) is a failure
/// that says so.
pub(crate) fn list(w: &mut World<'_>, globals: &Globals) -> Flow<Listing> {
    let Some(command) = w.table.iter().find(|c| c.path == LIST_PATH) else {
        return Err(Stop::fail(format!(
            "this {PROGRAM} has no Xcode.List method to ask the store with"
        )));
    };
    let given = Given::new(command.fields);
    let answer = exchange(w, globals, command, &given, None)?;
    let listing: Listing = serde_json::from_slice(&answer).map_err(|error| {
        Stop::fail(format!(
            "the answer of Xcode.List is not the coordinator's JSON ({error})"
        ))
    })?;
    if !listing.configured {
        return Err(Stop::fail(
            "the coordinator has no Xcode store: it was started without --xcode-dir",
        ));
    }
    Ok(listing)
}
