//! The MDM policy gate's device side (docs/design/hostkeeper-mdm.md sections 3 and 6.1), a
//! component linked into the coordinator.
//!
//! - [`CommandQueue`]: where the gate puts a command for a Mac. [`NanoHub`] is one: the client of
//!   NanoHUB's command queue, on loopback, with the API key read from a file ([`ApiKey`]); it
//!   sends only the five [`COMMAND_TYPES`]. The coordinator's own queue (`--mdm-listen`) is the
//!   sends only the eight [`COMMAND_TYPES`]. The coordinator's own queue (`--mdm-listen`) is the
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
//! - [`install`]: the hostkeeper pkg, linked in with [`Gate::with_install`]: the named installs
//!   ([`Installs`], the setting `installs`, seeded once from the old allowlist
//!   [`PkgAllowlist`]), the `InstallEnterpriseApplication` with its inline manifest
//!   ([`pkg::manifest`]), and its re-send an hour after the Mac's last answer.
//! - [`baseline`]: the enrollment baseline, linked in with [`Gate::with_baseline`]: per host
//!   group, the device name (`Settings`), Screen Sharing (`EnableRemoteDesktop`, supervised Macs
//!   only), profiles and every install but the hostkeeper pkg, sent
//!   once the config profile is acknowledged, kept by a `ProfileList` check, and reported per
//!   device by [`Gate::baseline_status`].

mod answer;
pub mod baseline;
mod command;
pub mod conformance;
pub mod enroll;
mod event;
mod gate;
mod ids;
pub mod install;
mod installs;
mod nanohub;
pub mod pkg;
mod profile;
mod record;
mod store;
pub mod webhook;

pub use answer::{DeviceInformation, SecurityInfo};
pub use baseline::{
    ACTION_BASELINE, Baseline, BaselineError, BaselineState, BaselineStatus, DEFAULT_CHECK_SECS,
    Item, ProfileItem,
};
pub use command::{
    COMMAND_TYPES, Command, DEVICE_INFORMATION, ENABLE_REMOTE_DESKTOP,
    INSTALL_ENTERPRISE_APPLICATION, INSTALL_PROFILE, PROFILE_LIST, QUERIES, REMOVE_PROFILE,
    SECURITY_INFO, SETTINGS,
};
pub use enroll::{EnrollSide, Enrollments, EnrollmentsError, Nonce, Waiting};
pub use event::{Event, EventError, parse as parse_event};
pub use gate::{DeviceStatus, Gate, Handled, Random, Unavailable};
pub use ids::{CommandUuid, EnrollmentId, is_serial};
pub use installs::{HOSTKEEPER_INSTALL, InstallEntry, Installs, InstallsError, SETTING_NAME};
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
    BaselineInstall, BaselineProfile, BaselineRecord, DeviceRecord, InstallRecord, NonceRecord,
    Phase, ProfileAnswer, ProfileCommand, Sent,
};
pub use store::{DeviceStore, FileDeviceStore, SCHEMA, StoreError, decode_record, encode_record};
pub use webhook::WebhookError;
