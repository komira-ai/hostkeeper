//! `release netboot --pins P --version V --dir D`: the release asset `netboot.json`, the boot
//! sets of network install (docs/design/network-install.md, section 5), rendered from the
//! committed pins (`packaging/netboot/pins.toml`). The artifacts workflow writes it beside the
//! other assets before it sums them, and attests it with a bundle of its own.
//!
//! The asset holds one boot set for each of `linux-x86_64` and `linux-arm64`, and in each the five
//! files a network install serves: the live-server ISO, the shim, the signed network GRUB, the
//! kernel and the initrd. Each file has the name it is served as, its size and SHA-256, and where
//! it comes from: the ISO its `url` and the `sums` file of Canonical's that lists it; the kernel
//! and the initrd their `iso_path` inside the ISO; the shim and GRUB the `iso_path` of a deb in the
//! ISO's pool and their `deb_path` inside it.
//!
//! ```json
//! {"platforms":{
//!   "linux-arm64":{
//!     "grub":{"bytes":N,"deb_path":"usr/lib/...","iso_path":"pool/...deb","name":"hostkeeper/grubaa64.efi","sha256":"<64 hex>"},
//!     "initrd":{"bytes":N,"iso_path":"casper/initrd","name":"initrd-linux-arm64","sha256":"<64 hex>"},
//!     "iso":{"bytes":N,"name":"<file name of url>","sha256":"<64 hex>","sums":"https://.../SHA256SUMS","url":"https://.../...-arm64.iso"},
//!     "kernel":{"bytes":N,"iso_path":"casper/vmlinuz","name":"vmlinuz-linux-arm64","sha256":"<64 hex>"},
//!     "shim":{"bytes":N,"deb_path":"usr/lib/...","iso_path":"pool/...deb","name":"hostkeeper/shimaa64.efi","sha256":"<64 hex>"}},
//!   "linux-x86_64":{... "hostkeeper/grubx64.efi" ... "hostkeeper/shimx64.efi" ...}},
//!  "ubuntu":"24.04.5","version":"X.Y.Z"}
//! ```
//!
//! It is written canonically: compact JSON with every object's keys sorted. It changes additively
//! only, so a reader ignores a field it does not know. The served names of the shim and GRUB are
//! the design's, in the directory of the router-mode first file `hostkeeper/boot.efi`
//! ([`LOADER_DIR`]): `hostkeeper/shimx64.efi` or `hostkeeper/shimaa64.efi`, and
//! `hostkeeper/grubx64.efi` or `hostkeeper/grubaa64.efi`, the names `hostkeeper-pxe-packets`
//! serves them by (`LoaderNames`). The shim reads GRUB by its own name from that directory.
//!
//! The pins are read strictly, and every problem is named at once: a missing platform, file or
//! key, an unknown one, a size that is not a whole number above 0, a digest that is not 64
//! lowercase hex digits, a URL that is not `https://`, a path that is not relative or has an empty,
//! `.` or `..` part, and an ISO or deb of another architecture than its platform's.

use std::path::Path;

use serde_json::{Map, Value, json};

use super::Error;
use super::contract::{NETBOOT, Platform, Version};

/// Whether `text` is 64 lowercase hex digits.
fn is_sha256(text: &str) -> bool {
    (text.len() == 64) & text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The platforms with a boot set, in the order of their names, each with its architecture as a
/// deb names it and as a UEFI boot file names it.
const BOOT_PLATFORMS: [Arch; 2] = [
    Arch {
        platform: Platform::LinuxArm64,
        deb: "arm64",
        efi: "aa64",
    },
    Arch {
        platform: Platform::LinuxX86_64,
        deb: "amd64",
        efi: "x64",
    },
];

/// The key of the pins that names the Ubuntu release of the ISOs.
const UBUNTU: &str = "ubuntu";

/// The directory the shim and GRUB are served from: that of `hostkeeper/boot.efi`, the first file
/// the router names (docs/design/network-install.md, section 3).
const LOADER_DIR: &str = "hostkeeper/";

/// The name of Canonical's checksum file that lists an ISO.
const SUMS_FILE: &str = "SHA256SUMS";

/// A platform with a boot set, and the names of its architecture.
#[derive(Clone, Copy, Debug)]
struct Arch {
    platform: Platform,
    /// The architecture in a deb's name: `amd64`, `arm64`.
    deb: &'static str,
    /// The architecture in a UEFI boot file's name: `x64`, `aa64`.
    efi: &'static str,
}

/// The files of a boot set, by what each is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BootFile {
    Iso,
    Shim,
    Grub,
    Kernel,
    Initrd,
}

impl BootFile {
    const ALL: [BootFile; 5] = [
        BootFile::Iso,
        BootFile::Shim,
        BootFile::Grub,
        BootFile::Kernel,
        BootFile::Initrd,
    ];

    /// The file's key in its platform's table, of the pins and of the asset.
    fn key(self) -> &'static str {
        match self {
            BootFile::Iso => "iso",
            BootFile::Shim => "shim",
            BootFile::Grub => "grub",
            BootFile::Kernel => "kernel",
            BootFile::Initrd => "initrd",
        }
    }

    /// The keys of the file's table in the pins, every one required: where it comes from, its
    /// size and its SHA-256.
    fn fields(self) -> &'static [&'static str] {
        match self {
            BootFile::Iso => &["url", "sums", "bytes", "sha256"],
            BootFile::Shim | BootFile::Grub => &["iso_path", "deb_path", "bytes", "sha256"],
            BootFile::Kernel | BootFile::Initrd => &["iso_path", "bytes", "sha256"],
        }
    }

    /// The name the file is served as on `arch`; the ISO's is the file name of its `url`.
    fn served_name(self, arch: Arch, url: &str) -> String {
        let platform = arch.platform.name();
        match self {
            BootFile::Iso => url.rsplit('/').next().unwrap_or_default().to_owned(),
            BootFile::Shim => format!("{LOADER_DIR}shim{}.efi", arch.efi),
            BootFile::Grub => format!("{LOADER_DIR}grub{}.efi", arch.efi),
            BootFile::Kernel => format!("vmlinuz-{platform}"),
            BootFile::Initrd => format!("initrd-{platform}"),
        }
    }
}

/// The boot sets of one Ubuntu release, read from the pins.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Netboot {
    /// The Ubuntu release of the ISOs, `X.Y` or `X.Y.Z`.
    ubuntu: String,
    /// Each platform's boot set, as the asset holds it.
    platforms: Map<String, Value>,
}

impl Netboot {
    /// Reads the pins `text`, from the file `what`. A text that is not TOML is
    /// [`Error::Failed`]; pins of the wrong form are [`Error::Refused`], one line per problem,
    /// each starting with `what`.
    pub(crate) fn parse(text: &str, what: &str) -> Result<Netboot, Error> {
        let table: toml::Table = text
            .parse()
            .map_err(|e| Error::Failed(format!("{what}: {e}")))?;
        let mut problems = Vec::new();
        for key in table.keys() {
            let known = (key == UBUNTU) | BOOT_PLATFORMS.iter().any(|a| a.platform.name() == key);
            if !known {
                problems.push(format!(
                    "{key} is not a key of the pins (expected {UBUNTU}, {})",
                    platform_names()
                ));
            }
        }
        let ubuntu = match table.get(UBUNTU) {
            Some(value) => release(value).unwrap_or_else(|why| {
                problems.push(format!("{UBUNTU}: {why}"));
                String::new()
            }),
            None => {
                problems.push(format!("missing {UBUNTU}, the Ubuntu release of the ISOs"));
                String::new()
            }
        };
        let mut platforms = Map::new();
        for arch in BOOT_PLATFORMS {
            let name = arch.platform.name();
            match table.get(name) {
                Some(value) => {
                    platforms.insert(name.to_owned(), boot_set(arch, value, &mut problems));
                }
                None => problems.push(format!("missing the boot set of {name}")),
            }
        }
        if problems.is_empty() {
            Ok(Netboot { ubuntu, platforms })
        } else {
            Err(Error::Refused(
                problems.iter().map(|p| format!("{what}: {p}")).collect(),
            ))
        }
    }

    /// The ISO of the boot set of `platform`: its `url`, and its served name, SHA-256 and size
    /// as a pin to fetch it by; none for a platform with no boot set.
    pub(crate) fn iso(&self, platform: &str) -> Option<(String, crate::mirrored::Pin)> {
        let iso = self.platforms.get(platform)?.get(BootFile::Iso.key())?;
        let text = |key: &str| iso.get(key).and_then(Value::as_str);
        let pin = crate::mirrored::Pin {
            name: text("name")?.to_owned(),
            sha256: text("sha256")?.parse().ok()?,
            bytes: Some(iso.get("bytes").and_then(Value::as_u64)?),
        };
        Some((text("url")?.to_owned(), pin))
    }

    /// The canonical text of the asset of the release `version`: compact JSON with every
    /// object's keys sorted (the `serde_json` map is ordered by key).
    pub(crate) fn to_json(&self, version: &Version) -> String {
        json!({
            "platforms": self.platforms,
            "ubuntu": self.ubuntu,
            "version": version.to_string(),
        })
        .to_string()
    }
}

/// The names of the platforms with a boot set, in order, comma separated.
fn platform_names() -> String {
    BOOT_PLATFORMS.map(|a| a.platform.name()).join(", ")
}

/// The boot set of `arch` from its table `value` in the pins; each problem goes to `problems`.
fn boot_set(arch: Arch, value: &toml::Value, problems: &mut Vec<String>) -> Value {
    let platform = arch.platform.name();
    let Some(table) = value.as_table() else {
        problems.push(format!("{platform} is not a table"));
        return Value::Null;
    };
    for key in table.keys() {
        if !BootFile::ALL.iter().any(|r| r.key() == key) {
            problems.push(format!(
                "{platform}.{key} is not a file of a boot set (expected {})",
                BootFile::ALL.map(BootFile::key).join(", ")
            ));
        }
    }
    let mut set = Map::new();
    for role in BootFile::ALL {
        match table.get(role.key()) {
            Some(value) => {
                set.insert(role.key().to_owned(), file(arch, role, value, problems));
            }
            None => problems.push(format!("{platform}: missing {}", role.key())),
        }
    }
    Value::Object(set)
}

/// The file `role` of the boot set of `arch` from its table `value` in the pins, with the name
/// it is served as; each problem goes to `problems`.
fn file(arch: Arch, role: BootFile, value: &toml::Value, problems: &mut Vec<String>) -> Value {
    let what = format!("{}.{}", arch.platform.name(), role.key());
    let Some(table) = value.as_table() else {
        problems.push(format!("{what} is not a table"));
        return Value::Null;
    };
    for key in table.keys() {
        if !role.fields().contains(&key.as_str()) {
            problems.push(format!(
                "{what}.{key} is not a key of a {} (expected {})",
                role.key(),
                role.fields().join(", ")
            ));
        }
    }
    let mut file = Map::new();
    for &key in role.fields() {
        match table.get(key).map(|v| field(arch, role, key, v)) {
            Some(Ok(v)) => {
                file.insert(key.to_owned(), v);
            }
            Some(Err(why)) => problems.push(format!("{what}.{key}: {why}")),
            None => problems.push(format!("{what}: missing {key}")),
        }
    }
    let url = file.get("url").and_then(Value::as_str).unwrap_or_default();
    file.insert("name".to_owned(), json!(role.served_name(arch, url)));
    Value::Object(file)
}

/// The value of the key `key` of the file `role` of `arch`, or why it is not one.
fn field(arch: Arch, role: BootFile, key: &str, value: &toml::Value) -> Result<Value, String> {
    let Some(text) = value.as_str() else {
        return match key {
            "bytes" => size(value),
            _ => Err(format!("{value} is not a string")),
        };
    };
    match key {
        "sha256" if is_sha256(text) => Ok(json!(text)),
        "sha256" => Err(format!(
            "{text:?} is not a SHA-256 (64 lowercase hex digits)"
        )),
        "url" => iso_url(text, arch.deb),
        "sums" => sums_url(text),
        "iso_path" if matches!(role, BootFile::Shim | BootFile::Grub) => {
            inner_path(text).and(deb_of(text, arch.deb))
        }
        "iso_path" | "deb_path" => inner_path(text),
        _ => size(value),
    }
}

/// A size in bytes: a whole number above 0.
fn size(value: &toml::Value) -> Result<Value, String> {
    match value.as_integer() {
        Some(n) if n > 0 => Ok(json!(n)),
        _ => Err(format!(
            "{value} is not a size (a whole number of bytes above 0)"
        )),
    }
}

/// The Ubuntu release `value`: `X.Y` or `X.Y.Z`, decimal numbers.
fn release(value: &toml::Value) -> Result<String, String> {
    let Some(text) = value.as_str() else {
        return Err(format!("{value} is not a string"));
    };
    let parts: Vec<&str> = text.split('.').collect();
    let numbers = parts
        .iter()
        .all(|p| !p.is_empty() & p.bytes().all(|b| b.is_ascii_digit()));
    if (2..=3).contains(&parts.len()) & numbers {
        Ok(text.to_owned())
    } else {
        Err(format!("{text:?} is not an Ubuntu release (X.Y or X.Y.Z)"))
    }
}

/// Whether `part` is one part of a path or URL: not empty, `.` or `..`, and made of ASCII
/// letters, digits and `.`, `_`, `+`, `~` and `-` only.
fn plain(part: &str) -> bool {
    let chars = part
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() | b"._+~-".contains(&b));
    !matches!(part, "" | "." | "..") & chars
}

/// The parts of the `https://` URL `text` after the scheme, when each is [`plain`].
fn https_parts(text: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = text.strip_prefix("https://")?.split('/').collect();
    parts.iter().all(|p| plain(p)).then_some(parts)
}

/// The ISO's URL `text`: `https://`, a host and a path, the file named `...-<deb>.iso`.
fn iso_url(text: &str, deb: &str) -> Result<Value, String> {
    let suffix = format!("-{deb}.iso");
    match https_parts(text).as_deref() {
        Some([_, .., name]) if name.ends_with(&suffix) => Ok(json!(text)),
        _ => Err(format!(
            "{text:?} is not an https:// URL of a live-server ISO of {deb} (a file named \
             ...{suffix})"
        )),
    }
}

/// The URL `text` of Canonical's checksum file: `https://`, a host and a path, the file named
/// [`SUMS_FILE`].
fn sums_url(text: &str) -> Result<Value, String> {
    match https_parts(text).as_deref() {
        Some([_, .., name]) if *name == SUMS_FILE => Ok(json!(text)),
        _ => Err(format!(
            "{text:?} is not an https:// URL of a file named {SUMS_FILE}"
        )),
    }
}

/// The path `text` of a file inside the ISO or a deb: relative, every part [`plain`].
fn inner_path(text: &str) -> Result<Value, String> {
    if text.split('/').all(plain) {
        Ok(json!(text))
    } else {
        Err(format!(
            "{text:?} is not a relative path (parts of ASCII letters, digits and . _ + ~ -, \
             none empty, . or ..)"
        ))
    }
}

/// The path `text` of a deb of the architecture `deb` in the ISO's pool.
fn deb_of(text: &str, deb: &str) -> Result<Value, String> {
    let suffix = format!("_{deb}.deb");
    if text.ends_with(&suffix) {
        Ok(json!(text))
    } else {
        Err(format!(
            "{text:?} is not a deb of {deb} (a name ending {suffix})"
        ))
    }
}

/// The `release netboot --pins P --version V --dir D` verb: writes `D/netboot.json`, the boot
/// sets of the pins `pins` for the release `version`. Pins of the wrong form are refused, naming
/// every problem, and nothing is written.
pub(crate) fn write_netboot(
    pins: &Path,
    version: &Version,
    dir: &Path,
) -> Result<Vec<String>, Error> {
    let what = pins.display().to_string();
    let text = std::fs::read_to_string(pins).map_err(Error::io(format!("read {what}")))?;
    let netboot = Netboot::parse(&text, &what)?;
    let path = dir.join(NETBOOT);
    std::fs::write(&path, netboot.to_json(version))
        .map_err(Error::io(format!("write {}", path.display())))?;
    Ok(vec![format!(
        "{}: the boot sets of Ubuntu {} for {}",
        path.display(),
        netboot.ubuntu,
        platform_names()
    )])
}

#[cfg(test)]
mod tests;
