//! The check-in messages: `POST` to the check-in URL, one property list each (Apple's
//! `mdm/checkin` schemas).

use plist::Dictionary;

use crate::Error;
use crate::dict::{data, opt_bool, opt_data, opt_id, opt_string, parse, string};

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
        Self::read_optional(d)?.ok_or(Error::NoDeviceId)
    }

    /// The identity when the message names one. An empty `UDID` or `EnrollmentID` is refused,
    /// not treated as absent, so it cannot hide the other.
    fn read_optional(d: &Dictionary) -> Result<Option<Self>, Error> {
        let udid = opt_id(d, "UDID")?;
        let enrollment_id = opt_id(d, "EnrollmentID")?;
        if udid.is_none() && enrollment_id.is_none() {
            return Ok(None);
        }
        Ok(Some(Self {
            udid,
            enrollment_id,
        }))
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

/// `TokenUpdate`: the Mac's push token and magic, and so a working enrollment. `Debug` prints
/// the lengths of the secret fields, never their bytes.
#[derive(Clone, PartialEq, Eq)]
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

impl std::fmt::Debug for TokenUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenUpdate")
            .field("id", &self.id)
            .field("topic", &self.topic)
            .field("token_len", &self.token.len())
            .field("push_magic_len", &self.push_magic.len())
            .field(
                "unlock_token_len",
                &self.unlock_token.as_ref().map(Vec::len),
            )
            .field("awaiting_configuration", &self.awaiting_configuration)
            .field("not_on_console", &self.not_on_console)
            .field("enrollment_user_id", &self.enrollment_user_id)
            .finish()
    }
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
    /// The `UDID` or `EnrollmentID` when the message carries one; see [`SetBootstrapToken::id`].
    pub id: Option<DeviceId>,
    /// `AwaitingConfiguration`, when sent.
    pub awaiting_configuration: Option<bool>,
}

/// `SetBootstrapToken`: the Mac gives the server its bootstrap token. `Debug` prints the length
/// of the token, never its bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct SetBootstrapToken {
    /// The `UDID` or `EnrollmentID` when the message carries one (Apple's schema does not list
    /// it, Macs send it). The caller keys the token on the identity of the mTLS-authenticated
    /// connection and refuses the message when this names another device.
    pub id: Option<DeviceId>,
    /// The token, absent when the Mac clears it.
    pub bootstrap_token: Option<Vec<u8>>,
    /// `AwaitingConfiguration`, when sent.
    pub awaiting_configuration: Option<bool>,
}

impl std::fmt::Debug for SetBootstrapToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetBootstrapToken")
            .field("id", &self.id)
            .field(
                "bootstrap_token_len",
                &self.bootstrap_token.as_ref().map(Vec::len),
            )
            .field("awaiting_configuration", &self.awaiting_configuration)
            .finish()
    }
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
                id: DeviceId::read_optional(&d)?,
                awaiting_configuration: opt_bool(&d, "AwaitingConfiguration")?,
            }),
            "SetBootstrapToken" => Self::SetBootstrapToken(SetBootstrapToken {
                id: DeviceId::read_optional(&d)?,
                bootstrap_token: opt_data(&d, "BootstrapToken")?,
                awaiting_configuration: opt_bool(&d, "AwaitingConfiguration")?,
            }),
            _ => return Err(Error::unknown_message_type(&message_type)),
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
