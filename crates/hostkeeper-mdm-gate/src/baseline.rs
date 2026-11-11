//! The enrollment baseline (docs/design/hostkeeper-mdm.md section 6.4, console-broker.md
//! section 10): what the gate sets on a Mac at enrollment besides the config profile and the
//! pkg, and keeps.
//!
//! **The file.** `--mdm-baseline-file` is a TOML file of root's that no one else can write, one
//! `[[group]]` per host group, each with `[[group.entry]]` rows, an entry being
//! `remote_desktop = true` or `profile = "<path>"` of an unsigned `.mobileconfig`, a file of
//! root's too, read at start. The host group of a Mac is the one its allowlist entry names
//! ([`crate::Enrollments::host_group`]); a Mac whose group the file does not list gets no
//! baseline.
//!
//! **When it is sent.** At a look at an enrolled Mac that the enrollment side acts on
//! ([`crate::enroll`]) and that acknowledged the config profile, whether or not its request
//! is approved yet: a `profile` entry is an `InstallProfile`, and `remote_desktop` an
//! `EnableRemoteDesktop`, sent only to a Mac whose `DeviceInformation` answer said it is
//! supervised (Apple's schema marks the command `supervised: true`). Each is sent once per
//! enrollment, queued with the pkg, and each send NanoHUB queued is audited
//! [`ACTION_BASELINE`]. A Mac enrolled before the baseline existed is sent it at its next look.
//!
//! **The name and the installs** are not in the file: the gate adds them to every group's
//! entries. The device name is a `Settings` command that sets `DeviceName` and `HostName` to the
//! host id of the Mac's allowlist entry ([`crate::Enrollments::host_name`]), for a supervised Mac
//! (Apple: `DeviceName` is for supervised devices); a Mac the enrollment names nothing for gets
//! none, and a changed name is sent again. Each install of the setting `installs`
//! ([`crate::Installs`]) except the hostkeeper pkg, which [`crate::install`] sends, is an
//! `InstallEnterpriseApplication` of its inline manifest, sent once per entry
//! ([`crate::InstallEntry::digest`]) and again when the entry changes, or, after an error answer,
//! at the check interval. A Mac's baseline is therefore changed by changing the setting, and
//! shows `Pending` until the new entry is acknowledged. A pkg file that is missing or whose bytes
//! are not its digest is not sent and is kept as the record's last error.
//!
//! **How it is kept.** A `ProfileList` command is sent when every profile entry has been
//! acknowledged and none is outstanding, at the Mac's next `TokenUpdate` and every
//! [`DEFAULT_CHECK_SECS`] (`--mdm-baseline-check-s`) after its last answer. A profile entry whose
//! identifier the answer does not list is sent again; one the Mac answered with an error, or
//! whose file changed since it was acknowledged, is sent again too (the error one after the check
//! interval). `EnableRemoteDesktop` has no read-back in Apple's schema (no `DeviceInformation`
//! query reports Screen Sharing), so it is sent again at the same interval after its last
//! acknowledged answer. A command NanoHUB never queued is sent at the next look, and one the
//! Mac has not answered is never sent again.
//!
//! **What it reports.** [`Gate::baseline_status`], for `Mdm.ListDevices`: `Pending` (an entry
//! not sent, not yet answered, or no config profile yet), `Failed` with the reason (the Mac
//! answered an entry with an error, or `remote_desktop` is listed for a Mac that is not
//! supervised) and `Applied` (every entry acknowledged, and none missing from the last
//! `ProfileList`). A baseline never fails an enrollment: nothing in this module touches the
//! nonce, the approval or the pkg.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use hostkeeper_enrollment::fs::Fs;
use plist::Value;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::command::Command;
use crate::enroll::EnrollSide;
use crate::gate::Gate;
use crate::ids::shown;
use crate::installs::InstallEntry;
use crate::pkg::{Install, manifest};
use crate::profile::ProfileIdentifier;
use crate::record::{BaselineInstall, BaselineRecord, DeviceRecord, ProfileCommand};

/// The coordinator flag that names the baseline file.
pub const FILE_FLAG: &str = "--mdm-baseline-file";

/// The coordinator flag that sets the check interval.
pub const CHECK_FLAG: &str = "--mdm-baseline-check-s";

/// The check interval when the flag is not given: six hours.
pub const DEFAULT_CHECK_SECS: u64 = 21_600;

/// The audit action of a baseline command NanoHUB queued.
pub const ACTION_BASELINE: &str = "mdm_baseline";

/// One profile entry of a group: the profile file's bytes, as sent.
#[derive(Clone, PartialEq, Eq)]
pub struct ProfileItem {
    /// The profile's `PayloadIdentifier`.
    pub identifier: String,
    bytes: Vec<u8>,
    sha256: String,
}

impl std::fmt::Debug for ProfileItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileItem")
            .field("identifier", &self.identifier)
            .field("sha256", &self.sha256)
            .finish_non_exhaustive()
    }
}

/// One entry of a group's baseline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// `EnableRemoteDesktop`, for a supervised Mac.
    RemoteDesktop,
    /// An `InstallProfile` of a profile file.
    Profile(ProfileItem),
}

/// The baseline of every host group, and how often it is checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Baseline {
    groups: BTreeMap<String, Vec<Item>>,
    check_secs: u64,
}

/// Why the baseline file is refused; the coordinator does not start.
#[derive(Debug, thiserror::Error)]
pub enum BaselineError {
    /// The baseline file or a profile file cannot be read.
    #[error("{FILE_FLAG} {path}: {source}")]
    Read {
        /// The file.
        path: String,
        /// What the system said.
        #[source]
        source: std::io::Error,
    },
    /// A file is not root's alone.
    #[error(
        "{FILE_FLAG} {path}: {why}; the baseline and its profiles must be files of root's that no one else can write"
    )]
    Untrusted {
        /// The file.
        path: String,
        /// What is wrong with it.
        why: &'static str,
    },
    /// The text breaks a rule of the baseline.
    #[error("{FILE_FLAG}: {0}")]
    Invalid(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileToml {
    #[serde(default)]
    group: Vec<GroupToml>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupToml {
    host_group: String,
    #[serde(default)]
    entry: Vec<EntryToml>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryToml {
    remote_desktop: Option<bool>,
    profile: Option<std::path::PathBuf>,
}

/// The bytes of the file `path` of `fs`, which must be a regular file of root's (uid 0) that
/// neither its group nor others can write.
fn trusted(fs: &dyn Fs, path: &Path) -> Result<Vec<u8>, BaselineError> {
    let shown = path.display().to_string();
    let read = fs
        .read_private(path)
        .and_then(|read| read.ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound)));
    let (bytes, stat) = read.map_err(|source| BaselineError::Read {
        path: shown.clone(),
        source,
    })?;
    let untrusted = |why| BaselineError::Untrusted {
        path: shown.clone(),
        why,
    };
    if !stat.is_file {
        return Err(untrusted("it is not a regular file"));
    }
    if stat.uid != 0 {
        return Err(untrusted("it is not owned by root"));
    }
    if stat.mode & 0o022 != 0 {
        return Err(untrusted("its group or others can write it"));
    }
    Ok(bytes)
}

/// The profile entry of the file `path`: an unsigned property list of a `Configuration`
/// profile with a `PayloadIdentifier`.
fn profile_item(fs: &dyn Fs, path: &Path) -> Result<ProfileItem, BaselineError> {
    let invalid = |why: &str| BaselineError::Invalid(format!("profile {}: {why}", path.display()));
    let bytes = trusted(fs, path)?;
    let dict = Value::from_reader(std::io::Cursor::new(&bytes))
        .ok()
        .and_then(Value::into_dictionary)
        .ok_or_else(|| invalid("it is not an unsigned property list of a profile"))?;
    if dict.get("PayloadType").and_then(Value::as_string) != Some("Configuration") {
        return Err(invalid("its PayloadType is not Configuration"));
    }
    let identifier = dict
        .get("PayloadIdentifier")
        .and_then(Value::as_string)
        .filter(|identifier| !identifier.is_empty())
        .ok_or_else(|| invalid("it has no PayloadIdentifier"))?
        .to_owned();
    let sha256 = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(ProfileItem {
        identifier,
        bytes,
        sha256,
    })
}

impl Baseline {
    /// The baseline the TOML `text` says, its profile files read through `fs`; `identifier` is
    /// the farm's profile identifier, whose `.config` and `.nonce` profiles a baseline profile
    /// may not replace.
    ///
    /// # Errors
    /// [`BaselineError`]: not the file's shape, no group, a group listed twice or with no
    /// entry, an entry that is neither `remote_desktop = true` nor a `profile`, or both, a
    /// profile that is not readable, root's alone and an unsigned `Configuration` profile, an
    /// identifier twice in a group, or the identifier of the gate's own profiles.
    pub fn parse(
        text: &str,
        fs: &dyn Fs,
        identifier: &ProfileIdentifier,
    ) -> Result<Self, BaselineError> {
        let invalid = BaselineError::Invalid;
        let file: FileToml = toml::from_str(text).map_err(|e| invalid(e.message().to_owned()))?;
        if file.group.is_empty() {
            return Err(invalid("the file lists no group ([[group]])".to_owned()));
        }
        let own = [identifier.config(), identifier.nonce()];
        let mut groups = BTreeMap::new();
        for group in file.group {
            let name = group.host_group;
            let mut items = Vec::new();
            for entry in group.entry {
                match (entry.remote_desktop, entry.profile) {
                    (Some(true), None) => {
                        if items.contains(&Item::RemoteDesktop) {
                            return Err(invalid(format!(
                                "group {name}: remote_desktop is listed twice"
                            )));
                        }
                        items.push(Item::RemoteDesktop);
                    }
                    (None, Some(path)) => {
                        let item = profile_item(fs, &path)?;
                        if own.contains(&item.identifier) {
                            return Err(invalid(format!(
                                "group {name}: profile {} has the identifier {}, which is the gate's own",
                                path.display(),
                                item.identifier
                            )));
                        }
                        let twice = items.iter().any(|other| {
                            matches!(other, Item::Profile(other) if other.identifier == item.identifier)
                        });
                        if twice {
                            return Err(invalid(format!(
                                "group {name}: the identifier {} is listed twice",
                                item.identifier
                            )));
                        }
                        items.push(Item::Profile(item));
                    }
                    _ => {
                        return Err(invalid(format!(
                            "group {name}: an entry is exactly one of remote_desktop = true and profile = \"<path>\""
                        )));
                    }
                }
            }
            if items.is_empty() {
                return Err(invalid(format!("group {name} has no entry")));
            }
            if groups.insert(name.clone(), items).is_some() {
                return Err(invalid(format!("group {name} is listed twice")));
            }
        }
        Ok(Self {
            groups,
            check_secs: DEFAULT_CHECK_SECS,
        })
    }

    /// The baseline in the file `path` of `fs`, which must be a regular file of root's that no
    /// one else can write.
    ///
    /// # Errors
    /// [`BaselineError`]: the file cannot be read, is not root's alone, or [`Self::parse`]
    /// refuses it.
    pub fn load(
        fs: &dyn Fs,
        path: &Path,
        identifier: &ProfileIdentifier,
    ) -> Result<Self, BaselineError> {
        let bytes = trusted(fs, path)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| BaselineError::Invalid("the file is not UTF-8".to_owned()))?;
        Self::parse(&text, fs, identifier)
    }

    /// The same baseline checked every `secs` seconds.
    #[must_use]
    pub fn with_check_secs(self, secs: u64) -> Self {
        Self {
            check_secs: secs,
            ..self
        }
    }

    /// The host groups the file lists.
    pub fn groups(&self) -> impl Iterator<Item = &str> {
        self.groups.keys().map(String::as_str)
    }

    /// The entries of the host group `name`.
    #[must_use]
    pub fn group(&self, name: &str) -> Option<&[Item]> {
        self.groups.get(name).map(Vec::as_slice)
    }
}

/// Where a device is in applying its baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaselineState {
    /// An entry is not sent or not answered yet, or the Mac has no config profile yet.
    Pending,
    /// Every entry was acknowledged, and none is missing from the last `ProfileList`.
    Applied,
    /// The Mac answered an entry with an error, or `remote_desktop` is listed for a Mac that
    /// is not supervised; the reason says which.
    Failed,
}

/// What the gate says of a device's baseline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaselineStatus {
    /// Where it is.
    pub state: BaselineState,
    /// For [`BaselineState::Failed`], what failed; empty otherwise.
    pub reason: String,
}

/// One entry's verdict at `now`: how it stands, and whether it is to be sent now.
struct Verdict {
    state: BaselineState,
    reason: String,
    due: bool,
}

impl Verdict {
    fn pending(due: bool) -> Self {
        Self {
            state: BaselineState::Pending,
            reason: String::new(),
            due,
        }
    }

    fn applied(due: bool) -> Self {
        Self {
            state: BaselineState::Applied,
            reason: String::new(),
            due,
        }
    }

    fn failed(reason: String, due: bool) -> Self {
        Self {
            state: BaselineState::Failed,
            reason,
            due,
        }
    }
}

/// How a baseline command stands: not answered, never queued (to send), answered with an error
/// (to send again `check` seconds after the answer, `retry`), or acknowledged at a time.
enum Standing {
    Unanswered,
    NeverQueued,
    Error { retry: bool },
    Acknowledged(u64),
}

fn standing(
    record: &DeviceRecord,
    command: Option<&ProfileCommand>,
    now: u64,
    check: u64,
) -> Standing {
    let Some(command) = command else {
        return Standing::NeverQueued;
    };
    if record.is_unanswered(&command.uuid) {
        return Standing::Unanswered;
    }
    match command.answer {
        None => Standing::NeverQueued,
        Some(answer) if answer.acknowledged => Standing::Acknowledged(answer.at_unix),
        Some(answer) => Standing::Error {
            retry: now.saturating_sub(answer.at_unix) >= check,
        },
    }
}

fn profile_verdict(record: &DeviceRecord, item: &ProfileItem, now: u64, check: u64) -> Verdict {
    let base = record.baseline.as_ref();
    let command = base.and_then(|base| base.profile(&item.identifier));
    match standing(record, command, now, check) {
        Standing::Unanswered => Verdict::pending(false),
        Standing::NeverQueued => Verdict::pending(true),
        Standing::Error { retry } => Verdict::failed(
            shown(&format!(
                "profile {}: the device answered an error",
                item.identifier
            )),
            retry,
        ),
        Standing::Acknowledged(at) => {
            let changed = command.and_then(|command| command.settings_sha256.as_deref())
                != Some(item.sha256.as_str());
            // A `ProfileList` answered after the acknowledgement that does not name the profile
            // says the Mac lost it.
            let missing = base.is_some_and(|base| {
                base.checked_unix.is_some_and(|checked| checked >= at)
                    && !base.installed.contains(&item.identifier)
            });
            if changed || missing {
                Verdict::pending(true)
            } else {
                Verdict::applied(false)
            }
        }
    }
}

fn desktop_verdict(record: &DeviceRecord, now: u64, check: u64) -> Verdict {
    // The gate looks only at a Mac whose `DeviceInformation` answer gave its serial, and that
    // answer says whether the Mac is supervised.
    if record.supervised != Some(true) {
        return Verdict::failed(
            "remote_desktop: the Mac is not supervised, and EnableRemoteDesktop needs a supervised Mac".to_owned(),
            false,
        );
    }
    let command = record
        .baseline
        .as_ref()
        .and_then(|base| base.remote_desktop.as_ref());
    match standing(record, command, now, check) {
        // A send that refreshes an acknowledged one keeps the earlier answer ([`Gate::send_one`]),
        // so the state does not flap to pending at every check.
        Standing::Unanswered => {
            let refresh = command.is_some_and(|command| command.acknowledged_unix().is_some());
            if refresh {
                Verdict::applied(false)
            } else {
                Verdict::pending(false)
            }
        }
        Standing::NeverQueued => Verdict::pending(true),
        Standing::Error { retry } => Verdict::failed(
            "remote_desktop: the device answered an error".to_owned(),
            retry,
        ),
        // No answer of Apple's reads Screen Sharing back, so it is sent again with each check.
        Standing::Acknowledged(at) => Verdict::applied(now.saturating_sub(at) >= check),
    }
}

/// The SHA-256 of `text` in lower-case hex: the digest a name's `Settings` is kept under.
fn text_digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// How a command that carries one value stands against the value's `digest`: one answered
/// with another digest is to be sent again, as is an error answer at the check interval.
fn valued_verdict(
    record: &DeviceRecord,
    command: Option<&ProfileCommand>,
    digest: &str,
    now: u64,
    check: u64,
    failed: &str,
) -> Verdict {
    let changed = command.and_then(|command| command.settings_sha256.as_deref()) != Some(digest);
    match standing(record, command, now, check) {
        Standing::Unanswered => Verdict::pending(false),
        Standing::NeverQueued => Verdict::pending(true),
        Standing::Error { retry } => Verdict::failed(failed.to_owned(), retry || changed),
        Standing::Acknowledged(_) if changed => Verdict::pending(true),
        Standing::Acknowledged(_) => Verdict::applied(false),
    }
}

fn name_verdict(record: &DeviceRecord, name: &str, now: u64, check: u64) -> Verdict {
    if record.supervised != Some(true) {
        return Verdict::failed(
            "device name: the Mac is not supervised, and DeviceName needs a supervised Mac"
                .to_owned(),
            false,
        );
    }
    let command = record.baseline.as_ref().and_then(|base| base.name.as_ref());
    valued_verdict(
        record,
        command,
        &text_digest(name),
        now,
        check,
        "device name: the device answered an error",
    )
}

fn install_verdict(record: &DeviceRecord, entry: &InstallEntry, now: u64, check: u64) -> Verdict {
    let command = record
        .baseline
        .as_ref()
        .and_then(|base| base.install(&entry.name));
    valued_verdict(
        record,
        command,
        &entry.digest(),
        now,
        check,
        &shown(&format!(
            "install {}: the device answered an error",
            entry.name
        )),
    )
}

/// Whether a `ProfileList` is to be sent: some profile entry exists, every one was
/// acknowledged (an entry not yet sent is sent without asking), none is outstanding, and the
/// last list is due again.
fn list_due(record: &DeviceRecord, items: &[Item], now: u64, check: u64) -> bool {
    let profiles: Vec<&ProfileItem> = items
        .iter()
        .filter_map(|item| match item {
            Item::Profile(profile) => Some(profile),
            Item::RemoteDesktop => None,
        })
        .collect();
    let base = record.baseline.as_ref();
    let acknowledged = |profile: &&ProfileItem| {
        base.and_then(|base| base.profile(&profile.identifier))
            .and_then(ProfileCommand::acknowledged_unix)
            .is_some()
    };
    if profiles.is_empty() || !profiles.iter().all(acknowledged) {
        return false;
    }
    let list = base.and_then(|base| base.list.as_ref());
    match standing(record, list, now, check) {
        Standing::Unanswered => false,
        Standing::NeverQueued => true,
        Standing::Error { retry } => retry,
        Standing::Acknowledged(at) => {
            base.is_some_and(|base| base.checked_unix.is_none()) || now.saturating_sub(at) >= check
        }
    }
}

/// Whether the device of `record` acknowledged its config profile.
fn configured(record: &DeviceRecord) -> bool {
    record
        .config_profile
        .as_ref()
        .and_then(ProfileCommand::acknowledged_unix)
        .is_some()
}

/// What the gate adds to a group's entries for one Mac: the device name, when the enrollment
/// has one, and the baseline installs.
struct Added {
    name: Option<String>,
    installs: Vec<InstallEntry>,
}

/// What the entries `items` and `added` say of the device of `record` at `now`.
fn status_of(
    record: &DeviceRecord,
    items: &[Item],
    added: &Added,
    now: u64,
    check: u64,
) -> BaselineStatus {
    let verdicts: Vec<Verdict> = items
        .iter()
        .map(|item| match item {
            Item::RemoteDesktop => desktop_verdict(record, now, check),
            Item::Profile(profile) => profile_verdict(record, profile, now, check),
        })
        .chain(
            added
                .name
                .iter()
                .map(|name| name_verdict(record, name, now, check)),
        )
        .chain(
            added
                .installs
                .iter()
                .map(|entry| install_verdict(record, entry, now, check)),
        )
        .collect();
    let failed = verdicts
        .iter()
        .find(|verdict| verdict.state == BaselineState::Failed);
    let pending = !configured(record)
        || verdicts
            .iter()
            .any(|verdict| verdict.state == BaselineState::Pending);
    match failed {
        Some(failed) => BaselineStatus {
            state: BaselineState::Failed,
            reason: failed.reason.clone(),
        },
        None if pending => BaselineStatus {
            state: BaselineState::Pending,
            reason: String::new(),
        },
        None => BaselineStatus {
            state: BaselineState::Applied,
            reason: String::new(),
        },
    }
}

/// What one look sends.
enum Step<'a> {
    Profile(&'a ProfileItem),
    RemoteDesktop,
    Name(&'a str),
    Install(&'a InstallEntry, &'a Install),
    List,
}

impl Gate {
    /// The baseline of the device of `record`, as the gate says it: `None` when the gate has no
    /// baseline, the device is not an enrolled Mac of the allowlist, or its host group has no
    /// entry in the baseline file.
    #[must_use]
    pub fn baseline_status(&self, record: &DeviceRecord) -> Option<BaselineStatus> {
        let (baseline, side) = (self.baseline()?, self.enroll_side()?);
        let serial = record.serial.as_deref().filter(|_| record.is_enrolled())?;
        let items = baseline.group(&side.enrollments.host_group(serial)?)?;
        let added = self.added(side, serial);
        Some(status_of(
            record,
            items,
            &added,
            self.now(),
            baseline.check_secs,
        ))
    }

    /// The device name of `serial` and the baseline installs, as they are now.
    fn added(&self, side: &EnrollSide, serial: &str) -> Added {
        Added {
            name: side.enrollments.host_name(serial),
            installs: self
                .install()
                .map(|install| install.installs().baseline().cloned().collect())
                .unwrap_or_default(),
        }
    }

    /// The baseline entries of the device of `record` (whose serial is `serial`) that are due,
    /// sent and kept in the record; the record as the look ends. The caller holds the turn.
    pub(crate) async fn send_baseline(
        &self,
        side: &EnrollSide,
        mut record: DeviceRecord,
        serial: &str,
    ) -> DeviceRecord {
        let Some(baseline) = self.baseline() else {
            return record;
        };
        let Some(group) = side.enrollments.host_group(serial) else {
            return record;
        };
        let Some(items) = baseline.group(&group) else {
            return record;
        };
        if !configured(&record) {
            return record;
        }
        let (now, check) = (self.now(), baseline.check_secs);
        let mut plan: Vec<Step<'_>> = Vec::new();
        for item in items {
            match item {
                Item::Profile(profile) if profile_verdict(&record, profile, now, check).due => {
                    plan.push(Step::Profile(profile));
                }
                Item::RemoteDesktop if desktop_verdict(&record, now, check).due => {
                    plan.push(Step::RemoteDesktop);
                }
                _ => {}
            }
        }
        let added = self.added(side, serial);
        if let Some(name) = added
            .name
            .as_deref()
            .filter(|name| name_verdict(&record, name, now, check).due)
        {
            plan.push(Step::Name(name));
        }
        if let Some(install) = self.install() {
            for entry in &added.installs {
                if install_verdict(&record, entry, now, check).due {
                    plan.push(Step::Install(entry, install));
                }
            }
        }
        // A profile sent now is checked at a later look.
        let installing = plan.iter().any(|step| matches!(step, Step::Profile(_)));
        if !installing && list_due(&record, items, now, check) {
            plan.push(Step::List);
        }
        for send in plan {
            if !self
                .send_one(side, &mut record, serial, &group, &send, now)
                .await
            {
                break;
            }
        }
        record
    }

    /// One baseline command: staged, kept, sent; audited once NanoHUB queued it. `false` when
    /// no UUID could be had, which ends the look's baseline sends.
    async fn send_one(
        &self,
        side: &EnrollSide,
        record: &mut DeviceRecord,
        serial: &str,
        group: &str,
        send: &Step<'_>,
        now: u64,
    ) -> bool {
        if let Step::Install(entry, install) = send
            && let Err(why) = install.check(entry)
        {
            tracing::warn!(enrollment_id = %record.enrollment_id, install = %entry.name, %why, "the install is not sent");
            record.last_error = Some(format!("install {}: {why}", entry.name));
            self.kept(record);
            return true;
        }
        let Some([uuid]) = self.uuids(record) else {
            return false;
        };
        let sent = ProfileCommand::new(uuid.clone(), now);
        let base = record.baseline.get_or_insert_with(BaselineRecord::default);
        let (what, command) = match send {
            Step::Profile(item) => {
                let sent = sent.with_settings(item.sha256.clone());
                let profile = crate::record::BaselineProfile {
                    identifier: item.identifier.clone(),
                    command: sent,
                };
                match base
                    .profiles
                    .iter_mut()
                    .find(|other| other.identifier == item.identifier)
                {
                    Some(other) => *other = profile,
                    None => base.profiles.push(profile),
                }
                (
                    format!("profile {}", item.identifier),
                    Command::install_profile(uuid, item.bytes.clone()),
                )
            }
            Step::RemoteDesktop => {
                // A refresh of an acknowledged send keeps that answer until the new one comes.
                let kept = base
                    .remote_desktop
                    .as_ref()
                    .and_then(|previous| previous.answer)
                    .filter(|answer| answer.acknowledged);
                base.remote_desktop = Some(ProfileCommand {
                    answer: kept,
                    ..sent
                });
                (
                    "remote_desktop".to_owned(),
                    Command::enable_remote_desktop(uuid),
                )
            }
            Step::Name(name) => {
                base.name = Some(sent.with_settings(text_digest(name)));
                (format!("device name {name}"), Command::settings(uuid, name))
            }
            Step::Install(entry, install) => {
                let sent = sent.with_settings(entry.digest());
                let record = BaselineInstall {
                    name: entry.name.clone(),
                    command: sent,
                };
                match base
                    .installs
                    .iter_mut()
                    .find(|other| other.name == entry.name)
                {
                    Some(other) => *other = record,
                    None => base.installs.push(record),
                }
                (
                    format!("install {} {}", entry.name, entry.version.as_str()),
                    Command::install_application(uuid, manifest(entry, &install.host)),
                )
            }
            Step::List => {
                base.list = Some(sent);
                ("profile list".to_owned(), Command::profile_list(uuid))
            }
        };
        if self.dispatch(record, &command, now).await {
            tracing::info!(enrollment_id = %record.enrollment_id, entry = %shown(&what), "sent a baseline command");
            side.enrollments.audit(
                ACTION_BASELINE,
                &shown(&format!("serial {serial}; group {group}; {what}")),
            );
        }
        true
    }

    /// The host groups of the baseline, none without one.
    #[must_use]
    pub fn baseline_groups(&self) -> BTreeSet<String> {
        self.baseline()
            .map(|baseline| baseline.groups().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}
