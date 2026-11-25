//! The operator's side of the Xcode store (`docs/design/maintenance-updates.md`, sections 2.2 and
//! 4.2): `Xcode.List` and `Xcode.Remove` of `hostkeeper.operator.v1`, the pruning of the store to
//! what its groups keep, and the alert `xcode_version_taken`. The upload is [`crate::xcode_put`]
//! and the read a Mac makes [`crate::xcode_get`].
//!
//! Both calls take the operator token only: a CI token is `forbidden`, and no token, or any
//! other, `unauthorized`. `List` reads and writes no audit entry; without a store (`--xcode-dir`)
//! it answers `configured` false. `Remove` is audited as `xcode_remove`, the entry written before
//! the files are removed, so that a removal whose audit line cannot be appended is refused
//! `audit_unavailable` and not made.
//!
//! The store keeps as many of the newest releases as the largest `xcode_keep` of any group (at
//! least one) and each release a host reports installed ([`crate::xcode_store::kept`]); the rest
//! is removed after each stored upload and by the daily job `xcode-feed`
//! ([`Coordinator::prune_xcode_store`]), each removal an `xcode_remove` entry by `coordinator`.

use std::collections::BTreeSet;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::XcodeRefusal as Refusal;
use hostkeeper_wire::HostId;

use crate::alerts::{Alert, Kind, Said, Subject};
use crate::audit_actions::{XCODE_REMOVE, about};
use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::Write;
use crate::reconcile::COORDINATOR;
use crate::service::Coordinator;
use crate::xcode_feed::{XcodeId, XcodeVersion};
use crate::xcode_store::{Archive, Taken, kept};

/// `archive` as the audit says it concerned: `Xcode 27.2 (27B52) <sha256>`.
fn subject_of(archive: &Archive) -> String {
    format!(
        "Xcode {} ({}) {}",
        archive.id.version, archive.id.build, archive.sha256
    )
}

/// `archive` as `Xcode.List` says it.
fn archive_proto(archive: &Archive) -> proto::XcodeArchive {
    proto::XcodeArchive {
        sha256: archive.sha256.clone(),
        version: archive.id.version.to_string(),
        build: archive.id.build.clone(),
        size_bytes: archive.size,
        uploaded_by: archive.by.clone(),
        uploaded_at_unix: archive.at_unix,
    }
}

/// `xcode_version_taken`: one alert for each version whose second build was refused, naming the
/// version, the build the store holds and the build that was refused.
pub(crate) fn version_taken(taken: &[Taken]) -> Vec<Alert> {
    taken
        .iter()
        .map(|taken| Alert {
            subject: Subject {
                version: taken.version.clone(),
                ..Subject::of(Kind::XcodeVersionTaken)
            },
            message: format!(
                "an upload of Xcode {} build {} was refused: the Xcode store holds build {} of \
                 that version, and a version names one build on our hosts; remove the held \
                 archive with `hk xcode remove` before pushing the other build",
                taken.version, taken.refused_build, taken.held_build
            ),
            said: Said::default(),
        })
        .collect()
}

impl Coordinator {
    /// `Xcode.List`: the archives the store holds, newest release first, with the release the
    /// alert `xcode_release_waiting` names, how many releases the store keeps and the largest
    /// archive it takes. Without a store the answer is `configured` false, with no archive.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `store_failed` (the store's directory could not be read).
    pub(crate) fn xcode_list(
        &self,
        caller: &Caller,
        _request: &proto::ListXcodeRequest,
    ) -> Result<proto::ListXcodeResponse, Refusal> {
        caller.operator()?;
        let waiting = self
            .xcode
            .release_waiting()
            .map(|release| proto::XcodeReleaseWaiting {
                version: release.id.version.to_string(),
                build: release.id.build,
                requires_macos: release.requires_macos,
            });
        let Some(store) = &self.xcode.store else {
            return Ok(proto::ListXcodeResponse {
                configured: false,
                archives: Vec::new(),
                waiting,
                keep: 0,
                max_bytes: 0,
            });
        };
        let archives = store.archives().map_err(|why| {
            tracing::error!(%why, "the Xcode store could not be listed");
            Refusal::StoreFailed
        })?;
        Ok(proto::ListXcodeResponse {
            configured: true,
            archives: archives.iter().map(archive_proto).collect(),
            waiting,
            keep: self.xcode_keep_largest().max(1),
            max_bytes: store.max_bytes(),
        })
    }

    /// `Xcode.Remove`: removes the archive `sha256` and its record, and with them the refusal of
    /// a second build of its version. One `xcode_remove` entry, by the operator, with the
    /// operator's reason and the release and archive as its subject, is appended before any file
    /// is removed.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (not 64 lowercase hex characters, or a
    /// reason that is not 1 to 256 bytes with no control character), `no_store`,
    /// `unknown_archive`, `store_failed`, `audit_unavailable`.
    pub(crate) fn xcode_remove(
        &self,
        caller: &Caller,
        request: proto::RemoveXcodeRequest,
    ) -> Result<proto::RemoveXcodeResponse, Refusal> {
        let by = caller.operator()?;
        if !hostkeeper_release::grammar::is_sha256(&request.sha256) {
            return Err(Refusal::InvalidRequest);
        }
        let reason = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let store = self.xcode.store.as_ref().ok_or(Refusal::NoStore)?;
        let _changing = store.changing();
        let archive = store
            .archive(&request.sha256)
            .map_err(|why| {
                tracing::error!(%why, "an Xcode archive's record could not be read");
                Refusal::StoreFailed
            })?
            .ok_or(Refusal::UnknownArchive)?;
        let mut entry = about(by, XCODE_REMOVE, &subject_of(&archive));
        entry.reason = Some(reason);
        let done = self.ops.change(Write::BeforeMemory, entry, |_| {})?;
        store.remove(&archive).map_err(|why| {
            tracing::error!(%why, "an Xcode archive could not be removed");
            Refusal::StoreFailed
        })?;
        tracing::info!(
            version = %archive.id.version,
            build = %archive.id.build,
            sha256 = %archive.sha256,
            "an Xcode archive was removed"
        );
        Ok(proto::RemoveXcodeResponse {
            entry: Some(entry_proto(&done)),
        })
    }

    /// The most Xcode releases any group keeps (`xcode_keep` of its maintenance table); 0 when no
    /// group keeps any.
    pub(crate) fn xcode_keep_largest(&self) -> u32 {
        self.desired
            .host_groups()
            .0
            .values()
            .map(|group| group.policy.maintenance.xcode_keep)
            .max()
            .unwrap_or(0)
    }

    /// How many Xcode releases the group of `host` keeps; 0 for a host in no group, and on a
    /// coordinator without groups.
    pub(crate) fn xcode_keep_of(&self, host: &HostId) -> u32 {
        let Some((group, _)) = self.membership_of(host) else {
            return 0;
        };
        self.desired
            .host_group(&group)
            .map_or(0, |group| group.policy.maintenance.xcode_keep)
    }

    /// The Xcode releases the hosts report installed (`MaintenanceReport.xcodes` of each kept
    /// report), which the store keeps whatever its groups keep: each by its version and build,
    /// in its own directory or not; one whose version is not a version number names no release.
    pub(crate) fn xcodes_installed(&self) -> BTreeSet<XcodeId> {
        self.lock_fleet()
            .nodes()
            .filter_map(|(_, node)| node.maintenance.as_ref())
            .flat_map(|maintenance| maintenance.report().xcodes.iter())
            .filter_map(|xcode| {
                Some(XcodeId {
                    version: XcodeVersion::parse(&xcode.version).ok()?,
                    build: xcode.build.clone(),
                })
            })
            .collect()
    }

    /// Removes from the store the archives it no longer keeps ([`kept`]: the newest
    /// [`Self::xcode_keep_largest`] releases, at least one, and each installed one). Each
    /// removal is audited first, as `xcode_remove` by `coordinator`, and then made; one that
    /// cannot be audited or made is a log line and the archive stays for the next run. Without a
    /// store it does nothing.
    pub(crate) fn prune_xcode_store(&self) {
        let Some(store) = &self.xcode.store else {
            return;
        };
        let _changing = store.changing();
        let archives = match store.archives() {
            Ok(archives) => archives,
            Err(why) => {
                tracing::error!(%why, "the Xcode store could not be listed to prune it");
                return;
            }
        };
        let (_, goes) = kept(
            &archives,
            self.xcode_keep_largest(),
            &self.xcodes_installed(),
        );
        for archive in goes {
            let mut entry = about(COORDINATOR, XCODE_REMOVE, &subject_of(&archive));
            entry.reason = Some(format!(
                "pruned: Xcode {} ({}), sha256 {}",
                archive.id.version, archive.id.build, archive.sha256
            ));
            if let Err(why) = self.ops.change(Write::BeforeMemory, entry, |_| {}) {
                tracing::error!(%why, sha256 = %archive.sha256, "a pruned Xcode archive could not be audited: it stays");
                continue;
            }
            match store.remove(&archive) {
                Ok(()) => tracing::info!(
                    version = %archive.id.version,
                    build = %archive.id.build,
                    sha256 = %archive.sha256,
                    "an Xcode archive was pruned"
                ),
                Err(why) => {
                    tracing::error!(%why, sha256 = %archive.sha256, "a pruned Xcode archive could not be removed");
                }
            }
        }
    }

    /// The refusals of a second build under a version the store holds that stand
    /// ([`crate::xcode_store::XcodeStore::taken`]); none without a store.
    pub(crate) fn xcode_taken(&self) -> Vec<Taken> {
        self.xcode
            .store
            .as_ref()
            .map_or_else(Vec::new, |store| store.taken())
    }
}

#[cfg(test)]
#[path = "xcode_service_tests.rs"]
mod tests;
