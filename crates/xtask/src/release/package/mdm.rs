//! The mdm package's contents (docs/design/two-programs.md section 6.2): the manifest and the
//! units of `hostkeeper_mdm_core::package`, and under `bin/` the binaries of the pinned tools
//! the package carries (`hostkeeper_mdm_core::package::TOOLS`), taken out of their archives,
//! each checked against its pin, by `native::mdm::tools`, which the `.deb` shares.

use std::path::Path;

use hostkeeper_mdm_core::package::{UNITS, manifest};
use hostkeeper_mdm_core::pins::Pins;

use super::{Contents, Unit};
use crate::release::Error;

/// The manifest and units of the mdm package of the version text `version`, with the tools of
/// `pins` out of the archives in `dir`, each checked against its pin; the manifest asks for a
/// daemon of `floor` or later.
pub(super) fn contents(
    pins: &Pins,
    dir: &Path,
    version: &str,
    floor: &str,
) -> Result<Contents, Error> {
    let binaries = crate::release::native::mdm::tools(pins, dir)?;
    let units = UNITS
        .iter()
        .map(|unit| Unit {
            file: unit.name.to_owned(),
            origin: format!("units/{}", unit.name),
            data: unit.text.as_bytes().to_vec(),
        })
        .collect();
    Ok(Contents {
        manifest: manifest(version, floor),
        units,
        trusted_root: None,
        binaries,
    })
}

#[cfg(test)]
mod tests;
