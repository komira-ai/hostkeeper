//! The seal: the last file `fetch` writes, `seal.json` in the fetched directory. It names every
//! file `serve` may serve from that directory, with the size and SHA-256 measured as `fetch`
//! wrote it, and what the attestations of `netboot.json` and of each deb confirmed. `serve`
//! (item N9 of docs/design/network-install.md) checks it at start and refuses to start on any
//! difference, so a directory with no seal, or whose files differ from it, is never served.
//!
//! ```json
//! {"netboot":{"commit":"<hex>","sha256":"<64 hex>","source_ref":"refs/heads/main"},
//!  "platforms":{"linux-x86_64":{
//!    "deb":{"bytes":N,"commit":"<hex>","name":"deb/hostkeeper_amd64.deb","sha256":"<64 hex>","source_ref":"refs/heads/main"},
//!    "grub":{"bytes":N,"name":"hostkeeper/grubx64.efi","sha256":"<64 hex>"},
//!    "initrd":{...},"iso":{...},"kernel":{...},"shim":{...}}},
//!  "seal":1,"ubuntu":"24.04.5","version":"X.Y.Z"}
//! ```
//!
//! It is one line of JSON, every object's keys sorted, and it changes additively only: `seal`
//! is the format's number, 1, and is raised only by a change a reader of 1 cannot read. A
//! reader takes a key it does not know as absent (`serve`'s reader, `serve/sealed.rs`, skips
//! them), so a key added later leaves this release able to read the seal.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The file name of the seal in the fetched directory.
pub const SEAL_FILE: &str = "seal.json";

/// The format number of [`Seal::seal`].
pub const SEAL_FORMAT: u32 = 1;

/// One sealed file: its name in the directory (the name it is served as), its size and its
/// SHA-256, as 64 lowercase hex digits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedFile {
    pub bytes: u64,
    pub name: String,
    pub sha256: String,
}

/// `netboot.json`: its SHA-256 and its attestation. The file itself is not kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedNetboot {
    pub commit: String,
    pub sha256: String,
    pub source_ref: String,
}

/// The hostkeeper deb of a platform, and its attestation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedDeb {
    pub bytes: u64,
    pub commit: String,
    pub name: String,
    pub sha256: String,
    pub source_ref: String,
}

/// The files of one platform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedSet {
    pub deb: SealedDeb,
    pub grub: SealedFile,
    pub initrd: SealedFile,
    pub iso: SealedFile,
    pub kernel: SealedFile,
    pub shim: SealedFile,
}

/// The seal of a fetched directory. Its fields are in the order of their names, which is the
/// order they are written in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seal {
    pub netboot: SealedNetboot,
    /// Each platform fetched, by its spelling.
    pub platforms: BTreeMap<String, SealedSet>,
    /// [`SEAL_FORMAT`].
    pub seal: u32,
    /// The Ubuntu release of the ISOs, as `netboot.json` names it.
    pub ubuntu: String,
    /// The hostkeeper release, as `netboot.json` names it.
    pub version: String,
}

impl Seal {
    /// The text of the seal: one line of JSON and a newline.
    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string(self).unwrap_or_default();
        text.push('\n');
        text
    }
}
