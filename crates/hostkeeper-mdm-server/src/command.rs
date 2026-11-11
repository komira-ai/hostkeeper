//! A command the server sends: the body of the answer to an `Idle` or a push-triggered connect.

use plist::{Dictionary, Value};

use crate::Error;
use crate::dict::{parse, string};

/// A command: its `CommandUUID`, its `RequestType` and the keys beside the type.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandRequest {
    /// `CommandUUID`, the identity the Mac's result carries back.
    pub uuid: String,
    /// `Command.RequestType`.
    pub request_type: String,
    /// The other keys of `Command`.
    pub fields: Dictionary,
}

impl CommandRequest {
    /// A command of `request_type` with `fields` beside the type.
    #[must_use]
    pub fn new(uuid: &str, request_type: &str, fields: Dictionary) -> Self {
        Self {
            uuid: uuid.to_owned(),
            request_type: request_type.to_owned(),
            fields,
        }
    }

    /// The XML property list the server answers with.
    ///
    /// # Errors
    ///
    /// [`Error::Write`] when the property list cannot be written, and [`Error::Empty`] for an
    /// empty uuid or request type, which a Mac would answer with `CommandFormatError`.
    pub fn to_plist_xml(&self) -> Result<Vec<u8>, Error> {
        if self.uuid.is_empty() {
            return Err(Error::Empty("CommandUUID"));
        }
        if self.request_type.is_empty() {
            return Err(Error::Empty("RequestType"));
        }
        let mut command = self.fields.clone();
        command.insert(
            "RequestType".to_owned(),
            Value::String(self.request_type.clone()),
        );
        let mut top = Dictionary::new();
        top.insert("CommandUUID".to_owned(), Value::String(self.uuid.clone()));
        top.insert("Command".to_owned(), Value::Dictionary(command));
        let mut out = Vec::new();
        Value::Dictionary(top)
            .to_writer_xml(&mut out)
            .map_err(|e| Error::Write(e.to_string()))?;
        Ok(out)
    }

    /// Reads a command back from its property list.
    ///
    /// # Errors
    ///
    /// [`Error`] when the bytes are not a dictionary with a `CommandUUID` and a `Command`
    /// dictionary that has a `RequestType`.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let top = parse(bytes)?;
        let uuid = string(&top, "CommandUUID")?;
        let mut fields = match top.get("Command") {
            None => return Err(Error::MissingKey("Command")),
            Some(Value::Dictionary(d)) => d.clone(),
            Some(_) => {
                return Err(Error::WrongType {
                    key: "Command",
                    expected: "a dictionary",
                });
            }
        };
        let request_type = string(&fields, "RequestType")?;
        fields.remove("RequestType");
        Ok(Self {
            uuid,
            request_type,
            fields,
        })
    }
}
