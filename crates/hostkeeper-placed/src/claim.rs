//! The paths a package claims (design section 4.7, "Paths" and "Placed paths"): each file of
//! `units/` at `<unit dir>/<name>`, each `[[files]]` entry at its `path`, each a link into
//! `<packages>/<service>/current/`.
//!
//! The rules are checked here again, whatever the loader checked, because a [`Manifest`]'s
//! fields can be set by any caller: a unit file's name is the service's; a `[[files]] path` is
//! absolute, in the path grammar (no `.` or `..` component) and under none of
//! [`REFUSED_PREFIXES`], compared on whole components (`/etc/hostkeeperx` is not
//! `/etc/hostkeeper`); and no two entries of one package claim one path.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use hostkeeper_config::manifest::{Manifest, REFUSED_PREFIXES};
use hostkeeper_config::names::{ServiceName, UnitName, Version, check_path, is_within};

use crate::error::ClaimError;
use crate::roots::Roots;
use crate::store::PathKind;

/// One path a package claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The absolute system path of the link.
    pub path: PathBuf,
    pub kind: PathKind,
    /// The target the link is given.
    pub link_target: PathBuf,
}

/// The paths one version of a service claims, each once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claims {
    service: ServiceName,
    version: Version,
    claims: BTreeMap<PathBuf, Claim>,
}

/// Checks a `[[files]] path`: not empty, absolute and in the path grammar, under none of the
/// refused prefixes.
pub fn check_files_path(path: &str) -> Result<(), ClaimError> {
    if path.is_empty() {
        return Err(ClaimError::EmptyPath);
    }
    check_path(path).map_err(|why| ClaimError::BadPath {
        path: path.to_owned(),
        why,
    })?;
    match REFUSED_PREFIXES
        .iter()
        .copied()
        .find(|prefix| is_within(path, prefix))
    {
        Some(prefix) => Err(ClaimError::Refused {
            path: path.to_owned(),
            prefix,
        }),
        None => Ok(()),
    }
}

impl Claims {
    /// The units and files `manifest` claims, linked from `<packages>/<service>/current/`.
    pub fn of(roots: &Roots, manifest: &Manifest) -> Result<Claims, ClaimError> {
        let service = manifest.name.as_str();
        let current = roots.packages().current(service);
        let mut claims = Claims::empty(&manifest.name, &manifest.version);
        for file in &manifest.unit_files {
            UnitName::of(service, file, true).map_err(|_| ClaimError::UnitName {
                service: service.to_owned(),
                file: file.clone(),
            })?;
            claims.add(Claim {
                path: roots.unit_dir().join(file),
                kind: PathKind::Unit,
                link_target: current.join("units").join(file),
            })?;
        }
        for entry in &manifest.files {
            check_files_path(&entry.path)?;
            claims.add(Claim {
                path: PathBuf::from(&entry.path),
                kind: PathKind::File,
                link_target: current.join(&entry.source),
            })?;
        }
        Ok(claims)
    }

    /// No claim, for `version` of `service`.
    pub(crate) fn empty(service: &ServiceName, version: &Version) -> Claims {
        Claims {
            service: service.clone(),
            version: version.clone(),
            claims: BTreeMap::new(),
        }
    }

    /// Adds `claim`; a path already claimed is [`ClaimError::Duplicate`].
    pub(crate) fn add(&mut self, claim: Claim) -> Result<(), ClaimError> {
        let path = claim.path.clone();
        match self.claims.insert(path.clone(), claim) {
            Some(_) => Err(ClaimError::Duplicate { path }),
            None => Ok(()),
        }
    }

    /// Adds every claim of `other`, which is of the same version.
    pub(crate) fn extend(&mut self, other: Claims) -> Result<(), ClaimError> {
        other
            .claims
            .into_values()
            .try_for_each(|claim| self.add(claim))
    }

    pub fn service(&self) -> &ServiceName {
        &self.service
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    /// The claims, by path.
    pub fn iter(&self) -> impl Iterator<Item = &Claim> {
        self.claims.values()
    }

    /// The claimed paths.
    pub fn paths(&self) -> BTreeSet<PathBuf> {
        self.claims.keys().cloned().collect()
    }
}
