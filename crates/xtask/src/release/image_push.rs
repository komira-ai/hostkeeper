//! `release push-image`: pushes the coordinator's multi-platform image of a release to the
//! registry ([`IMAGE_REPOSITORY`], tagged with the release's version) from the OCI layout
//! `release image-layout` wrote, and reads the image index back by its digest, which must be the
//! release's asset `hostkeeper-coordinator-<version>.image.json` byte for byte.
//!
//! The push keeps every digest (`skopeo copy --all --preserve-digests`, which fails rather than
//! change a manifest), so the digest the release notes name is the attested one. A push is
//! idempotent: a blob the registry holds is not sent again, and a manifest of the same bytes has
//! the same digest. So an attempt that fails, the push or the read back (the registry refusing a
//! valid token for one request, a connection reset), is tried again whole, after a wait, in up to
//! [`ATTEMPTS`] attempts in all; a re-run of the job pushes the same release the same way. The
//! registry serving other bytes for the digest is never tried again: no later attempt can make a
//! digest name other bytes.

use std::path::{Path, PathBuf};

use super::Error;
use super::contract::Version;
use super::git::stdout_of;
use super::image_index::{IMAGE_REPOSITORY, digest_of, index_name};
use super::ports::{Exec, PortError, Registry, Wait};

/// The waits, in seconds, before the second and each later attempt: about a quarter of an hour
/// in all, inside the job's half hour.
pub(crate) const WAITS: [u64; 4] = [60, 120, 240, 480];

/// The attempts in all: the first, and one after each of [`WAITS`].
pub(crate) const ATTEMPTS: usize = WAITS.len() + 1;

/// What `release push-image` is asked.
pub(crate) struct Request {
    /// The release's files, which hold the image index.
    pub(crate) dir: PathBuf,
    pub(crate) version: Version,
    /// The OCI layout `release image-layout` wrote of those files.
    pub(crate) layout: PathBuf,
}

/// Pushes the image and reads it back, as the module says; the lines it prints.
pub(crate) fn push(
    registry: &dyn Registry,
    wait: &dyn Wait,
    request: &Request,
) -> Result<Vec<String>, Error> {
    let name = index_name(&request.version);
    let path = request.dir.join(&name);
    let index = std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
    let digest = digest_of(&index);
    let tag = request.version.to_string();
    let mut lines = Vec::new();
    for attempt in 1..=ATTEMPTS {
        let served = registry
            .copy(&request.layout, &tag, IMAGE_REPOSITORY)
            .and_then(|()| registry.served(IMAGE_REPOSITORY, &digest));
        match served {
            Ok(bytes) if bytes == index => {
                lines.push(format!(
                    "pushed {IMAGE_REPOSITORY}:{tag} ({digest}) in attempt {attempt} of \
                     {ATTEMPTS}; the registry serves {name} byte for byte"
                ));
                return Ok(lines);
            }
            Ok(bytes) => {
                return Err(Error::Failed(format!(
                    "{IMAGE_REPOSITORY}@{digest} serves other bytes than {name} ({})",
                    digest_of(&bytes)
                )));
            }
            Err(e) => {
                let why = format!("attempt {attempt} of {ATTEMPTS} failed: {e}");
                if let Some(secs) = WAITS.get(attempt - 1) {
                    wait.wait(*secs, &why);
                }
                lines.push(why);
            }
        }
    }
    Err(Error::Refused(lines))
}

/// The registry, through `skopeo` run by an [`Exec`], signed in already (`skopeo login`).
pub(crate) struct SkopeoCli<E> {
    exec: E,
}

impl<E: Exec> SkopeoCli<E> {
    pub(crate) fn new(exec: E) -> SkopeoCli<E> {
        SkopeoCli { exec }
    }

    fn run(&self, args: Vec<String>) -> Result<Vec<u8>, PortError> {
        stdout_of(&self.exec, "skopeo", &args)
    }
}

impl<E: Exec> Registry for SkopeoCli<E> {
    fn copy(&self, layout: &Path, tag: &str, repository: &str) -> Result<(), PortError> {
        let layout = layout.to_str().ok_or_else(|| {
            PortError::new(
                "skopeo copy",
                format!("the layout {} is not UTF-8", layout.display()),
            )
        })?;
        self.run(vec![
            "copy".to_owned(),
            "--all".to_owned(),
            "--preserve-digests".to_owned(),
            format!("oci:{layout}:{tag}"),
            format!("docker://{repository}:{tag}"),
        ])
        .map(drop)
    }

    fn served(&self, repository: &str, digest: &str) -> Result<Vec<u8>, PortError> {
        self.run(vec![
            "inspect".to_owned(),
            "--raw".to_owned(),
            format!("docker://{repository}@{digest}"),
        ])
    }
}

/// What the real wait says on standard error before it sleeps `secs` after an attempt that failed
/// for `why`.
pub(crate) fn notice(secs: u64, why: &str) -> String {
    format!("{why}; trying again in {secs} seconds")
}

/// The real wait: says [`notice`] on standard error, then sleeps through its function (the wired
/// one is `std::thread::sleep`; a test hands in one that records the duration).
pub(crate) struct Sleep(pub(crate) fn(std::time::Duration));

impl Wait for Sleep {
    fn wait(&self, secs: u64, why: &str) {
        eprintln!("{}", notice(secs, why));
        (self.0)(std::time::Duration::from_secs(secs));
    }
}

#[cfg(test)]
mod tests;
