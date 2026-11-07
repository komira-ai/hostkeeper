//! TFTP for UEFI PXE (network-install.md sections 3 and 4): read requests resolved against the
//! files `hostkeeper-pxe` serves, and the transfer of one file.
//!
//! [`on_request`] reads a datagram from the server port and gives the [`Target`] it reads, or
//! the [`Refused`] reason and the TFTP error to send. Only names of the [`FileSet`] are served:
//! a write request, a name with a `..`, `.` or empty component, a backslash or a character that
//! is not printable ASCII, and an unknown name are refused. One leading `/` names the root of
//! the served tree, as a TFTP server confined to its directory reads it, so `/grub/grub.cfg` is
//! `grub/grub.cfg` (GRUB asks with the leading `/`, firmware without); a name that then still
//! starts with `/` is refused, and so is every other absolute path, because none is in the set.
//!
//! [`BOOT_EFI`] is the one name router mode's router gives every client. It is served as the
//! shim of the architecture learned for the reader's address ([`crate::dhcp::Lease`]), or, when
//! the client stated none, of its record's platform; an address with no lease, a client with
//! neither, an architecture this design does not install and a platform with no boot set are
//! refused with "file not found", so the firmware goes on to its next boot device.
//!
//! [`Transfer`] then sends the file: the OACK of the options taken, DATA blocks numbered from
//! 1, each sent again on a timeout until acknowledged, and a final block shorter than the block
//! size (empty when the size is a multiple of it), as RFC 1350 and RFC 2347 give them.

mod packet;
mod transfer;

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_wire::Platform;

use crate::dhcp::Lease;
use crate::{BootSets, ClientArch, Mac};

pub use packet::{
    ClientPacket, DEFAULT_BLKSIZE, ErrorCode, MAX_BLKSIZE, MAX_REQUEST, Options, Request,
    TftpError, data_packet, parse_request,
};
pub use transfer::{Aborted, MAX_BLOCKS, MAX_RETRANSMITS, Send, Step, Transfer};

/// The name the router names in router mode, for every client: served as the shim of the
/// client's platform.
pub const BOOT_EFI: &str = "hostkeeper/boot.efi";

/// The platform of each inventory record that states one, by each of its data MACs, whatever
/// the record's state: the platform of `hostkeeper/boot.efi` for a client that stated no
/// architecture.
pub type RecordPlatforms = BTreeMap<Mac, Platform>;

/// A name a [`FileSet`] is not given.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{0:?} is not a served name: one or more components of printable ASCII other than space and \
     backslash, separated by single `/`, none `.` or `..`, with no leading `/`"
)]
pub struct NameError(pub String);

/// Whether `name` is a relative path of components that are neither empty, `.` nor `..`, of
/// printable ASCII other than space and `\`.
fn clean(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_graphic() && b != b'\\')
        && name.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// The names served: files, by exact name, and the directories whose every name is generated
/// at the moment it is asked for (the per-MAC boot menus of item N6, which give the local-disk
/// menu for a name that is not a well-formed MAC).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileSet {
    files: BTreeSet<String>,
    generated: BTreeSet<String>,
}

impl FileSet {
    /// The empty set: every name refused.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves the file `name`.
    ///
    /// # Errors
    /// [`NameError`] for a name [`on_request`] would refuse before looking it up.
    pub fn add_file(&mut self, name: &str) -> Result<(), NameError> {
        if !clean(name) {
            return Err(NameError(name.to_owned()));
        }
        self.files.insert(name.to_owned());
        Ok(())
    }

    /// Serves every name directly under the directory `dir` as a generated file.
    ///
    /// # Errors
    /// [`NameError`] for a directory name [`on_request`] would refuse.
    pub fn add_generated_dir(&mut self, dir: &str) -> Result<(), NameError> {
        if !clean(dir) {
            return Err(NameError(dir.to_owned()));
        }
        self.generated.insert(dir.to_owned());
        Ok(())
    }
}

/// What a read request reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A file of the set, by its name.
    File(String),
    /// A name directly under a generated directory.
    Generated {
        /// The directory.
        dir: String,
        /// The name in it.
        name: String,
    },
    /// [`BOOT_EFI`]: the shim of `platform`, served as the file `shim` is.
    BootEfi {
        /// The client.
        mac: Mac,
        /// Its platform.
        platform: Platform,
        /// The name of that platform's shim.
        shim: &'static str,
    },
}

/// A read request taken: what it reads, and the options it asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    /// What it reads.
    pub target: Target,
    /// The options to start its [`Transfer`] with.
    pub options: Options,
}

/// Why a request is refused. [`Refused::code`] is the TFTP error sent for it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    /// Not a well-formed request.
    #[error(transparent)]
    Malformed(#[from] TftpError),
    /// A write request: nothing is ever written.
    #[error("a write request for {0:?}")]
    Write(String),
    /// A mode other than `octet`.
    #[error("the mode {0:?}: only octet is served")]
    Mode(String),
    /// A name that could leave the served tree, or that no set holds.
    #[error("the name {0:?} is not a relative path of plain components")]
    Unsafe(String),
    /// A clean name that is not served.
    #[error("{0:?} is not served")]
    Unknown(String),
    /// [`BOOT_EFI`] from an address router mode learned no client for.
    #[error("{BOOT_EFI} from an address with no lease")]
    NoLease,
    /// [`BOOT_EFI`] for a client that stated no architecture and has no record platform.
    #[error("{BOOT_EFI} for {mac}, which stated no architecture and has no record platform")]
    NoPlatform {
        /// The client.
        mac: Mac,
    },
    /// [`BOOT_EFI`] for an architecture this design does not install.
    #[error("{BOOT_EFI} for {mac}, whose architecture {arch} is not installed")]
    UnsupportedArch {
        /// The client.
        mac: Mac,
        /// Its architecture.
        arch: ClientArch,
    },
    /// [`BOOT_EFI`] for a platform with no boot set.
    #[error("{BOOT_EFI} for {mac}, which is {platform}, with no boot set")]
    NoBootSet {
        /// The client.
        mac: Mac,
        /// Its platform.
        platform: Platform,
    },
    /// A file with more blocks than a block number counts.
    #[error("{size} bytes need more than {MAX_BLOCKS} blocks of {blksize}")]
    TooLarge {
        /// The file's size.
        size: u64,
        /// The block size.
        blksize: u16,
    },
}

impl Refused {
    /// The TFTP error code sent for this refusal.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Malformed(_) => ErrorCode::IllegalOperation,
            Self::Write(_) | Self::Unsafe(_) => ErrorCode::AccessViolation,
            Self::Mode(_) | Self::TooLarge { .. } => ErrorCode::NotDefined,
            Self::Unknown(_)
            | Self::NoLease
            | Self::NoPlatform { .. }
            | Self::UnsupportedArch { .. }
            | Self::NoBootSet { .. } => ErrorCode::FileNotFound,
        }
    }

    /// The ERROR packet sent for this refusal.
    #[must_use]
    pub fn packet(&self) -> Vec<u8> {
        self.code().packet()
    }
}

/// What the request `bytes` reads. `client` is what router mode learned for the address it
/// came from; `records` and `boot` are those of this run.
///
/// # Errors
/// [`Refused`], whose [`Refused::packet`] is the answer to send.
pub fn on_request(
    bytes: &[u8],
    files: &FileSet,
    client: Option<&Lease>,
    records: &RecordPlatforms,
    boot: &BootSets,
) -> Result<Read, Refused> {
    let request = parse_request(bytes)?;
    if request.write {
        return Err(Refused::Write(request.name));
    }
    if !request.mode.eq_ignore_ascii_case("octet") {
        return Err(Refused::Mode(request.mode));
    }
    let name = request.name.strip_prefix('/').unwrap_or(&request.name);
    if !clean(name) {
        return Err(Refused::Unsafe(request.name));
    }
    let target = if name == BOOT_EFI {
        boot_efi(client, records, boot)?
    } else if files.files.contains(name) {
        Target::File(name.to_owned())
    } else {
        match name.rsplit_once('/') {
            Some((dir, leaf)) if files.generated.contains(dir) => Target::Generated {
                dir: dir.to_owned(),
                name: leaf.to_owned(),
            },
            _ => return Err(Refused::Unknown(request.name)),
        }
    };
    Ok(Read {
        target,
        options: request.options,
    })
}

/// The shim [`BOOT_EFI`] is for `client`: by its architecture, or else its record's platform.
fn boot_efi(
    client: Option<&Lease>,
    records: &RecordPlatforms,
    boot: &BootSets,
) -> Result<Target, Refused> {
    let lease = client.ok_or(Refused::NoLease)?;
    let mac = lease.mac;
    let platform = match lease.arch {
        Some(arch) => arch
            .platform()
            .ok_or(Refused::UnsupportedArch { mac, arch })?,
        None => *records.get(&mac).ok_or(Refused::NoPlatform { mac })?,
    };
    let loaders = boot
        .loaders(platform)
        .ok_or(Refused::NoBootSet { mac, platform })?;
    Ok(Target::BootEfi {
        mac,
        platform,
        shim: loaders.shim,
    })
}
