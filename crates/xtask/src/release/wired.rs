//! [`Wired`]: the adapters of the release verbs over one [`Exec`], which `real::connect` builds
//! over the process-starting one and a test over a fake.

use super::check_binary::VersionOf;
use super::git::GitCli;
use super::github::Gh;
use super::image_push;
use super::macos_pkg::PkgbuildCli;
use super::mdm_fetch;
use super::ports::{Adapters, Exec, Ports};
use crate::advisories::DenyCli;

/// The adapters wired over one [`Exec`]: git, gh (runs, issues, releases and attestations),
/// the `--version` of a binary, `pkgbuild`, `curl`, `cargo deny` and `skopeo`, each running its
/// program through a copy of `exec`. The real ones (`real::connect`) are these over the
/// process-starting `Exec`; a test wires them over a fake one.
pub(crate) struct Wired<E> {
    git: GitCli<E>,
    gh: Gh<E>,
    version_of: VersionOf<E>,
    pkgbuild: PkgbuildCli<E>,
    curl: mdm_fetch::CurlCli<E>,
    deny: DenyCli<E>,
    skopeo: image_push::SkopeoCli<E>,
    sleep: image_push::Sleep,
}

impl<E: Exec + Clone> Wired<E> {
    pub(crate) fn new(exec: E) -> Wired<E> {
        Wired {
            git: GitCli::new(exec.clone()),
            gh: Gh::new(exec.clone()),
            version_of: VersionOf::new(exec.clone()),
            pkgbuild: PkgbuildCli::new(exec.clone()),
            curl: mdm_fetch::CurlCli::new(exec.clone()),
            deny: DenyCli::new(exec.clone()),
            skopeo: image_push::SkopeoCli::new(exec),
            sleep: image_push::Sleep(std::thread::sleep),
        }
    }
}

impl<E: Exec> Adapters for Wired<E> {
    fn ports(&self) -> Ports<'_> {
        Ports {
            git: &self.git,
            runs: &self.gh,
            store: &self.gh,
            source: &self.gh,
            verifier: &self.gh,
            run_version: &self.version_of,
            pkgbuild: &self.pkgbuild,
            advisories: &self.deny,
            download: &self.curl,
            registry: &self.skopeo,
            wait: &self.sleep,
        }
    }
}
