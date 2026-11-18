//! A machine's NoCloud seed: the autoinstall `user-data` and the `meta-data`
//! (docs/design/network-install.md, section 4, "The seed").
//!
//! Each is a pure function of its inputs. What the seed holds is exactly this, and nothing
//! else:
//!
//! - An offline install: no refresh of the installer, no geoip, and one apt mirror that the
//!   boot server never serves ([`NO_MIRROR_PATH`]), so the installer takes the ISO's own pool
//!   (`fallback: offline-install`).
//! - The storage layout `direct` on the one disk [`InstallDisk`] matches.
//! - An SSH server with password login off.
//! - Two fixed `late-commands`: fetch the platform's deb into the target, then `dpkg -i` it
//!   there.
//! - The installed system's cloud-config: the host name (the record's host id); one account,
//!   `operator`, with its password locked, the given SSH keys and sudo; the daemon config at
//!   `/etc/hostkeeper/hostkeeper.toml`, owned by root, mode 0600, byte for byte (as base64); when
//!   the operator gave repository files, each of them first, at
//!   `/etc/hostkeeper/repositories/<file name>`, owned by root, mode 0644, byte for byte (as
//!   base64), so the entries are there when the daemon's config appears; the
//!   datasource list `[ NoCloud, None ]`; and, with a serial console, the kernel's `console=`
//!   and an enabled serial getty held at 115200 baud by a drop-in (`getty_drop_in`: the
//!   stock unit's `--keep-baud` list lets the port fall to a slower rate when a line arrives,
//!   and a BMC's serial console then shows NUL bytes), and on a `ttyS<n>` port GRUB's terminal
//!   on that port too (`GRUB_TERMINAL="console serial"` and `GRUB_SERIAL_COMMAND="serial
//!   --unit=<n> --speed=115200"`), so GRUB's output shows on the serial console as well
//!   (docs/design/console-broker.md, section 5.2).
//! - `meta-data`: the instance id, from the host id.
//!
//! Curtin's `reorder_uefi` is left at its default. The record's role is not in the seed.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::inputs::{BootServer, DaemonConfigText, InstallDisk, RepositoryFiles, SerialConsole};
use crate::keys::AuthorizedKeys;
use crate::layout::{NO_MIRROR_PATH, deb_path};
use crate::menu::Install;

/// The name of the installed system's one local account.
pub const OPERATOR_ACCOUNT: &str = "operator";

/// Where the late-commands put the deb in the target, as the target sees it.
const DEB_IN_TARGET: &str = "/var/tmp/hostkeeper.deb";

/// The cloud-init file of the installed system that pins its datasource list. Its name sorts
/// after the installer's own `99-installer.cfg` (`_` after `-`), so its list is the one in
/// force whatever the installer's file says.
const DATASOURCE_FILE: &str = "/etc/cloud/cloud.cfg.d/99_hostkeeper.cfg";

/// Where the installed system keeps its repository entries and keyrings.
const REPOSITORIES_DIR: &str = "/etc/hostkeeper/repositories";

/// The GRUB defaults file of the installed system that adds the serial console.
const GRUB_CONSOLE_FILE: &str = "/etc/default/grub.d/99_hostkeeper.cfg";

/// The getty's `ExecStart` in the drop-in: agetty at 115200 baud alone. The stock
/// `serial-getty@.service` runs agetty with `--keep-baud 115200,57600,38400,9600`, which on a
/// BMC's Serial over LAN drops the port to 9600 at the first Enter; with one rate and no
/// `--keep-baud` the port stays at the kernel console's 115200. `-o '-p -- \\u'` are the stock
/// unit's options for login (keep the environment; `\u`, which agetty replaces with the user
/// name, written `\\u` because systemd reads a backslash as an escape).
const GETTY_EXEC_START: &str = "-/sbin/agetty -o '-p -- \\\\u' 115200 - $TERM";

/// The drop-in of the installed system's `serial-getty@<port>.service`, at
/// `/etc/systemd/system/serial-getty@<port>.service.d/`, whose file name is this.
const GETTY_DROP_IN_FILE: &str = "99_hostkeeper.conf";

/// The drop-in that holds `port`'s getty at 115200 baud: an empty `ExecStart=` clears the
/// stock unit's command, and [`GETTY_EXEC_START`] is the one that runs.
fn getty_drop_in(port: &SerialConsole) -> (String, String) {
    (
        format!(
            "/etc/systemd/system/serial-getty@{}.service.d/{GETTY_DROP_IN_FILE}",
            port.as_str()
        ),
        format!("[Service]\nExecStart=\nExecStart={GETTY_EXEC_START}\n"),
    )
}

/// What a seed is made from.
#[derive(Clone, Debug)]
pub struct SeedInputs<'a> {
    /// The install the machine's menu chose: its record and the platform of the install.
    pub install: &'a Install,
    /// The boot server the deb is fetched from.
    pub server: BootServer,
    /// The daemon config the installed system gets.
    pub daemon_config: &'a DaemonConfigText,
    /// The operator account's SSH public keys.
    pub keys: &'a AuthorizedKeys,
    /// The disk the installer writes.
    pub install_disk: &'a InstallDisk,
    /// The installed system's serial console, if any.
    pub serial_console: Option<&'a SerialConsole>,
    /// The files of the installed system's `/etc/hostkeeper/repositories/`, if any.
    pub repositories: Option<&'a RepositoryFiles>,
}

/// The two files of a NoCloud seed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seed {
    /// `user-data`: a cloud-config whose one key is `autoinstall`.
    pub user_data: String,
    /// `meta-data`.
    pub meta_data: String,
}

/// The seed of `inputs`.
pub fn seed(inputs: &SeedInputs<'_>) -> Seed {
    Seed {
        user_data: user_data(inputs),
        meta_data: format!(
            "instance-id: hostkeeper-{}\n",
            inputs.install.record().host_id
        ),
    }
}

/// `text` as a YAML double-quoted scalar. Every text a seed quotes is printable ASCII and line
/// feeds, so `"`, `\` and the line feed are the only characters to escape.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The `match` lines of the storage layout, at six spaces.
fn disk_match(disk: &InstallDisk) -> String {
    let (key, value) = match disk {
        InstallDisk::SmallestSsd => ("ssd", "true".to_owned()),
        InstallDisk::Serial(glob) => ("serial", quoted(glob)),
        InstallDisk::Model(glob) => ("model", quoted(glob)),
        InstallDisk::Path(glob) => ("path", quoted(glob)),
    };
    format!("        {key}: {value}\n        size: smallest\n")
}

fn user_data(inputs: &SeedInputs<'_>) -> String {
    let mirror = quoted(&inputs.server.url(NO_MIRROR_PATH));
    let fetch = quoted(&format!(
        "curl --fail --silent --show-error --output /target{DEB_IN_TARGET} {}",
        inputs.server.url(deb_path(inputs.install.platform()))
    ));
    let install = quoted(&format!("curtin in-target -- dpkg -i {DEB_IN_TARGET}"));
    let keys: String = inputs
        .keys
        .keys()
        .iter()
        .map(|key| format!("          - {}\n", quoted(key)))
        .collect();
    let config = STANDARD.encode(inputs.daemon_config.as_str());
    let repositories: String = inputs
        .repositories
        .map(RepositoryFiles::files)
        .unwrap_or_default()
        .iter()
        .map(|(name, bytes)| {
            format!(
                "      - path: {REPOSITORIES_DIR}/{name}
        owner: \"root:root\"
        permissions: \"0644\"
        encoding: b64
        content: {}
",
                STANDARD.encode(bytes)
            )
        })
        .collect();
    let mut text = format!(
        "\
#cloud-config
autoinstall:
  version: 1
  refresh-installer:
    update: false
  apt:
    geoip: false
    mirror-selection:
      primary:
        - uri: {mirror}
    fallback: offline-install
  storage:
    layout:
      name: direct
      match:
{disk}  ssh:
    install-server: true
    allow-pw: false
  late-commands:
    - {fetch}
    - {install}
  user-data:
    hostname: {host}
    users:
      - name: {OPERATOR_ACCOUNT}
        lock_passwd: true
        shell: /bin/bash
        sudo: \"ALL=(ALL) NOPASSWD:ALL\"
        ssh_authorized_keys:
{keys}    write_files:
{repositories}      - path: /etc/hostkeeper/hostkeeper.toml
        owner: \"root:root\"
        permissions: \"0600\"
        encoding: b64
        content: {config}
      - path: {DATASOURCE_FILE}
        owner: \"root:root\"
        permissions: \"0644\"
        content: {datasource}
",
        disk = disk_match(inputs.install_disk),
        host = quoted(inputs.install.record().host_id.as_str()),
        datasource = quoted("datasource_list: [ NoCloud, None ]\n"),
    );
    if let Some(console) = inputs.serial_console {
        let port = console.as_str();
        let mut defaults = format!(
            "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX console=tty0 console={port},115200n8\"\n"
        );
        if let Some(unit) = console.grub_unit() {
            defaults.push_str(&format!(
                "GRUB_TERMINAL=\"console serial\"\n\
                 GRUB_SERIAL_COMMAND=\"serial --unit={unit} --speed=115200\"\n"
            ));
        }
        let defaults = quoted(&defaults);
        let (getty_path, getty) = getty_drop_in(console);
        let getty = quoted(&getty);
        text.push_str(&format!(
            "      - path: {GRUB_CONSOLE_FILE}
        owner: \"root:root\"
        permissions: \"0644\"
        content: {defaults}
      - path: {getty_path}
        owner: \"root:root\"
        permissions: \"0644\"
        content: {getty}
    runcmd:
      - [update-grub]
      - [systemctl, daemon-reload]
      - [systemctl, enable, --now, serial-getty@{port}.service]
"
        ));
    }
    text
}
