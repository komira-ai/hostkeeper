//! The inputs of a seed, each checked when it is made: the daemon config by the daemon's own
//! parser, the SSH keys file with any private key refused, the install disk, the serial console,
//! the boot server and the platform. Every refusal is asserted by its whole message.

mod support;

use std::net::{Ipv4Addr, SocketAddrV4};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_pxe_seed::inputs::{
    BootServer, BootServerError, DaemonConfigText, InstallDisk, InstallDiskError, IsoName,
    IsoNameError, LinuxPlatform, SerialConsole, SerialConsoleError,
};
use hostkeeper_pxe_seed::keys::{AuthorizedKeys, KEY_TYPES, KeysError};
use hostkeeper_pxe_seed::layout::{deb_path, initrd_path, iso_path, kernel_path};
use support::{DAEMON_CONFIG, blob, key_line};

/// Catches a seed made from a daemon config the daemon would refuse at its first start, on
/// every machine of the run: each config the daemon's parser refuses is refused here with the
/// parser's own error, and the accepted text is kept byte for byte.
#[test]
fn a_daemon_config_the_daemon_refuses_is_refused_with_its_error() {
    let refused = [
        String::new(),
        "coordinator = ".to_owned(),
        DAEMON_CONFIG.replace("https://192.0.2.1:8443", "http://192.0.2.1:8443"),
        DAEMON_CONFIG.replace("[bounds]", "[bounds]\nmax_package_bytes = 0"),
        DAEMON_CONFIG.replace("[trusted_signers]", "token = \"x\"\n[trusted_signers]"),
        DAEMON_CONFIG.replace("refs          = [\"refs/tags/*\"]\n", ""),
    ];
    for text in &refused {
        let expected = DaemonConfig::parse(text).expect_err("the daemon refuses it");
        let got = DaemonConfigText::parse(text).expect_err("refused here too");
        assert_eq!(got.to_string(), expected.to_string(), "{text:?}");
    }
    let accepted = DaemonConfigText::parse(DAEMON_CONFIG).expect("accepted");
    assert_eq!(accepted.as_str(), DAEMON_CONFIG);
}

/// Catches a private key given by mistake reaching a seed, or its text reaching a log: every
/// private key form refuses the whole file, naming only the line, and the message holds no byte
/// of the key.
#[test]
fn a_private_key_in_the_keys_file_is_refused_without_its_text() {
    let public = key_line("ssh-ed25519", 1, "operator-laptop");
    let armour = |kind: &str| format!("-----BEGIN {kind}PRIVATE KEY-----");
    let cases = [
        (
            format!("{}\nb3BlbnNzaC1rZXktdjEAAAAA\n", armour("OPENSSH ")),
            1,
        ),
        (
            format!(
                "{public}\n{}\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n",
                armour("")
            ),
            2,
        ),
        (format!("{public}\n\n{}\n", armour("RSA ")), 3),
        (format!("{public}\n# a note\n{}\n", armour("EC ")), 3),
        (
            format!("{public}\nPuTTY-User-Key-File-3: ssh-ed25519\nPrivate-Lines: 1\n"),
            2,
        ),
        // A private key after a line that is not a key is still named as a private key.
        (format!("not a key\n{}\n", armour("OPENSSH ")), 2),
    ];
    for (text, line) in cases {
        let error = AuthorizedKeys::parse(&text).expect_err("a private key is refused");
        assert_eq!(error, KeysError::PrivateKey { line });
        assert_eq!(
            error.to_string(),
            format!(
                "the SSH keys file holds a private key at line {line}; it must hold public keys only"
            )
        );
    }
}

/// Catches a keys file that would leave the operator account unreachable or carry a line that is
/// not a public key: each is refused, naming its line and kind, and never its text.
#[test]
fn a_keys_file_with_a_line_that_is_not_a_public_key_is_refused() {
    let good = key_line("ssh-ed25519", 1, "");
    let ed_blob = blob("ssh-ed25519", 1);
    let short = STANDARD.encode([&[0, 0, 0, 11][..], b"ssh"].concat());
    let huge = STANDARD.encode([&[255, 255, 255, 255][..], b"ssh-ed25519"].concat());
    let cases = [
        (
            format!("{good}\nssh-ed25519\n"),
            KeysError::NotAPublicKey { line: 2 },
        ),
        (
            format!("ssh-dss {}\n", blob("ssh-dss", 1)),
            KeysError::NotAPublicKey { line: 1 },
        ),
        (
            format!("from=\"192.0.2.0/24\" {good}\n"),
            KeysError::NotAPublicKey { line: 1 },
        ),
        (
            format!("SSH-ED25519 {ed_blob}\n"),
            KeysError::NotAPublicKey { line: 1 },
        ),
        (
            "ssh-ed25519 not*base64\n".to_owned(),
            KeysError::Blob { line: 1 },
        ),
        (format!("ssh-rsa {ed_blob}\n"), KeysError::Blob { line: 1 }),
        ("ssh-ed25519 AAAA\n".to_owned(), KeysError::Blob { line: 1 }),
        // A length longer than the blob, and one that does not fit in it at all.
        (
            format!("ssh-ed25519 {short}\n"),
            KeysError::Blob { line: 1 },
        ),
        (format!("ssh-ed25519 {huge}\n"), KeysError::Blob { line: 1 }),
        (
            format!("{good} caf\u{e9}\n"),
            KeysError::Comment { line: 1 },
        ),
        (format!("{good} a\u{7}b\n"), KeysError::Comment { line: 1 }),
        (String::new(), KeysError::Empty),
        ("\n# only a comment\n   \n".to_owned(), KeysError::Empty),
    ];
    for (text, expected) in cases {
        assert_eq!(AuthorizedKeys::parse(&text), Err(expected), "{text:?}");
    }
    let messages = [
        (
            KeysError::NotAPublicKey { line: 4 },
            "the SSH keys file's line 4 is not <type> <base64> [comment] with a known key type",
        ),
        (
            KeysError::Blob { line: 5 },
            "the SSH keys file's line 5 has a key blob that is not base64 of its key type",
        ),
        (
            KeysError::Comment { line: 6 },
            "the SSH keys file's line 6 has a comment that is not printable ASCII",
        ),
        (KeysError::Empty, "the SSH keys file holds no public key"),
    ];
    for (error, text) in messages {
        assert_eq!(error.to_string(), text);
    }
}

/// Catches a key type `ssh-keygen` makes that is refused, and a comment's white space or a
/// line's CR kept as it was written rather than as one space.
#[test]
fn every_key_type_is_read_and_written_in_one_form() {
    for kind in KEY_TYPES {
        let text = format!("  {kind}\t{}   two\twords \r\n", blob(kind, 7));
        let keys = AuthorizedKeys::parse(&text).expect(kind);
        assert_eq!(keys.keys(), [format!("{kind} {} two words", blob(kind, 7))]);
    }
}

/// Catches an `--install-disk` value that is read as a form it does not name, or a glob that
/// would need quoting the seed does not do.
#[test]
fn install_disk_forms_and_refusals() {
    assert_eq!(InstallDisk::default(), InstallDisk::SmallestSsd);
    let read = [
        ("serial=S4EV*", InstallDisk::Serial("S4EV*".to_owned())),
        (
            "model=Samsung SSD 980*",
            InstallDisk::Model("Samsung SSD 980*".to_owned()),
        ),
        (
            "path=/dev/nvme0n1",
            InstallDisk::Path("/dev/nvme0n1".to_owned()),
        ),
        (
            "path=/dev/disk/by-id/nvme-*",
            InstallDisk::Path("/dev/disk/by-id/nvme-*".to_owned()),
        ),
        ("serial=a=b", InstallDisk::Serial("a=b".to_owned())),
        (
            &format!("serial={}", "x".repeat(128)),
            InstallDisk::Serial("x".repeat(128)),
        ),
    ];
    for (text, disk) in read {
        assert_eq!(text.parse::<InstallDisk>(), Ok(disk), "{text}");
    }
    let refused = [
        ("", InstallDiskError::Form),
        ("smallest", InstallDiskError::Form),
        ("vendor=Samsung", InstallDiskError::Form),
        ("Serial=S4EV", InstallDiskError::Form),
        ("serial=", InstallDiskError::Glob),
        (
            &format!("serial={}", "x".repeat(129)),
            InstallDiskError::Glob,
        ),
        ("serial= S4EV", InstallDiskError::Glob),
        ("serial=S4EV ", InstallDiskError::Glob),
        ("model=a\"b", InstallDiskError::Glob),
        ("model=a\\b", InstallDiskError::Glob),
        ("model=a\tb", InstallDiskError::Glob),
        ("model=caf\u{e9}", InstallDiskError::Glob),
        ("path=nvme0n1", InstallDiskError::NotDev),
        ("path=/devices/x", InstallDiskError::NotDev),
    ];
    for (text, error) in refused {
        assert_eq!(text.parse::<InstallDisk>(), Err(error), "{text}");
    }
    let messages = [
        (
            InstallDiskError::Form,
            "--install-disk is not serial=<glob>, model=<glob> or path=<glob>",
        ),
        (
            InstallDiskError::Glob,
            "--install-disk's glob is not 1 to 128 printable ASCII characters without a quote or a backslash, and with no space at either end",
        ),
        (
            InstallDiskError::NotDev,
            "--install-disk path=<glob> does not start with /dev/",
        ),
    ];
    for (error, text) in messages {
        assert_eq!(error.to_string(), text);
    }
}

/// Catches a serial console that names no device, or a port name that would not be the
/// kernel's.
#[test]
fn serial_console_ports_and_refusals() {
    for port in ["ttyS0", "ttyS1", "ttyS10", "ttyS99", "ttyAMA0", "ttyAMA12"] {
        let console: SerialConsole = port.parse().expect(port);
        assert_eq!(console.as_str(), port);
    }
    for port in [
        "", "ttyS", "ttyS100", "ttyS01", "ttyS-1", "ttyAMA", "ttyUSB0", "tty1", "TTYS0",
    ] {
        assert_eq!(
            port.parse::<SerialConsole>(),
            Err(SerialConsoleError),
            "{port}"
        );
    }
    assert_eq!(
        SerialConsoleError.to_string(),
        "--serial-console is not ttyS<n> or ttyAMA<n>, with n from 0 to 99"
    );
}

/// Catches a boot server address no bare machine can dial.
#[test]
fn a_boot_server_is_one_host_on_a_port() {
    let at = |a, b, c, d, port| SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), port);
    let server = BootServer::new(at(192, 0, 2, 10, 8080)).expect("a host and a port");
    assert_eq!(server.address(), at(192, 0, 2, 10, 8080));
    for (ip, port) in [
        (Ipv4Addr::UNSPECIFIED, 80),
        (Ipv4Addr::BROADCAST, 80),
        (Ipv4Addr::new(224, 0, 0, 1), 80),
    ] {
        let error = BootServer::new(SocketAddrV4::new(ip, port)).expect_err("no one host");
        assert_eq!(error, BootServerError::NotOneHost(ip));
        assert_eq!(
            error.to_string(),
            format!("the boot server's address {ip} names no one host")
        );
    }
    let error = BootServer::new(at(192, 0, 2, 10, 0)).expect_err("port 0");
    assert_eq!(error, BootServerError::PortZero);
    assert_eq!(error.to_string(), "the boot server's HTTP port is 0");
}

/// Catches a platform whose names disagree between the menu, the deb and the boot set, or a
/// GRUB CPU of another platform read as one of ours.
#[test]
fn each_platform_names_its_own_files() {
    let expected = [
        (
            LinuxPlatform::X86_64,
            ["linux-x86_64", "amd64", "x86_64"],
            [
                "/vmlinuz-linux-x86_64",
                "/initrd-linux-x86_64",
                "/deb/hostkeeper_amd64.deb",
            ],
        ),
        (
            LinuxPlatform::Arm64,
            ["linux-arm64", "arm64", "arm64"],
            [
                "/vmlinuz-linux-arm64",
                "/initrd-linux-arm64",
                "/deb/hostkeeper_arm64.deb",
            ],
        ),
    ];
    assert_eq!(LinuxPlatform::ALL, expected.map(|(p, _, _)| p));
    for (platform, names, paths) in expected {
        assert_eq!(
            [platform.as_str(), platform.deb_arch(), platform.grub_cpu()],
            names
        );
        assert_eq!(platform.to_string(), names[0]);
        assert_eq!(LinuxPlatform::from_grub_cpu(names[2]), Some(platform));
        assert_eq!(
            [
                kernel_path(platform),
                initrd_path(platform),
                deb_path(platform)
            ],
            paths
        );
    }
    for cpu in ["", "i386", "arm", "riscv64", "x86-64", "aarch64", "ARM64"] {
        assert_eq!(LinuxPlatform::from_grub_cpu(cpu), None, "{cpu}");
    }
}

/// Catches an ISO name that would need quoting in a URL or a GRUB word, or that is not one file
/// name: the served names of `netboot.json` are read, every other is refused.
#[test]
fn an_iso_name_is_one_file_name_ending_in_iso() {
    for name in [
        "ubuntu-24.04.5-live-server-amd64.iso",
        "ubuntu-24.04.5-live-server-arm64.iso",
        "a.iso",
        "x_y+z.iso",
        &format!("{}.iso", "a".repeat(251)),
    ] {
        let iso: IsoName = name.parse().expect(name);
        assert_eq!(iso.as_str(), name);
        assert_eq!(iso_path(&iso), format!("/{name}"));
    }
    for name in [
        "",
        ".iso",
        ".hidden.iso",
        "ubuntu.img",
        "ubuntu.ISO",
        "boot/ubuntu.iso",
        "../ubuntu.iso",
        "ubuntu live.iso",
        "ubuntu;x.iso",
        "ubuntu$x.iso",
        "caf\u{e9}.iso",
        &format!("{}.iso", "a".repeat(252)),
    ] {
        assert_eq!(name.parse::<IsoName>(), Err(IsoNameError), "{name:?}");
    }
    assert_eq!(
        IsoNameError.to_string(),
        "the ISO's served name is not 1 to 255 of A-Za-z0-9._+-, not starting with a dot and ending in .iso"
    );
}

/// Catches repository files that could leave the directory or collide: each refusal is asserted by
/// its whole message, the accepted files are in name order, and their bytes are kept.
#[test]
fn repository_files_are_plain_names_in_order() {
    use hostkeeper_pxe_seed::inputs::{
        MAX_REPOSITORY_FILES, RepositoryFiles, RepositoryFilesError,
    };
    let file = |name: &str| (name.to_owned(), name.as_bytes().to_vec());
    let refusal = |files: Vec<(String, Vec<u8>)>| {
        RepositoryFiles::new(files)
            .expect_err("refused")
            .to_string()
    };
    assert_eq!(refusal(vec![]), "there is no repository file");
    for bad in [
        "",
        ".hidden",
        "a/b",
        "../x",
        "a b",
        "a\"b",
        &"a".repeat(256),
    ] {
        assert_eq!(
            refusal(vec![file(bad)]),
            format!(
                "the repository file name {bad:?} is not 1 to 255 of A-Za-z0-9._+-, not starting with a dot"
            )
        );
    }
    assert_eq!(
        refusal(vec![file("a.gpg"), file("b.sources"), file("a.gpg")]),
        "the repository file name \"a.gpg\" is given twice"
    );
    let many: Vec<_> = (0..=MAX_REPOSITORY_FILES)
        .map(|i| file(&format!("f{i}")))
        .collect();
    assert_eq!(
        RepositoryFiles::new(many),
        Err(RepositoryFilesError::TooMany(MAX_REPOSITORY_FILES + 1))
    );
    let longest = "a".repeat(255);
    let ok = RepositoryFiles::new(vec![file("z.sources"), file(&longest), file("a+b_c-d.gpg")])
        .expect("plain names");
    let names: Vec<&str> = ok.files().iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["a+b_c-d.gpg", longest.as_str(), "z.sources"]);
    assert_eq!(ok.files()[2].1, b"z.sources");
}
