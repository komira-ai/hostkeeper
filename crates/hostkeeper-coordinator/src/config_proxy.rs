//! The start's wiring of the package proxy (`docs/design/package-proxy.md`): the flag
//! `--proxy-cache-dir`, the cache it opens and the reader of the pkgs' origins that goes with it.

use std::path::PathBuf;

use clap::Args;
use hostkeeper_oidc::Roots;

use crate::config::StartError;
use crate::mdm_setup::MdmSetup;
use crate::pkg_origin::PkgOrigin;
use crate::proxy_cache::{ProxyCache, open_at_start};
use crate::service::Coordinator;

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
    /// digest and kept. Without the flag there is no cache and nothing is fetched.
    #[arg(long)]
    pub proxy_cache_dir: Option<PathBuf>,
}

/// What the flags make at start: the cache, and the reader of origins that only a cache needs.
pub(crate) struct ProxyParts {
    cache: Option<ProxyCache>,
    origin: Option<PkgOrigin>,
}

impl ProxyParts {
    /// The cache of `flags` under the limits of `setup`, and the reader of origins trusting
    /// `roots`; neither without the flag.
    ///
    /// # Errors
    /// [`StartError`]: the settings cannot be read, the directory cannot be made or read, or
    /// `roots` holds no usable certificate.
    pub(crate) fn open(
        flags: &ProxyFlags,
        setup: &MdmSetup,
        roots: &Roots,
    ) -> Result<Self, StartError> {
        let cache = open_at_start(flags.proxy_cache_dir.as_deref(), setup)?;
        let origin = cache
            .as_ref()
            .map(|_| PkgOrigin::new(roots.clone()))
            .transpose()?;
        Ok(Self { cache, origin })
    }

    /// `coordinator` serving from the cache and fetching from the origins.
    pub(crate) fn attach(self, coordinator: Coordinator) -> Coordinator {
        coordinator
            .with_proxy_cache(self.cache)
            .with_pkg_origin(self.origin)
    }
}

#[cfg(test)]
#[path = "config_proxy_tests.rs"]
mod tests;
