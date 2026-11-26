//! A machine's NoCloud seed: the autoinstall `user-data` and the `meta-data`
//! (docs/design/network-install.md, section 4, "The seed").
//!
//! Each is a pure function of its inputs. What the seed holds is exactly this, and nothing
//! else:
//!
//! - An offline install: no refresh of the installer, no geoip, and one apt mirror that the
//!   boot server never serves ([`NO_MIRROR_PATH`]), so the installer takes the ISO's own pool
//!   (`fallback: offline-install`).
//! - The storage layout `direct` on the one disk [`InstallDisk`] matches; with extra partitions
//!   ([`ExtraPartitions`]), a storage config in its place on that disk: a
//!   GPT table, the EFI system partition, each extra partition in the order given, and root
//!   (ext4) over the rest of the disk, each extra one mounted with its options.
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
//!   (docs/design/console-broker.md, section 5.2). These files are the daemon's own
//!   ([`hostkeeper_config::serial_console`]): a daemon whose config names the same port keeps them.
//!   With kernel parameters ([`KernelParameters`]), their own GRUB defaults file
//!   ([`KERNEL_PARAMETERS_FILE`]), after the console's files, which adds them to
//!   `GRUB_CMDLINE_LINUX`. One `update-grub` applies both, at the first boot, so both are in
//!   force from the second.
//! - `meta-data`: the instance id, from the host id.
//!
//! Curtin's `reorder_uefi` is left at its default. The record's role is not in the seed.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_config::serial_console::{GETTY_DROP_IN, GRUB_CONSOLE_FILE};

use crate::inputs::{BootServer, DaemonConfigText, InstallDisk, RepositoryFiles, SerialConsole};
use crate::kernel::{KERNEL_PARAMETERS_FILE, KernelParameters};
use crate::keys::AuthorizedKeys;
use crate::layout::{NO_MIRROR_PATH, deb_path};
use crate::menu::Install;
use crate::partitions::{ExtraPartition, ExtraPartitions};

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

/// The size of the EFI system partition of a storage config: 1 GiB.
const EFI_SIZE_BYTES: u64 = 1 << 30;

/// The drop-in that holds `port`'s getty at 115200 baud, and where it goes.
fn getty_drop_in(port: &SerialConsole) -> (String, &'static str) {
    (port.port().getty_drop_in_path(), GETTY_DROP_IN)
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
    /// The partitions the installer makes on the install disk beside root; with none, the
    /// layout is `direct`.
    pub extra_partitions: &'a ExtraPartitions,
    /// The parameters added to the installed kernel's command line; with none, no file of them
    /// is written.
    pub kernel_parameters: &'a KernelParameters,
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

/// The lines of a disk's `match`, each indented by `indent`.
fn disk_match(disk: &InstallDisk, indent: &str) -> String {
    let (key, value) = match disk {
        InstallDisk::SmallestSsd => ("ssd", "true".to_owned()),
        InstallDisk::Serial(glob) => ("serial", quoted(glob)),
        InstallDisk::Model(glob) => ("model", quoted(glob)),
        InstallDisk::Path(glob) => ("path", quoted(glob)),
    };
    format!("{indent}{key}: {value}\n{indent}size: smallest\n")
}

/// The `storage` section, at two spaces: the layout `direct` on the matched disk, or with extra
/// partitions the storage config of [`storage_config`] in its place.
fn storage(disk: &InstallDisk, extra: &ExtraPartitions) -> String {
    match extra.partitions() {
        [] => format!(
            "  storage:\n    layout:\n      name: direct\n      match:\n{}",
            disk_match(disk, "        ")
        ),
        _ => storage_config(disk, extra),
    }
}

/// The storage config of a disk with extra partitions, in the installer's action form: the disk
/// `disk` matches, wiped, with a GPT table; partition 1, the EFI system partition (1 GiB, fat32,
/// the one GRUB is installed to, at `/boot/efi`); then each extra partition in the order given,
/// with its GPT label, size and file system; and last root, ext4 at `/`, whose size `-1` takes the
/// rest of the disk. The mounts come after every partition: root, the EFI system partition, then
/// the extra ones in their mount points' order, so a mount point below another is mounted after
/// it; each extra one with its options when given.
fn storage_config(disk: &InstallDisk, extra: &ExtraPartitions) -> String {
    let partition_action = |id: &str, number: usize, size: &str, name: &str| {
        format!(
            "      - type: partition
        id: {id}
        device: disk-install
        number: {number}
        size: {size}
{name}        wipe: superblock
        preserve: false
"
        )
    };
    let format_action = |id: &str, volume: &str, fstype: &str| {
        format!(
            "      - type: format
        id: {id}
        volume: {volume}
        fstype: {fstype}
        preserve: false
"
        )
    };
    let mount_action = |id: &str, device: &str, path: &str, options: Option<&str>| {
        let options = options
            .map(|options| format!("        options: {}\n", quoted(options)))
            .unwrap_or_default();
        format!(
            "      - type: mount
        id: {id}
        device: {device}
        path: {path}
{options}"
        )
    };
    let mut text = format!(
        "  storage:
    config:
      - type: disk
        id: disk-install
        match:
{}        ptable: gpt
        wipe: superblock-recursive
        preserve: false
",
        disk_match(disk, "          ")
    );
    text.push_str(&partition_action(
        "part-efi",
        1,
        &EFI_SIZE_BYTES.to_string(),
        "        flag: boot\n        grub_device: true\n",
    ));
    text.push_str(&format_action("format-efi", "part-efi", "fat32"));
    let partitions = extra.partitions();
    for (index, each) in partitions.iter().enumerate() {
        let n = index + 1;
        text.push_str(&partition_action(
            &format!("part-extra-{n}"),
            n + 1,
            &each.size_bytes().to_string(),
            &format!("        partition_name: {}\n", quoted(each.label())),
        ));
        text.push_str(&format_action(
            &format!("format-extra-{n}"),
            &format!("part-extra-{n}"),
            each.filesystem().as_str(),
        ));
    }
    text.push_str(&partition_action(
        "part-root",
        partitions.len() + 2,
        "-1",
        "",
    ));
    text.push_str(&format_action("format-root", "part-root", "ext4"));
    text.push_str(&mount_action("mount-root", "format-root", "/", None));
    text.push_str(&mount_action("mount-efi", "format-efi", "/boot/efi", None));
    let mut mounts: Vec<(usize, &ExtraPartition)> = (1..).zip(partitions).collect();
    mounts.sort_by(|a, b| a.1.mount_point().cmp(b.1.mount_point()));
    for (n, each) in mounts {
        text.push_str(&mount_action(
            &format!("mount-extra-{n}"),
            &format!("format-extra-{n}"),
            each.mount_point(),
            each.mount_options(),
        ));
    }
    text
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
{storage}  ssh:
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
        storage = storage(inputs.install_disk, inputs.extra_partitions),
        host = quoted(inputs.install.record().host_id.as_str()),
        datasource = quoted("datasource_list: [ NoCloud, None ]\n"),
    );
    let mut runcmd = String::new();
    if let Some(console) = inputs.serial_console {
        let port = console.as_str();
        let defaults = console.port().grub_defaults();
        let defaults = quoted(&defaults);
        let (getty_path, getty) = getty_drop_in(console);
        let getty = quoted(getty);
        text.push_str(&format!(
            "      - path: {GRUB_CONSOLE_FILE}
        owner: \"root:root\"
        permissions: \"0644\"
        content: {defaults}
      - path: {getty_path}
        owner: \"root:root\"
        permissions: \"0644\"
        content: {getty}
"
        ));
        runcmd = format!(
            "      - [systemctl, daemon-reload]
      - [systemctl, enable, --now, serial-getty@{port}.service]
"
        );
    }
    let parameters = !inputs.kernel_parameters.parameters().is_empty();
    if parameters {
        let defaults = quoted(&inputs.kernel_parameters.grub_defaults());
        text.push_str(&format!(
            "      - path: {KERNEL_PARAMETERS_FILE}
        owner: \"root:root\"
        permissions: \"0644\"
        content: {defaults}
"
        ));
    }
    if inputs.serial_console.is_some() | parameters {
        text.push_str("    runcmd:\n      - [update-grub]\n");
        text.push_str(&runcmd);
    }
    text
}
