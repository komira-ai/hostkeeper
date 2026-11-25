//! The seed of a machine, against its goldens by exact equality: for each platform, with and
//! without a role, with one SSH key and several, for each `--install-disk` form, with a serial
//! console (`ttyS1` with GRUB's serial terminal, an Arm `ttyAMA` port without, and the getty
//! held at 115200 baud on each), and on a port
//! other than 80 (network-install.md section 11, "Seed generator"). Each
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

/// The GRUB defaults file of a seed: the content of its third `write_files` entry, read back as
/// YAML.
fn grub_defaults(user_data: &str) -> String {
    scalars(user_data)
        .into_iter()
        .find(|(path, _)| path == ".autoinstall.user-data.write_files[2].content")
        .map(|(_, value)| value)
        .expect("a GRUB defaults file")
}

/// The fleet's serial console, COM2 = `ttyS1` (docs/design/console-broker.md, section 5.2), on
/// x86-64: the installed kernel's `console=` and the getty on `ttyS1`, and GRUB's terminal on
/// the console and on serial unit 1 at 115200 baud, so GRUB's output shows over the BMC's serial
/// console too.
///
/// Catches a seed that leaves GRUB's output off the serial console, puts it on another unit or
/// speed, or differs from its golden in any byte.
#[test]
#[expect(
    non_snake_case,
    reason = "named after the port, ttyS1, as the console design names this test"
)]
fn the_seed_writes_the_grub_serial_terminal_for_ttyS1() {
    let mut build = Build::new(LinuxPlatform::X86_64);
    build.console = Some("ttyS1".parse().expect("a serial port"));
    let user_data = build.seed().user_data;
    assert_eq!(
        user_data,
        include_str!("golden/user-data-serial-console.yaml")
    );
    assert_eq!(
        grub_defaults(&user_data),
        "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX console=tty0 console=ttyS1,115200n8\"\n\
         GRUB_TERMINAL=\"console serial\"\n\
         GRUB_SERIAL_COMMAND=\"serial --unit=1 --speed=115200\"\n"
    );
}

/// GRUB's serial unit is the number of the `ttyS` port, at each end of its range and with two
/// digits; a `ttyAMA` port gets the kernel's `console=` alone and no GRUB serial terminal.
///
/// Catches a unit fixed at 1 whatever the port, a unit cut to its first digit, or a GRUB serial
/// command written for an Arm port, which has no GRUB unit.
#[test]
fn the_grub_serial_unit_is_the_ttys_number_and_a_ttyama_port_has_none() {
    for (platform, port, grub) in [
        (LinuxPlatform::X86_64, "ttyS0", Some("0")),
        (LinuxPlatform::X86_64, "ttyS10", Some("10")),
        (LinuxPlatform::X86_64, "ttyS99", Some("99")),
        (LinuxPlatform::Arm64, "ttyAMA1", None),
        (LinuxPlatform::Arm64, "ttyAMA12", None),
    ] {
        let mut build = Build::new(platform);
        build.console = Some(port.parse().expect("a serial port"));
        let mut want = format!(
            "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX console=tty0 console={port},115200n8\"\n"
        );
        if let Some(unit) = grub {
            want.push_str(&format!(
                "GRUB_TERMINAL=\"console serial\"\n\
                 GRUB_SERIAL_COMMAND=\"serial --unit={unit} --speed=115200\"\n"
            ));
        }
        assert_eq!(grub_defaults(&build.seed().user_data), want, "{port}");
    }
}

/// The getty drop-in of a seed with a serial console, as the installed system reads it: the
/// stock `ExecStart` cleared, then agetty at 115200 baud alone, with no `--keep-baud` list to
/// fall back to 9600 from (`\\u` is systemd's escape of agetty's `\u`).
const GETTY_DROP_IN: &str =
    "[Service]\nExecStart=\nExecStart=-/sbin/agetty -o '-p -- \\\\u' 115200 - $TERM\n";

/// `(path, content)` of every `write_files` entry of a seed, and its `runcmd` lines, each as
/// its words, read back as YAML.
fn files_and_runcmd(user_data: &str) -> (Vec<(String, String)>, Vec<Vec<String>>) {
    let all = scalars(user_data);
    let field = |index: usize, name: &str| {
        all.iter()
            .find(|(path, _)| {
                *path == format!(".autoinstall.user-data.write_files[{index}].{name}")
            })
            .map(|(_, value)| value.clone())
    };
    let files = (0..)
        .map_while(|index| Some((field(index, "path")?, field(index, "content")?)))
        .collect();
    let mut runcmd: Vec<Vec<String>> = Vec::new();
    for (path, value) in &all {
        let Some(rest) = path.strip_prefix(".autoinstall.user-data.runcmd[") else {
            continue;
        };
        let line: usize = rest[..rest.find(']').expect("an index")]
            .parse()
            .expect("a number");
        if runcmd.len() == line {
            runcmd.push(Vec::new());
        }
        runcmd[line].push(value.clone());
    }
    (files, runcmd)
}

/// The fleet's serial console `ttyS1` (docs/design/console-broker.md, section 5.2): the seed
/// writes the getty's drop-in, which clears the stock `ExecStart` and runs agetty at 115200
/// baud alone, and reloads systemd before it enables and starts the getty, so the getty that
/// starts is the one held at 115200.
///
/// Catches a getty left on the stock `--keep-baud 115200,57600,38400,9600` (which drops the
/// port to 9600 at the first Enter over Serial over LAN, after which the BMC's serial console
/// shows NUL bytes), a drop-in that adds a second `ExecStart` without clearing the first, a
/// drop-in for another port's getty or in another directory, a getty enabled before systemd
/// has read its drop-in, or a seed that differs from its golden in any byte.
#[test]
#[expect(
    non_snake_case,
    reason = "named after the port, ttyS1, as the console design names this test"
)]
fn the_seed_holds_the_ttyS1_getty_at_115200_baud() {
    let mut build = Build::new(LinuxPlatform::X86_64);
    build.console = Some("ttyS1".parse().expect("a serial port"));
    let user_data = build.seed().user_data;
    assert_eq!(
        user_data,
        include_str!("golden/user-data-serial-console.yaml")
    );
    let (files, runcmd) = files_and_runcmd(&user_data);
    let drop_in: Vec<&(String, String)> = files
        .iter()
        .filter(|(path, _)| path.contains("serial-getty"))
        .collect();
    assert_eq!(
        drop_in,
        [&(
            "/etc/systemd/system/serial-getty@ttyS1.service.d/99_hostkeeper.conf".to_owned(),
            GETTY_DROP_IN.to_owned()
        )]
    );
    assert!(!GETTY_DROP_IN.contains("keep-baud"));
    assert_eq!(
        runcmd,
        [
            vec!["update-grub"],
            vec!["systemctl", "daemon-reload"],
            vec!["systemctl", "enable", "--now", "serial-getty@ttyS1.service"],
        ]
    );
}

/// The drop-in is the getty's of the seed's own port, at each end of the `ttyS` range, with
/// two digits, and on an Arm `ttyAMA` port, whose kernel console is at 115200 baud too; a seed
/// without a serial console writes no drop-in and runs nothing.
///
/// Catches a drop-in fixed at `ttyS1` whatever the port, one cut to the first digit of the
/// port, one written for `ttyS` ports alone, or a drop-in or a `runcmd` in a seed with no
/// serial console.
#[test]
fn the_getty_drop_in_is_the_ports_own_and_absent_without_a_console() {
    for (platform, port) in [
        (LinuxPlatform::X86_64, "ttyS0"),
        (LinuxPlatform::X86_64, "ttyS10"),
        (LinuxPlatform::X86_64, "ttyS99"),
        (LinuxPlatform::Arm64, "ttyAMA1"),
        (LinuxPlatform::Arm64, "ttyAMA12"),
    ] {
        let mut build = Build::new(platform);
        build.console = Some(port.parse().expect("a serial port"));
        let (files, runcmd) = files_and_runcmd(&build.seed().user_data);
        assert_eq!(
            files.last(),
            Some(&(
                format!("/etc/systemd/system/serial-getty@{port}.service.d/99_hostkeeper.conf"),
                GETTY_DROP_IN.to_owned()
            )),
            "{port}"
        );
        assert_eq!(
            runcmd.last(),
            Some(&vec![
                "systemctl".to_owned(),
                "enable".to_owned(),
                "--now".to_owned(),
                format!("serial-getty@{port}.service")
            ]),
            "{port}"
        );
    }
    for platform in [LinuxPlatform::X86_64, LinuxPlatform::Arm64] {
        let user_data = Build::new(platform).seed().user_data;
        let (files, runcmd) = files_and_runcmd(&user_data);
        assert_eq!(files.len(), 2, "{platform}");
        assert!(runcmd.is_empty(), "{platform}");
        assert!(!user_data.contains("serial-getty"), "{platform}");
    }
}

/// Catches a serial console that is not the installed kernel's `console=`, or whose getty is
/// not enabled or not held at 115200 baud, on an Arm port, whose GRUB defaults are the kernel's
/// `console=` alone.
#[test]
fn a_serial_console_sets_the_kernel_console_and_enables_its_getty() {
    let mut build = Build::new(LinuxPlatform::Arm64);
    build.console = Some("ttyAMA0".parse().expect("a serial port"));
    let user_data = build.seed().user_data;
    assert_eq!(
        user_data,
        include_str!("golden/user-data-serial-console-arm64.yaml")
    );
    let added: Vec<(String, String)> = scalars(&user_data)
        .into_iter()
        .filter(|(path, _)| {
            path.starts_with(".autoinstall.user-data.write_files[2]")
                | path.starts_with(".autoinstall.user-data.write_files[3]")
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
            (
                ".autoinstall.user-data.write_files[3].path",
                "/etc/systemd/system/serial-getty@ttyAMA0.service.d/99_hostkeeper.conf"
            ),
            (".autoinstall.user-data.write_files[3].owner", "root:root"),
            (".autoinstall.user-data.write_files[3].permissions", "0644"),
            (
                ".autoinstall.user-data.write_files[3].content",
                GETTY_DROP_IN
            ),
            (".autoinstall.user-data.runcmd[0][0]", "update-grub"),
            (".autoinstall.user-data.runcmd[1][0]", "systemctl"),
            (".autoinstall.user-data.runcmd[1][1]", "daemon-reload"),
            (".autoinstall.user-data.runcmd[2][0]", "systemctl"),
            (".autoinstall.user-data.runcmd[2][1]", "enable"),
            (".autoinstall.user-data.runcmd[2][2]", "--now"),
            (
                ".autoinstall.user-data.runcmd[2][3]",
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

/// Catches a host id that YAML 1.1 (PyYAML, which cloud-init and the installer read the seed
/// with) would read as a boolean, a null, a number or a date: every host id is written quoted,
/// so each of these is the host name as text.
#[test]
fn a_host_id_that_yaml_reads_as_another_type_is_written_as_text() {
    for host_id in [
        "on",
        "no",
        "yes",
        "off",
        "true",
        "null",
        "123",
        "0123",
        "0x1f",
        "0b101",
        "1e3",
        "2026-11-06",
    ] {
        let mut build = Build::new(LinuxPlatform::X86_64);
        build.record = record(host_id, None, Some(LinuxPlatform::X86_64));
        let seed = build.seed();
        assert!(
            seed.user_data
                .contains(&format!("\n    hostname: \"{host_id}\"\n")),
            "{host_id}"
        );
        let hostname: Vec<(String, String)> = scalars(&seed.user_data)
            .into_iter()
            .filter(|(path, _)| path == ".autoinstall.user-data.hostname")
            .collect();
        assert_eq!(
            hostname,
            pairs(&[(".autoinstall.user-data.hostname", host_id)])
        );
        assert_eq!(
            seed.meta_data,
            format!("instance-id: hostkeeper-{host_id}\n")
        );
    }
}

/// Catches a disk glob whose YAML-special characters (`#`, `:`, `$`, `'`, `[`, `&`, `*`, `!`)
/// are read as a comment, a mapping, a flow sequence, an anchor, an alias or a tag: each glob
/// reads back as written.
#[test]
fn a_disk_glob_with_yaml_special_characters_reads_back_as_written() {
    for glob in [
        "Disk #1: [a]",
        "$x'y'",
        "&anchor *alias !tag",
        "{a, b}",
        "% @ `x`",
        "- a",
        "?x|>",
    ] {
        let mut build = Build::new(LinuxPlatform::X86_64);
        build.disk = format!("model={glob}").parse::<InstallDisk>().expect(glob);
        let model: Vec<(String, String)> = scalars(&build.seed().user_data)
            .into_iter()
            .filter(|(path, _)| path == ".autoinstall.storage.layout.match.model")
            .collect();
        assert_eq!(
            model,
            pairs(&[(".autoinstall.storage.layout.match.model", glob)]),
            "{glob}"
        );
    }
}

/// Catches repository files that are not written first, byte for byte, root-owned and mode 0644
/// at `/etc/hostkeeper/repositories/<name>` in name order, or that disturb the daemon config's
/// own entry: the seed is its golden, and the YAML read back has each file's fields in name order
/// before the daemon config's, whose content decodes to the given bytes.
#[test]
fn repository_files_are_written_first_byte_for_byte() {
    let mut build = Build::new(LinuxPlatform::X86_64);
    build.repositories = Some(support::repository_files());
    let user_data = build.seed().user_data;
    assert_eq!(
        user_data,
        include_str!("golden/user-data-repositories.yaml")
    );
    let files: Vec<(String, String)> = scalars(&user_data)
        .into_iter()
        .filter(|(path, _)| path.starts_with(".autoinstall.user-data.write_files["))
        .collect();
    let mut paths = Vec::new();
    for (index, (name, bytes)) in support::repository_files().files().iter().enumerate() {
        let at = format!(".autoinstall.user-data.write_files[{index}]");
        let get = |field: &str| {
            files
                .iter()
                .find(|(path, _)| *path == format!("{at}.{field}"))
                .map(|(_, value)| value.clone())
                .expect("the field")
        };
        paths.push(get("path"));
        assert_eq!(get("path"), format!("/etc/hostkeeper/repositories/{name}"));
        assert_eq!(get("owner"), "root:root");
        assert_eq!(get("permissions"), "0644");
        assert_eq!(get("encoding"), "b64");
        assert_eq!(STANDARD.decode(get("content")).expect("base64"), *bytes);
    }
    assert!(paths.windows(2).all(|pair| pair[0] < pair[1]), "name order");
    assert!(files.iter().any(
        |(path, value)| path == ".autoinstall.user-data.write_files[3].path"
            && value == "/etc/hostkeeper/hostkeeper.toml"
    ));
    // Three files of five fields, the daemon config's five and the datasource file's four.
    assert_eq!(files.len(), 3 * 5 + 5 + 4);
}

/// The console runbook's move of an installed server to the serial baseline, against this seed.
#[path = "seed/console_runbook.rs"]
mod console_runbook;
