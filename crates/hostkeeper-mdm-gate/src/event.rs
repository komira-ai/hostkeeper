//! The events NanoHUB's webhook posts, read into what the gate acts on.
//!
//! NanoHUB 0.2.0 (`-webhook-url`) posts NanoMDM v0.9.0's MicroMDM-compatible event
//! (`service/webhook` of NanoMDM): a JSON object with a `topic`, and a `checkin_event` or an
//! `acknowledge_event`. Each carries `ids` (the NanoMDM enrollment id and its type), the
//! device's `udid`, and `raw_payload`, the base64 of the property list the device sent. The
//! gate reads four topics:
//!
//! - `mdm.Authenticate`: a device begins to enroll; its plist may carry `SerialNumber`;
//! - `mdm.TokenUpdate`: the device channel has a push token (`token_update_tally` counts the
//!   ones since its `Authenticate`, NanoMDM's tally);
//! - `mdm.CheckOut`: the device left management;
//! - `mdm.Connect`: a command result, with the `command_uuid` and the `status` it answered.
//!
//! Any other topic, and any event of a channel that is not a device's (`ids.type` other than
//! `Device`: a user channel), is passed over. The check-in's plist must say the same
//! `MessageType` as the topic.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use plist::Value;
use serde::Deserialize;

use crate::ids::{EnrollmentId, is_serial};

/// What one webhook event says, as the gate acts on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// `mdm.Authenticate` of a device channel.
    Authenticate {
        /// The enrollment id.
        id: EnrollmentId,
        /// The device's UDID; empty when the event carries none.
        udid: String,
        /// `SerialNumber` of the plist, when it carries one that could be a serial.
        serial: Option<String>,
    },
    /// `mdm.TokenUpdate` of a device channel.
    TokenUpdate {
        /// The enrollment id.
        id: EnrollmentId,
        /// The device's UDID; empty when the event carries none.
        udid: String,
        /// NanoMDM's count of `TokenUpdate`s since the device's `Authenticate`, when sent.
        tally: Option<u64>,
    },
    /// `mdm.CheckOut` of a device channel.
    CheckOut {
        /// The enrollment id.
        id: EnrollmentId,
    },
    /// `mdm.Connect` of a device channel: the device answered a command, or said it is idle.
    Result {
        /// The enrollment id.
        id: EnrollmentId,
        /// The command answered; `None` for an `Idle` with no command.
        command_uuid: Option<String>,
        /// `Acknowledged`, `Error`, `CommandFormatError`, `NotNow` or `Idle`.
        status: String,
        /// The result's property list.
        payload: Vec<u8>,
    },
}

/// Why a webhook body is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EventError {
    /// The body is not the event's JSON.
    #[error("the body is not a NanoHUB webhook event: {0}")]
    Json(String),
    /// The topic's event object is missing.
    #[error("the {0} event carries no {1}")]
    Missing(String, &'static str),
    /// The enrollment id is not 1 to 64 ASCII letters, digits and `-`.
    #[error("the {0} event's enrollment id is not one the gate keeps")]
    Id(String),
    /// `raw_payload` is not base64 of a property-list dictionary.
    #[error("the {0} event's raw_payload is not a property list")]
    Payload(String),
    /// The check-in's `MessageType` is not the topic's.
    #[error("the {0} event's payload says MessageType {1}")]
    MessageType(String, String),
}

#[derive(Deserialize)]
struct Wire {
    topic: String,
    checkin_event: Option<Inner>,
    acknowledge_event: Option<Inner>,
}

#[derive(Deserialize)]
struct Inner {
    ids: Option<Ids>,
    udid: Option<String>,
    #[serde(default)]
    raw_payload: String,
    token_update_tally: Option<u64>,
    command_uuid: Option<String>,
    status: Option<String>,
}

#[derive(Deserialize)]
struct Ids {
    id: String,
    #[serde(rename = "type")]
    kind: String,
}

/// The topics the gate acts on.
const AUTHENTICATE: &str = "mdm.Authenticate";
const TOKEN_UPDATE: &str = "mdm.TokenUpdate";
const CHECK_OUT: &str = "mdm.CheckOut";
const CONNECT: &str = "mdm.Connect";

/// The event in `body`; `Ok(None)` for one the gate passes over (another topic, or a channel
/// that is not a device's).
///
/// # Errors
/// [`EventError`]: the body is not an event, or an event of a topic the gate reads is not
/// whole.
pub fn parse(body: &[u8]) -> Result<Option<Event>, EventError> {
    let wire: Wire =
        serde_json::from_slice(body).map_err(|why| EventError::Json(why.to_string()))?;
    let topic = wire.topic;
    let inner = match topic.as_str() {
        AUTHENTICATE | TOKEN_UPDATE | CHECK_OUT => wire.checkin_event,
        CONNECT => wire.acknowledge_event,
        _ => return Ok(None),
    }
    .ok_or_else(|| EventError::Missing(topic.clone(), "event object"))?;
    let ids = inner
        .ids
        .ok_or_else(|| EventError::Missing(topic.clone(), "ids"))?;
    if ids.kind != "Device" {
        return Ok(None);
    }
    let id = EnrollmentId::parse(&ids.id).ok_or_else(|| EventError::Id(topic.clone()))?;
    let payload = STANDARD
        .decode(inner.raw_payload.as_bytes())
        .map_err(|_| EventError::Payload(topic.clone()))?;
    let dict = Value::from_reader(std::io::Cursor::new(&payload))
        .ok()
        .and_then(Value::into_dictionary)
        .ok_or_else(|| EventError::Payload(topic.clone()))?;
    let udid = inner.udid.unwrap_or_default();
    if topic == CONNECT {
        return Ok(Some(Event::Result {
            id,
            command_uuid: inner.command_uuid,
            status: inner.status.unwrap_or_default(),
            payload,
        }));
    }
    let said = dict
        .get("MessageType")
        .and_then(Value::as_string)
        .unwrap_or_default();
    if Some(said) != topic.strip_prefix("mdm.") {
        return Err(EventError::MessageType(topic, said.to_owned()));
    }
    Ok(Some(match topic.as_str() {
        AUTHENTICATE => Event::Authenticate {
            id,
            udid,
            serial: dict
                .get("SerialNumber")
                .and_then(Value::as_string)
                .filter(|serial| is_serial(serial))
                .map(str::to_owned),
        },
        TOKEN_UPDATE => Event::TokenUpdate {
            id,
            udid,
            tally: inner.token_update_tally,
        },
        _ => Event::CheckOut { id },
    }))
}
