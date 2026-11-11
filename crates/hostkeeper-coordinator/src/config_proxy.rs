//! The flags of the package proxy (`docs/design/package-proxy.md`): `--proxy-cache-dir`. The cache
//! and the readers of origins it makes are [`crate::proxy_origin::Proxy::start`].

use std::path::PathBuf;

use clap::Args;

/// The flags of the package proxy.
#[derive(Clone, Debug, Args)]
pub struct ProxyFlags {
    /// The package proxy's cache (`docs/design/package-proxy.md`): a directory of the
    /// coordinator's own, made when missing, holding `objects/` (the committed files, each with
    /// its sidecar) and `tmp/` (the fills in progress). At start every file of `tmp/` and every
    /// partial object a crash left is deleted, and the objects past `proxy_cache_max_bytes`
    /// (a setting, 20 GiB by default) are evicted, the least recently fetched first. No file
    /// is synced to disk. The directory is the coordinator's alone: not shared with another
    /// coordinator. With it, a pkg that an entry of the setting `installs` names with a `url`
    /// and that is not in `--mdm-pkg-dir` is fetched from that `url` once, checked against its
    /// digest and kept, and the `/apt/<repo>/...` routes of the MDM listener serve the public
    /// repositories through it. Without the flag there is no cache and nothing is fetched.
    #[arg(long)]
    pub proxy_cache_dir: Option<PathBuf>,
}
