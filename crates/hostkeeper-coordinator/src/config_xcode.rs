//! The flag of the job `xcode-feed` ([`crate::xcode_feed`]): `--xcode-feed-url`.

use clap::Args;

use crate::xcode_feed::FeedUrl;

/// The flags of the list of Xcode releases.
#[derive(Clone, Debug, Args)]
pub struct XcodeFlags {
    /// The `https://` address of the list of Xcode releases the coordinator reads once a day (the
    /// job `xcode-feed`): the Xcode Releases site's `data.json`
    /// (`https://xcodereleases.com/data.json`). Empty, the default, turns the job off, so no
    /// coordinator reads a third party unless an operator asks it to. A URL that is not
    /// `https://`, or names no file, is refused at start.
    #[arg(long, value_name = "URL", default_value = "", value_parser = feed_url_text)]
    pub xcode_feed_url: String,
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
