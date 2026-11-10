//! What a Mac posts to the server URL: the status of its last command, or `Idle`.

use plist::{Dictionary, Value};

use crate::Error;
use crate::dict::{opt_id, opt_string, parse, string};

/// One entry of an `Error` result's `ErrorChain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorEntry {
    /// `ErrorCode`.
    pub code: i64,
    /// `ErrorDomain`.
    pub domain: String,
    /// `LocalizedDescription`, when sent.
    pub localized_description: Option<String>,
    /// `USEnglishDescription`, when sent.
    pub us_english_description: Option<String>,
}

/// The `Status` of a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The Mac has no command in hand and asks for the next one.
    Idle,
    /// The Mac ran the command.
    Acknowledged,
    /// The Mac could not run the command; the chain says why, possibly empty.
    Error(Vec<ErrorEntry>),
    /// The Mac cannot run the command now and will not be asked again until it checks in.
    NotNow,
    /// The command was not well formed.
    CommandFormatError(Vec<ErrorEntry>),
}

/// A result posted to the server URL.
#[derive(Clone, PartialEq)]
pub struct CommandResult {
    /// `UDID`, or `EnrollmentID` for a user enrollment: the connection's identity.
    pub device: String,
    /// The `Status`.
    pub status: Status,
    /// The `CommandUUID` the result answers; absent for [`Status::Idle`].
    pub command_uuid: Option<String>,
    /// The whole dictionary, for a command's answer keys (`QueryResponses`, `ProfileList`, ...).
    /// `Debug` of the result prints its key names only, as answers can hold secrets.
    pub raw: Dictionary,
}

impl std::fmt::Debug for CommandResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<&String> = self.raw.keys().collect();
        f.debug_struct("CommandResult")
            .field("device", &self.device)
            .field("status", &self.status)
            .field("command_uuid", &self.command_uuid)
            .field("raw_keys", &keys)
            .finish()
    }
}

impl CommandResult {
    /// Reads a result from a property list, XML or binary.
    ///
    /// # Errors
    ///
    /// [`Error`] when the bytes are not a dictionary, `Status` is missing or unknown, the device
    /// identity is missing, or a status other than `Idle` carries no `CommandUUID`.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let raw = parse(bytes)?;
        let status_text = string(&raw, "Status")?;
        let device = match opt_id(&raw, "UDID")? {
            Some(udid) => udid,
            None => opt_id(&raw, "EnrollmentID")?.ok_or(Error::NoDeviceId)?,
        };
        let status = match status_text.as_str() {
            "Idle" => Status::Idle,
            "Acknowledged" => Status::Acknowledged,
            "NotNow" => Status::NotNow,
            "Error" => Status::Error(chain(&raw)?),
            "CommandFormatError" => Status::CommandFormatError(chain(&raw)?),
            _ => return Err(Error::unknown_status(&status_text)),
        };
        let command_uuid = match status {
            Status::Idle => opt_id(&raw, "CommandUUID")?,
            _ => Some(string(&raw, "CommandUUID")?),
        };
        Ok(Self {
            device,
            status,
            command_uuid,
            raw,
        })
    }
}

fn chain(d: &Dictionary) -> Result<Vec<ErrorEntry>, Error> {
    let wrong = Error::WrongType {
        key: "ErrorChain",
        expected: "an array of dictionaries",
    };
    let Some(value) = d.get("ErrorChain") else {
        return Ok(Vec::new());
    };
    let Value::Array(items) = value else {
        return Err(wrong);
    };
    items
        .iter()
        .map(|item| {
            let entry = item.as_dictionary().ok_or_else(|| wrong.clone())?;
            let code = entry
                .get("ErrorCode")
                .and_then(Value::as_signed_integer)
                .ok_or(Error::WrongType {
                    key: "ErrorCode",
                    expected: "an integer",
                })?;
            Ok(ErrorEntry {
                code,
                domain: string(entry, "ErrorDomain")?,
                localized_description: opt_string(entry, "LocalizedDescription")?,
                us_english_description: opt_string(entry, "USEnglishDescription")?,
            })
        })
        .collect()
}
