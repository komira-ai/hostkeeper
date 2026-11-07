//! The seed of a machine, against its goldens by exact equality: for each platform, with and
//! without a role, with one SSH key and several, for each `--install-disk` form, with a serial
//! console, and on a port other than 80 (network-install.md section 11, "Seed generator"). Each
//! user-data is also read back as YAML, and the whole list of its keys and values is asserted, so
//! no field beyond the documented ones is in a seed.

mod support;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_pxe_seed::inputs::{InstallDisk, LinuxPlatform};
use saphyr_parser::{Event, Parser};
use support::{Build, DAEMON_CONFIG, record, server_on, several_keys};

/// Every scalar of a YAML document, as `(path, value)` in document order. A mapping's entry is
/// `.key`, a sequence's item `[index]`. A document with an alias, an anchor, a tag or more than
/// one document panics.
fn scalars(text: &str) -> Vec<(String, String)> {
    enum Open {
        Map { path: String, key: Option<String> },
        Seq { path: String, next: usize },
    }
    /// The path of the next node in `top`, and moves `top` on past it.
    fn child(top: Option<&mut Open>, scalar: Option<&str>) -> Option<String> {
        match top {
            None => Some(String::new()),
            Some(Open::Seq { path, next }) => {
                *next += 1;
                Some(format!("{path}[{}]", *next - 1))
            }
            Some(Open::Map { path, key }) => match key.take() {
                Some(key) => Some(format!("{path}.{key}")),
                None => {
                    *key = Some(scalar.expect("a mapping key is a scalar").to_owned());
                    None
                }
            },
        }
    }
    let mut stack: Vec<Open> = Vec::new();
    let mut out = Vec::new();
    let mut documents = 0;
    for event in Parser::new_from_str(text) {
        let (event, _) = event.expect("the seed is YAML");
        match event {
            Event::DocumentStart(_) => documents += 1,
            Event::Scalar(value, _, anchor, tag) => {
                assert_eq!((anchor, tag.is_none()), (0, true), "no anchor or tag");
                if let Some(path) = child(stack.last_mut(), Some(&value)) {
                    out.push((path, value.into_owned()));
                }
            }
            Event::MappingStart(anchor, ref tag) | Event::SequenceStart(anchor, ref tag) => {
                assert_eq!((anchor, tag.is_none()), (0, true), "no anchor or tag");
                let is_map = matches!(event, Event::MappingStart(..));
                let path = child(stack.last_mut(), None).expect("a collection is not a key");
                stack.push(if is_map {
                    Open::Map { path, key: None }
                } else {
                    Open::Seq { path, next: 0 }
                });
            }
            Event::MappingEnd | Event::SequenceEnd => {
                stack.pop();
            }
            Event::Alias(_) => panic!("the seed holds no alias"),
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => {}
        }
    }
    assert_eq!(documents, 1, "one YAML document");
    out
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(path, value)| ((*path).to_owned(), (*value).to_owned()))
        .collect()
}

/// Catches a seed whose deb is another platform's, or that differs from its golden in any byte.
#[test]
fn the_seed_of_each_platform_is_its_golden() {
    let goldens = [
        (
            LinuxPlatform::X86_64,
            include_str!("golden/user-data-x86_64.yaml"),
        ),
        (
            LinuxPlatform::Arm64,
            include_str!("golden/user-data-arm64.yaml"),
        ),
    ];
    for (platform, golden) in goldens {
        let seed = Build::new(platform).seed();
        assert_eq!(seed.user_data, golden, "{platform}");
        assert_eq!(seed.meta_data, include_str!("golden/meta-data.yaml"));
    }
}

/// Catches a field beyond the documented ones (a token, a mirror, a second account) slipping
/// into a seed: the whole list of the user-data's keys and values, read back as YAML. It also
/// catches quoting that YAML reads differently from what was meant, and a daemon config that is
/// not carried byte for byte.
#[test]
fn the_seed_holds_exactly_the_documented_fields() {
    let seed = Build::new(LinuxPlatform::Arm64).seed();
    let config = STANDARD.encode(DAEMON_CONFIG);
    let key = support::key_line("ssh-ed25519", 1, "operator-laptop");
    let expected = pairs(&[
        (".autoinstall.version", "1"),
        (".autoinstall.refresh-installer.update", "false"),
        (".autoinstall.apt.geoip", "false"),
        (
            ".autoinstall.apt.mirror-selection.primary[0].uri",
            "http://192.0.2.10/no-mirror/",
        ),
        (".autoinstall.apt.fallback", "offline-install"),
        (".autoinstall.storage.layout.name", "direct"),
        (".autoinstall.storage.layout.match.ssd", "true"),
        (".autoinstall.storage.layout.match.size", "smallest"),
        (".autoinstall.ssh.install-server", "true"),
        (".autoinstall.ssh.allow-pw", "false"),
        (
            ".autoinstall.late-commands[0]",
            "curl --fail --silent --show-error --output /target/var/tmp/hostkeeper.deb http://192.0.2.10/deb/hostkeeper_arm64.deb",
        ),
        (
            ".autoinstall.late-commands[1]",
            "curtin in-target -- dpkg -i /var/tmp/hostkeeper.deb",
        ),
        (".autoinstall.user-data.hostname", "node-a"),
        (".autoinstall.user-data.users[0].name", "operator"),
        (".autoinstall.user-data.users[0].lock_passwd", "true"),
        (".autoinstall.user-data.users[0].shell", "/bin/bash"),
        (
            ".autoinstall.user-data.users[0].sudo",
            "ALL=(ALL) NOPASSWD:ALL",
        ),
        (
            ".autoinstall.user-data.users[0].ssh_authorized_keys[0]",
            &key,
        ),
        (
            ".autoinstall.user-data.write_files[0].path",
            "/etc/hostkeeper/hostkeeper.toml",
        ),
        (".autoinstall.user-data.write_files[0].owner", "root:root"),
        (".autoinstall.user-data.write_files[0].permissions", "0600"),
        (".autoinstall.user-data.write_files[0].encoding", "b64"),
        (".autoinstall.user-data.write_files[0].content", &config),
        (
            ".autoinstall.user-data.write_files[1].path",
            "/etc/cloud/cloud.cfg.d/99_hostkeeper.cfg",
        ),
        (".autoinstall.user-data.write_files[1].owner", "root:root"),
        (".autoinstall.user-data.write_files[1].permissions", "0644"),
        (
            ".autoinstall.user-data.write_files[1].content",
            "datasource_list: [ NoCloud, None ]\n",
        ),
    ]);
    assert_eq!(scalars(&seed.user_data), expected);
    assert!(seed.user_data.starts_with("#cloud-config\n"));
    let decoded = STANDARD.decode(&config).expect("base64");
    assert_eq!(decoded, DAEMON_CONFIG.as_bytes());
    assert_eq!(
        scalars(&seed.meta_data),
        pairs(&[(".instance-id", "hostkeeper-node-a")])
    );
}

/// Catches the role reaching the seed, or a record with a role made into another install: the
/// coordinator gives the role at approval, and the seed is the same with or without one.
#[test]
fn a_record_with_a_role_has_the_seed_of_one_without() {
    for platform in LinuxPlatform::ALL {
        let without = Build::new(platform);
        let mut with = Build::new(platform);
        with.record = record("node-a", Some("build-farm"), Some(platform));
        assert_eq!(with.seed(), without.seed(), "{platform}");
    }
}

/// Catches keys lost, reordered, or quoted so that a comment with a quote or a backslash breaks
/// the YAML or reads back as other text.
#[test]
fn several_keys_are_all_in_the_seed_in_order() {
    let mut build = Build::new(LinuxPlatform::X86_64);
    build.keys = several_keys();
    let seed = build.seed();
    assert_eq!(
        seed.user_data,
        include_str!("golden/user-data-several-keys.yaml")
    );
    let keys: Vec<String> = scalars(&seed.user_data)
        .into_iter()
        .filter(|(path, _)| path.contains(".ssh_authorized_keys["))
        .map(|(_, value)| value)
        .collect();
    assert_eq!(
        keys,
        [
            support::key_line("ssh-ed25519", 1, "operator-laptop"),
            support::key_line("ecdsa-sha2-nistp256", 2, "on-call \"pager\" key \\ spare"),
            support::key_line("ssh-rsa", 3, ""),
        ]
    );
}

/// Catches an `--install-disk` form written as another, or a match that may pick more than
/// one disk: each form is its golden, and every match ends in `size: smallest`.
#[test]
fn each_install_disk_form_is_its_golden() {
    let cases = [
        (
            "serial=S4EV*",
            include_str!("golden/user-data-disk-serial.yaml"),
        ),
        (
            "model=Samsung SSD 980*",
            include_str!("golden/user-data-disk-model.yaml"),
        ),
        (
            "path=/dev/nvme0n1",
            include_str!("golden/user-data-disk-path.yaml"),
        ),
    ];
    for (flag, golden) in cases {
        let mut build = Build::new(LinuxPlatform::X86_64);
        build.disk = flag.parse::<InstallDisk>().expect("a disk form");
        let user_data = build.seed().user_data;
        assert_eq!(user_data, golden, "{flag}");
        let (form, glob) = flag.split_once('=').expect("form=glob");
        let matched: Vec<(String, String)> = scalars(&user_data)
            .into_iter()
            .filter(|(path, _)| path.starts_with(".autoinstall.storage."))
            .collect();
        assert_eq!(
            matched,
            pairs(&[
                (".autoinstall.storage.layout.name", "direct"),
                (&format!(".autoinstall.storage.layout.match.{form}"), glob),
                (".autoinstall.storage.layout.match.size", "smallest"),
            ])
        );
    }
}

/// Catches a serial console that is not the installed kernel's `console=`, or whose getty is
/// not enabled.
#[test]
fn a_serial_console_sets_the_kernel_console_and_enables_its_getty() {
    let mut build = Build::new(LinuxPlatform::Arm64);
    build.console = Some("ttyAMA0".parse().expect("a serial port"));
    let user_data = build.seed().user_data;
    assert_eq!(
        user_data,
        include_str!("golden/user-data-serial-console.yaml")
    );
    let added: Vec<(String, String)> = scalars(&user_data)
        .into_iter()
        .filter(|(path, _)| {
            path.starts_with(".autoinstall.user-data.write_files[2]")
                | path.starts_with(".autoinstall.user-data.runcmd")
        })
        .collect();
    assert_eq!(
        added,
        pairs(&[
            (
                ".autoinstall.user-data.write_files[2].path",
                "/etc/default/grub.d/99_hostkeeper.cfg"
            ),
            (".autoinstall.user-data.write_files[2].owner", "root:root"),
            (".autoinstall.user-data.write_files[2].permissions", "0644"),
            (
                ".autoinstall.user-data.write_files[2].content",
                "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX console=tty0 console=ttyAMA0,115200n8\"\n",
            ),
            (".autoinstall.user-data.runcmd[0][0]", "update-grub"),
            (".autoinstall.user-data.runcmd[1][0]", "systemctl"),
            (".autoinstall.user-data.runcmd[1][1]", "enable"),
            (".autoinstall.user-data.runcmd[1][2]", "--now"),
            (
                ".autoinstall.user-data.runcmd[1][3]",
                "serial-getty@ttyAMA0.service"
            ),
        ])
    );
}

/// Catches a seed on a boot server's port other than 80 that fetches the deb from port 80, or
/// names a mirror on the internet.
#[test]
fn a_port_other_than_80_is_in_the_deb_and_mirror_urls() {
    let mut build = Build::new(LinuxPlatform::X86_64);
    build.server = server_on(8080);
    assert_eq!(
        build.seed().user_data,
        include_str!("golden/user-data-port-8080.yaml")
    );
}
