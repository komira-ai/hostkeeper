//! The MDM policy gate's device side (docs/design/hostkeeper-mdm.md sections 3 and 6.1), a
//! component linked into the coordinator.
//!
//! - [`CommandQueue`]: where the gate puts a command for a Mac. [`NanoHub`] is one: the client of
//!   NanoHUB's command queue, on loopback, with the API key read from a file ([`ApiKey`]); it
//!   sends only the five [`COMMAND_TYPES`]. The coordinator's own queue (`--mdm-listen`) is the
//!   other, and the Macs' check-ins then reach [`Gate::handle`] directly.
//! - [`webhook::serve`]: the loopback listener NanoHUB's webhook posts each check-in and
//!   command result to; a thin adapter that reads the body ([`parse_event`]) and calls
//!   [`Gate::handle`], the one entry point for events.
//! - [`Gate`]: the records of the devices, kept behind [`DeviceStore`] ([`FileDeviceStore`]: a
//!   file per device; the coordinator's database is another, held to the same cases by
//!   [`conformance`]), the `DeviceInformation` and `SecurityInfo` it asks each newly enrolled
//!   device, and [`Gate::status`], what it says of a Mac serial: the coordinator's
//!   `DeviceState`.
//!
//! - [`enroll`]: the enrollment side, linked in with [`Gate::with_enrollment`]: the config
//!   profile ([`ConfigProfile`]), sent again when its settings change, and the one-time nonce issued through the coordinator's
//!   [`Enrollments`] and pushed in the nonce profile once the Mac's daemon has posted its
//!   request; re-issue, re-push and removal.
//! - [`install`]: the pkg, linked in with [`Gate::with_install`]: the allowlist of pkg digests
//!   ([`PkgAllowlist`]), the `InstallEnterpriseApplication` with its inline manifest
//!   ([`pkg::manifest`]), and its re-send an hour after the Mac's last answer.

mod answer;
mod command;
pub mod conformance;
pub mod enroll;
mod event;
mod gate;
mod ids;
pub mod install;
mod nanohub;
pub mod pkg;
mod profile;
mod record;
mod store;
pub mod webhook;

pub use answer::{DeviceInformation, SecurityInfo};
pub use command::{
    COMMAND_TYPES, Command, DEVICE_INFORMATION, INSTALL_ENTERPRISE_APPLICATION, INSTALL_PROFILE,
    QUERIES, REMOVE_PROFILE, SECURITY_INFO,
};
pub use enroll::{EnrollSide, Enrollments, EnrollmentsError, Nonce, Waiting};
pub use event::{Event, EventError, parse as parse_event};
pub use gate::{DeviceStatus, Gate, Handled, Random, Unavailable};
pub use ids::{CommandUuid, EnrollmentId, is_serial};
pub use nanohub::{
    ApiKey, ApiKeyError, ApiUrl, ApiUrlError, CALL_TIMEOUT, CommandQueue, EnqueueError, KEY_FLAG,
    NanoHub, Queued, URL_FLAG,
};
pub use pkg::{
    ALLOWLIST_FLAG, HOST_FLAG, Install, MdmHost, MdmHostError, PKG_IDENTIFIER, PkgAllowlist,
    PkgAllowlistError, PkgDigest, PkgEntry, PkgFileError,
};
pub use profile::{
    CONFIG_DOMAIN, CONFIG_FLAG, ConfigProfile, DaemonProfile, DaemonProfileError,
    ENROLLMENT_DOMAIN, IDENTIFIER_FLAG, IdentifierError, MACOS_PKG_DOMAIN, MACOS_PKG_FLAG,
    MacosPkgProfile, MacosPkgProfileError, PREFERENCES_PAYLOAD, ProfileIdentifier, STORES_DOMAIN,
    STORES_FLAG, StoresProfile, StoresProfileError, nonce_profile,
};
pub use record::{
    DeviceRecord, InstallRecord, NonceRecord, Phase, ProfileAnswer, ProfileCommand, Sent,
};
pub use store::{DeviceStore, FileDeviceStore, SCHEMA, StoreError, decode_record, encode_record};
pub use webhook::WebhookError;
