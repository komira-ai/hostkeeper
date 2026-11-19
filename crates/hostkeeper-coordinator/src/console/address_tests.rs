//! The neighbour table's reader, the order in which a call tries a BMC's addresses and the error
//! it ends with, and the answer's account of an address.

use std::net::{IpAddr, Ipv4Addr};

use hostkeeper_console::bmc::BmcError;
use hostkeeper_console::targets::Address;
use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::{Found, Source, arp_entry, arp_table, through_addresses};
use crate::testutil::scratch;

const HEADING: &str =
    "IP address       HW type     Flags       HW address            Mask     Device\n";

fn mac(text: &str) -> MacAddress {
    MacAddress::parse(text).expect("a MAC")
}

fn ip(last: u8) -> Option<IpAddr> {
    Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, last)))
}

/// Proves: the address of the first complete entry for the MAC is found, the MAC compared in any
/// case; an incomplete entry (flags without `0x2`, as a neighbour still being resolved), another
/// MAC's entry, the heading and a line that does not read (too few fields, flags or an address
/// that are no number) are passed over.
///
/// Catches: an incomplete entry taken (its MAC is all zeros or a stale guess), the heading read
/// as an entry, a broken line ending the search, and an entry of another BMC answered.
#[test]
fn the_first_complete_entry_of_the_mac_is_its_address() {
    let bmc = mac("0c:c4:7a:00:00:01");
    let table = format!(
        "{HEADING}\
         192.0.2.5        0x1         0x2         0c:c4:7a:00:00:09     *        eth0\n\
         192.0.2.6        0x1         0x0         0c:c4:7a:00:00:01     *        eth0\n\
         192.0.2.7        0x1\n\
         192.0.2.8        0x1         0xzz        0c:c4:7a:00:00:01     *        eth0\n\
         not-an-ip        0x1         0x2         0c:c4:7a:00:00:01     *        eth0\n\
         192.0.2.12       0x1         0x6         0C:C4:7A:00:00:01     *        eth0\n\
         192.0.2.13       0x1         0x2         0c:c4:7a:00:00:01     *        eth1\n"
    );
    assert_eq!(arp_entry(&table, &bmc), ip(12));
    assert_eq!(arp_entry(&table, &mac("0c:c4:7a:00:00:09")), ip(5));
    assert_eq!(arp_entry(&table, &mac("0c:c4:7a:00:00:02")), None);
    assert_eq!(arp_entry(HEADING, &bmc), None);
    assert_eq!(arp_entry("", &bmc), None);
    let heading_like =
        "192.0.2.12       0x1         0x2         0c:c4:7a:00:00:01     *        eth0\n";
    assert_eq!(
        arp_entry(heading_like, &bmc),
        None,
        "the first line is the heading"
    );
}

/// Proves: the table is read from its file at each lookup, so an entry added after the console
/// started is found; a file that cannot be read holds no entry.
///
/// Catches: a table read once at start (a BMC the coordinator spoke to later never found), and a
/// missing file taken as an error that stops the call.
#[test]
fn the_table_is_read_from_its_file_at_each_lookup() {
    let dir = scratch("console-arp-file");
    let path = dir.path().join("arp");
    let table = arp_table(&path);
    let bmc = mac("0c:c4:7a:00:00:01");
    assert_eq!(table(&bmc), None, "no file");
    std::fs::write(&path, HEADING).expect("written");
    assert_eq!(table(&bmc), None);
    std::fs::write(
        &path,
        format!("{HEADING}192.0.2.12 0x1 0x2 0c:c4:7a:00:00:01 * eth0\n"),
    )
    .expect("written");
    assert_eq!(table(&bmc), ip(12));
}

fn found(last: u8, source: Source) -> Found {
    Found::of(Address::Ip(ip(last).expect("an address")), source)
}

/// What a run of [`through_addresses`] did: each check and the call, by the address they were made
/// at, in order, and what it came to (the call's answer and the source of its address).
type Ran = (Vec<String>, Result<(u8, Source), BmcError>);

/// Runs a call over `addresses` whose check at each address is the next of `checks` and whose call
/// answers `called`.
fn run(
    addresses: &[Found],
    checks: Vec<Result<(), BmcError>>,
    called: Result<u8, BmcError>,
) -> Ran {
    let checks = std::cell::RefCell::new(checks.into_iter());
    let made = std::cell::RefCell::new(Vec::new());
    let answer = through_addresses(
        "node-02",
        addresses,
        |address| {
            made.borrow_mut().push(format!("check {address}"));
            checks
                .borrow_mut()
                .next()
                .unwrap_or(Err(BmcError::Unreachable))
        },
        |address| {
            made.borrow_mut().push(format!("call {address}"));
            called
        },
    );
    (
        made.into_inner(),
        answer.map(|(value, found)| (value, found.source)),
    )
}

fn made(steps: &[&str]) -> Vec<String> {
    steps.iter().map(|step| (*step).to_owned()).collect()
}

/// Proves: with one address the call is made there with no check of its own (the call checks
/// the MAC); with more, the MAC is checked at each in turn until one passes, and the call is made
/// once, there, and answered with that address.
///
/// Catches: a fallback never tried, tried before the first address, or tried after a pass; and
/// an extra session at every call of a host with one address.
#[test]
fn the_call_is_made_once_at_the_first_address_whose_check_passes() {
    let one = [found(12, Source::Heartbeat)];
    assert_eq!(
        run(&one, Vec::new(), Ok(1)),
        (made(&["call 192.0.2.12"]), Ok((1, Source::Heartbeat)))
    );
    let addresses = [
        found(12, Source::Heartbeat),
        found(77, Source::NeighborTable),
        found(99, Source::NeighborTable),
    ];
    assert_eq!(
        run(&addresses, vec![Ok(())], Ok(1)),
        (
            made(&["check 192.0.2.12", "call 192.0.2.12"]),
            Ok((1, Source::Heartbeat))
        )
    );
    assert_eq!(
        run(
            &addresses,
            vec![
                Err(BmcError::Unreachable),
                Err(BmcError::AddressMismatch),
                Ok(())
            ],
            Ok(2)
        ),
        (
            made(&[
                "check 192.0.2.12",
                "check 192.0.2.77",
                "check 192.0.2.99",
                "call 192.0.2.99"
            ]),
            Ok((2, Source::NeighborTable))
        )
    );
}

/// Proves: a call that fails after its address's check passed is not made again anywhere, its
/// error the answer, even when the error is that the BMC stopped answering.
///
/// Catches: a power command whose answer was lost sent again at another address of the same
/// BMC, so that one operator's `cycle` power-cycles the host twice.
#[test]
fn a_call_that_fails_is_not_made_again_elsewhere() {
    let addresses = [
        found(12, Source::Heartbeat),
        found(77, Source::NeighborTable),
    ];
    assert_eq!(
        run(&addresses, vec![Ok(())], Err(BmcError::Unreachable)),
        (
            made(&["check 192.0.2.12", "call 192.0.2.12"]),
            Err(BmcError::Unreachable)
        )
    );
}

/// Proves: when no address passes its check, the call is never made, and the answer is
/// `address_mismatch` only if each BMC answered with another MAC, else `device_unreachable`; any
/// other failure of a check (a refused credential, a busy BMC) ends the tries at once.
///
/// Catches: the last address's error answered for all of them (a BMC that answered with another
/// MAC hidden behind one that did not answer, or the reverse), and a refused credential tried at
/// every address.
#[test]
fn no_address_that_passes_its_check_says_how() {
    let addresses = [
        found(12, Source::Heartbeat),
        found(77, Source::NeighborTable),
    ];
    let mismatch = BmcError::AddressMismatch;
    let unreachable = BmcError::Unreachable;
    let both = made(&["check 192.0.2.12", "check 192.0.2.77"]);
    assert_eq!(
        run(&addresses, vec![Err(mismatch), Err(mismatch)], Ok(1)),
        (both.clone(), Err(mismatch))
    );
    assert_eq!(
        run(&addresses, vec![Err(unreachable), Err(mismatch)], Ok(1)).1,
        Err(unreachable)
    );
    assert_eq!(
        run(&addresses, vec![Err(mismatch), Err(unreachable)], Ok(1)),
        (both, Err(unreachable))
    );
    for ended in [BmcError::AuthFailed, BmcError::Busy] {
        assert_eq!(
            run(&addresses, vec![Err(ended), Ok(())], Ok(1)),
            (made(&["check 192.0.2.12"]), Err(ended))
        );
    }
}

/// Proves: the answer names the address as text and its source, whether a heartbeat's is stale,
/// and when it was reported.
///
/// Catches: a source answered as another, and the staleness or the time of a report lost.
#[test]
fn the_answer_names_the_address_and_where_it_was_found() {
    let named = Found::of(
        Address::Name("bmc-03.lab.example".to_owned()),
        Source::TargetsFile,
    );
    assert_eq!(
        named.to_proto(),
        proto::ConsoleAddress {
            address: "bmc-03.lab.example".to_owned(),
            source: proto::ConsoleAddressSource::TargetsFile.into(),
            stale: false,
            reported_unix: 0,
        }
    );
    let stale = Found {
        stale: true,
        reported_unix: 1_800_000_000,
        ..found(12, Source::Heartbeat)
    };
    assert_eq!(
        stale.to_proto(),
        proto::ConsoleAddress {
            address: "192.0.2.12".to_owned(),
            source: proto::ConsoleAddressSource::Heartbeat.into(),
            stale: true,
            reported_unix: 1_800_000_000,
        }
    );
    assert_eq!(
        found(77, Source::NeighborTable).to_proto().source(),
        proto::ConsoleAddressSource::NeighborTable
    );
}
