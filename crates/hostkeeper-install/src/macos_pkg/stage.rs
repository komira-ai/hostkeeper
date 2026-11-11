//! `stage` (packages-repositories.md section 4.7, steps 2 to 7): from the verified `.pkg` to a
//! staging directory the core then walks, checks and seals:
//!
//! ```text
//! <staging>/artifact               the verified .pkg, 0444, kept for rollback
//! <staging>/tree/                  the component's payload, standing for /
//! <staging>/macos-pkg/PackageInfo  the component's two files, as the expansion gave them
//! <staging>/macos-pkg/Bom
//! <staging>/macos-pkg/artifact.pkg the kept .pkg again (a hard link), the name installer takes
//! ```
//!
//! The hook program the order names, if any, is checked in the payload ([`program::check`]).
//!
//! No package script runs at `stage` and no tool fetches (`installer` runs a package's scripts at
//! `activate`, not here): `pkgutil` only reads the file, and `lsbom` the `Bom`. The signature is checked before anything is expanded.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::payload::{self, LSBOM_ARGS};
use super::{ARTIFACT, INSTALLER_PKG, META, MacosPkg, Order, TREE, arg, io, shape, signature};
use crate::host::{self, remove_tree};
use crate::outcome::{Failure, Reason};
use crate::program;
use crate::tools::{Tool, may_be_cut};

/// The expansion's directory in the staging directory, removed once `stage` has taken from it.
pub const EXPANDED: &str = "expanded";

impl MacosPkg<'_> {
    /// Stages `order`'s verified package at `artifact` into `staging`, an empty directory of the
    /// version's. On a failure, what is in `staging` is the core's to remove.
    pub fn stage(&self, order: &Order, artifact: &Path, staging: &Path) -> Result<(), Failure> {
        let deadline = order.bounds.stage;
        let holder = self
            .records
            .holder(&order.identifier, &order.service)
            .map_err(|e| Failure::new(Reason::IoError, e.to_string()))?;
        if let Some(other) = holder {
            return Err(Failure::new(
                Reason::PackageConflict,
                format!(
                    "the service {other} records the package {}",
                    order.identifier
                ),
            ));
        }
        let pkg = arg(artifact)?;
        let checked = self.run(
            Tool::Pkgutil,
            &["--check-signature".into(), pkg.clone()],
            deadline,
        )?;
        signature::check(
            checked.code,
            &String::from_utf8_lossy(&checked.stdout),
            self.signers,
        )
        .map_err(|e| Failure::new(Reason::Signature, e.to_string()))?;

        let expanded = staging.join(EXPANDED);
        let out = self.run(
            Tool::Pkgutil,
            &["--expand-full".into(), pkg, arg(&expanded)?],
            deadline,
        )?;
        if out.code != Some(0) {
            return Err(Failure::new(
                Reason::ManifestInvalid,
                format!(
                    "pkgutil --expand-full exited {:?}: {}",
                    out.code,
                    out.stderr_text().trim()
                ),
            ));
        }
        let invalid =
            |e: &dyn std::fmt::Display| Failure::new(Reason::ManifestInvalid, e.to_string());
        let component = shape::check(&expanded, &order.identifier, order.version.as_str())
            .map_err(|e| invalid(&e))?;

        let listed = self.run(
            Tool::Lsbom,
            &[
                LSBOM_ARGS[0].into(),
                LSBOM_ARGS[1].into(),
                arg(&component.bom())?,
            ],
            deadline,
        )?;
        if listed.code != Some(0) || may_be_cut(&listed.stdout) {
            return Err(Failure::new(
                Reason::ManifestInvalid,
                format!(
                    "lsbom of the Bom exited {:?} or printed more than it may",
                    listed.code
                ),
            ));
        }
        let bom = payload::parse_bom(&String::from_utf8_lossy(&listed.stdout))
            .map_err(|e| invalid(&e))?;
        let walked = host::walk(&component.payload(), self.host.ids())
            .map_err(|e| io("the expanded payload", e))?;
        payload::check(&bom, &walked).map_err(|e| invalid(&e))?;
        payload::check_jobs(&component.payload(), &bom, &order.run).map_err(|e| invalid(&e))?;
        if let Some(hook) = &order.hook {
            program::check(&component.payload(), hook).map_err(|e| invalid(&e))?;
        }

        let meta = staging.join(META);
        fs::create_dir(&meta).map_err(|e| io("the staging directory", e))?;
        for name in ["PackageInfo", "Bom"] {
            fs::copy(component.dir.join(name), meta.join(name)).map_err(|e| io(name, e))?;
            set_mode(&meta.join(name), 0o444)?;
        }
        fs::rename(component.payload(), staging.join(TREE)).map_err(|e| io("the payload", e))?;
        let kept = staging.join(ARTIFACT);
        fs::copy(artifact, &kept).map_err(|e| io("the artifact", e))?;
        set_mode(&kept, 0o444)?;
        fs::hard_link(&kept, meta.join(INSTALLER_PKG))
            .map_err(|e| io("the artifact's .pkg name", e))?;
        remove_tree(&expanded).map_err(|e| io("the expansion", e))
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<(), Failure> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|e| io("a staged file", e))
}
