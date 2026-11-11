//! The one error of the crate.

/// The most characters of a Mac-supplied string an error text carries.
pub const MAX_QUOTED: usize = 64;

fn cut(s: &str) -> String {
    s.chars().take(MAX_QUOTED).collect()
}

/// Why a message was refused. The text names the key, never a key's value, so it can be logged.
/// The two texts that quote what a Mac sent (an unknown `MessageType` or `Status`) and the
/// parser's message are cut to [`MAX_QUOTED`] characters, so an error is bounded however large
/// the message was.
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

impl Error {
    pub(crate) fn not_plist(message: &str) -> Self {
        Self::NotPlist(cut(message))
    }

    pub(crate) fn unknown_message_type(found: &str) -> Self {
        Self::UnknownMessageType(cut(found))
    }

    pub(crate) fn unknown_status(found: &str) -> Self {
        Self::UnknownStatus(cut(found))
    }
}
