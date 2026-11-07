//! The MDM policy gate's device side (docs/design/hostkeeper-mdm.md sections 3 and 6.1), a
//! component linked into the coordinator.
//!
//! - [`NanoHub`]: the client of NanoHUB's command queue, on loopback, with the API key read
//!   from a file ([`ApiKey`]); it sends only the five [`COMMAND_TYPES`].
//! - [`webhook::serve`]: the loopback listener NanoHUB's webhook posts each check-in and
//!   command result to.
//! - [`Gate`]: the records of the devices, kept behind [`DeviceStore`] ([`FileDeviceStore`]: a
//!   file per device), the `DeviceInformation` and `SecurityInfo` it asks each newly enrolled
//!   device, and [`Gate::status`], what it says of a Mac serial: the coordinator's
//!   `DeviceState`.
//!
//! - [`enroll`]: the enrollment side, linked in with [`Gate::with_enrollment`]: the config
//!   profile ([`DaemonProfile`]), and the one-time nonce issued through the coordinator's
//!   [`Enrollments`] and pushed in the nonce profile once the Mac's daemon has posted its
//!   request; re-issue, re-push and removal. The pkg is not here yet.

mod answer;
mod command;
pub mod enroll;
mod event;
mod gate;
mod ids;
mod nanohub;
mod profile;
mod record;
mod store;
pub mod webhook;

pub use answer::{DeviceInformation, SecurityInfo};
pub use command::{
    COMMAND_TYPES, Command, DEVICE_INFORMATION, INSTALL_PROFILE, QUERIES, REMOVE_PROFILE,
    SECURITY_INFO,
};
pub use enroll::{EnrollSide, Enrollments, EnrollmentsError, Nonce, Waiting};
pub use event::{Event, EventError, parse as parse_event};
pub use gate::{DeviceStatus, Gate, Handled, Random, Unavailable, WebhookError};
pub use ids::{CommandUuid, EnrollmentId, is_serial};
pub use nanohub::{
    ApiKey, ApiKeyError, ApiUrl, ApiUrlError, CALL_TIMEOUT, EnqueueError, KEY_FLAG, NanoHub,
    Queued, URL_FLAG,
};
pub use profile::{
    CONFIG_DOMAIN, CONFIG_FLAG, DaemonProfile, DaemonProfileError, ENROLLMENT_DOMAIN,
    IDENTIFIER_FLAG, IdentifierError, PREFERENCES_PAYLOAD, ProfileIdentifier, nonce_profile,
};
pub use record::{DeviceRecord, NonceRecord, Phase, ProfileAnswer, ProfileCommand, Sent};
pub use store::{DeviceStore, FileDeviceStore, SCHEMA, StoreError};
