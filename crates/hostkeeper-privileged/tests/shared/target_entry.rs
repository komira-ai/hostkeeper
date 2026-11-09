//! The entry of an order set by targets, as the self-update tests give it to the fixture daemon
//! (`tests/bin/selfupdate_daemon.rs`) and read it back from the package store the daemon records
//! it in.
//!
//! The entry is one `hostkeeper-archive` entry of the release the order names, as CI's set
//! writes it (`crates/xtask/src/release/targets.rs`) but for the second store: the `channel`
//! store, the archive at `S/V/S-V-<platform>.tar.zst` and its bundle at the same path with
//! `.sigstore.json`, the archive's digest (the order's `package_digest`), its size, the bundle's
//! digest and size, and the manifest's `min_hostkeeper`. The JSON is the file `order.json`'s
//! `entry` key and the record's `entry` key, in one shape ([`entry_json`]), so that what the
//! store was given is compared with what the order carried by equality.
//!
//! The module is shared by the Linux and the macOS self-update rigs; each includes it by path.

use hostkeeper_config::platform::Platform;
use serde_json::{Value, json};

/// The store both locations name: the one a daemon without a `stores.toml` has.
pub const STORE: &str = "channel";
/// The size the entry gives the archive.
pub const ARCHIVE_BYTES: u64 = 4096;
/// The digest the entry gives the attestation bundle.
pub const BUNDLE_DIGEST: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
/// The size the entry gives the attestation bundle.
pub const BUNDLE_BYTES: u64 = 512;
/// The `min_hostkeeper` of the entry's manifest: below every version the rigs run.
pub const MIN_HOSTKEEPER: &str = "0.1.0";

/// The entry of `service` at `version` whose archive has the SHA-256 `digest`.
pub fn entry_json(service: &str, version: &str, digest: &str) -> Value {
    let archive = format!(
        "{service}/{version}/{service}-{version}-{}.tar.zst",
        Platform::current()
    );
    json!({
        "installer": "hostkeeper-archive",
        "locations": [{"store": STORE, "locator": archive}],
        "digest": digest,
        "bytes": ARCHIVE_BYTES,
        "attestation": {
            "digest": BUNDLE_DIGEST,
            "bytes": BUNDLE_BYTES,
            "locations": [{"store": STORE, "locator": format!("{archive}.sigstore.json")}],
        },
        "min_hostkeeper": MIN_HOSTKEEPER,
    })
}
