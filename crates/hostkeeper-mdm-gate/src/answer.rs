//! What a device's answer to `DeviceInformation` or `SecurityInfo` says (Apple's command
//! schema, `mdm/commands/information.device.yaml` and `security.info.yaml`).
//!
//! - `DeviceInformation`: `QueryResponses` holds `SerialNumber`, `UDID` and `IsSupervised`. The
//!   answer decides the serial the gate knows the device by, and whether it is supervised: a
//!   missing `IsSupervised` is not supervised, and a missing or malformed serial is none.
//! - `SecurityInfo`: `SecurityInfo.ManagementStatus` holds `EnrolledViaDEP` and
//!   `UserApprovedEnrollment`. The gate shows both and decides nothing on them.
//!
//! The answer's own `CommandUUID` must be the command's: a result whose plist names another
//! command is not read.

use plist::{Dictionary, Value};

use crate::ids::is_serial;

/// What `DeviceInformation` answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInformation {
    /// `SerialNumber`, when it is 1 to 64 ASCII letters and digits.
    pub serial: Option<String>,
    /// `UDID`, when given.
    pub udid: Option<String>,
    /// `IsSupervised`, false when not given.
    pub supervised: bool,
}

/// What `SecurityInfo` answered: the `ManagementStatus` keys, each `None` when not given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecurityInfo {
    /// `EnrolledViaDEP`: the Mac enrolled through Automated Device Enrollment.
    pub enrolled_via_dep: Option<bool>,
    /// `UserApprovedEnrollment`.
    pub user_approved: Option<bool>,
}

/// The dictionary of the result `payload`, when it is one and names the command `uuid`.
fn result_of(payload: &[u8], uuid: &str) -> Option<Dictionary> {
    let dict = Value::from_reader(std::io::Cursor::new(payload))
        .ok()
        .and_then(Value::into_dictionary)?;
    let named = dict.get("CommandUUID").and_then(Value::as_string);
    (named == Some(uuid)).then_some(dict)
}

/// The `DeviceInformation` answer in `payload` to the command `uuid`; `None` when the payload
/// is not a result of that command or holds no `QueryResponses`.
#[must_use]
pub fn device_information(payload: &[u8], uuid: &str) -> Option<DeviceInformation> {
    let dict = result_of(payload, uuid)?;
    let answers = dict.get("QueryResponses")?.as_dictionary()?;
    let text = |key: &str| answers.get(key).and_then(Value::as_string);
    Some(DeviceInformation {
        serial: text("SerialNumber")
            .filter(|serial| is_serial(serial))
            .map(str::to_owned),
        udid: text("UDID").map(str::to_owned),
        supervised: answers
            .get("IsSupervised")
            .and_then(Value::as_boolean)
            .unwrap_or(false),
    })
}

/// The `SecurityInfo` answer in `payload` to the command `uuid`; `None` when the payload is not
/// a result of that command or holds no `SecurityInfo`.
#[must_use]
pub fn security_info(payload: &[u8], uuid: &str) -> Option<SecurityInfo> {
    let dict = result_of(payload, uuid)?;
    let info = dict.get("SecurityInfo")?.as_dictionary()?;
    let status = info.get("ManagementStatus").and_then(Value::as_dictionary);
    let flag = |key: &str| status.and_then(|s| s.get(key)).and_then(Value::as_boolean);
    Some(SecurityInfo {
        enrolled_via_dep: flag("EnrolledViaDEP"),
        user_approved: flag("UserApprovedEnrollment"),
    })
}
