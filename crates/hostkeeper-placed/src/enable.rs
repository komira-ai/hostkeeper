//! Enabling the `run` units (design section 4.7, "Units, enable and boot"): the daemon writes
//! their `WantedBy=` links itself, `<unit dir>/<target>.wants/<unit>` pointing at the placed
//! unit file (for an instance, at its template's), and records each in `placed.json` as a
//! `wants_link`, by the same write-ahead path as every other placed path.

use std::path::PathBuf;

use hostkeeper_config::manifest::Manifest;
use hostkeeper_config::names::ServiceName;

use crate::claim::{Claim, Claims};
use crate::error::PlacedError;
use crate::lint::{run_targets, unit_texts};
use crate::placer::Placer;
use crate::remove::Removal;
use crate::store::PathKind;

impl Placer<'_> {
    /// The `WantedBy=` links of the `run` units of `manifest`, from the `[Install]` section of
    /// each unit file of its version directory. A `run` unit whose file has no `WantedBy=` is
    /// [`crate::LintError::NoWantedBy`].
    pub(crate) fn wants_claims(&self, manifest: &Manifest) -> Result<Claims, PlacedError> {
        let texts = unit_texts(self.fs, &self.roots, manifest)?;
        let unit_dir = self.roots.unit_dir();
        let mut claims = Claims::empty(&manifest.name, &manifest.version);
        for unit in &manifest.run {
            let (file, targets) = run_targets(manifest, &texts, unit)?;
            for target in targets {
                claims.add(Claim {
                    path: unit_dir.join(format!("{target}.wants")).join(unit),
                    kind: PathKind::WantsLink,
                    link_target: unit_dir.join(&file),
                })?;
            }
        }
        Ok(claims)
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
