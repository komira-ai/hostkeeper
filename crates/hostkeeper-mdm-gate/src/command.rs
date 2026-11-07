//! The MDM commands the gate sends, and the one list of the command types it may send at all.
//!
//! Whoever holds the NanoHUB API key can send any command to every enrolled Mac, erase
//! included (design section 3). The gate's own code is bounded by [`COMMAND_TYPES`]: every
//! command goes through [`crate::NanoHub::enqueue`], which refuses a type off the list before
//! it calls NanoHUB. The list bounds this code, not a holder of the key.

use plist::{Dictionary, Value};

use crate::ids::CommandUuid;

/// The five command types the gate sends (design section 3): the two queries it asks a new
/// device, and the profile and package commands the enrollment side sends.
pub const COMMAND_TYPES: [&str; 5] = [
    "DeviceInformation",
    "SecurityInfo",
    "InstallProfile",
    "RemoveProfile",
    "InstallEnterpriseApplication",
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
        // Writing a dictionary of strings, data and arrays into a Vec cannot fail: the writer only
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
