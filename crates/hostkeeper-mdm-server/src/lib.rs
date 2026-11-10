//! The wire layer of the coordinator's MDM server (docs/design/coordinator-mdm.md section 8,
//! task CM5): the property-list messages of Apple's MDM protocol as typed values.
//!
//! - [`CheckIn`]: what a Mac posts to the check-in URL, one of `Authenticate`, `TokenUpdate`,
//!   `CheckOut`, `DeclarativeManagement`, `GetBootstrapToken` and `SetBootstrapToken`.
//! - [`CommandResult`]: what a Mac posts to the server URL, the answer to a command or, with
//!   [`Status::Idle`], a request for the next one.
//! - [`CommandRequest`]: a command the server sends, rendered as an XML property list.
//!
//! The crate is pure: bytes in, values out, no I/O, no clock, no randomness. A reader takes XML
//! or binary property lists, ignores keys it does not know (Apple adds keys), and refuses a
//! message whose type it does not know or whose required keys are missing or of the wrong type,
//! with an [`Error`] that names the key. The fixtures of its tests are Apple's machine-readable
//! schemas (`tests/fixtures/apple-schema`, MIT licence, with its notice) and property lists
//! written to them; each message type has a negative case.

mod checkin;
mod command;
mod dict;
mod error;
mod result;

pub use checkin::{
    Authenticate, CheckIn, CheckOut, DeclarativeManagement, DeviceId, GetBootstrapToken,
    SetBootstrapToken, TokenUpdate,
};
pub use command::CommandRequest;
pub use error::{Error, MAX_QUOTED};
pub use result::{CommandResult, ErrorEntry, Status};
