//! The inputs of a menu and a seed, each checked once when it is made, so the generators
//! cannot fail: the install's platform, the boot server's address, the inventory record, the
//! install disk, the serial console and the daemon config.

use std::fmt;
use std::net::SocketAddrV4;
use std::str::FromStr;

use hostkeeper_config::daemon::{ConfigError, DaemonConfig};
use hostkeeper_wire::names::{HostId, RoleName};

/// A platform `hostkeeper-pxe` installs: one of the two Linux platforms of a release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinuxPlatform {
    /// `linux-x86_64`.
    X86_64,
    /// `linux-arm64`.
    Arm64,
}

impl LinuxPlatform {
    /// Both platforms.
    pub const ALL: [LinuxPlatform; 2] = [LinuxPlatform::X86_64, LinuxPlatform::Arm64];

    /// The spelling of the release: `linux-x86_64` or `linux-arm64`.
    pub const fn as_str(self) -> &'static str {
        match self {
            LinuxPlatform::X86_64 => "linux-x86_64",
            LinuxPlatform::Arm64 => "linux-arm64",
        }
    }

    /// The Debian architecture of the platform's deb: `amd64` or `arm64`.
    pub const fn deb_arch(self) -> &'static str {
        match self {
            LinuxPlatform::X86_64 => "amd64",
            LinuxPlatform::Arm64 => "arm64",
        }
    }

    /// The value GRUB gives `${grub_cpu}` on the platform: `x86_64` or `arm64`.
    pub const fn grub_cpu(self) -> &'static str {
        match self {
            LinuxPlatform::X86_64 => "x86_64",
            LinuxPlatform::Arm64 => "arm64",
        }
    }

    /// The platform whose [`LinuxPlatform::grub_cpu`] is exactly `cpu`; `None` for every other
    /// text, such as `i386` (a legacy BIOS GRUB) or `arm` (32-bit ARM).
    pub fn from_grub_cpu(cpu: &str) -> Option<LinuxPlatform> {
        LinuxPlatform::ALL
            .into_iter()
            .find(|platform| platform.grub_cpu() == cpu)
    }
}

impl fmt::Display for LinuxPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an address is not one a bare machine can reach `hostkeeper-pxe` at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BootServerError {
    /// `0.0.0.0`, a broadcast or a multicast address: it names no one host.
    #[error("the boot server's address {0} names no one host")]
    NotOneHost(std::net::Ipv4Addr),
    /// Port 0.
    #[error("the boot server's HTTP port is 0")]
    PortZero,
}

/// The address of `hostkeeper-pxe`'s HTTP server, as a bare machine dials it: an IPv4 address
/// and a port. A bare machine has no name service of ours, so it is an address, not a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootServer(SocketAddrV4);

impl BootServer {
    /// `address` as the boot server's: refused when it names no one host, or its port is 0.
    pub fn new(address: SocketAddrV4) -> Result<BootServer, BootServerError> {
        let ip = *address.ip();
        if ip.is_unspecified() | ip.is_broadcast() | ip.is_multicast() {
            return Err(BootServerError::NotOneHost(ip));
        }
        if address.port() == 0 {
            return Err(BootServerError::PortZero);
        }
        Ok(BootServer(address))
    }

    /// The address and port.
    pub fn address(&self) -> SocketAddrV4 {
        self.0
    }

    /// The URL authority: the address, then `:port` unless the port is HTTP's own, 80. A URL
    /// on port 80 names no port, so a GRUB that does not read a port in its `(http,...)`
    /// device still reaches the default port.
    fn authority(&self) -> String {
        match self.0.port() {
            80 => self.0.ip().to_string(),
            port => format!("{}:{port}", self.0.ip()),
        }
    }

    /// `http://<authority><path>`; `path` starts with `/`.
    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.authority())
    }

    /// GRUB's device for a file on this server, `(http,<authority>)`.
    pub(crate) fn grub_device(&self) -> String {
        format!("(http,{})", self.authority())
    }
}

/// The longest [`IsoName`], in bytes: a file name.
pub const MAX_ISO_NAME: usize = 255;

/// Why a live-server ISO's served name was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "the ISO's served name is not 1 to 255 of A-Za-z0-9._+-, not starting with a dot and ending in .iso"
)]
pub struct IsoNameError;

/// The name a platform's live-server ISO is served under: its `name` in the release's
/// `netboot.json` (network-install.md section 5), which is the file name of its URL, such as
/// `ubuntu-24.04.5-live-server-amd64.iso`. It changes with the pinned Ubuntu release, so it is an
/// input, not a constant. One file name: no `/`, nothing a URL or a GRUB word would need to quote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsoName(String);

impl IsoName {
    /// The name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for IsoName {
    type Err = IsoNameError;

    fn from_str(text: &str) -> Result<IsoName, IsoNameError> {
        let ok = (1..=MAX_ISO_NAME).contains(&text.len())
            & text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'_' | b'+' | b'-'))
            & !text.starts_with('.')
            & text.ends_with(".iso");
        ok.then(|| IsoName(text.to_owned())).ok_or(IsoNameError)
    }
}

/// What a menu and a seed need of an inventory record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The host id: the installed system's host name.
    pub host_id: HostId,
    /// The record's role. The coordinator gives the role at approval, from the record, so the
    /// role is not in the seed: a record with a role and one without give the same seed.
    pub role: Option<RoleName>,
    /// The record's platform; `None` when it was not recorded.
    pub platform: Option<LinuxPlatform>,
}

/// The longest glob of an [`InstallDisk`], in bytes.
pub const MAX_DISK_GLOB: usize = 128;

/// Why a `--install-disk` value was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InstallDiskError {
    #[error("--install-disk is not serial=<glob>, model=<glob> or path=<glob>")]
    Form,
    #[error(
        "--install-disk's glob is not 1 to 128 printable ASCII characters without a quote or a backslash, and with no space at either end"
    )]
    Glob,
    #[error("--install-disk path=<glob> does not start with /dev/")]
    NotDev,
}

/// The disk the installer writes, as an autoinstall storage match. Only that disk is written.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum InstallDisk {
    /// The smallest disk the kernel reports as not rotational: the default (Question 5 (a)).
    #[default]
    SmallestSsd,
    /// The smallest disk whose serial matches the glob.
    Serial(String),
    /// The smallest disk whose model matches the glob.
    Model(String),
    /// The smallest disk whose device path matches the glob, which starts with `/dev/`.
    Path(String),
}

impl FromStr for InstallDisk {
    type Err = InstallDiskError;

    /// The value of `--install-disk`: `serial=<glob>`, `model=<glob>` or `path=<glob>`.
    fn from_str(text: &str) -> Result<InstallDisk, InstallDiskError> {
        let (form, glob) = text.split_once('=').ok_or(InstallDiskError::Form)?;
        let make: fn(String) -> InstallDisk = match form {
            "serial" => InstallDisk::Serial,
            "model" => InstallDisk::Model,
            "path" => InstallDisk::Path,
            _ => return Err(InstallDiskError::Form),
        };
        let printable = glob
            .bytes()
            .all(|b| (b' '..=b'~').contains(&b) & (b != b'"') & (b != b'\\'));
        let ok = (1..=MAX_DISK_GLOB).contains(&glob.len())
            & printable
            & !glob.starts_with(' ')
            & !glob.ends_with(' ');
        if !ok {
            return Err(InstallDiskError::Glob);
        }
        if (form == "path") & !glob.starts_with("/dev/") {
            return Err(InstallDiskError::NotDev);
        }
        Ok(make(glob.to_owned()))
    }
}

/// Why a `--serial-console` value was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("--serial-console is not ttyS<n> or ttyAMA<n>, with n from 0 to 99")]
pub struct SerialConsoleError;

/// The serial port the installed kernel's console and a getty use: `ttyS<n>` (a 16550 port) or
/// `ttyAMA<n>` (an Arm PL011 port), `n` from 0 to 99, at 115200 baud, 8 bits, no parity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialConsole(String);

impl SerialConsole {
    /// The port's name, such as `ttyS1`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for SerialConsole {
    type Err = SerialConsoleError;

    fn from_str(text: &str) -> Result<SerialConsole, SerialConsoleError> {
        let number = text
            .strip_prefix("ttyAMA")
            .or_else(|| text.strip_prefix("ttyS"))
            .ok_or(SerialConsoleError)?;
        let digits = (1..=2).contains(&number.len()) & number.bytes().all(|b| b.is_ascii_digit());
        // `ttyS01` would name no device: a number is written without a leading 0.
        let canonical = (number.len() == 1) | !number.starts_with('0');
        (digits & canonical)
            .then(|| SerialConsole(text.to_owned()))
            .ok_or(SerialConsoleError)
    }
}

/// The text of a daemon config the daemon's own parser accepts, kept byte for byte: the seed
/// writes exactly this text to `/etc/hostkeeper/hostkeeper.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonConfigText(String);

impl DaemonConfigText {
    /// `text` once [`DaemonConfig::parse`] accepts it; the parser's refusal otherwise.
    pub fn parse(text: &str) -> Result<DaemonConfigText, ConfigError> {
        DaemonConfig::parse(text)?;
        Ok(DaemonConfigText(text.to_owned()))
    }

    /// The text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The longest repository file name, in bytes: a file name.
pub const MAX_REPOSITORY_NAME: usize = 255;

/// The most files of a host's repository directory a seed carries.
pub const MAX_REPOSITORY_FILES: usize = 64;

/// Why the repository files of a seed were refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RepositoryFilesError {
    /// No file: the directory names nothing an install could use.
    #[error("there is no repository file")]
    Empty,
    /// More than [`MAX_REPOSITORY_FILES`].
    #[error("there are {0} repository files, over {MAX_REPOSITORY_FILES}")]
    TooMany(usize),
    /// A name that is not one plain file name.
    #[error(
        "the repository file name {0:?} is not 1 to 255 of A-Za-z0-9._+-, not starting with a dot"
    )]
    Name(String),
    /// Two files of one name.
    #[error("the repository file name {0:?} is given twice")]
    Duplicate(String),
}

/// The files of the installed host's `/etc/hostkeeper/repositories/` (packages-repositories.md
/// section 3.2), as the seed writes them: each a file name and its bytes, in name order. The
/// caller has checked the files with the daemon's own loader; this checks only that each name is
/// one plain file name, so a seed's `write_files` path cannot leave the directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryFiles(Vec<(String, Vec<u8>)>);

impl RepositoryFiles {
    /// `files` sorted by name, once each name is one plain file name, none is given twice, and
    /// there is at least one and at most [`MAX_REPOSITORY_FILES`].
    pub fn new(mut files: Vec<(String, Vec<u8>)>) -> Result<RepositoryFiles, RepositoryFilesError> {
        if files.is_empty() {
            return Err(RepositoryFilesError::Empty);
        }
        if files.len() > MAX_REPOSITORY_FILES {
            return Err(RepositoryFilesError::TooMany(files.len()));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, _) in &files {
            let plain = (1..=MAX_REPOSITORY_NAME).contains(&name.len())
                & name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'_' | b'+' | b'-'))
                & !name.starts_with('.');
            if !plain {
                return Err(RepositoryFilesError::Name(name.clone()));
            }
        }
        if let Some(pair) = files.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(RepositoryFilesError::Duplicate(pair[0].0.clone()));
        }
        Ok(RepositoryFiles(files))
    }

    /// The files, `(name, bytes)`, in name order.
    pub fn files(&self) -> &[(String, Vec<u8>)] {
        &self.0
    }
}
