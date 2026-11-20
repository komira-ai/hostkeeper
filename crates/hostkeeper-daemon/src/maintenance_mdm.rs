//! The device-management settings of a Mac that must not be set (docs/design/maintenance-updates.md
//! section 5.8), each reported by name in `MacUpdateFacts.mdm_conflicts`: a profile or a
//! declaration that defers, enforces or turns on software updates stands in the way of the
//! updates hostkeeper runs. The collector `mdm` reads four sources, in this order, and reports each
//! name once (at most [`hostkeeper_proto::maintenance::MAX_MDM_CONFLICTS`], 16):
//!
//! 1. [`DDM_STATE`], the software-update declarations the Mac holds. Apple does not document the
//!    file; its keys are as published write-ups show them on macOS 26, and every key of this
//!    source is `[V]`: to be shown on a Mac by the Mac proof.
//!    - `com.apple.configuration.softwareupdate.enforcement.specific`: a dictionary anywhere in
//!      the file (through dictionaries and arrays) holds `TargetOSVersion` or
//!      `TargetBuildVersion`, an enforced update's target;
//!    - in the dictionary at `SUCorePersistedStatePolicyFields` then
//!      `SUCoreDDMDeclarationGlobalSettings` (the `softwareupdate.settings` declaration):
//!      a key whose name contains `deferral` (ASCII case-insensitive) that is on is reported as
//!      `Deferrals`; `automaticallyInstallOSUpdates` or
//!      `automaticallyInstallSystemAndSecurityUpdates` on is reported as `AutomaticActions`; a
//!      key whose name contains `beta` (ASCII case-insensitive) that is on is reported as `Beta`.
//! 2. [`MANAGED_PREFERENCES`]`/com.apple.applicationaccess.plist` (the restrictions payload):
//!    `forceDelayedSoftwareUpdates`, `forceDelayedMajorSoftwareUpdates`,
//!    `forceDelayedAppSoftwareUpdates`, `enforcedSoftwareUpdateDelay`,
//!    `enforcedSoftwareUpdateMajorOSDeferredInstallDelay`,
//!    `enforcedSoftwareUpdateMinorOSDeferredInstallDelay` and
//!    `enforcedSoftwareUpdateNonOSDeferredInstallDelay`, each by its own name when on.
//! 3. [`MANAGED_PREFERENCES`]`/com.apple.SoftwareUpdate.plist`: `CatalogURL`,
//!    `AllowPreReleaseInstallation`, `AutomaticallyInstallMacOSUpdates` and
//!    `restrict-software-update-require-admin-to-install`, each by its own name when on.
//! 4. [`APP_STORE_XCODE`] when an Xcode in `/Applications` holds `Contents/_MASReceipt/receipt`:
//!    the App Store installed it and would update it in place.
//!
//! A key is on by one rule (`is_on`): a boolean when true, an integer when not 0, and any other
//! value (a string, a date, data, a dictionary, an array, a real) whenever the key is there.
//!
//! Only the device's own files at the top of [`MANAGED_PREFERENCES`] are read, not the per-user
//! subdirectories: software-update settings are device settings. A file that is not there adds
//! nothing; one that is there and cannot be read is the error of the port ([`Plists`]), so an
//! unreadable file is reported unreadable and never as a clean Mac. The section always has the
//! Mac's facts: an empty list is a clean Mac.

use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use plist::{Dictionary, Value};

use crate::maintenance_collect::{APPLICATIONS, Collector, HostRoot, Look, Section, xcode_apps};
use crate::maintenance_plist::{HostPlists, Plists};

/// The directory of the device's managed preferences.
pub const MANAGED_PREFERENCES: &str = "/Library/Managed Preferences";

/// The software-update declarations' state, which Apple does not document.
pub const DDM_STATE: &str = "/var/db/softwareupdate/SoftwareUpdateDDMStatePersistence.plist";

/// The conflict named when the App Store installed an Xcode.
pub const APP_STORE_XCODE: &str = "xcode-app-store";

const ENFORCED: &str = "com.apple.configuration.softwareupdate.enforcement.specific";
const TARGETS: [&str; 2] = ["TargetOSVersion", "TargetBuildVersion"];
const POLICY_FIELDS: &str = "SUCorePersistedStatePolicyFields";
const GLOBAL_SETTINGS: &str = "SUCoreDDMDeclarationGlobalSettings";
const DEFERRALS: &str = "Deferrals";
const AUTOMATIC_ACTIONS: &str = "AutomaticActions";
const AUTOMATIC_KEYS: [&str; 2] = [
    "automaticallyInstallOSUpdates",
    "automaticallyInstallSystemAndSecurityUpdates",
];
const BETA: &str = "Beta";

const RESTRICTIONS: &str = "com.apple.applicationaccess.plist";
const RESTRICTION_KEYS: [&str; 7] = [
    "forceDelayedSoftwareUpdates",
    "forceDelayedMajorSoftwareUpdates",
    "forceDelayedAppSoftwareUpdates",
    "enforcedSoftwareUpdateDelay",
    "enforcedSoftwareUpdateMajorOSDeferredInstallDelay",
    "enforcedSoftwareUpdateMinorOSDeferredInstallDelay",
    "enforcedSoftwareUpdateNonOSDeferredInstallDelay",
];

const SOFTWARE_UPDATE: &str = "com.apple.SoftwareUpdate.plist";
const SOFTWARE_UPDATE_KEYS: [&str; 4] = [
    "CatalogURL",
    "AllowPreReleaseInstallation",
    "AutomaticallyInstallMacOSUpdates",
    "restrict-software-update-require-admin-to-install",
];

/// The collector of the device-management settings that conflict with a run.
pub struct MdmConflicts {
    plists: Box<dyn Plists>,
    root: HostRoot,
}

impl MdmConflicts {
    /// The collector reading property lists through `plists` and the Xcodes' files under `root`.
    #[must_use]
    pub fn new(plists: Box<dyn Plists>, root: HostRoot) -> MdmConflicts {
        MdmConflicts { plists, root }
    }

    /// The collector over the host's own files.
    #[must_use]
    pub fn at_host() -> MdmConflicts {
        MdmConflicts::new(Box::new(HostPlists::at_host()), HostRoot::host())
    }

    /// The names in the managed-preferences file `file` among `keys` that are on, in the order
    /// of `keys`; none when there is no such file.
    fn managed(&self, file: &str, keys: &[&str]) -> Result<Vec<String>, String> {
        let path = format!("{MANAGED_PREFERENCES}/{file}");
        let found: Vec<String> = match self.plists.dictionary(&path)? {
            Some(dictionary) => keys
                .iter()
                .filter(|key| dictionary.get(key).is_some_and(is_on))
                .map(|key| (*key).to_owned())
                .collect(),
            None => Vec::new(),
        };
        Ok(found)
    }
}

impl Collector for MdmConflicts {
    fn name(&self) -> &'static str {
        "mdm"
    }

    fn collect(&self, _look: &Look<'_>) -> Result<Section, String> {
        let mut found = match self.plists.dictionary(DDM_STATE)? {
            Some(state) => declarations(&state),
            None => Vec::new(),
        };
        found.extend(self.managed(RESTRICTIONS, &RESTRICTION_KEYS)?);
        found.extend(self.managed(SOFTWARE_UPDATE, &SOFTWARE_UPDATE_KEYS)?);
        let from_app_store = xcode_apps(&self.root)?.iter().any(|app| {
            self.root.exists(&format!(
                "{APPLICATIONS}/{app}/Contents/_MASReceipt/receipt"
            ))
        });
        if from_app_store {
            found.push(APP_STORE_XCODE.to_owned());
        }
        Ok(Section {
            mac: Some(pb::MacUpdateFacts {
                mdm_conflicts: found,
                ..Default::default()
            }),
            ..Default::default()
        })
    }
}

/// Whether a key holding `value` is on: a boolean when true, an integer when not 0, any other
/// value whenever the key is there.
fn is_on(value: &Value) -> bool {
    match value {
        Value::Boolean(on) => *on,
        Value::Integer(number) => number.as_signed() != Some(0),
        _ => true,
    }
}

/// The names of the software-update declarations' conflicts in the state file `state`.
fn declarations(state: &Dictionary) -> Vec<String> {
    let mut found = Vec::new();
    if holds_target(state) {
        found.push(ENFORCED.to_owned());
    }
    let settings = state
        .get(POLICY_FIELDS)
        .and_then(Value::as_dictionary)
        .and_then(|fields| fields.get(GLOBAL_SETTINGS))
        .and_then(Value::as_dictionary);
    let Some(settings) = settings else {
        return found;
    };
    if named_on(settings, "deferral") {
        found.push(DEFERRALS.to_owned());
    }
    if AUTOMATIC_KEYS
        .iter()
        .any(|key| settings.get(key).is_some_and(is_on))
    {
        found.push(AUTOMATIC_ACTIONS.to_owned());
    }
    if named_on(settings, "beta") {
        found.push(BETA.to_owned());
    }
    found
}

/// Whether `settings` holds a key whose name contains `part` (ASCII case-insensitive) that is
/// on.
fn named_on(settings: &Dictionary, part: &str) -> bool {
    settings
        .iter()
        .any(|(key, value)| key.to_ascii_lowercase().contains(part) && is_on(value))
}

/// Whether `dictionary`, or a dictionary anywhere inside it, holds an enforced update's target.
fn holds_target(dictionary: &Dictionary) -> bool {
    TARGETS.iter().any(|key| dictionary.contains_key(key))
        || dictionary.values().any(value_holds_target)
}

/// [`holds_target`] for a value: through dictionaries and arrays.
fn value_holds_target(value: &Value) -> bool {
    match value {
        Value::Dictionary(dictionary) => holds_target(dictionary),
        Value::Array(values) => values.iter().any(value_holds_target),
        _ => false,
    }
}

#[cfg(test)]
#[path = "maintenance_mdm_tests.rs"]
mod tests;
