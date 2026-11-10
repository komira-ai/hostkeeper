//! The check-in messages: `POST` to the check-in URL, one property list each (Apple's
//! `mdm/checkin` schemas).

use plist::Dictionary;

use crate::Error;
use crate::dict::{data, opt_bool, opt_data, opt_string, parse, string};

/// The identity a message names a device by: `UDID` of a device enrollment, `EnrollmentID` of a
/// user enrollment. A device enrollment sends both on some messages; the `UDID` wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceId {
    /// The `UDID`, when sent.
    pub udid: Option<String>,
    /// The `EnrollmentID`, when sent.
    pub enrollment_id: Option<String>,
}

impl DeviceId {
    fn read(d: &Dictionary) -> Result<Self, Error> {
        let udid = opt_string(d, "UDID")?;
        let enrollment_id = opt_string(d, "EnrollmentID")?;
        if udid.is_none() && enrollment_id.is_none() {
            return Err(Error::NoDeviceId);
        }
        Ok(Self {
            udid,
            enrollment_id,
        })
    }

    /// The identifier the server keys the device on.
    #[must_use]
    pub fn key(&self) -> &str {
        self.udid
            .as_deref()
            .or(self.enrollment_id.as_deref())
            .unwrap_or_default()
    }
}

/// `Authenticate`: the Mac has begun installing the MDM payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authenticate {
    /// The device.
    pub id: DeviceId,
    /// The push topic the Mac subscribed to.
    pub topic: String,
    /// `DeviceName`.
    pub device_name: Option<String>,
    /// `Model`.
    pub model: Option<String>,
    /// `ModelName`.
    pub model_name: Option<String>,
    /// `OSVersion`.
    pub os_version: Option<String>,
    /// `BuildVersion`.
    pub build_version: Option<String>,
    /// `ProductName`.
    pub product_name: Option<String>,
    /// `SerialNumber`, which a user enrollment never sends.
    pub serial_number: Option<String>,
}

/// `TokenUpdate`: the Mac's push token and magic, and so a working enrollment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUpdate {
    /// The device.
    pub id: DeviceId,
    /// The push topic.
    pub topic: String,
    /// The APNs device token, raw bytes.
    pub token: Vec<u8>,
    /// The magic string a push carries.
    pub push_magic: String,
    /// `UnlockToken`, when sent.
    pub unlock_token: Option<Vec<u8>>,
    /// `AwaitingConfiguration`, when sent.
    pub awaiting_configuration: Option<bool>,
    /// `NotOnConsole`, when sent: set for a user channel's update.
    pub not_on_console: Option<bool>,
    /// `EnrollmentUserID`, set on a user channel's update.
    pub enrollment_user_id: Option<String>,
}

/// `CheckOut`: the Mac left management.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOut {
    /// The device.
    pub id: DeviceId,
    /// The push topic.
    pub topic: String,
}

/// `DeclarativeManagement`: a request of the declarative channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarativeManagement {
    /// The device.
    pub id: DeviceId,
    /// `tokens`, `declaration-items`, `status` or `declaration/<type>/<identifier>`.
    pub endpoint: String,
    /// The request body, when sent (a status report is JSON).
    pub data: Option<Vec<u8>>,
}

/// `GetBootstrapToken`: the Mac asks for the bootstrap token the server holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetBootstrapToken {
    /// `AwaitingConfiguration`, when sent.
    pub awaiting_configuration: Option<bool>,
}

/// `SetBootstrapToken`: the Mac gives the server its bootstrap token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetBootstrapToken {
    /// The token, absent when the Mac clears it.
    pub bootstrap_token: Option<Vec<u8>>,
    /// `AwaitingConfiguration`, when sent.
    pub awaiting_configuration: Option<bool>,
}

/// A check-in message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckIn {
    /// See [`Authenticate`].
    Authenticate(Authenticate),
    /// See [`TokenUpdate`].
    TokenUpdate(TokenUpdate),
    /// See [`CheckOut`].
    CheckOut(CheckOut),
    /// See [`DeclarativeManagement`].
    DeclarativeManagement(DeclarativeManagement),
    /// See [`GetBootstrapToken`].
    GetBootstrapToken(GetBootstrapToken),
    /// See [`SetBootstrapToken`].
    SetBootstrapToken(SetBootstrapToken),
}

impl CheckIn {
    /// Reads a check-in from a property list, XML or binary.
    ///
    /// # Errors
    ///
    /// [`Error`] when the bytes are not a dictionary, the `MessageType` is not one of the six,
    /// or a key the message needs is missing, empty or of the wrong type.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let d = parse(bytes)?;
        let message_type = string(&d, "MessageType")?;
        Ok(match message_type.as_str() {
            "Authenticate" => Self::Authenticate(Authenticate {
                id: DeviceId::read(&d)?,
                topic: string(&d, "Topic")?,
                device_name: opt_string(&d, "DeviceName")?,
                model: opt_string(&d, "Model")?,
                model_name: opt_string(&d, "ModelName")?,
                os_version: opt_string(&d, "OSVersion")?,
                build_version: opt_string(&d, "BuildVersion")?,
                product_name: opt_string(&d, "ProductName")?,
                serial_number: opt_string(&d, "SerialNumber")?,
            }),
            "TokenUpdate" => Self::TokenUpdate(TokenUpdate {
                id: DeviceId::read(&d)?,
                topic: string(&d, "Topic")?,
                token: data(&d, "Token")?,
                push_magic: string(&d, "PushMagic")?,
                unlock_token: opt_data(&d, "UnlockToken")?,
                awaiting_configuration: opt_bool(&d, "AwaitingConfiguration")?,
                not_on_console: opt_bool(&d, "NotOnConsole")?,
                enrollment_user_id: opt_string(&d, "EnrollmentUserID")?,
            }),
            "CheckOut" => Self::CheckOut(CheckOut {
                id: DeviceId::read(&d)?,
                topic: string(&d, "Topic")?,
            }),
            "DeclarativeManagement" => Self::DeclarativeManagement(DeclarativeManagement {
                id: DeviceId::read(&d)?,
                endpoint: string(&d, "Endpoint")?,
                data: opt_data(&d, "Data")?,
            }),
            "GetBootstrapToken" => Self::GetBootstrapToken(GetBootstrapToken {
                awaiting_configuration: opt_bool(&d, "AwaitingConfiguration")?,
            }),
            "SetBootstrapToken" => Self::SetBootstrapToken(SetBootstrapToken {
                bootstrap_token: opt_data(&d, "BootstrapToken")?,
                awaiting_configuration: opt_bool(&d, "AwaitingConfiguration")?,
            }),
            _ => return Err(Error::UnknownMessageType(message_type)),
        })
    }

    /// The message's `MessageType`.
    #[must_use]
    pub const fn message_type(&self) -> &'static str {
        match self {
            Self::Authenticate(_) => "Authenticate",
            Self::TokenUpdate(_) => "TokenUpdate",
            Self::CheckOut(_) => "CheckOut",
            Self::DeclarativeManagement(_) => "DeclarativeManagement",
            Self::GetBootstrapToken(_) => "GetBootstrapToken",
            Self::SetBootstrapToken(_) => "SetBootstrapToken",
        }
    }
}
