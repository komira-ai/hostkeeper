//! The ABM side of the MDM server inside the coordinator (`docs/design/coordinator-mdm.md`, 4.5):
//! the sync of Apple Business Manager's device list into `abm_devices`, the ADE profile and its
//! assignment, and the token and key the operator hands over.
//!
//! **Parts.** [`oauth`]: the HMAC-SHA1 signature that opens a session. [`transport`]: the port to
//! `mdmenrollment.apple.com` and its HTTPS implementation; [`fake`] (tests) is the scripted one.
//! [`client`]: the session, the retry rules and the calls. [`profile`]: the ADE profile. [`keys`]
//! and [`p7m`]: the RSA key pair and the CMS envelope the token comes in. [`Abm`] here: one pass of
//! the sync ([`Abm::sync_once`]), the token upload ([`Abm::accept_token`]), the key pair
//! ([`Abm::public_certificate`]) and the question the approval of a Mac asks ([`Abm::serial_in_abm`]).
//!
//! **A pass.** The cursor in `abm_state` says where the last pass ended. With none, or when the
//! last pass is older than a cursor lives (seven days), or when ABM answers a cursor error (`INVALID_CURSOR` and the other `*_CURSOR` codes), the
//! pass is a full fetch (`POST /server/devices`, pages of 1000), after which every serial the
//! table holds and ABM did not list is marked removed; the cursor of a full fetch seeds
//! `POST /devices/sync`. Every other pass is `POST /devices/sync`, whose records say `added`,
//! `modified` or `deleted`. The devices and the new cursor are written in one change. Then, when
//! the setting `mdm_name` names the host Macs enroll at, the ADE profile is defined at ABM if its
//! digest differs from the stored one, and every device that has no profile gets it
//! (`POST /profile/devices`, `PUT` when ABM answers the `POST` with a `4xx`).
//!
//! **Single timer.** [`sync_every`] runs a pass every `abm_sync_s` seconds. Running it under the
//! store's lease, so that two coordinators do not both sync, is CM19; the coordinator refuses to
//! serve MDM beside another one until then.
//!
//! **Not probed.** The assignment writes to ABM and the spike did not make one: its tests use the
//! fake, and nothing here makes a write to Apple on its own before an operator has uploaded a
//! token and named the MDM host.

pub(crate) mod client;
#[cfg(test)]
pub(crate) mod fake;
pub(crate) mod keys;
pub(crate) mod oauth;
pub(crate) mod p7m;
pub(crate) mod profile;
pub(crate) mod transport;
pub(crate) mod wire;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_oidc::Clock;

use self::client::{AbmClient, AbmError, PAGE_LIMIT, Pacing, Sleep};
use self::profile::{AdeProfile, is_host_name};
use self::transport::AbmTransport;
use self::wire::{Account, DeviceRecord};
use crate::enrollment::Random;
use crate::mdm_secrets::{AbmToken, SecretKind};
use crate::mdm_settings;
use crate::mdm_setup::{MdmSetup, SetupFailure};
use crate::store::mdm::{AbmBatch, AbmDevice, AbmState, MdmError, MdmStore};

/// How long ABM keeps a cursor: seven days. A pass that starts later asks for a full fetch.
const CURSOR_LIFE_SECS: i64 = 7 * 86_400;

/// What a pass found and did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SyncReport {
    /// Whether it was a full fetch.
    pub(crate) full: bool,
    /// Devices written to the table.
    pub(crate) upserted: usize,
    /// Devices marked removed.
    pub(crate) removed: usize,
    /// Whether an ADE profile was defined at ABM.
    pub(crate) profile_defined: bool,
    /// Devices that were given the profile.
    pub(crate) assigned: usize,
    /// Devices ABM did not give the profile (`NOT_ACCESSIBLE`, `FAILED`); the next pass tries again.
    pub(crate) unassigned: usize,
}

/// What a pass came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    /// No ABM token is held: nothing was asked of ABM.
    NoToken,
    /// The pass ran.
    Done(SyncReport),
}

/// Why a pass or a token check failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SyncError {
    /// The setup could not be read, or the token did not open.
    #[error(transparent)]
    Setup(#[from] SetupFailure),
    /// The table could not be read or written.
    #[error(transparent)]
    Store(#[from] MdmError),
    /// ABM did not answer as the pass needs.
    #[error(transparent)]
    Abm(#[from] AbmError),
    /// The held token is not the five-part JSON.
    #[error("the ABM token held is not readable")]
    Token,
}

/// When the last pass ran and what it said, for the status the setup page shows (CM21).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SyncStatus {
    /// When the last pass ended without an error.
    pub(crate) last_ok_unix: Option<u64>,
    /// What the last pass failed with; none after a pass that did not.
    pub(crate) last_error: Option<String>,
}

/// The ABM client of the coordinator: the store, the session, and the status of the last pass.
pub(crate) struct Abm {
    store: Arc<dyn MdmStore>,
    client: AbmClient,
    now: Clock,
    status: Mutex<SyncStatus>,
}

impl Abm {
    /// The client over `store`, speaking through `transport`; `now` is the clock the OAuth
    /// timestamp and the table's times read.
    pub(crate) fn new(
        store: Arc<dyn MdmStore>,
        transport: Arc<dyn AbmTransport>,
        now: Clock,
        random: Random,
        sleep: Sleep,
    ) -> Self {
        Self {
            store,
            client: AbmClient::new(transport, Arc::clone(&now), random, sleep),
            now,
            status: Mutex::new(SyncStatus::default()),
        }
    }

    /// The status of the last pass.
    pub(crate) fn status(&self) -> SyncStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Whether `serial` is in our ABM now: in the table and not marked removed. The approval of a
    /// Mac asks it (CM9), and so does the enrollment of one (CM12c): a serial that is not is
    /// declined `serial_not_in_abm`. The table is the sync's last word; nothing is asked of ABM.
    ///
    /// # Errors
    /// [`MdmError`]: the table cannot be read.
    pub(crate) fn serial_in_abm(&self, serial: &str) -> Result<bool, MdmError> {
        Ok(self
            .store
            .abm_device(serial)?
            .is_some_and(|device| device.removed_unix.is_none()))
    }

    /// The token held, opened; none when none is held.
    fn token(setup: &MdmSetup) -> Result<Option<AbmToken>, SyncError> {
        let Some(secret) = setup.open(SecretKind::AbmToken)? else {
            return Ok(None);
        };
        AbmToken::parse(secret.expose())
            .map(Some)
            .map_err(|_| SyncError::Token)
    }

    /// The pacing the settings say.
    fn pacing(set: &BTreeMap<String, String>) -> Pacing {
        let secs = |name, fallback| u64::from(mdm_settings::number_of(set, name, fallback));
        Pacing {
            timeout: Duration::from_secs(secs("abm_request_timeout_s", 30)),
            retries: mdm_settings::number_of(set, "abm_retries", 3),
            max_wait: Duration::from_secs(secs("abm_sync_s", 300)),
        }
    }

    /// Proves `token` to ABM: opens a session with it and reads the account, so that a token that
    /// is not good is refused before it is kept.
    ///
    /// # Errors
    /// [`SyncError`]: the settings cannot be read, or [`AbmError`], whose faults the caller names
    /// to the operator ([`AbmError::CredentialRefused`], [`AbmError::ClockRefused`]).
    pub(crate) async fn accept_token(
        &self,
        setup: &MdmSetup,
        token: &AbmToken,
    ) -> Result<Account, SyncError> {
        let pacing = Self::pacing(&setup.settings()?);
        Ok(self.client.api(token, pacing).account().await?)
    }

    /// One pass of the sync, with the token the setup holds.
    ///
    /// # Errors
    /// [`SyncError`]. What the pass had done before the failure stays written: the table and the
    /// cursor move together, and the next pass starts from them.
    pub(crate) async fn sync_once(&self, setup: &MdmSetup) -> Result<Pass, SyncError> {
        let Some(token) = Self::token(setup)? else {
            return Ok(Pass::NoToken);
        };
        let set = setup.settings()?;
        let api = self.client.api(&token, Self::pacing(&set));
        let now = i64::try_from((self.now)()).unwrap_or(i64::MAX);
        let state = self.store.abm_state()?.unwrap_or(AbmState {
            cursor: String::new(),
            profile_uuid: None,
            profile_digest: None,
            synced_unix: None,
        });
        let mut report = SyncReport::default();

        let stale = state
            .synced_unix
            .is_none_or(|at| now.saturating_sub(at) > CURSOR_LIFE_SECS);
        let fetched = if state.cursor.is_empty() || stale {
            full_fetch(&api).await?
        } else {
            match changes(&api, &state.cursor).await {
                Err(AbmError::InvalidCursor) => full_fetch(&api).await?,
                other => other?,
            }
        };
        report.full = fetched.full;
        let mut state = AbmState {
            cursor: fetched.cursor,
            synced_unix: Some(now),
            ..state
        };
        let batch = self.page_batch(&fetched.records, fetched.full, state.clone(), now)?;
        report.upserted = batch.upserts.len();
        report.removed = batch.removed.len();
        self.store.abm_apply(&batch)?;

        let Some(wanted) = wanted_profile(&set) else {
            return Ok(Pass::Done(report));
        };
        if state.profile_uuid.is_none() || state.profile_digest.as_deref() != Some(&wanted.digest())
        {
            let uuid = api.define_profile(&wanted.json()).await?;
            state.profile_uuid = Some(uuid);
            state.profile_digest = Some(wanted.digest());
            self.store.abm_apply(&AbmBatch {
                state: Some(state.clone()),
                ..AbmBatch::default()
            })?;
            report.profile_defined = true;
        }
        let uuid = state.profile_uuid.clone().unwrap_or_default();
        self.assign_all(&api, &uuid, now, &mut report).await?;
        Ok(Pass::Done(report))
    }

    /// The batch of one fetch: its devices, and for a full fetch the serials it left out.
    fn page_batch(
        &self,
        records: &[DeviceRecord],
        full: bool,
        state: AbmState,
        now: i64,
    ) -> Result<AbmBatch, MdmError> {
        let mut upserts = BTreeMap::new();
        let mut removed = BTreeSet::new();
        for record in records {
            if record.is_removed() {
                upserts.remove(&record.serial_number);
                removed.insert(record.serial_number.clone());
            } else {
                removed.remove(&record.serial_number);
                upserts.insert(record.serial_number.clone(), device_of(record, now));
            }
        }
        if full {
            for held in self.store.abm_devices()? {
                if held.removed_unix.is_none() && !upserts.contains_key(&held.serial) {
                    removed.insert(held.serial);
                }
            }
        }
        Ok(AbmBatch {
            upserts: upserts.into_values().collect(),
            removed: removed.into_iter().collect(),
            removed_unix: now,
            state: Some(state),
        })
    }

    /// Gives `uuid` to every device of the table that is ours and has no profile.
    async fn assign_all(
        &self,
        api: &client::Api<'_>,
        uuid: &str,
        now: i64,
        report: &mut SyncReport,
    ) -> Result<(), SyncError> {
        let waiting: Vec<AbmDevice> = self
            .store
            .abm_devices()?
            .into_iter()
            .filter(|device| {
                device.removed_unix.is_none()
                    && (device.profile_uuid.is_none()
                        || matches!(device.profile_status.as_str(), "empty" | "removed"))
            })
            .collect();
        for chunk in waiting.chunks(PAGE_LIMIT as usize) {
            let serials: Vec<String> = chunk.iter().map(|device| device.serial.clone()).collect();
            let answer = api.assign(uuid, &serials).await?;
            let mut upserts = Vec::new();
            for device in chunk {
                let done =
                    answer.devices.get(&device.serial).map(String::as_str) == Some("SUCCESS");
                if done {
                    upserts.push(AbmDevice {
                        profile_status: "assigned".to_owned(),
                        profile_uuid: Some(uuid.to_owned()),
                        synced_unix: now,
                        ..device.clone()
                    });
                } else {
                    report.unassigned += 1;
                }
            }
            report.assigned += upserts.len();
            self.store.abm_apply(&AbmBatch {
                upserts,
                removed_unix: now,
                ..AbmBatch::default()
            })?;
        }
        Ok(())
    }

    /// A pass, with its result noted in the status and logged.
    pub(crate) async fn sync_noted(&self, setup: &MdmSetup) {
        let outcome = self.sync_once(setup).await;
        let mut status = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        match outcome {
            Ok(Pass::NoToken) => {}
            Ok(Pass::Done(report)) => {
                tracing::info!(?report, "the ABM sync ran");
                status.last_ok_unix = Some((self.now)());
                status.last_error = None;
            }
            Err(why) => {
                tracing::error!(%why, "the ABM sync failed");
                status.last_error = Some(why.to_string());
            }
        }
    }
}

/// What a fetch of devices came to.
struct Fetched {
    full: bool,
    cursor: String,
    records: Vec<DeviceRecord>,
}

/// Every page of a full fetch: its devices, and the cursor of the last page.
async fn full_fetch(api: &client::Api<'_>) -> Result<Fetched, AbmError> {
    let mut records = Vec::new();
    let mut page = api.fetch(None).await?;
    loop {
        records.append(&mut page.devices);
        if !page.more_to_follow {
            return Ok(Fetched {
                full: true,
                cursor: page.cursor,
                records,
            });
        }
        page = api.fetch(Some(&page.cursor)).await?;
    }
}

/// Every page of changes after `cursor`.
async fn changes(api: &client::Api<'_>, cursor: &str) -> Result<Fetched, AbmError> {
    let mut records = Vec::new();
    let mut page = api.changes(cursor).await?;
    loop {
        records.append(&mut page.devices);
        if !page.more_to_follow {
            return Ok(Fetched {
                full: false,
                cursor: page.cursor,
                records,
            });
        }
        page = api.changes(&page.cursor).await?;
    }
}

/// The table's row of a device ABM listed.
fn device_of(record: &DeviceRecord, now: i64) -> AbmDevice {
    AbmDevice {
        serial: record.serial_number.clone(),
        model: record.model.clone(),
        description: record.description.clone(),
        profile_status: record
            .profile_status
            .clone()
            .unwrap_or_else(|| "empty".to_owned()),
        profile_uuid: record.profile_uuid.clone(),
        first_seen_unix: now,
        synced_unix: now,
        removed_unix: None,
    }
}

/// The ADE profile the settings want: none until `mdm_name` names a host name.
fn wanted_profile(set: &BTreeMap<String, String>) -> Option<AdeProfile> {
    let host = set.get("mdm_name").filter(|name| is_host_name(name))?;
    let name = mdm_settings::text_of(set, "ade_profile_name")?;
    Some(AdeProfile::new(name, host))
}

/// Runs a pass every `abm_sync_s` seconds, for as long as it is polled.
pub(crate) async fn sync_every(
    abm: Arc<Abm>,
    setup: Arc<MdmSetup>,
    sleep: Sleep,
) -> std::convert::Infallible {
    loop {
        abm.sync_noted(&setup).await;
        let secs = setup.settings().map_or(300, |set| {
            u64::from(mdm_settings::number_of(&set, "abm_sync_s", 300))
        });
        sleep(Duration::from_secs(secs)).await;
    }
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
