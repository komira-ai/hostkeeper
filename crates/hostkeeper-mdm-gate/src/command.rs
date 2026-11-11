//! The MDM commands the gate sends, and the one list of the command types it may send at all.
//!
//! Whoever holds the NanoHUB API key can send any command to every enrolled Mac, erase
//! included (design section 3). The gate's own code is bounded by [`COMMAND_TYPES`]: every
//! command goes through [`crate::NanoHub::enqueue`], which refuses a type off the list before
//! it calls NanoHUB. The list bounds this code, not a holder of the key.

use plist::{Dictionary, Value};

use crate::ids::CommandUuid;

/// The eight command types the gate sends (design section 3, coordinator-mdm.md section 4.8): the
/// two queries it asks a new device, the profile and package commands the enrollment side sends,
/// and the three commands of the enrollment baseline (design section 6.4): remote desktop, the
/// profile list and the device name. A test pins this list; a ninth type is a change of it.
pub const COMMAND_TYPES: [&str; 8] = [
    "DeviceInformation",
    "SecurityInfo",
    "InstallProfile",
    "RemoveProfile",
    "InstallEnterpriseApplication",
    "EnableRemoteDesktop",
    "ProfileList",
    "Settings",
];

/// `DeviceInformation`, whose answer decides a device's serial and supervision.
pub const DEVICE_INFORMATION: &str = "DeviceInformation";

/// `SecurityInfo`, whose answer says whether the Mac enrolled through ADE and whether the
/// enrollment is user-approved.
pub const SECURITY_INFO: &str = "SecurityInfo";

/// `InstallProfile`, which installs the config profile and the nonce profile.
pub const INSTALL_PROFILE: &str = "InstallProfile";

/// `RemoveProfile`, which removes the nonce profile once its request is approved.
pub const REMOVE_PROFILE: &str = "RemoveProfile";

/// `InstallEnterpriseApplication`, which installs the hostkeeper pkg by its inline manifest.
pub const INSTALL_ENTERPRISE_APPLICATION: &str = "InstallEnterpriseApplication";

/// `EnableRemoteDesktop`, which turns Screen Sharing on for a supervised Mac (Apple's
/// `mdm/commands/remotedesktop.enable.yaml`: no keys, `supervised: true`).
pub const ENABLE_REMOTE_DESKTOP: &str = "EnableRemoteDesktop";

/// `ProfileList`, which lists the profiles installed on a Mac (Apple's
/// `mdm/commands/profile.list.yaml`); the baseline's check reads its `ProfileList` answer.
pub const PROFILE_LIST: &str = "ProfileList";

/// `Settings`, which sets the device's name and host name (Apple's `mdm/commands/settings.yaml`,
/// the items `DeviceName` and `HostName`; a supervised device only for `DeviceName`).
pub const SETTINGS: &str = "Settings";

/// The queries of `DeviceInformation`: the serial, the UDID and the supervision.
pub const QUERIES: [&str; 3] = ["SerialNumber", "UDID", "IsSupervised"];

/// One MDM command: its request type, its UUID and the other keys of its `Command` dictionary.
/// Its `Debug` shows the type and the UUID only: an `InstallProfile` of the nonce profile
/// carries the nonce.
#[derive(Clone, PartialEq)]
pub struct Command {
    request_type: String,
    uuid: CommandUuid,
    fields: Dictionary,
}

impl Command {
    /// The command `request_type` with the UUID `uuid` and the keys `fields` beside its
    /// `RequestType`. Any type can be made; only one of [`COMMAND_TYPES`] is sent.
    #[must_use]
    pub fn new(request_type: &str, uuid: CommandUuid, fields: Dictionary) -> Self {
        Self {
            request_type: request_type.to_owned(),
            uuid,
            fields,
        }
    }

    /// `DeviceInformation` for [`QUERIES`].
    #[must_use]
    pub fn device_information(uuid: CommandUuid) -> Self {
        let mut fields = Dictionary::new();
        let queries = QUERIES.iter().map(|q| Value::from(*q)).collect();
        fields.insert("Queries".to_owned(), Value::Array(queries));
        Self::new(DEVICE_INFORMATION, uuid, fields)
    }

    /// `SecurityInfo`, which takes no keys.
    #[must_use]
    pub fn security_info(uuid: CommandUuid) -> Self {
        Self::new(SECURITY_INFO, uuid, Dictionary::new())
    }

    /// `InstallProfile` of the profile whose property list is `profile`.
    #[must_use]
    pub fn install_profile(uuid: CommandUuid, profile: Vec<u8>) -> Self {
        let mut fields = Dictionary::new();
        fields.insert("Payload".to_owned(), Value::Data(profile));
        Self::new(INSTALL_PROFILE, uuid, fields)
    }

    /// `RemoveProfile` of the profile identified `identifier`.
    #[must_use]
    pub fn remove_profile(uuid: CommandUuid, identifier: &str) -> Self {
        let mut fields = Dictionary::new();
        fields.insert("Identifier".to_owned(), Value::from(identifier));
        Self::new(REMOVE_PROFILE, uuid, fields)
    }

    /// `Settings` that sets the `DeviceName` and the `HostName` of the Mac to `name`: two
    /// items, each `Item` with its own key, as Apple's schema has it.
    #[must_use]
    pub fn settings(uuid: CommandUuid, name: &str) -> Self {
        let item = |item: &str| {
            let mut dict = Dictionary::new();
            dict.insert("Item".to_owned(), Value::from(item));
            dict.insert(item.to_owned(), Value::from(name));
            Value::Dictionary(dict)
        };
        let mut fields = Dictionary::new();
        fields.insert(
            "Settings".to_owned(),
            Value::Array(vec![item("DeviceName"), item("HostName")]),
        );
        Self::new(SETTINGS, uuid, fields)
    }

    /// `InstallEnterpriseApplication` of the inline `manifest` ([`crate::pkg::manifest`]).
    #[must_use]
    pub fn install_application(uuid: CommandUuid, manifest: Dictionary) -> Self {
        let mut fields = Dictionary::new();
        fields.insert("Manifest".to_owned(), Value::Dictionary(manifest));
        Self::new(INSTALL_ENTERPRISE_APPLICATION, uuid, fields)
    }

    /// `EnableRemoteDesktop`, which takes no keys.
    #[must_use]
    pub fn enable_remote_desktop(uuid: CommandUuid) -> Self {
        Self::new(ENABLE_REMOTE_DESKTOP, uuid, Dictionary::new())
    }

    /// `ProfileList` of every installed profile (`ManagedOnly` false, so a profile the Mac
    /// holds from another source is listed too, and a baseline profile is judged by its
    /// identifier alone).
    #[must_use]
    pub fn profile_list(uuid: CommandUuid) -> Self {
        let mut fields = Dictionary::new();
        fields.insert("ManagedOnly".to_owned(), Value::Boolean(false));
        Self::new(PROFILE_LIST, uuid, fields)
    }

    /// The request type.
    #[must_use]
    pub fn request_type(&self) -> &str {
        &self.request_type
    }

    /// The command's UUID.
    #[must_use]
    pub fn uuid(&self) -> &CommandUuid {
        &self.uuid
    }

    /// Whether the type is one of [`COMMAND_TYPES`].
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        COMMAND_TYPES.contains(&self.request_type.as_str())
    }

    /// The command as the XML property list NanoHUB's enqueue API takes: `Command` (holding
    /// `RequestType` and the other keys) and `CommandUUID`.
    #[must_use]
    pub fn to_plist(&self) -> Vec<u8> {
        let mut command = self.fields.clone();
        command.insert(
            "RequestType".to_owned(),
            Value::from(self.request_type.as_str()),
        );
        let mut root = Dictionary::new();
        root.insert("Command".to_owned(), Value::Dictionary(command));
        root.insert("CommandUUID".to_owned(), Value::from(self.uuid.as_str()));
        let mut bytes = Vec::new();
        // Writing a dictionary of strings, data, arrays and dictionaries into a Vec cannot fail: the writer only
        // fails on the sink's errors, and a date or a non-string key, neither of which is here.
        let _ = Value::Dictionary(root).to_writer_xml(&mut bytes);
        bytes
    }
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("request_type", &self.request_type)
            .field("uuid", &self.uuid)
            .finish_non_exhaustive()
    }
}
