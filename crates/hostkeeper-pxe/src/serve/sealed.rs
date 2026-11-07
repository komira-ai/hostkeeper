//! The sealed directory, checked at start (docs/design/network-install.md, section 5, point 1:
//! "`hostkeeper-pxe serve` checks the seal at start and refuses to start on any difference").
//!
//! [`Sealed::open`] reads `seal.json` ([`crate::seal`]), and for each platform it names opens
//! every file of its boot set and its deb, without following a link, measures its size and
//! SHA-256 from the open file and compares both with the seal. Any difference refuses the
//! start, naming the file: a file that is missing, not a regular file, of another size or
//! another digest; a seal that is missing, does not parse, or is of a format this release
//! cannot read; and a file the seal names under another name than its platform's (a mixed or
//! hand-edited set). A platform key this release does not serve is skipped, and named in the
//! start line; a seal with no platform this release serves is refused.
//!
//! Each file is kept open, and every answer reads the file that was checked, by offset, from
//! that descriptor: a file renamed over or replaced after the start is not what is served.
//! Files in the directory that the seal does not name are never opened, and never served.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;

use hostkeeper_pxe_seed::inputs::{IsoName, LinuxPlatform};
use hostkeeper_pxe_seed::layout::{deb_path, initrd_path, kernel_path};
use sha2::{Digest as _, Sha256};

use crate::platform::BootPlatform;
use crate::seal::{SEAL_FILE, SEAL_FORMAT, Seal, SealedFile as SealEntry};
use crate::serve::platforms::{linux_of_boot, loaders_of};

/// The bytes read from a file at a time, when it is measured and when it is served.
pub const CHUNK: usize = 1 << 20;

/// Why the sealed directory is refused. Each names the file.
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    /// No `seal.json` could be read: the directory was never sealed, or is not there.
    #[error(
        "{SEAL_FILE} cannot be read ({0}): the directory was not sealed by hostkeeper-pxe fetch"
    )]
    NoSeal(io::Error),
    /// `seal.json` is not a seal.
    #[error("{SEAL_FILE} is not a seal: {0}")]
    Unreadable(String),
    /// The seal is of a format this release cannot read.
    #[error("{SEAL_FILE} is of format {0}; this release reads format {SEAL_FORMAT}")]
    Format(u32),
    /// A file of a platform is sealed under another name than the platform's.
    #[error("the {part} of {platform} is sealed as {found:?}, not {want:?}")]
    Name {
        platform: LinuxPlatform,
        part: &'static str,
        found: String,
        want: String,
    },
    /// The ISO's sealed name is not one file name ending in `.iso`.
    #[error("the ISO of {platform} is sealed as {found:?}, which is not an ISO's file name")]
    IsoName {
        platform: LinuxPlatform,
        found: String,
    },
    /// A sealed file cannot be opened or read.
    #[error("{name}: {error}")]
    Open { name: String, error: io::Error },
    /// A sealed name is not a regular file.
    #[error("{name}: not a regular file")]
    NotRegular { name: String },
    /// A sealed file's size differs from the seal.
    #[error("{name}: {found} bytes, and the seal says {sealed}")]
    Size {
        name: String,
        sealed: u64,
        found: u64,
    },
    /// A sealed file's SHA-256 differs from the seal.
    #[error("{name}: its SHA-256 is {found}, and the seal says {sealed}")]
    Digest {
        name: String,
        sealed: String,
        found: String,
    },
    /// The seal names no platform this release serves.
    #[error("{SEAL_FILE} names no platform this release serves (linux-x86_64, linux-arm64)")]
    NoPlatform,
}

/// One sealed file, open: its name, its size, and the descriptor every answer reads it from.
#[derive(Debug)]
pub struct OpenFile {
    name: String,
    bytes: u64,
    file: File,
}

impl OpenFile {
    /// Its name in the directory, which is the name it is served as.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its size, as sealed and measured.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The `len` bytes of the file from `start`. A file cut short since the start is an
    /// error, so a short answer is never sent as a whole one.
    ///
    /// # Errors
    /// The read's error, `UnexpectedEof` for a file shorter than `start + len`.
    pub fn read(&self, start: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; len];
        self.file.read_exact_at(&mut bytes, start)?;
        Ok(bytes)
    }
}

/// The boot set of one platform, as served.
#[derive(Clone, Debug)]
pub struct BootSet {
    /// The served name of its live-server ISO.
    pub iso: IsoName,
    /// The TFTP names of its boot loaders: the shim and GRUB.
    pub shim: String,
    pub grub: String,
}

/// The checked directory: every sealed file, open, by its served name, and each platform's boot
/// set.
#[derive(Debug)]
pub struct Sealed {
    files: BTreeMap<String, Arc<OpenFile>>,
    sets: BTreeMap<LinuxPlatform, BootSet>,
    skipped: Vec<String>,
    version: String,
}

impl Sealed {
    /// The directory `dir`, checked against its seal.
    ///
    /// # Errors
    /// [`SealError`], naming the file.
    pub fn open(dir: &Path) -> Result<Sealed, SealError> {
        let text = std::fs::read_to_string(dir.join(SEAL_FILE)).map_err(SealError::NoSeal)?;
        let seal: Seal =
            serde_json::from_str(&text).map_err(|e| SealError::Unreadable(e.to_string()))?;
        if seal.seal != SEAL_FORMAT {
            return Err(SealError::Format(seal.seal));
        }
        let mut sealed = Sealed {
            files: BTreeMap::new(),
            sets: BTreeMap::new(),
            skipped: Vec::new(),
            version: seal.version,
        };
        for (key, set) in seal.platforms {
            let Ok(boot) = key.parse::<BootPlatform>() else {
                sealed.skipped.push(key);
                continue;
            };
            let platform = linux_of_boot(boot);
            let loaders = loaders_of(platform);
            let deb = SealEntry {
                bytes: set.deb.bytes,
                name: set.deb.name,
                sha256: set.deb.sha256,
            };
            let parts = [
                ("shim", set.shim, loaders.shim.to_owned()),
                ("grub", set.grub, loaders.grub.to_owned()),
                ("kernel", set.kernel, http_name(kernel_path(platform))),
                ("initrd", set.initrd, http_name(initrd_path(platform))),
                ("deb", deb, http_name(deb_path(platform))),
            ];
            for (part, entry, want) in parts {
                if entry.name != want {
                    return Err(SealError::Name {
                        platform,
                        part,
                        found: entry.name,
                        want,
                    });
                }
                sealed.check(dir, entry)?;
            }
            let iso = set
                .iso
                .name
                .parse::<IsoName>()
                .map_err(|_| SealError::IsoName {
                    platform,
                    found: set.iso.name.clone(),
                })?;
            sealed.check(dir, set.iso)?;
            sealed.sets.insert(
                platform,
                BootSet {
                    iso,
                    shim: loaders.shim.to_owned(),
                    grub: loaders.grub.to_owned(),
                },
            );
        }
        if sealed.sets.is_empty() {
            return Err(SealError::NoPlatform);
        }
        Ok(sealed)
    }

    /// Opens `entry` under `dir`, measures it, and keeps it once it matches.
    fn check(&mut self, dir: &Path, entry: SealEntry) -> Result<(), SealError> {
        let name = entry.name;
        let open = |error| SealError::Open {
            name: name.clone(),
            error,
        };
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join(&name))
            .map_err(open)?;
        if !file.metadata().map_err(open)?.is_file() {
            return Err(SealError::NotRegular { name });
        }
        let (bytes, digest) = measure(&file).map_err(open)?;
        if bytes != entry.bytes {
            return Err(SealError::Size {
                name,
                sealed: entry.bytes,
                found: bytes,
            });
        }
        if digest != entry.sha256 {
            return Err(SealError::Digest {
                name,
                sealed: entry.sha256,
                found: digest,
            });
        }
        self.files
            .insert(name.clone(), Arc::new(OpenFile { name, bytes, file }));
        Ok(())
    }

    /// The sealed file served as `name` (relative, no leading `/`), if the seal names it.
    pub fn file(&self, name: &str) -> Option<Arc<OpenFile>> {
        self.files.get(name).cloned()
    }

    /// Every sealed file's served name.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    /// The boot set of `platform`, if the directory holds one.
    pub fn set(&self, platform: LinuxPlatform) -> Option<&BootSet> {
        self.sets.get(&platform)
    }

    /// Each boot set, by platform, in [`LinuxPlatform`]'s order.
    pub fn sets(&self) -> impl Iterator<Item = (LinuxPlatform, &BootSet)> {
        self.sets.iter().map(|(platform, set)| (*platform, set))
    }

    /// The platforms with a boot set, in [`LinuxPlatform`]'s order.
    pub fn platforms(&self) -> impl Iterator<Item = LinuxPlatform> + '_ {
        self.sets.keys().copied()
    }

    /// The platform keys of the seal this release does not serve.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    /// The hostkeeper release the directory was fetched from.
    pub fn version(&self) -> &str {
        &self.version
    }
}

/// The name an HTTP path of the seed crate's layout is sealed under: the path without its `/`.
fn http_name(path: &str) -> String {
    path.trim_start_matches('/').to_owned()
}

/// The size and SHA-256 of `file`, read from its start.
fn measure(mut file: &File) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; CHUNK];
    let mut bytes = 0_u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            return Ok((bytes, hex::encode(hasher.finalize())));
        }
        hasher.update(&buffer[..n]);
        bytes += n as u64;
    }
}
