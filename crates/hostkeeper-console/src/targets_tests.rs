//! The console targets file: an address for each device's MAC.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use hostkeeper_enrollment::names::MacAddress;

use super::{Address, FLAG, MAX_FILE_BYTES, Targets, TargetsError};
use crate::file::ReadError;
use crate::testutil::{put, scratch};

fn mac(text: &str) -> MacAddress {
    MacAddress::parse(text).expect("a MAC")
}

/// Proves: a target maps its MAC, however it is spelt (either case, `:`, `-` or none), to its
/// address: an IPv4 address, an IPv6 address, or a DNS name in lower case; a MAC the file does not
/// name has no address.
///
/// Catches: a lookup that misses a MAC written in another spelling than the record's, an
/// address read as another, and a MAC that is not in the file given some address.
#[test]
fn a_target_maps_a_mac_to_an_address() {
    let targets = Targets::parse(concat!(
        "[[target]]\nmac = \"0C:C4:7A:00:00:02\"\naddress = \"192.0.2.12\"\n",
        "[[target]]\nmac = \"0c-c4-7a-00-00-03\"\naddress = \"2001:db8::3\"\n",
        "[[target]]\nmac = \"a85e45000001\"\naddress = \"Mac-01.Lab.example\"\n",
    ))
    .expect("parsed");
    assert_eq!(
        targets.address_of(&mac("0c:c4:7a:00:00:02")),
        Some(&Address::Ip(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 12))))
    );
    assert_eq!(
        targets.address_of(&mac("0c:c4:7a:00:00:03")),
        Some(&Address::Ip(IpAddr::V6(Ipv6Addr::new(
            0x2001, 0xdb8, 0, 0, 0, 0, 0, 3
        ))))
    );
    let name = targets
        .address_of(&mac("a8:5e:45:00:00:01"))
        .expect("an address");
    assert_eq!(name, &Address::Name("mac-01.lab.example".to_owned()));
    assert_eq!(name.to_string(), "mac-01.lab.example");
    assert_eq!(
        targets
            .address_of(&mac("0c:c4:7a:00:00:02"))
            .map(ToString::to_string)
            .as_deref(),
        Some("192.0.2.12")
    );
    assert_eq!(targets.address_of(&mac("0c:c4:7a:00:00:09")), None);
    assert_eq!(
        Targets::parse("").expect("an empty file parses"),
        Targets::default()
    );
}

/// Proves: a file that breaks a rule is refused with an error naming the flag, the entry and the
/// rule, by exact text: a MAC that does not parse, an address with a port, a URL, a label that
/// starts or ends with `-`, an empty label, a label of 64 bytes, a name over 253 bytes, a name
/// whose last label is a number (`1234`, `0300.0.2.1`, `bmc.0x7f`), a MAC named twice in two
/// spellings, an address named twice (an IPv4 address and the same written as IPv6 among them),
/// an unknown key, and text that is not TOML. A label of 63 bytes, a name of 253 bytes and names
/// with digits that are not numbers (`1234.example`, `a.0xg`) are taken.
///
/// Catches: a power command dialled at an address the operator did not mean (a numeric name a
/// resolver reads as another IPv4 address, a port taken as
/// part of a name, two BMCs at one address), and a typo that drops a target without a word.
#[test]
fn a_targets_file_that_breaks_a_rule_is_refused_naming_the_flag() {
    let entry = |mac: &str, address: &str| {
        format!("[[target]]\nmac = \"{mac}\"\naddress = \"{address}\"\n")
    };
    let refused = |text: &str| Targets::parse(text).expect_err("refused").to_string();
    let label63 = "a".repeat(63);
    let name253 = format!("{label63}.{label63}.{label63}.{}", "b".repeat(61));
    assert_eq!(name253.len(), 253);
    assert!(Targets::parse(&entry("0c:c4:7a:00:00:02", &label63)).is_ok());
    assert!(Targets::parse(&entry("0c:c4:7a:00:00:02", &name253)).is_ok());
    for name in ["0x7g.example", "1234.example", "bmc-01", "a.0xg"] {
        assert!(
            Targets::parse(&entry("0c:c4:7a:00:00:02", name)).is_ok(),
            "{name}"
        );
    }

    assert_eq!(
        refused(&entry("0c:c4:7a:00:00", "192.0.2.1")),
        "--console-targets-file: target 1: \"0c:c4:7a:00:00\" is not a MAC address"
    );
    for address in [
        "192.0.2.1:623",
        "https://bmc.example",
        "-bmc.example",
        "bmc-.example",
        "bmc..example",
        "",
        &format!("{label63}a"),
        &format!("{name253}c"),
        "bmc_01",
        "1234",
        "192.0.513",
        "0300.0.2.1",
        "bmc.0x7f",
        "bmc.0X1F",
        "0x",
    ] {
        assert_eq!(
            refused(&format!(
                "{}{}",
                entry("0c:c4:7a:00:00:01", "192.0.2.1"),
                entry("0c:c4:7a:00:00:02", address)
            )),
            format!(
                "--console-targets-file: target 2: {address:?} is not an IP address or a DNS name"
            )
        );
    }
    assert_eq!(
        refused(&format!(
            "{}{}",
            entry("0c:c4:7a:00:00:02", "192.0.2.1"),
            entry("0CC47A000002", "192.0.2.2")
        )),
        "--console-targets-file: the MAC 0c:c4:7a:00:00:02 is named twice"
    );
    assert_eq!(
        refused(&format!(
            "{}{}",
            entry("0c:c4:7a:00:00:02", "BMC.example"),
            entry("0c:c4:7a:00:00:03", "bmc.example")
        )),
        "--console-targets-file: the address bmc.example is named twice"
    );
    assert_eq!(
        refused(&format!(
            "{}{}",
            entry("0c:c4:7a:00:00:02", "192.0.2.5"),
            entry("0c:c4:7a:00:00:03", "::ffff:192.0.2.5")
        )),
        "--console-targets-file: the address 192.0.2.5 is named twice"
    );
    for text in [
        "[[target]]\nmac = \"0c:c4:7a:00:00:02\"\naddress = \"192.0.2.1\"\nport = 623\n",
        "[[target]]\nmac = \"0c:c4:7a:00:00:02\"\n",
        "[[targets]]\nmac = \"0c:c4:7a:00:00:02\"\naddress = \"192.0.2.1\"\n",
        "not toml",
    ] {
        let text = refused(text);
        assert!(text.starts_with("--console-targets-file: "), "{text}");
    }
}

/// Proves: the file is read from disk with the rules of the text, and a missing path, a
/// directory, a file over the cap and one that is not UTF-8 are refused naming the flag.
///
/// Catches: a targets file read without its cap, and a directory read as a file with no target.
#[test]
fn the_targets_file_is_read_from_disk_and_a_bad_file_is_refused() {
    let dir = scratch("targets-read");
    let good = put(
        dir.path(),
        "good.toml",
        b"[[target]]\nmac = \"0c:c4:7a:00:00:02\"\naddress = \"192.0.2.12\"\n",
        0o644,
    );
    let targets = Targets::read(&good).expect("read");
    assert!(targets.address_of(&mac("0c:c4:7a:00:00:02")).is_some());
    assert_eq!(
        Targets::read(dir.path()),
        Err(TargetsError::Read(ReadError::NotAFile { flag: FLAG }))
    );
    assert!(matches!(
        Targets::read(&dir.path().join("missing.toml")),
        Err(TargetsError::Read(ReadError::Io { flag: FLAG, .. }))
    ));
    let big = put(
        dir.path(),
        "big.toml",
        &vec![b'#'; usize::try_from(MAX_FILE_BYTES).expect("small") + 1],
        0o644,
    );
    assert_eq!(
        Targets::read(&big),
        Err(TargetsError::Read(ReadError::TooLong {
            flag: FLAG,
            limit: MAX_FILE_BYTES
        }))
    );
    let binary = put(dir.path(), "binary.toml", &[0xc3], 0o644);
    assert_eq!(
        Targets::read(&binary),
        Err(TargetsError::Read(ReadError::NotText { flag: FLAG }))
    );
}
