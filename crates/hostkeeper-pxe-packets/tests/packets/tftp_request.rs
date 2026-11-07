//! TFTP read requests: only names of the set are served, and `hostkeeper/boot.efi` is the shim
//! of the reader's platform.

use std::net::Ipv4Addr;

use hostkeeper_pxe_packets::dhcp::Lease;
use hostkeeper_pxe_packets::tftp::{
    BOOT_EFI, ErrorCode, FileSet, MAX_BLKSIZE, MAX_REQUEST, NameError, Options, Read,
    RecordPlatforms, Refused, Target, TftpError, on_request, parse_request,
};
use hostkeeper_pxe_packets::{BootSets, ClientArch};
use hostkeeper_wire::Platform;

use crate::build::{ENROLLED, OFFERED, RECORDED, both_sets};

/// A request of `opcode` for `name` in `mode`, with `options` as name and value pairs.
fn request(opcode: u16, name: &[u8], mode: &str, options: &[(&str, &str)]) -> Vec<u8> {
    let mut out = opcode.to_be_bytes().to_vec();
    out.extend_from_slice(name);
    out.push(0);
    out.extend_from_slice(mode.as_bytes());
    out.push(0);
    for (option, value) in options {
        out.extend_from_slice(option.as_bytes());
        out.push(0);
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }
    out
}

fn rrq(name: &str) -> Vec<u8> {
    request(1, name.as_bytes(), "octet", &[])
}

/// The served set of the tests: both boot loaders of each platform, the fixed `grub.cfg`, and
/// the generated menus under `grub/menu`.
fn files() -> FileSet {
    let mut files = FileSet::new();
    for name in [
        "hostkeeper/shimx64.efi",
        "hostkeeper/grubx64.efi",
        "hostkeeper/shimaa64.efi",
        "hostkeeper/grubaa64.efi",
        "grub/grub.cfg",
    ] {
        files.add_file(name).expect("a clean name");
    }
    files.add_generated_dir("grub/menu").expect("a clean name");
    files
}

/// The target of a read request with no lease and no records.
fn read(bytes: &[u8]) -> Result<Read, Refused> {
    on_request(bytes, &files(), None, &RecordPlatforms::new(), &both_sets())
}

fn target(name: &str) -> Result<Target, Refused> {
    read(&rrq(name)).map(|read| read.target)
}

#[test]
fn names_of_the_set_are_served() {
    for name in [
        "hostkeeper/shimx64.efi",
        "hostkeeper/grubaa64.efi",
        "grub/grub.cfg",
    ] {
        assert_eq!(target(name), Ok(Target::File(name.to_owned())), "{name}");
    }
    assert_eq!(
        target("grub/menu/0c:c4:7a:1f:ab:02"),
        Ok(Target::Generated {
            dir: "grub/menu".to_owned(),
            name: "0c:c4:7a:1f:ab:02".to_owned()
        })
    );
    assert_eq!(
        target("grub/menu/not-a-mac"),
        Ok(Target::Generated {
            dir: "grub/menu".to_owned(),
            name: "not-a-mac".to_owned()
        }),
        "the generator, not the TFTP layer, gives a bad MAC its local-disk menu"
    );
}

/// One leading `/` names the root of the served tree: GRUB asks for `/grub/grub.cfg`.
#[test]
fn a_single_leading_slash_names_the_root_of_the_served_tree() {
    assert_eq!(
        target("/grub/grub.cfg"),
        Ok(Target::File("grub/grub.cfg".to_owned()))
    );
    assert_eq!(
        target("/grub/menu/0c:c4:7a:1f:ab:02"),
        Ok(Target::Generated {
            dir: "grub/menu".to_owned(),
            name: "0c:c4:7a:1f:ab:02".to_owned()
        })
    );
}

/// A write request (even for a served name), a path with `..`, an absolute path, an unknown
/// name and every other name that could leave the tree are refused, each with its TFTP error.
/// Catches: a server that writes, or that reads a host file through `..` or an absolute path.
#[test]
fn every_other_request_is_refused_with_its_error() {
    let cases: Vec<(&str, Vec<u8>, Refused, ErrorCode)> = vec![
        (
            "a write request",
            request(2, b"grub/grub.cfg", "octet", &[]),
            Refused::Write("grub/grub.cfg".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "a write request for boot.efi",
            request(2, BOOT_EFI.as_bytes(), "octet", &[]),
            Refused::Write(BOOT_EFI.to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "an absolute host path",
            rrq("/etc/passwd"),
            Refused::Unknown("/etc/passwd".to_owned()),
            ErrorCode::FileNotFound,
        ),
        (
            "a doubled leading slash",
            rrq("//grub/grub.cfg"),
            Refused::Unsafe("//grub/grub.cfg".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "a `..` out of the tree",
            rrq("../etc/passwd"),
            Refused::Unsafe("../etc/passwd".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "a `..` inside a served path",
            rrq("hostkeeper/../grub/grub.cfg"),
            Refused::Unsafe("hostkeeper/../grub/grub.cfg".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "a `..` after the leading slash",
            rrq("/../grub/grub.cfg"),
            Refused::Unsafe("/../grub/grub.cfg".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "a `..` as the generated name",
            rrq("grub/menu/.."),
            Refused::Unsafe("grub/menu/..".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "a `.` component",
            rrq("grub/./grub.cfg"),
            Refused::Unsafe("grub/./grub.cfg".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "an empty component",
            rrq("grub//grub.cfg"),
            Refused::Unsafe("grub//grub.cfg".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "a trailing slash",
            rrq("grub/menu/"),
            Refused::Unsafe("grub/menu/".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "a backslash",
            rrq("grub\\grub.cfg"),
            Refused::Unsafe("grub\\grub.cfg".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "a space",
            rrq("grub/grub .cfg"),
            Refused::Unsafe("grub/grub .cfg".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "an empty name",
            rrq(""),
            Refused::Unsafe(String::new()),
            ErrorCode::AccessViolation,
        ),
        (
            "the root alone",
            rrq("/"),
            Refused::Unsafe("/".to_owned()),
            ErrorCode::AccessViolation,
        ),
        (
            "a name that is not ASCII",
            request(1, "grub/gr\u{fc}b.cfg".as_bytes(), "octet", &[]),
            Refused::Malformed(TftpError::NotText),
            ErrorCode::IllegalOperation,
        ),
        (
            "an unknown name",
            rrq("hostkeeper/grubia32.efi"),
            Refused::Unknown("hostkeeper/grubia32.efi".to_owned()),
            ErrorCode::FileNotFound,
        ),
        (
            "a generated directory itself",
            rrq("grub/menu"),
            Refused::Unknown("grub/menu".to_owned()),
            ErrorCode::FileNotFound,
        ),
        (
            "a name below a generated name",
            rrq("grub/menu/a/b"),
            Refused::Unknown("grub/menu/a/b".to_owned()),
            ErrorCode::FileNotFound,
        ),
        (
            "netascii mode",
            request(1, b"grub/grub.cfg", "netascii", &[]),
            Refused::Mode("netascii".to_owned()),
            ErrorCode::NotDefined,
        ),
        (
            "mail mode",
            request(1, b"grub/grub.cfg", "mail", &[]),
            Refused::Mode("mail".to_owned()),
            ErrorCode::NotDefined,
        ),
    ];
    for (what, bytes, refused, code) in cases {
        let got = read(&bytes);
        assert_eq!(got, Err(refused.clone()), "{what}");
        assert_eq!(refused.code(), code, "{what}");
        let mut packet = vec![0, 5];
        packet.extend_from_slice(&code.value().to_be_bytes());
        packet.extend_from_slice(code.message().as_bytes());
        packet.push(0);
        assert_eq!(refused.packet(), packet, "{what}");
    }
}

/// The mode is matched without case: `OCTET` is octet.
#[test]
fn the_mode_is_matched_without_case() {
    let bytes = request(1, b"grub/grub.cfg", "OCTET", &[]);
    assert_eq!(
        read(&bytes).map(|r| r.target),
        Ok(Target::File("grub/grub.cfg".to_owned()))
    );
}

/// Each ERROR packet, byte for byte: opcode 5, the code, its message and a NUL.
#[test]
fn each_error_packet_is_exact() {
    let cases = [
        (ErrorCode::NotDefined, 0, "not served"),
        (ErrorCode::FileNotFound, 1, "file not found"),
        (ErrorCode::AccessViolation, 2, "access violation"),
        (ErrorCode::IllegalOperation, 4, "illegal TFTP operation"),
        (ErrorCode::UnknownTid, 5, "unknown transfer ID"),
    ];
    for (code, value, message) in cases {
        let mut want = vec![0, 5, 0, value];
        want.extend_from_slice(message.as_bytes());
        want.push(0);
        assert_eq!(code.packet(), want, "{code:?}");
    }
}

/// Requests that are not well formed, each refused by name with illegal operation.
#[test]
fn a_malformed_request_is_refused_by_name() {
    let oversized = request(1, &[b'a'; MAX_REQUEST], "octet", &[]);
    let mut at_bound = request(1, b"grub/grub.cfg", "octet", &[]);
    let fill = MAX_REQUEST - at_bound.len() - 2;
    at_bound.extend(std::iter::repeat_n(b'x', fill / 2));
    at_bound.push(0);
    at_bound.extend(std::iter::repeat_n(b'y', fill - fill / 2));
    at_bound.push(0);
    assert_eq!(at_bound.len(), MAX_REQUEST);
    assert!(
        read(&at_bound).is_ok(),
        "a request of {MAX_REQUEST} bytes is read"
    );
    let cases: Vec<(&str, Vec<u8>, TftpError)> = vec![
        ("an empty datagram", Vec::new(), TftpError::Truncated(0)),
        ("one byte", vec![0], TftpError::Truncated(1)),
        ("an opcode alone", vec![0, 1], TftpError::Unterminated),
        (
            "an oversized request",
            oversized.clone(),
            TftpError::Oversized(oversized.len()),
        ),
        (
            "an ACK on the server port",
            vec![0, 4, 0, 1],
            TftpError::NotRequest(4),
        ),
        (
            "a DATA on the server port",
            vec![0, 3, 0, 1, 7],
            TftpError::NotRequest(3),
        ),
        (
            "an OACK on the server port",
            vec![0, 6, 0],
            TftpError::NotRequest(6),
        ),
        (
            "a name with no NUL",
            b"\x00\x01grub/grub.cfg".to_vec(),
            TftpError::Unterminated,
        ),
        (
            "a mode with no NUL",
            b"\x00\x01grub/grub.cfg\x00octet".to_vec(),
            TftpError::Unterminated,
        ),
        (
            "no mode",
            b"\x00\x01grub/grub.cfg\x00".to_vec(),
            TftpError::Truncated(16),
        ),
        (
            "an option with no value",
            b"\x00\x01grub/grub.cfg\x00octet\x00blksize\x00".to_vec(),
            TftpError::NoValue("blksize".to_owned()),
        ),
        (
            "an option named twice",
            request(
                1,
                b"grub/grub.cfg",
                "octet",
                &[("blksize", "512"), ("BLKSIZE", "1024")],
            ),
            TftpError::DuplicateOption("blksize".to_owned()),
        ),
        (
            "a control character in the mode",
            request(1, b"grub/grub.cfg", "oc\ttet", &[]),
            TftpError::NotText,
        ),
    ];
    for (what, bytes, error) in cases {
        assert_eq!(parse_request(&bytes), Err(error.clone()), "{what}");
        assert_eq!(read(&bytes), Err(Refused::Malformed(error)), "{what}");
    }
}

/// The options taken: `blksize` within RFC 2348's range, at most [`MAX_BLKSIZE`], and `tsize`
/// with a decimal value; every other option, and either with a value that is not decimal or
/// out of range, is left out.
#[test]
fn blksize_and_tsize_are_taken_and_every_other_option_left_out() {
    let options = |pairs: &[(&str, &str)]| {
        let bytes = request(1, b"grub/grub.cfg", "octet", pairs);
        parse_request(&bytes).expect("a request").options
    };
    let none = Options::default();
    let cases: Vec<(&[(&str, &str)], Options)> = vec![
        (&[], none),
        (
            &[("blksize", "1024")],
            Options {
                blksize: Some(1024),
                tsize: false,
            },
        ),
        (
            &[("BlkSize", "8")],
            Options {
                blksize: Some(8),
                tsize: false,
            },
        ),
        (
            &[("blksize", "1468")],
            Options {
                blksize: Some(MAX_BLKSIZE),
                tsize: false,
            },
        ),
        (
            &[("blksize", "1469")],
            Options {
                blksize: Some(MAX_BLKSIZE),
                tsize: false,
            },
        ),
        (
            &[("blksize", "65464")],
            Options {
                blksize: Some(MAX_BLKSIZE),
                tsize: false,
            },
        ),
        (&[("blksize", "65465")], none),
        (&[("blksize", "7")], none),
        (&[("blksize", "")], none),
        (&[("blksize", "+512")], none),
        (&[("blksize", "1234567890")], none),
        (
            &[("tsize", "0")],
            Options {
                blksize: None,
                tsize: true,
            },
        ),
        (&[("tsize", "zero")], none),
        (&[("timeout", "5"), ("windowsize", "4")], none),
        (
            &[("tsize", "0"), ("blksize", "1432"), ("multicast", "")],
            Options {
                blksize: Some(1432),
                tsize: true,
            },
        ),
    ];
    for (pairs, want) in cases {
        assert_eq!(options(pairs), want, "{pairs:?}");
    }
    let read = read(&request(1, b"grub/grub.cfg", "octet", &[("tsize", "0")]));
    assert_eq!(
        read.map(|r| r.options),
        Ok(Options {
            blksize: None,
            tsize: true
        })
    );
}

/// The leases and record platforms `hostkeeper/boot.efi` is resolved with.
fn boot_efi(
    lease: Option<Lease>,
    records: &RecordPlatforms,
    boot: &BootSets,
) -> Result<Target, Refused> {
    on_request(&rrq(BOOT_EFI), &files(), lease.as_ref(), records, boot).map(|read| read.target)
}

fn lease(arch: Option<u16>) -> Lease {
    Lease {
        address: OFFERED,
        mac: RECORDED,
        arch: arch.map(ClientArch::new),
    }
}

fn shim(platform: Platform) -> Target {
    let shim = match platform {
        Platform::LinuxArm64 => "hostkeeper/shimaa64.efi",
        _ => "hostkeeper/shimx64.efi",
    };
    Target::BootEfi {
        mac: RECORDED,
        platform,
        shim,
    }
}

/// `hostkeeper/boot.efi` is the shim of the architecture learned for the reader's address:
/// 7, 9 and 16 the x86-64 shim, 11 and 19 the arm64 one, whatever its record says.
/// Catches: an arm64 machine sent the x86-64 shim in router mode.
#[test]
fn boot_efi_is_the_shim_of_the_learned_architecture() {
    let arm_record = RecordPlatforms::from([(RECORDED, Platform::LinuxArm64)]);
    for (arch, platform) in [
        (7, Platform::LinuxX86_64),
        (9, Platform::LinuxX86_64),
        (16, Platform::LinuxX86_64),
        (11, Platform::LinuxArm64),
        (19, Platform::LinuxArm64),
    ] {
        assert_eq!(
            boot_efi(
                Some(lease(Some(arch))),
                &RecordPlatforms::new(),
                &both_sets()
            ),
            Ok(shim(platform)),
            "arch {arch}"
        );
        assert_eq!(
            boot_efi(Some(lease(Some(arch))), &arm_record, &both_sets()),
            Ok(shim(platform)),
            "arch {arch}, an arm64 record"
        );
    }
    let slash = format!("/{BOOT_EFI}");
    let got = on_request(
        &rrq(&slash),
        &files(),
        Some(&lease(Some(11))),
        &RecordPlatforms::new(),
        &both_sets(),
    );
    assert_eq!(got.map(|r| r.target), Ok(shim(Platform::LinuxArm64)));
}

/// With no architecture learned, `hostkeeper/boot.efi` is the shim of the record platform of
/// the client's MAC; every other case is "file not found".
#[test]
fn boot_efi_falls_back_on_the_record_and_is_refused_otherwise() {
    let records = RecordPlatforms::from([
        (RECORDED, Platform::LinuxArm64),
        (ENROLLED, Platform::LinuxX86_64),
    ]);
    assert_eq!(
        boot_efi(Some(lease(None)), &records, &both_sets()),
        Ok(shim(Platform::LinuxArm64))
    );
    let x86_only = BootSets::new([Platform::LinuxX86_64]).expect("a boot set");
    let cases = [
        (
            "no lease",
            None,
            records.clone(),
            both_sets(),
            Refused::NoLease,
        ),
        (
            "no architecture and no record",
            Some(lease(None)),
            RecordPlatforms::from([(ENROLLED, Platform::LinuxArm64)]),
            both_sets(),
            Refused::NoPlatform { mac: RECORDED },
        ),
        (
            "architecture 0",
            Some(lease(Some(0))),
            records.clone(),
            both_sets(),
            Refused::UnsupportedArch {
                mac: RECORDED,
                arch: ClientArch::new(0),
            },
        ),
        (
            "architecture 10",
            Some(lease(Some(10))),
            records.clone(),
            both_sets(),
            Refused::UnsupportedArch {
                mac: RECORDED,
                arch: ClientArch::new(10),
            },
        ),
        (
            "an architecture with no boot set",
            Some(lease(Some(11))),
            records.clone(),
            x86_only.clone(),
            Refused::NoBootSet {
                mac: RECORDED,
                platform: Platform::LinuxArm64,
            },
        ),
        (
            "a record platform with no boot set",
            Some(lease(None)),
            records,
            x86_only,
            Refused::NoBootSet {
                mac: RECORDED,
                platform: Platform::LinuxArm64,
            },
        ),
    ];
    for (what, lease, records, boot, refused) in cases {
        assert_eq!(
            boot_efi(lease, &records, &boot),
            Err(refused.clone()),
            "{what}"
        );
        assert_eq!(refused.code(), ErrorCode::FileNotFound, "{what}");
    }
}

/// A lease is looked up by address by the caller: another address's lease is not this one's.
#[test]
fn boot_efi_reads_only_the_lease_it_is_given() {
    let other = Lease {
        address: Ipv4Addr::new(192, 0, 2, 78),
        mac: ENROLLED,
        arch: Some(ClientArch::ARM64_UEFI),
    };
    assert_eq!(
        boot_efi(Some(other), &RecordPlatforms::new(), &both_sets()),
        Ok(Target::BootEfi {
            mac: ENROLLED,
            platform: Platform::LinuxArm64,
            shim: "hostkeeper/shimaa64.efi"
        })
    );
}

/// A set is given only names a request could name.
#[test]
fn a_set_refuses_a_name_no_request_could_read() {
    for bad in [
        "",
        "/grub/grub.cfg",
        "grub/../x",
        "./x",
        "grub//x",
        "a b",
        "a\\b",
        "grub/",
    ] {
        let mut files = FileSet::new();
        assert_eq!(
            files.add_file(bad),
            Err(NameError(bad.to_owned())),
            "{bad:?}"
        );
        assert_eq!(
            files.add_generated_dir(bad),
            Err(NameError(bad.to_owned())),
            "{bad:?}"
        );
        assert_eq!(files, FileSet::new(), "{bad:?}");
    }
    assert_eq!(
        NameError("a b".to_owned()).to_string(),
        "\"a b\" is not a served name: one or more components of printable ASCII other than \
         space and backslash, separated by single `/`, none `.` or `..`, with no leading `/`"
    );
}

/// The empty set serves nothing but `hostkeeper/boot.efi`, which is not a file of it.
#[test]
fn the_empty_set_serves_nothing() {
    let got = on_request(
        &rrq("grub/grub.cfg"),
        &FileSet::new(),
        None,
        &RecordPlatforms::new(),
        &both_sets(),
    );
    assert_eq!(got, Err(Refused::Unknown("grub/grub.cfg".to_owned())));
}
