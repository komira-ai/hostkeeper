//! The one error of the crate.

/// Why a message was refused. The text names the key, never a value, so it can be logged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The bytes are not a property list.
    #[error("not a property list: {0}")]
    NotPlist(String),
    /// The property list is not a dictionary at its top.
    #[error("the property list is not a dictionary")]
    NotDictionary,
    /// A required key is missing.
    #[error("key {0} is missing")]
    MissingKey(&'static str),
    /// A key has a value of another type than the protocol's.
    #[error("key {key} is not {expected}")]
    WrongType {
        /// The key.
        key: &'static str,
        /// The type the protocol gives it.
        expected: &'static str,
    },
    /// A key that must not be empty is.
    #[error("key {0} is empty")]
    Empty(&'static str),
    /// The `MessageType` of a check-in is not one this crate reads.
    #[error("unknown check-in message type {0:?}")]
    UnknownMessageType(String),
    /// The `Status` of a result is not one of the protocol's.
    #[error("unknown command status {0:?}")]
    UnknownStatus(String),
    /// A check-in names no device: neither `UDID` nor `EnrollmentID`.
    #[error("neither UDID nor EnrollmentID is present")]
    NoDeviceId,
    /// A property list could not be written.
    #[error("cannot write the property list: {0}")]
    Write(String),
}
