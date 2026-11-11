//! The JSON of ABM's device service, as far as the sync reads it (`spikes/abm.md`, 5): a page of
//! devices with the cursor to ask the next one by, and the account the token belongs to. A field
//! of a later answer that is not named here is ignored.

use std::collections::BTreeMap;

use serde::Deserialize;

/// `GET /session`.
#[derive(Deserialize)]
pub(crate) struct SessionAnswer {
    /// The token every later call carries as `X-ADM-Auth-Session`.
    pub(crate) auth_session_token: String,
}

/// `GET /account`: what the token proves, shown in the answer of a token upload.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct Account {
    /// The MDM server entry's name in ABM (`js-mdm`).
    #[serde(default)]
    pub(crate) server_name: String,
    /// The organisation.
    #[serde(default)]
    pub(crate) org_name: String,
}

/// One page of `POST /server/devices` (a full fetch) or `POST /devices/sync`.
#[derive(Debug, Deserialize)]
pub(crate) struct DevicePage {
    /// What the next call names to continue after this page; a full fetch's seeds `/devices/sync`.
    pub(crate) cursor: String,
    /// The devices of the page.
    #[serde(default)]
    pub(crate) devices: Vec<DeviceRecord>,
    /// Whether another page follows.
    #[serde(default)]
    pub(crate) more_to_follow: bool,
}

/// One device of a page. A full fetch's records carry no `op_type`; a sync's say `added`,
/// `modified` or `deleted`.
#[derive(Debug, Deserialize)]
pub(crate) struct DeviceRecord {
    /// The serial.
    pub(crate) serial_number: String,
    /// The model.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// Apple's description of it.
    #[serde(default)]
    pub(crate) description: Option<String>,
    /// `empty`, `assigned`, `pushed` or `removed`.
    #[serde(default)]
    pub(crate) profile_status: Option<String>,
    /// The profile assigned to it.
    #[serde(default)]
    pub(crate) profile_uuid: Option<String>,
    /// What the sync says happened to it.
    #[serde(default)]
    pub(crate) op_type: Option<String>,
}

impl DeviceRecord {
    /// Whether ABM says the device is no longer ours: a sync's `deleted`. A `profile_status` of
    /// `removed` says only that the MDM profile left the device, which is still in the account.
    pub(crate) fn is_removed(&self) -> bool {
        self.op_type.as_deref() == Some("deleted")
    }
}

/// `POST /profile`.
#[derive(Deserialize)]
pub(crate) struct ProfileAnswer {
    /// The uuid ABM gave the profile.
    pub(crate) profile_uuid: String,
}

/// `POST /profile/devices`: the outcome per serial (`SUCCESS`, `NOT_ACCESSIBLE`, `FAILED`).
#[derive(Deserialize)]
pub(crate) struct AssignAnswer {
    /// The outcome of each serial asked.
    #[serde(default)]
    pub(crate) devices: BTreeMap<String, String>,
}
