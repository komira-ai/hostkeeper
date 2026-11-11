//! Typed reads of a property-list dictionary, so each failure names its key.

use std::io::Cursor;

use plist::{Dictionary, Value};

use crate::Error;

/// The top dictionary of a property list in XML or binary form.
pub(crate) fn parse(bytes: &[u8]) -> Result<Dictionary, Error> {
    let value =
        Value::from_reader(Cursor::new(bytes)).map_err(|e| Error::not_plist(&e.to_string()))?;
    value.into_dictionary().ok_or(Error::NotDictionary)
}

/// A string key that must be present and not empty.
pub(crate) fn string(d: &Dictionary, key: &'static str) -> Result<String, Error> {
    let s = opt_string(d, key)?.ok_or(Error::MissingKey(key))?;
    if s.is_empty() {
        return Err(Error::Empty(key));
    }
    Ok(s)
}

/// An identity key (`UDID`, `EnrollmentID`, `CommandUUID`) that may be absent but, when present,
/// must not be empty: an empty identity would key a device row or a queue on "".
pub(crate) fn opt_id(d: &Dictionary, key: &'static str) -> Result<Option<String>, Error> {
    match opt_string(d, key)? {
        Some(s) if s.is_empty() => Err(Error::Empty(key)),
        other => Ok(other),
    }
}

/// A string key that may be absent.
pub(crate) fn opt_string(d: &Dictionary, key: &'static str) -> Result<Option<String>, Error> {
    match d.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(Error::WrongType {
            key,
            expected: "a string",
        }),
    }
}

/// A data key that must be present and not empty.
pub(crate) fn data(d: &Dictionary, key: &'static str) -> Result<Vec<u8>, Error> {
    let bytes = opt_data(d, key)?.ok_or(Error::MissingKey(key))?;
    if bytes.is_empty() {
        return Err(Error::Empty(key));
    }
    Ok(bytes)
}

/// A data key that may be absent.
pub(crate) fn opt_data(d: &Dictionary, key: &'static str) -> Result<Option<Vec<u8>>, Error> {
    match d.get(key) {
        None => Ok(None),
        Some(Value::Data(b)) => Ok(Some(b.clone())),
        Some(_) => Err(Error::WrongType {
            key,
            expected: "data",
        }),
    }
}

/// A boolean key that may be absent.
pub(crate) fn opt_bool(d: &Dictionary, key: &'static str) -> Result<Option<bool>, Error> {
    match d.get(key) {
        None => Ok(None),
        Some(Value::Boolean(b)) => Ok(Some(*b)),
        Some(_) => Err(Error::WrongType {
            key,
            expected: "a boolean",
        }),
    }
}
