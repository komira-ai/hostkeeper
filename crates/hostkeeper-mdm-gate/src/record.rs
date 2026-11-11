//! The gate's record of one device (design 6.1), and how each event changes it.
//!
//! A device is enrolled from its first `TokenUpdate` until a `CheckOut`. An `Authenticate`
//! begins a new enrollment: everything the device said in its last one (its serial, its
//! supervision, its ADE and user-approval status, its unanswered commands) is forgotten, and
//! asked again once its `TokenUpdate` makes it enrolled. A `TokenUpdate` after a `CheckOut`, or
//! one NanoMDM counts as the first since an `Authenticate` the gate did not hear, also begins a
//! new enrollment. [`DeviceRecord::enrollment`] counts them.
//!
//! The record keeps the commands the gate sent and the device has not answered, so a result is
//! matched to its command across a restart, and a command is never sent twice while one is
//! unanswered: a Mac that is off piles nothing up in NanoHUB's queue.
//!
//! The enrollment side (design 4 and 5) adds two things: the config profile's `InstallProfile`
//! of this enrollment ([`DeviceRecord::config_profile`]), forgotten with the enrollment, and the
//! last nonce the gate issued for the device ([`DeviceRecord::nonce`]), kept across enrollments,
//! since a nonce lives its hour whatever the device does, unless an approval consumes it. The
//! install (design 7.1 and 5.4) adds the `InstallEnterpriseApplication` sends of this
//! enrollment ([`DeviceRecord::install`]), forgotten with the enrollment, so a new enrollment
//! starts the count again.

use hostkeeper_enrollment::names::KeyHash;

use crate::answer::{DeviceInformation, SecurityInfo};
use crate::command::{DEVICE_INFORMATION, SECURITY_INFO};
use crate::ids::{CommandUuid, EnrollmentId, shown};
use crate::pkg::PkgDigest;

/// Where a device is in its enrollment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// It sent `Authenticate` and no `TokenUpdate` since: not enrolled yet.
    Authenticated,
    /// It sent a `TokenUpdate` and no `CheckOut` since: enrolled.
    Enrolled,
    /// It sent `CheckOut`: not enrolled.
    CheckedOut,
}

/// One command the gate sent that the device has not answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    /// The command's UUID.
    pub uuid: CommandUuid,
    /// The command's request type.
    pub request_type: String,
    /// When the gate asked NanoHUB to queue it, in unix seconds.
    pub sent_unix: u64,
    /// NanoHUB said it queued it. The command is written as sent before NanoHUB is asked, so
    /// that its result is matched however soon it comes; an entry NanoHUB never confirmed (the
    /// coordinator stopped while asking, or the confirmation could not be written) is dropped
    /// at the device's next event, and its query is due again.
    pub queued: bool,
}

/// How a device answered a profile command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileAnswer {
    /// `Acknowledged`; otherwise `Error` or `CommandFormatError`.
    pub acknowledged: bool,
    /// When the answer came, in unix seconds.
    pub at_unix: u64,
}

/// One profile command the gate sent (`InstallProfile` or `RemoveProfile`), and its answer.
///
/// While the device has not answered, the command is also on [`DeviceRecord::sent`]. A
/// command with no answer that is not on that list was never queued (NanoHUB refused it, or
/// the coordinator stopped while asking), and may be sent again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileCommand {
    /// The command's UUID.
    pub uuid: CommandUuid,
    /// When the gate asked NanoHUB to queue it, in unix seconds.
    pub sent_unix: u64,
    /// The device's answer; `None` before one.
    pub answer: Option<ProfileAnswer>,
    /// For the config profile: the SHA-256 (64 lower-case hex digits) of the settings the
    /// command carried. It counts as delivered only once the device acknowledged the command.
    /// `None` on a command that carries none, and on a record written before the field.
    pub settings_sha256: Option<String>,
}

impl ProfileCommand {
    /// The command `uuid`, sent at `now` and not answered.
    #[must_use]
    pub fn new(uuid: CommandUuid, now: u64) -> Self {
        Self {
            uuid,
            sent_unix: now,
            answer: None,
            settings_sha256: None,
        }
    }

    /// This command carrying the settings whose digest is `sha256`.
    #[must_use]
    pub fn with_settings(self, sha256: String) -> Self {
        Self {
            settings_sha256: Some(sha256),
            ..self
        }
    }

    /// When the device acknowledged it; `None` when it did not.
    #[must_use]
    pub fn acknowledged_unix(&self) -> Option<u64> {
        self.answer
            .filter(|answer| answer.acknowledged)
            .map(|answer| answer.at_unix)
    }
}

/// The last nonce the gate issued for a device (design 5.1 and 5.4). The nonce's bytes are not
/// here: only the coordinator's enrollment store knows the nonce, by its SHA-256, and the gate
/// holds the bytes in memory, to push the profile again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonceRecord {
    /// The serial it was issued for.
    pub serial: String,
    /// The pending request it is bound to: the request's key hash.
    pub request_id: KeyHash,
    /// When it was issued, in unix seconds; it is good for an hour from then.
    pub issued_unix: u64,
    /// The device's enrollment it was issued in ([`DeviceRecord::enrollment`]).
    pub enrollment: u32,
    /// How many times NanoHUB queued its profile.
    pub pushes: u32,
    /// The last `InstallProfile` of its profile.
    pub push: Option<ProfileCommand>,
    /// The `RemoveProfile` of its profile, once the request was approved.
    pub removal: Option<ProfileCommand>,
}

/// The pkg sends of one enrollment (design 5.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallRecord {
    /// The digest of the pkg last sent.
    pub sha256: PkgDigest,
    /// How many times NanoHUB queued an `InstallEnterpriseApplication` in this enrollment.
    pub sends: u32,
    /// The last `InstallEnterpriseApplication`, and its answer.
    pub last: ProfileCommand,
}

/// The last `InstallProfile` of one baseline profile (design 6.4), by its `PayloadIdentifier`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineProfile {
    /// The profile's `PayloadIdentifier`.
    pub identifier: String,
    /// The last `InstallProfile` of it; its `settings_sha256` is the digest of the profile's
    /// bytes, so a profile file that changed is sent again.
    pub command: ProfileCommand,
}

/// The last `InstallEnterpriseApplication` of one baseline install, by its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineInstall {
    /// The install's name.
    pub name: String,
    /// The last command of it; its `settings_sha256` is the entry's digest
    /// ([`crate::InstallEntry::digest`]), so an entry that changed is sent again.
    pub command: ProfileCommand,
}

/// What the gate sent a device of its host group's baseline in this enrollment (design 6.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaselineRecord {
    /// The last `EnableRemoteDesktop`.
    pub remote_desktop: Option<ProfileCommand>,
    /// The last `InstallProfile` of each baseline profile.
    pub profiles: Vec<BaselineProfile>,
    /// The last `ProfileList`.
    pub list: Option<ProfileCommand>,
    /// The last `Settings` of the device name; its `settings_sha256` is the digest of the name
    /// it carried, so a changed name is sent again.
    pub name: Option<ProfileCommand>,
    /// The last `InstallEnterpriseApplication` of each baseline install.
    pub installs: Vec<BaselineInstall>,
    /// The `PayloadIdentifier`s of the profiles the last answered `ProfileList` named.
    pub installed: Vec<String>,
    /// When the last `ProfileList` was answered; `None` before one, and after a `TokenUpdate`,
    /// which makes the check due again.
    pub checked_unix: Option<u64>,
}

impl BaselineRecord {
    /// The last send of the profile `identifier`.
    #[must_use]
    pub fn profile(&self, identifier: &str) -> Option<&ProfileCommand> {
        self.profiles
            .iter()
            .find(|profile| profile.identifier == identifier)
            .map(|profile| &profile.command)
    }

    /// The last send of the install `name`.
    #[must_use]
    pub fn install(&self, name: &str) -> Option<&ProfileCommand> {
        self.installs
            .iter()
            .find(|install| install.name == name)
            .map(|install| &install.command)
    }

    /// The command `uuid`, with the name it is shown by when it fails.
    fn command_mut(&mut self, uuid: &str) -> Option<(String, &mut ProfileCommand)> {
        let named = [
            (
                "the baseline's remote desktop",
                self.remote_desktop.as_mut(),
            ),
            ("the baseline's profile list", self.list.as_mut()),
            ("the baseline's device name", self.name.as_mut()),
        ]
        .into_iter()
        .find_map(|(name, command)| {
            command
                .filter(|command| command.uuid.as_str() == uuid)
                .map(|command| (name.to_owned(), command))
        });
        let named = named.or_else(|| {
            self.profiles
                .iter_mut()
                .find(|profile| profile.command.uuid.as_str() == uuid)
                .map(|profile| {
                    (
                        format!("the baseline profile {}", profile.identifier),
                        &mut profile.command,
                    )
                })
        });
        named.or_else(|| {
            self.installs
                .iter_mut()
                .find(|install| install.command.uuid.as_str() == uuid)
                .map(|install| {
                    (
                        format!("the baseline install {}", install.name),
                        &mut install.command,
                    )
                })
        })
    }
}

/// The gate's record of one MDM enrollment id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRecord {
    /// The enrollment id the record is kept under.
    pub enrollment_id: EnrollmentId,
    /// The device's UDID, as its check-ins name it.
    pub udid: String,
    /// The `SerialNumber` of its last `Authenticate`, when it carried one. Shown only: the
    /// serial the gate answers by is [`DeviceRecord::serial`].
    pub authenticate_serial: Option<String>,
    /// The serial its `DeviceInformation` answer gave in this enrollment.
    pub serial: Option<String>,
    /// Where it is in its enrollment.
    pub phase: Phase,
    /// How many enrollments the gate has seen it begin.
    pub enrollment: u32,
    /// `IsSupervised` of its `DeviceInformation` answer in this enrollment.
    pub supervised: Option<bool>,
    /// `EnrolledViaDEP` of its `SecurityInfo` answer in this enrollment.
    pub enrolled_via_dep: Option<bool>,
    /// `UserApprovedEnrollment` of its `SecurityInfo` answer in this enrollment.
    pub user_approved: Option<bool>,
    /// The first `TokenUpdate` of this enrollment, in unix seconds.
    pub enrolled_since_unix: Option<u64>,
    /// Its last check-in, in unix seconds.
    pub last_checkin_unix: u64,
    /// Its `CheckOut`, when it checked out, in unix seconds.
    pub checked_out_unix: Option<u64>,
    /// When it answered `DeviceInformation` in this enrollment, in unix seconds.
    pub device_information_unix: Option<u64>,
    /// When it answered `SecurityInfo` in this enrollment, in unix seconds.
    pub security_info_unix: Option<u64>,
    /// The commands sent and not answered.
    pub sent: Vec<Sent>,
    /// The query types it answered with an error since its last `TokenUpdate`: they are asked
    /// again at the next one, never sooner, so a device that refuses a query is not asked in a
    /// loop.
    pub refused: Vec<String>,
    /// The last thing that went wrong with a command for it: NanoHUB refused it, or the device
    /// answered an error.
    pub last_error: Option<String>,
    /// The `InstallProfile` of the farm's config profile in this enrollment: the last one sent,
    /// which a change of the profile's settings replaces.
    pub config_profile: Option<ProfileCommand>,
    /// The last nonce issued for the device, in this enrollment or an earlier one.
    pub nonce: Option<NonceRecord>,
    /// The pkg sends of this enrollment.
    pub install: Option<InstallRecord>,
    /// The baseline sends of this enrollment.
    pub baseline: Option<BaselineRecord>,
}

impl DeviceRecord {
    /// A record of `id` first heard of at `now`, not enrolled.
    #[must_use]
    pub fn new(id: EnrollmentId, now: u64) -> Self {
        Self {
            enrollment_id: id,
            udid: String::new(),
            authenticate_serial: None,
            serial: None,
            phase: Phase::Authenticated,
            enrollment: 0,
            supervised: None,
            enrolled_via_dep: None,
            user_approved: None,
            enrolled_since_unix: None,
            last_checkin_unix: now,
            checked_out_unix: None,
            device_information_unix: None,
            security_info_unix: None,
            sent: Vec::new(),
            refused: Vec::new(),
            last_error: None,
            config_profile: None,
            nonce: None,
            install: None,
            baseline: None,
        }
    }

    /// Whether the device is enrolled now.
    #[must_use]
    pub fn is_enrolled(&self) -> bool {
        self.phase == Phase::Enrolled
    }

    /// Forgets what the device said in its last enrollment.
    fn forget_enrollment(&mut self) {
        self.serial = None;
        self.supervised = None;
        self.enrolled_via_dep = None;
        self.user_approved = None;
        self.enrolled_since_unix = None;
        self.device_information_unix = None;
        self.security_info_unix = None;
        self.sent.clear();
        self.refused.clear();
        self.config_profile = None;
        self.install = None;
        self.baseline = None;
    }

    fn heard(&mut self, udid: &str, now: u64) {
        if !udid.is_empty() {
            udid.clone_into(&mut self.udid);
        }
        self.last_checkin_unix = now;
    }

    /// `Authenticate` at `now`: a new enrollment begins, not enrolled until its `TokenUpdate`.
    pub fn authenticate(&mut self, udid: &str, serial: Option<String>, now: u64) {
        self.forget_enrollment();
        self.heard(udid, now);
        self.phase = Phase::Authenticated;
        self.authenticate_serial = serial;
        self.checked_out_unix = None;
    }

    /// `TokenUpdate` at `now`, the `tally`-th since an `Authenticate` when NanoMDM counts it.
    /// The first after an `Authenticate` or a `CheckOut` makes the device enrolled, as does a
    /// first one (tally 1) the gate hears while it holds the device enrolled. Either way the
    /// queries it refused are due again.
    pub fn token_update(&mut self, udid: &str, tally: Option<u64>, now: u64) {
        let first = tally == Some(1);
        if !self.is_enrolled() || first {
            if self.is_enrolled() {
                self.forget_enrollment();
            }
            self.phase = Phase::Enrolled;
            self.enrollment = self.enrollment.saturating_add(1);
            self.enrolled_since_unix = Some(now);
            self.checked_out_unix = None;
        }
        self.refused.clear();
        // The baseline's check is due again (design 6.4).
        if let Some(baseline) = self.baseline.as_mut() {
            baseline.checked_unix = None;
        }
        self.heard(udid, now);
    }

    /// `CheckOut` at `now`: no longer enrolled; its unanswered commands are dropped.
    pub fn check_out(&mut self, now: u64) {
        self.heard("", now);
        self.phase = Phase::CheckedOut;
        self.checked_out_unix = Some(now);
        self.sent.clear();
    }

    /// The queries to send now: those of an enrolled device it has not answered in this
    /// enrollment, that are not sent and unanswered, and that it has not refused since its last
    /// `TokenUpdate`. `DeviceInformation` first.
    #[must_use]
    pub fn due(&self) -> Vec<&'static str> {
        if !self.is_enrolled() {
            return Vec::new();
        }
        [
            (DEVICE_INFORMATION, self.device_information_unix),
            (SECURITY_INFO, self.security_info_unix),
        ]
        .into_iter()
        .filter(|(kind, answered)| {
            answered.is_none()
                && !self.sent.iter().any(|sent| sent.request_type == *kind)
                && !self.refused.iter().any(|refused| refused == kind)
        })
        .map(|(kind, _)| kind)
        .collect()
    }

    /// Whether the command `uuid` is sent and not answered.
    #[must_use]
    pub fn is_unanswered(&self, uuid: &CommandUuid) -> bool {
        self.sent.iter().any(|sent| sent.uuid == *uuid)
    }

    /// Drops the commands NanoHUB never confirmed it queued ([`Sent::queued`]).
    pub fn drop_unqueued(&mut self) {
        self.sent.retain(|sent| sent.queued);
    }

    /// The unanswered command `uuid`, taken off the list; `None` when the gate sent no such
    /// command to this device or it was answered already.
    pub fn take_sent(&mut self, uuid: &str) -> Option<Sent> {
        let at = self
            .sent
            .iter()
            .position(|sent| sent.uuid.as_str() == uuid)?;
        Some(self.sent.remove(at))
    }

    /// Notes that the device answered `request_type` with an error, or NanoHUB refused it:
    /// `why` is kept as the last error.
    pub fn refuse(&mut self, request_type: &str, why: &str) {
        if !self.refused.iter().any(|kind| kind == request_type) {
            self.refused.push(request_type.to_owned());
        }
        self.last_error = Some(shown(&format!("{request_type}: {why}")));
    }

    /// The `DeviceInformation` answer `info`, given at `now`. An answer naming another UDID than
    /// the device's is refused, as a malformed one is.
    pub fn answer_device_information(&mut self, info: Option<DeviceInformation>, now: u64) {
        let Some(info) = info else {
            return self.refuse(
                DEVICE_INFORMATION,
                "the answer is not a DeviceInformation result",
            );
        };
        if info
            .udid
            .as_deref()
            .is_some_and(|udid| !self.udid.is_empty() && udid != self.udid)
        {
            return self.refuse(DEVICE_INFORMATION, "the answer names another UDID");
        }
        self.serial = info.serial;
        self.supervised = Some(info.supervised);
        self.device_information_unix = Some(now);
    }

    /// The answer of the profile command or pkg install `uuid` (already taken off
    /// [`DeviceRecord::sent`]):
    /// `acknowledged`, or an error, at `now`. An error is kept as the last error. A UUID that is
    /// no profile command of the record changes nothing.
    pub fn answer_profile(&mut self, uuid: &str, acknowledged: bool, now: u64) {
        let answer = Some(ProfileAnswer {
            acknowledged,
            at_unix: now,
        });
        let nonce = self.nonce.as_mut();
        let (push, removal) = nonce.map_or((None, None), |nonce| {
            (nonce.push.as_mut(), nonce.removal.as_mut())
        });
        let which = [
            ("the config profile", self.config_profile.as_mut()),
            ("the nonce profile", push),
            ("the nonce profile's removal", removal),
            (
                "the pkg install",
                self.install.as_mut().map(|install| &mut install.last),
            ),
        ]
        .into_iter()
        .find_map(|(name, command)| {
            command
                .filter(|command| command.uuid.as_str() == uuid)
                .map(|command| (name.to_owned(), command))
        });
        let which = which.or_else(|| {
            self.baseline
                .as_mut()
                .and_then(|baseline| baseline.command_mut(uuid))
        });
        if let Some((name, command)) = which {
            command.answer = answer;
            if !acknowledged {
                self.last_error = Some(format!("{name}: the device answered an error"));
            }
        }
    }

    /// The answer of the `ProfileList` command `uuid` (already taken off
    /// [`DeviceRecord::sent`]) at `now`: the identifiers `listed` it named, or `None` when the
    /// answer is not a `ProfileList` result, which counts as an error answer.
    pub fn answer_profile_list(&mut self, uuid: &str, listed: Option<Vec<String>>, now: u64) {
        self.answer_profile(uuid, listed.is_some(), now);
        let current = self.baseline.as_mut().filter(|baseline| {
            baseline
                .list
                .as_ref()
                .is_some_and(|list| list.uuid.as_str() == uuid)
        });
        if let (Some(baseline), Some(listed)) = (current, listed) {
            baseline.installed = listed;
            baseline.checked_unix = Some(now);
        }
    }

    /// The `SecurityInfo` answer `info`, given at `now`.
    pub fn answer_security_info(&mut self, info: Option<SecurityInfo>, now: u64) {
        let Some(info) = info else {
            return self.refuse(SECURITY_INFO, "the answer is not a SecurityInfo result");
        };
        self.enrolled_via_dep = info.enrolled_via_dep;
        self.user_approved = info.user_approved;
        self.security_info_unix = Some(now);
    }
}
