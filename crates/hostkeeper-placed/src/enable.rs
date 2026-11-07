//! Enabling the `run` units (design section 4.7, "Units, enable and boot"): the daemon writes
//! their `WantedBy=` links itself, `<unit dir>/<target>.wants/<unit>` pointing at the placed
//! unit file (for an instance, at its template's), and records each in `placed.json` as a
//! `wants_link`, by the same write-ahead path as every other placed path. A launchd label has
//! none: its placed plist is what launchd loads at boot.
//!
//! A `WantedBy=` link already there that `placed.json` does not list, and that is a link to a
//! unit file of the same name (`systemctl enable` of the unit a distribution package installed,
//! as the deb and the rpm of hostkeeper enable `hostkeeper.service`), already enables the unit:
//! systemd reads a `.wants/` link by its name, so it starts the unit of that name from the first
//! directory of its search path that has it, `/etc/systemd/system` before `/usr/lib`. It is left
//! as it is and is not claimed, so it is no `path_conflict`, and the daemon never removes it: it
//! is not the daemon's. Only the link's target's file name is compared, wherever it points: the
//! unit directory is root's, so whatever wrote a link there is trusted as the host's own setup.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::Manifest;
use hostkeeper_config::names::ServiceName;

use crate::check::occupant;
use crate::claim::{Claim, Claims};
use crate::error::{Owner, PlacedError};
use crate::lint::{run_targets, unit_texts};
use crate::placer::Placer;
use crate::remove::Removal;
use crate::store::{PathKind, PlacedRecord};

impl Placer<'_> {
    /// The `WantedBy=` links of the `run` units of `manifest`, from the `[Install]` section of
    /// each unit file of its version directory. A systemd `run` unit whose file has no
    /// `WantedBy=` is [`crate::LintError::NoWantedBy`]; a launchd label adds no link. A link
    /// that already enables the unit and that `placed.json` does not list ([`Self::enabled_by_other`])
    /// is not claimed.
    pub(crate) fn wants_claims(&self, manifest: &Manifest) -> Result<Claims, PlacedError> {
        self.wants_claims_in(manifest, &self.store().load()?)
    }

    /// [`Self::wants_claims`] against `record`, the record as the caller holds it.
    pub(crate) fn wants_claims_in(
        &self,
        manifest: &Manifest,
        record: &PlacedRecord,
    ) -> Result<Claims, PlacedError> {
        let texts = unit_texts(self.fs, &self.roots, manifest)?;
        let unit_dir = self.roots.unit_dir();
        let mut claims = Claims::empty(&manifest.name, &manifest.version);
        for unit in &manifest.run {
            let (file, targets) = run_targets(manifest, &texts, unit)?;
            for target in targets {
                let path = unit_dir.join(format!("{target}.wants")).join(unit);
                if !record.paths.contains_key(&path) && self.enabled_by_other(&path, &file)? {
                    continue;
                }
                claims.add(Claim {
                    path,
                    kind: PathKind::WantsLink,
                    link_target: unit_dir.join(&file),
                })?;
            }
        }
        Ok(claims)
    }

    /// Whether the `WantedBy=` link `path` is a link whose target is a unit file named `file`:
    /// the unit is enabled already, by whatever wrote the link (a distribution package's
    /// `systemctl enable`). Anything else there, or nothing, is not.
    fn enabled_by_other(&self, path: &Path, file: &str) -> Result<bool, PlacedError> {
        Ok(match occupant(self.fs, path)? {
            Some(Owner::Link(target)) => target.file_name() == Some(OsStr::new(file)),
            _ => false,
        })
    }

    /// Writes and records the `WantedBy=` links of the `run` units of `manifest`: begin, link,
    /// commit. `true` when a link was written. No `daemon-reload` runs here.
    pub fn enable_run_units(&self, manifest: &Manifest) -> Result<bool, PlacedError> {
        let claims = self.wants_claims(manifest)?;
        let linking = self.begin(&claims)?;
        let changed = self.link(&linking)?;
        self.commit(linking)?;
        Ok(changed)
    }

    /// Removes every `WantedBy=` link of `service` and its entry, stopping nothing, then runs
    /// `daemon-reload` when one went.
    pub fn disable(&self, service: &ServiceName) -> Result<Removal, PlacedError> {
        self.prune(service.as_str(), false, |record| {
            record
                .of_service(service.as_str())
                .filter(|(_, entry)| entry.kind != PathKind::WantsLink)
                .map(|(path, _)| path.clone())
                .collect::<std::collections::BTreeSet<PathBuf>>()
        })
    }
}
