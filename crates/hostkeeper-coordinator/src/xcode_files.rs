//! The list of Xcode releases over files ([`crate::xcode_feed::KnownReleases`]): one file beside
//! the state file, `<state-file>.xcode-releases`, holding `{"releases": [...]}`, each release as
//! its version, build, channel and required macOS ([`KeptRelease`]), replaced whole by each read
//! of the list that succeeds. A file that does not exist is an empty list.

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::desired::write_whole;
use crate::xcode_feed::{KeptRelease, KnownReleases, XcodeRelease};

/// The file of the list beside the state file at `state`: `<state>.xcode-releases`.
pub(crate) fn xcode_releases_path(state: &Path) -> PathBuf {
    let mut path = state.as_os_str().to_owned();
    path.push(".xcode-releases");
    PathBuf::from(path)
}

/// The list in one file.
#[derive(Debug)]
pub(crate) struct FileReleases {
    path: PathBuf,
}

impl FileReleases {
    /// The list kept in the file at `path`.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// `why`, naming the file.
    fn at(&self, why: impl std::fmt::Display) -> String {
        format!("{}: {why}", self.path.display())
    }
}

/// What the file holds.
#[derive(Deserialize)]
struct Held {
    releases: Vec<KeptRelease>,
}

impl KnownReleases for FileReleases {
    fn keep(&self, releases: &[XcodeRelease]) -> Result<(), String> {
        let kept: Vec<KeptRelease> = releases.iter().map(KeptRelease::from).collect();
        let text = serde_json::json!({ "releases": kept }).to_string();
        write_whole(&self.path, text.as_bytes()).map_err(|why| why.to_string())
    }

    fn known(&self) -> Result<Vec<XcodeRelease>, String> {
        let text = match std::fs::read(&self.path) {
            Ok(text) => text,
            Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(why) => return Err(self.at(why)),
        };
        let held: Held = serde_json::from_slice(&text).map_err(|why| self.at(why))?;
        held.releases
            .iter()
            .map(|kept| kept.release().map_err(|why| self.at(why)))
            .collect()
    }
}

#[cfg(test)]
#[path = "xcode_files_tests.rs"]
mod tests;
