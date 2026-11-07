//! `netboot.json`, the release asset of the boot sets (`cargo xtask release netboot`,
//! docs/design/network-install.md section 5), read for the platforms a fetch asks for.
//!
//! ```json
//! {"platforms":{"linux-x86_64":{
//!    "grub":{"bytes":N,"deb_path":"usr/lib/...","iso_path":"pool/...deb","name":"hostkeeper/grubx64.efi","sha256":"<64 hex>"},
//!    "initrd":{"bytes":N,"iso_path":"casper/initrd","name":"initrd-linux-x86_64","sha256":"<64 hex>"},
//!    "iso":{"bytes":N,"name":"<file name>.iso","sha256":"<64 hex>","sums":"https://...","url":"https://..."},
//!    "kernel":{"bytes":N,"iso_path":"casper/vmlinuz","name":"vmlinuz-linux-x86_64","sha256":"<64 hex>"},
//!    "shim":{"bytes":N,"deb_path":"usr/lib/...","iso_path":"pool/...deb","name":"hostkeeper/shimx64.efi","sha256":"<64 hex>"}},
//!   "linux-arm64":{...}},
//!  "ubuntu":"24.04.5","version":"X.Y.Z"}
//! ```
//!
//! The asset changes additively only, so a field this reader does not know is ignored, and so is
//! the boot set of a platform not asked for. What it reads must be of its form, or the asset is
//! refused naming the first problem: the version `X.Y.Z` (it names the release's debs); for each
//! platform asked for, its boot set; for each of its five files a size above 0 and a SHA-256 of 64
//! lowercase hex digits; the ISO's `url` an `https://` URL and its name one file name ending in
//! `.iso`; the other four files served by the names the design fixes (`hostkeeper/shim<efi>.efi`,
//! `hostkeeper/grub<efi>.efi`, `vmlinuz-<platform>`, `initrd-<platform>`), and the paths they are
//! taken from relative, with no empty, `.` or `..` part.
//!
//! The asset is read only after its attestation is checked: what it says is then the release's.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use crate::platform::BootPlatform;

/// A file's pin: the name it is served as, its size and its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub name: String,
    pub bytes: u64,
    /// 64 lowercase hex digits.
    pub sha256: String,
}

/// A file taken out of the ISO: its path there, and its pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InIso {
    pub iso_path: String,
    pub pin: Pin,
}

/// A file taken out of a deb of the ISO's pool: the deb's path in the ISO, the file's path in the
/// deb, and the file's pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InDeb {
    pub iso_path: String,
    pub deb_path: String,
    pub pin: Pin,
}

/// The ISO: where it is downloaded from, and its pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Iso {
    pub url: String,
    pub pin: Pin,
}

/// The boot set of one platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootSet {
    pub iso: Iso,
    pub shim: InDeb,
    pub grub: InDeb,
    pub kernel: InIso,
    pub initrd: InIso,
}

/// What a fetch reads of `netboot.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Netboot {
    /// The release, `X.Y.Z`.
    pub version: String,
    /// The Ubuntu release of the ISOs.
    pub ubuntu: String,
    /// The boot set of each platform asked for.
    pub sets: BTreeMap<BootPlatform, BootSet>,
}

/// Why `netboot.json` is refused: the first problem, naming the key.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NetbootInvalid(pub String);

#[derive(Deserialize)]
struct AssetText {
    version: String,
    ubuntu: String,
    platforms: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct SetText {
    iso: IsoText,
    shim: DebText,
    grub: DebText,
    kernel: InIsoText,
    initrd: InIsoText,
}

#[derive(Deserialize)]
struct PinText {
    name: String,
    bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
struct IsoText {
    url: String,
    #[serde(flatten)]
    pin: PinText,
}

#[derive(Deserialize)]
struct InIsoText {
    iso_path: String,
    #[serde(flatten)]
    pin: PinText,
}

#[derive(Deserialize)]
struct DebText {
    iso_path: String,
    deb_path: String,
    #[serde(flatten)]
    pin: PinText,
}

/// `Err` naming `why` unless `ok`.
fn require(ok: bool, why: impl FnOnce() -> String) -> Result<(), NetbootInvalid> {
    match ok {
        true => Ok(()),
        false => Err(NetbootInvalid(why())),
    }
}

/// Whether `path` is relative, printable, and has no empty, `.` or `..` part.
pub(crate) fn is_safe_path(path: &str) -> bool {
    let printable = !path.is_empty() & path.bytes().all(|b| b.is_ascii_graphic());
    printable & path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// Whether `text` is `X.Y.Z`, three runs of decimal digits.
fn is_version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    (parts.len() == 3)
        & parts
            .iter()
            .all(|part| !part.is_empty() & part.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `text` is 64 lowercase hex digits.
fn is_sha256(text: &str) -> bool {
    (text.len() == 64) & text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Netboot {
    /// Reads `bytes` for the boot sets of `platforms`.
    pub fn parse(
        bytes: &[u8],
        platforms: &BTreeSet<BootPlatform>,
    ) -> Result<Netboot, NetbootInvalid> {
        let asset: AssetText = serde_json::from_slice(bytes)
            .map_err(|e| NetbootInvalid(format!("is not the boot-set asset: {e}")))?;
        require(is_version(&asset.version), || {
            format!("version {:?} is not X.Y.Z", asset.version)
        })?;
        let mut sets = BTreeMap::new();
        for &platform in platforms {
            let text = asset
                .platforms
                .get(platform.as_str())
                .ok_or_else(|| NetbootInvalid(format!("has no boot set for {platform}")))?;
            let set: SetText = serde_json::from_value(text.clone())
                .map_err(|e| NetbootInvalid(format!("{platform}: {e}")))?;
            sets.insert(platform, boot_set(platform, set)?);
        }
        Ok(Netboot {
            version: asset.version,
            ubuntu: asset.ubuntu,
            sets,
        })
    }
}

/// The boot set of `platform`, every value checked.
fn boot_set(platform: BootPlatform, set: SetText) -> Result<BootSet, NetbootInvalid> {
    let efi = platform.efi_arch();
    let iso = set.iso;
    require(
        iso.url.starts_with("https://") & iso.url.bytes().all(|b| b.is_ascii_graphic()),
        || format!("{platform} iso: url {:?} is not an https:// URL", iso.url),
    )?;
    let iso_name = &iso.pin.name;
    require(
        is_safe_path(iso_name) & !iso_name.contains('/') & iso_name.ends_with(".iso"),
        || format!("{platform} iso: name {iso_name:?} is not one file name ending in .iso"),
    )?;
    let pins = [
        ("iso", &iso.pin, None),
        (
            "shim",
            &set.shim.pin,
            Some(format!("hostkeeper/shim{efi}.efi")),
        ),
        (
            "grub",
            &set.grub.pin,
            Some(format!("hostkeeper/grub{efi}.efi")),
        ),
        (
            "kernel",
            &set.kernel.pin,
            Some(format!("vmlinuz-{platform}")),
        ),
        (
            "initrd",
            &set.initrd.pin,
            Some(format!("initrd-{platform}")),
        ),
    ];
    for (part, pin, served) in pins {
        require(pin.bytes > 0, || format!("{platform} {part}: bytes is 0"))?;
        require(is_sha256(&pin.sha256), || {
            format!(
                "{platform} {part}: sha256 {:?} is not 64 lowercase hex digits",
                pin.sha256
            )
        })?;
        let named = served.as_ref().is_none_or(|served| *served == pin.name);
        require(named, || {
            format!(
                "{platform} {part}: name {:?} is not {:?}",
                pin.name,
                served.clone().unwrap_or_default()
            )
        })?;
    }
    let paths = [
        ("shim iso_path", &set.shim.iso_path),
        ("shim deb_path", &set.shim.deb_path),
        ("grub iso_path", &set.grub.iso_path),
        ("grub deb_path", &set.grub.deb_path),
        ("kernel iso_path", &set.kernel.iso_path),
        ("initrd iso_path", &set.initrd.iso_path),
    ];
    for (key, path) in paths {
        require(is_safe_path(path), || {
            format!("{platform} {key} {path:?} is not a relative path without empty, . or .. parts")
        })?;
    }
    Ok(BootSet {
        iso: Iso {
            url: iso.url,
            pin: pin(iso.pin),
        },
        shim: in_deb(set.shim),
        grub: in_deb(set.grub),
        kernel: in_iso(set.kernel),
        initrd: in_iso(set.initrd),
    })
}

fn pin(text: PinText) -> Pin {
    Pin {
        name: text.name,
        bytes: text.bytes,
        sha256: text.sha256,
    }
}

fn in_iso(text: InIsoText) -> InIso {
    InIso {
        iso_path: text.iso_path,
        pin: pin(text.pin),
    }
}

fn in_deb(text: DebText) -> InDeb {
    InDeb {
        iso_path: text.iso_path,
        deb_path: text.deb_path,
        pin: pin(text.pin),
    }
}

#[cfg(test)]
#[path = "netboot_tests.rs"]
mod tests;
