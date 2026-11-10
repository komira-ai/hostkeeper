//! What mdm's `.deb` holds (docs/operations/mdm-package.md): its program and the binaries of the
//! pinned tools of `hostkeeper_mdm_core::package::TOOLS` in `/usr/lib/mdm/`
//! (`hostkeeper_mdm_core::package::DEB_BIN`), and its eight units in
//! `/usr/lib/systemd/system/`, each the committed unit with its programs in `/usr/lib/mdm/`
//! (`Layout::Deb`'s `unit_text`). The `.deb` is built for linux-x86_64 only: the pinned tools
//! are linux-amd64 builds.
//!
//! [`tools`] takes the tools out of their archives, for the `.deb` and for the archive package
//! (`package::mdm`): each archive is read from the directory given with `--archives` under the
//! file name of its pin's URL and refused, naming the file, unless its SHA-256 is the pin's (a
//! missing file is refused too); its members are read by the kit's own reader,
//! `hostkeeper_mdm::archive`, which refuses an archive that is damaged or lacks a member. The
//! archives are checked here even though `release mdm-fetch` checked them when it downloaded
//! them: the package is built from what the directory holds now, and what it holds is the pins'
//! or nothing is built.

use std::path::Path;

use hostkeeper_mdm_core::package::{DEB_BIN, Layout, SERVICE, TOOLS, UNITS};
use hostkeeper_mdm_core::pins::Pins;
use sha2::{Digest, Sha256};

use super::hello::File;
use super::{FILE_MODE, PROGRAM_MODE, SYSTEMD_UNITS};
use crate::release::Error;

/// The binaries of the tools of `pins` out of the archives in `dir`, each archive checked
/// against its pin: `(name, bytes)` in the order of [`TOOLS`] and of each pin's members.
pub(crate) fn tools(pins: &Pins, dir: &Path) -> Result<Vec<(String, Vec<u8>)>, Error> {
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
    Ok(binaries)
}

/// The files of mdm's `.deb`: the program `binary`, the tools `tools` ([`tools`]) and the
/// units, in path order.
pub(crate) fn deb_files(binary: Vec<u8>, tools: Vec<(String, Vec<u8>)>) -> Vec<File> {
    let file = |path: String, mode: u32, data: Vec<u8>| File { path, mode, data };
    let mut files = vec![file(format!("{DEB_BIN}/{SERVICE}"), PROGRAM_MODE, binary)];
    files.extend(
        tools
            .into_iter()
            .map(|(name, data)| file(format!("{DEB_BIN}/{name}"), PROGRAM_MODE, data)),
    );
    files.extend(UNITS.iter().map(|unit| {
        file(
            format!("{SYSTEMD_UNITS}/{}", unit.name),
            FILE_MODE,
            Layout::Deb.unit_text(unit).into_bytes(),
        )
    }));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

#[cfg(test)]
mod tests;
