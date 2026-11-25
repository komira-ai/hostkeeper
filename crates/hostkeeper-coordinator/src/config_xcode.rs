//! The flags of the Xcode releases: `--xcode-feed-url`, the list the job `xcode-feed`
//! ([`crate::xcode_feed`]) reads, and `--xcode-dir` and `--xcode-max-bytes`, the private store of
//! the archives ([`crate::xcode_store`]).

use std::path::PathBuf;

use clap::Args;

use crate::xcode_feed::FeedUrl;
use crate::xcode_store::DEFAULT_MAX_BYTES;

/// The flags of the list of Xcode releases and of the store of their archives.
#[derive(Clone, Debug, Args)]
pub struct XcodeFlags {
    /// The `https://` address of the list of Xcode releases the coordinator reads once a day (the
    /// job `xcode-feed`): the Xcode Releases site's `data.json`
    /// (`https://xcodereleases.com/data.json`). Empty, the default, turns the job off, so no
    /// coordinator reads a third party unless an operator asks it to. A URL that is not
    /// `https://`, or names no file, is refused at start.
    #[arg(long, value_name = "URL", default_value = "", value_parser = feed_url_text)]
    pub xcode_feed_url: String,
    /// The directory of the private Xcode store: the archives an operator pushes with
    /// `PUT /v1/xcode/<sha256>`, kept as Apple ships them and read only by the coordinator's own
    /// Macs. It must exist; the coordinator does not create it, and does not start without it.
    /// Absent, the default, there is no store: the two routes answer 404, `Xcode.List` answers
    /// that none is configured and `Xcode.Remove` refuses. Several coordinators on one database
    /// each mount the same directory.
    #[arg(long, value_name = "DIR")]
    pub xcode_dir: Option<PathBuf>,
    /// The most bytes of one archive the store takes (default 21474836480, 20 GiB); at least 1.
    /// An upload that says it is larger, or that turns out to be, is refused. Only with
    /// `--xcode-dir`.
    #[arg(
        long,
        value_name = "BYTES",
        default_value_t = DEFAULT_MAX_BYTES,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub xcode_max_bytes: u64,
}

impl XcodeFlags {
    /// The address of the list: none when the flag is empty.
    pub(crate) fn feed(&self) -> Option<FeedUrl> {
        FeedUrl::parse(&self.xcode_feed_url).ok()
    }
}

/// `text` of `--xcode-feed-url`: empty, or the address of a file over HTTPS.
fn feed_url_text(text: &str) -> Result<String, String> {
    if !text.is_empty() {
        FeedUrl::parse(text).map_err(|why| why.to_string())?;
    }
    Ok(text.to_owned())
}

#[cfg(test)]
#[path = "config_xcode_tests.rs"]
mod tests;
