//! The mdm package's contents (docs/design/two-programs.md section 6.2): the manifest and the
//! units of `hostkeeper_mdm_core::package`, and under `bin/` the binaries of the pinned tools
//! the package carries ([`TOOLS`]), taken out of their archives.
//!
//! Each archive is read from the directory given with `--archives` under the file name of its
//! pin's URL and refused, naming the file, unless its SHA-256 is the pin's (a missing file is
//! refused too); its members are read by the kit's own reader, `hostkeeper_mdm::archive`, which
//! refuses an archive that is damaged or lacks a member. The archives are checked here even
//! though `release mdm-fetch` checked them when it downloaded them: the package is built from
//! what the directory holds now, and what it holds is the pins' or nothing is built.

use std::path::Path;

use hostkeeper_mdm_core::package::{TOOLS, UNITS, manifest};
use hostkeeper_mdm_core::pins::Pins;
use sha2::{Digest, Sha256};

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
    let mut binaries = Vec::new();
    for tool in TOOLS {
        let pin = pins.get(tool);
        let path = dir.join(pin.archive());
        let bytes = std::fs::read(&path).map_err(Error::io(format!(
            "read {}, the pinned archive of {tool}",
            path.display()
        )))?;
        let got = hex::encode(Sha256::digest(&bytes));
        let want = pin.sha256_hex();
        if got != want {
            return Err(Error::Failed(format!(
                "{}: its SHA-256 is {got}, the pin of {tool} is {want}",
                path.display()
            )));
        }
        for (member, name) in pin.members() {
            let data = hostkeeper_mdm::archive::member(pin.archive(), pin.packing, &bytes, &member)
                .map_err(|e| Error::Failed(e.to_string()))?;
            binaries.push((name.to_owned(), data));
        }
    }
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
