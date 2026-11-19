//! The BMC's LAN settings the daemon's heartbeat reports ([`bmc_lan`]), read through a fake
//! [`Ipmi`] that answers by channel and parameter: the address of the channel that gave the MAC,
//! the answers that are no address, and a BMC with no MAC or no address.

use std::collections::BTreeMap;
use std::io;
use std::net::Ipv4Addr;
use std::sync::Mutex;

use hostkeeper_enroll::{BmcLan, Ipmi, LAN_IP_PARAMETER, LAN_MAC_PARAMETER, bmc_lan, parse_lan_ip};

/// A BMC that answers each (channel, parameter) as `answers` says, an error for any other, and
/// records each question.
struct FakeBmc {
    answers: BTreeMap<(u8, u8), Vec<u8>>,
    asked: Mutex<Vec<(u8, u8)>>,
}

impl FakeBmc {
    fn new(answers: &[((u8, u8), Vec<u8>)]) -> FakeBmc {
        FakeBmc {
            answers: answers.iter().cloned().collect(),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<(u8, u8)> {
        self.asked.lock().expect("asked").clone()
    }
}

impl Ipmi for FakeBmc {
    fn lan_parameter(&self, channel: u8, parameter: u8) -> io::Result<Vec<u8>> {
        self.asked.lock().expect("asked").push((channel, parameter));
        self.answers
            .get(&(channel, parameter))
            .cloned()
            .ok_or_else(|| io::Error::other("no such channel"))
    }
}

/// A response with completion code 0, revision 0x11 and `data`.
fn answer(data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x11];
    bytes.extend_from_slice(data);
    bytes
}

const MAC: [u8; 6] = [0x0c, 0xc4, 0x7a, 0x00, 0x00, 0x01];
const IP: [u8; 4] = [192, 0, 2, 12];

/// Catches the address read from another parameter than 3, or from another channel than the one
/// that gave the MAC, the MAC of one channel paired with the address of another, and a BMC asked
/// past the channel that answered.
#[test]
fn the_address_is_the_one_of_the_channel_that_gave_the_mac() {
    let bmc = FakeBmc::new(&[
        ((1, LAN_IP_PARAMETER), answer(&[192, 0, 2, 99])),
        ((2, LAN_MAC_PARAMETER), answer(&[0; 6])),
        ((3, LAN_MAC_PARAMETER), answer(&MAC)),
        ((3, LAN_IP_PARAMETER), answer(&IP)),
        ((4, LAN_MAC_PARAMETER), answer(&[0x0c, 0xc4, 0x7a, 0, 0, 2])),
        ((4, LAN_IP_PARAMETER), answer(&[192, 0, 2, 13])),
    ]);
    assert_eq!(
        bmc_lan(&bmc),
        Some(BmcLan {
            ip: Ipv4Addr::new(192, 0, 2, 12),
            mac: "0c:c4:7a:00:00:01".to_owned(),
        })
    );
    assert_eq!(
        bmc.asked(),
        vec![
            (1, LAN_MAC_PARAMETER),
            (2, LAN_MAC_PARAMETER),
            (3, LAN_MAC_PARAMETER),
            (3, LAN_IP_PARAMETER),
        ]
    );
    assert_eq!(LAN_IP_PARAMETER, 3);
}

/// Catches a BMC with no MAC, or whose channel with the MAC gives no address or one no BMC can be
/// reached at, reported with an address anyway (or with another channel's), and a BMC that cannot
/// be asked asked for an address.
#[test]
fn no_mac_or_no_usable_address_is_no_report() {
    let none = FakeBmc::new(&[]);
    assert_eq!(bmc_lan(&none), None);
    assert!(none.asked().iter().all(|(_, p)| *p == LAN_MAC_PARAMETER));
    assert_eq!(none.asked().len(), 11);

    let unasked = FakeBmc::new(&[
        ((1, LAN_MAC_PARAMETER), answer(&MAC)),
        ((2, LAN_IP_PARAMETER), answer(&IP)),
    ]);
    assert_eq!(bmc_lan(&unasked), None);
    assert_eq!(
        unasked.asked(),
        vec![(1, LAN_MAC_PARAMETER), (1, LAN_IP_PARAMETER)]
    );

    let unset = FakeBmc::new(&[
        ((1, LAN_MAC_PARAMETER), answer(&MAC)),
        ((1, LAN_IP_PARAMETER), answer(&[0, 0, 0, 0])),
    ]);
    assert_eq!(bmc_lan(&unset), None);

    let refused = FakeBmc::new(&[
        ((1, LAN_MAC_PARAMETER), answer(&MAC)),
        ((1, LAN_IP_PARAMETER), vec![0xc1]),
    ]);
    assert_eq!(bmc_lan(&refused), None);
}

/// Catches a refused command, a short or long response, or an address no BMC can be reached at
/// read as an address, and the octets read in another order.
#[test]
fn only_a_complete_answer_with_a_usable_address_is_an_address() {
    assert_eq!(
        parse_lan_ip(&answer(&IP)),
        Some(Ipv4Addr::new(192, 0, 2, 12))
    );
    let mut refused = answer(&IP);
    refused[0] = 0xc1;
    assert_eq!(parse_lan_ip(&refused), None);
    assert_eq!(parse_lan_ip(&answer(&IP)[..5]), None);
    let mut long = answer(&IP);
    long.push(0);
    assert_eq!(parse_lan_ip(&long), None);
    assert_eq!(parse_lan_ip(&[]), None);
    for unusable in [
        [0, 0, 0, 0],
        [127, 0, 0, 1],
        [169, 254, 0, 12],
        [224, 0, 0, 1],
        [255, 255, 255, 255],
    ] {
        assert_eq!(parse_lan_ip(&answer(&unusable)), None, "{unusable:?}");
    }
    assert_eq!(
        parse_lan_ip(&answer(&[10, 255, 255, 254])),
        Some(Ipv4Addr::new(10, 255, 255, 254))
    );
}
