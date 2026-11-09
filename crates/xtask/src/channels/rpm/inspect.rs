//! A package read back the way `rpm` would, before it is signed or listed: the digests of the
//! signature header match, the payload unpacks and holds every file the header lists, and the
//! fields the repository's `primary.xml` is made of are there.
//!
//! Adapted from `inspect` in komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/release/rpm.rs`), which read `%post` and `%postun` unconditionally. `nfpm` writes `%post` (from `postinstall`) and `%preun` (from `preremove`) and
//! no `%postun`, so `%postun` is read only for a package whose spec says it has one
//! ([`Package::postun_required`]); and the name the header carries must be the spec's.

use std::collections::BTreeSet;

use rpm::Package as Rpm;

use crate::channels::Error;
use crate::channels::spec::Package;

/// The error for a failure of the `rpm` crate while it does `what`.
fn failed(what: &'static str) -> impl FnOnce(rpm::Error) -> Error {
    move |e| Error::Failed(format!("{what}: {e}"))
}

/// What a package says about itself that the checks use.
#[derive(Debug, PartialEq, Eq)]
pub struct Info {
    /// Name, version, release and architecture.
    pub nevra: (String, String, String, String),
    /// The names the package requires, sorted, besides the `rpmlib(...)` ones.
    pub requires: Vec<String>,
    /// The absolute paths of the files and directories it owns, sorted.
    pub paths: Vec<String>,
}

/// Reads `bytes` as the rpm of `package` and checks its digests and its payload.
///
/// # Errors
/// The bytes are not a package, a digest does not match, a field is missing (`%postun` only when
/// the spec requires it), or the name is not the spec's.
pub fn inspect(bytes: &[u8], package: &Package) -> Result<Info, Error> {
    let rpm = Rpm::parse(&mut &bytes[..]).map_err(failed("read the package"))?;
    rpm.verify_digests().map_err(failed("check the digests"))?;
    let meta = &rpm.metadata;
    // The payload decompresses and holds every file the header lists.
    rpm.files()
        .and_then(|mut files| files.try_for_each(|file| file.map(drop)))
        .map_err(failed("unpack the payload"))?;
    let name = meta.get_name().map_err(failed("read the name"))?;
    if name != package.name {
        return Err(Error::Failed(format!(
            "the package is named {name:?}, not {:?}",
            package.name
        )));
    }
    let requires: Vec<String> = meta
        .get_requires()
        .map_err(failed("read the requirements"))?
        .into_iter()
        .map(|d| d.name)
        .filter(|name| !name.starts_with("rpmlib("))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut paths: Vec<String> = meta
        .get_file_entries()
        .map_err(failed("read the files"))?
        .iter()
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();
    paths.sort();
    meta.get_post_install_script()
        .map_err(failed("read %post"))?;
    if package.postun_required {
        meta.get_post_uninstall_script()
            .map_err(failed("read %postun"))?;
    }
    Ok(Info {
        nevra: (
            name.to_owned(),
            meta.get_version()
                .map_err(failed("read the version"))?
                .to_owned(),
            meta.get_release()
                .map_err(failed("read the release"))?
                .to_owned(),
            meta.get_arch()
                .map_err(failed("read the architecture"))?
                .to_owned(),
        ),
        requires,
        paths,
    })
}

#[cfg(test)]
mod tests;
