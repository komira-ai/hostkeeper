//! The fixtures the tests share: a daemon config, SSH public keys, records and the boot server.

#![allow(
    dead_code,
    reason = "each test binary includes this module and uses a different part of it"
)]

pub mod grub;

use std::net::{Ipv4Addr, SocketAddrV4};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_pxe_seed::inputs::{
    BootServer, DaemonConfigText, InstallDisk, IsoName, LinuxPlatform, Record, RepositoryFiles,
    SerialConsole,
};
use hostkeeper_pxe_seed::keys::AuthorizedKeys;
use hostkeeper_pxe_seed::menu::{Install, MenuChoice, MenuRequest, Mode};
use hostkeeper_pxe_seed::seed::{Seed, SeedInputs, seed};

/// A daemon config the daemon's parser accepts, with a comment, a `[bounds]` table and a
/// trailing line, so the seed is shown to carry it byte for byte.
pub const DAEMON_CONFIG: &str = "\
# The farm's daemon config for network-installed hosts.
coordinator     = \"https://192.0.2.1:8443\"
ca_sha256       = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"
release_channel = \"https://releases.example\"

[trusted_signers]
organisation  = \"komira-ai\"
workflow      = \".github/workflows/release.yml\"
refs          = [\"refs/tags/*\"]
hosted_runner = true

[bounds]
max_entries = 30000
";

/// The boot server of the goldens: 192.0.2.10 on HTTP's own port.
pub fn server() -> BootServer {
    server_on(80)
}

pub fn server_on(port: u16) -> BootServer {
    BootServer::new(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 10), port))
        .expect("a documentation address is a boot server")
}

pub fn record(host_id: &str, role: Option<&str>, platform: Option<LinuxPlatform>) -> Record {
    Record {
        host_id: host_id.parse().expect("a host id"),
        role: role.map(|r| r.parse().expect("a role")),
        platform,
    }
}

/// The base64 blob of a public key of type `kind`: the SSH string of the type, then `filler`
/// bytes standing in for the key itself.
pub fn blob(kind: &str, filler: u8) -> String {
    let mut bytes = u32::try_from(kind.len())
        .expect("a short type")
        .to_be_bytes()
        .to_vec();
    bytes.extend_from_slice(kind.as_bytes());
    bytes.extend_from_slice(&[0, 0, 0, 32]);
    bytes.extend(std::iter::repeat_n(filler, 32));
    STANDARD.encode(bytes)
}

/// One `authorized_keys` line.
pub fn key_line(kind: &str, filler: u8, comment: &str) -> String {
    match comment {
        "" => format!("{kind} {}", blob(kind, filler)),
        _ => format!("{kind} {} {comment}", blob(kind, filler)),
    }
}

pub fn one_key() -> AuthorizedKeys {
    AuthorizedKeys::parse(&format!(
        "{}\n",
        key_line("ssh-ed25519", 1, "operator-laptop")
    ))
    .expect("one public key")
}

pub fn several_keys() -> AuthorizedKeys {
    let text = format!(
        "# The farm's operators.\n{}\n\n{}\n{}\n",
        key_line("ssh-ed25519", 1, "operator-laptop"),
        key_line("ecdsa-sha2-nistp256", 2, "on-call \"pager\" key \\ spare"),
        key_line("ssh-rsa", 3, ""),
    );
    AuthorizedKeys::parse(&text).expect("three public keys")
}

pub fn daemon_config() -> DaemonConfigText {
    DaemonConfigText::parse(DAEMON_CONFIG).expect("the fixture config parses")
}

/// The seed of `record` with the defaults of the goldens: one key, the default disk, no serial
/// console, the boot server on port 80.
pub struct Build {
    pub record: Record,
    pub platform: LinuxPlatform,
    pub server: BootServer,
    pub keys: AuthorizedKeys,
    pub disk: InstallDisk,
    pub console: Option<SerialConsole>,
    pub repositories: Option<RepositoryFiles>,
}

impl Build {
    pub fn new(platform: LinuxPlatform) -> Build {
        Build {
            record: record("node-a", None, Some(platform)),
            platform,
            server: server(),
            keys: one_key(),
            disk: InstallDisk::default(),
            console: None,
            repositories: None,
        }
    }

    pub fn seed(&self) -> Seed {
        let config = daemon_config();
        let install = install(&self.record, self.platform);
        seed(&SeedInputs {
            install: &install,
            server: self.server,
            daemon_config: &config,
            keys: &self.keys,
            install_disk: &self.disk,
            serial_console: self.console.as_ref(),
            repositories: self.repositories.as_ref(),
        })
    }
}

/// The install of `record` on `platform`, chosen as `hostkeeper-pxe serve` chooses it: GRUB of
/// `platform` asks for the menu of a MAC in the answer set, in proxy mode.
pub fn install(record: &Record, platform: LinuxPlatform) -> Install {
    let path = format!("grub/hostkeeper/{}/52:54:00:12:34:56", platform.grub_cpu());
    let choice = MenuRequest::parse(&path)
        .expect("a menu path")
        .choose(Mode::Proxy, &|_| Some(record));
    match choice {
        MenuChoice::Install(install) => install,
        MenuChoice::LocalDisk(reason) => panic!("not an install: {reason}"),
    }
}

/// The served name of the platform's live-server ISO in the release's `netboot.json` today (the
/// file name of its pinned URL, packaging/netboot/pins.toml).
pub fn iso(platform: LinuxPlatform) -> IsoName {
    match platform {
        LinuxPlatform::X86_64 => "ubuntu-24.04.5-live-server-amd64.iso",
        LinuxPlatform::Arm64 => "ubuntu-24.04.5-live-server-arm64.iso",
    }
    .parse()
    .expect("an ISO name")
}

/// The repository files of the golden: two entries and the keyring they share.
pub fn repository_files() -> RepositoryFiles {
    let entry = |suite: &str| {
        format!(
            "Types: deb\nURIs: https://packages.example.com/apt\nSuites: {suite}\nComponents: main\n\
             Signed-By: /etc/hostkeeper/repositories/example-archive-keyring.gpg\n"
        )
        .into_bytes()
    };
    RepositoryFiles::new(vec![
        ("example-testing.sources".to_owned(), entry("testing")),
        (
            "example-archive-keyring.gpg".to_owned(),
            vec![0x99, 0x01, 0x0d, 0x00, 0xff],
        ),
        ("example-stable.sources".to_owned(), entry("stable")),
    ])
    .expect("three plain file names")
}
