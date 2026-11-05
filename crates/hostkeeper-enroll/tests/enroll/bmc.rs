//! The BMC MAC claim of a Linux host, read through a fake [`Ipmi`]: the channel that answers, the
//! answers that are no MAC, a BMC that cannot be asked, and the claim on the enrollment request.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use hostkeeper_enroll::{
    ClaimsSource, Enroller, Ipmi, LAN_CHANNELS, LAN_MAC_PARAMETER, LinuxClaims, Outcome,
    parse_lan_mac,
};

use crate::support::World;

/// A BMC that answers each channel as `answers` says (an error for a channel not named), and
/// records each (channel, parameter) it was asked.
struct FakeBmc {
    answers: BTreeMap<u8, Vec<u8>>,
    asked: Mutex<Vec<(u8, u8)>>,
}

impl FakeBmc {
    fn new(answers: &[(u8, Vec<u8>)]) -> Arc<FakeBmc> {
        Arc::new(FakeBmc {
            answers: answers.iter().cloned().collect(),
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<(u8, u8)> {
        self.asked.lock().expect("asked").clone()
    }
}

impl Ipmi for FakeBmc {
    fn lan_parameter(&self, channel: u8, parameter: u8) -> io::Result<Vec<u8>> {
        self.asked.lock().expect("asked").push((channel, parameter));
        self.answers
            .get(&channel)
            .cloned()
            .ok_or_else(|| io::Error::other("no such channel"))
    }
}

/// A response with completion code 0, revision 0x11 and `mac`.
fn answer(mac: [u8; 6]) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x11];
    bytes.extend_from_slice(&mac);
    bytes
}

const MAC: [u8; 6] = [0x0c, 0xc4, 0x7a, 0x00, 0x00, 0x01];

/// The claims of a host with no files to read, whose BMC is `bmc`.
fn claims_with(world: &World, bmc: &Arc<FakeBmc>) -> hostkeeper_enroll::Claims {
    let root = world.dir.path();
    let reader: Arc<dyn Ipmi> = bmc.clone();
    LinuxClaims::new(
        &root.join("machine-id"),
        &root.join("serial"),
        &root.join("uuid"),
        &root.join("net"),
    )
    .with_ipmi(reader)
    .claims()
    .expect("Linux claims are never an error")
}

/// Catches the BMC MAC read from another parameter than 5, a BMC asked past the channel that
/// answered, or a MAC spelt in another case than the one the coordinator compares.
#[test]
fn a_bmc_that_answers_on_channel_one_gives_the_claim_in_lower_case() {
    let world = World::new("bmc-one");
    let bmc = FakeBmc::new(&[(1, answer([0x0C, 0xC4, 0x7A, 0xAB, 0xCD, 0xEF]))]);
    assert_eq!(
        claims_with(&world, &bmc).bmc_mac.as_deref(),
        Some("0c:c4:7a:ab:cd:ef")
    );
    assert_eq!(bmc.asked(), vec![(1, LAN_MAC_PARAMETER)]);
    assert_eq!(LAN_MAC_PARAMETER, 5);
}

/// Catches a BMC whose LAN channel is not 1 claiming nothing, and a channel without a MAC (an
/// error, a refusal, an unconfigured all-zero MAC) ending the search.
#[test]
fn the_first_channel_that_answers_with_a_mac_gives_the_claim() {
    let world = World::new("bmc-later");
    let bmc = FakeBmc::new(&[
        (2, vec![0xcc]),
        (3, answer([0; 6])),
        (8, answer(MAC)),
        (9, answer([0x0c, 0xc4, 0x7a, 0x00, 0x00, 0x02])),
    ]);
    assert_eq!(
        claims_with(&world, &bmc).bmc_mac.as_deref(),
        Some("0c:c4:7a:00:00:01")
    );
    let channels: Vec<u8> = bmc.asked().iter().map(|(channel, _)| *channel).collect();
    assert_eq!(channels, (1..=8).collect::<Vec<u8>>());
}

/// Catches a host with no BMC, or a BMC that cannot be asked, failing the request instead of
/// leaving the claim absent, and every channel of IPMI not being tried before giving up.
#[test]
fn a_bmc_that_cannot_be_asked_leaves_the_claim_absent() {
    let world = World::new("bmc-none");
    let root = world.dir.path();
    std::fs::write(
        root.join("machine-id"),
        "0123456789abcdef0123456789abcdef\n",
    )
    .expect("write");
    let bmc = FakeBmc::new(&[]);
    let claims = claims_with(&world, &bmc);
    assert_eq!(claims.bmc_mac, None);
    assert_eq!(
        claims.machine_id.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    let channels: Vec<u8> = bmc.asked().iter().map(|(channel, _)| *channel).collect();
    assert_eq!(channels, LAN_CHANNELS.collect::<Vec<u8>>());
    assert_eq!(channels, (1..=11).collect::<Vec<u8>>());
}

/// Catches a refused command, a short or long response, or an unconfigured channel read as a
/// MAC.
#[test]
fn only_a_complete_answer_with_a_mac_is_a_mac() {
    assert_eq!(
        parse_lan_mac(&answer(MAC)).as_deref(),
        Some("0c:c4:7a:00:00:01")
    );
    let mut refused = answer(MAC);
    refused[0] = 0xc1;
    assert_eq!(parse_lan_mac(&refused), None);
    assert_eq!(parse_lan_mac(&answer(MAC)[..7]), None);
    let mut long = answer(MAC);
    long.push(0);
    assert_eq!(parse_lan_mac(&long), None);
    assert_eq!(parse_lan_mac(&[]), None);
    assert_eq!(parse_lan_mac(&answer([0; 6])), None);
    assert_eq!(
        parse_lan_mac(&answer([0xff; 6])).as_deref(),
        Some("ff:ff:ff:ff:ff:ff")
    );
}

/// Catches the BMC MAC read but dropped on the way to the coordinator.
#[test]
fn the_bmc_mac_reaches_the_enrollment_request() {
    let world = World::new("bmc-request");
    let root = world.dir.path();
    let bmc = FakeBmc::new(&[(1, answer(MAC))]);
    let reader: Arc<dyn Ipmi> = bmc;
    let mut ports = world.ports_with(None, None);
    ports.claims = Box::new(
        LinuxClaims::new(
            &root.join("machine-id"),
            &root.join("serial"),
            &root.join("uuid"),
            &root.join("net"),
        )
        .with_ipmi(reader),
    );
    let mut enroller = Enroller::open(world.settings(), ports).expect("open");
    assert_eq!(enroller.run_once().expect("posted"), Outcome::Pending);
    let enrolls = &world.coordinator().enrolls;
    assert_eq!(enrolls.len(), 1);
    assert_eq!(
        enrolls[0].claims.bmc_mac.as_deref(),
        Some("0c:c4:7a:00:00:01")
    );
}

/// Catches two sources that read different BMCs, or one with a BMC and one without, being taken
/// for the same source, and the reader missing from what a source prints.
#[test]
fn a_source_is_the_same_only_with_the_same_files_and_reader() {
    let first: Arc<dyn Ipmi> = FakeBmc::new(&[]);
    let second: Arc<dyn Ipmi> = FakeBmc::new(&[]);
    let host = LinuxClaims::host();
    let with_first = LinuxClaims::host().with_ipmi(Arc::clone(&first));
    assert_eq!(
        with_first,
        LinuxClaims::host().with_ipmi(Arc::clone(&first))
    );
    assert_ne!(with_first, LinuxClaims::host().with_ipmi(second));
    assert_ne!(with_first, host);
    assert_ne!(host, with_first);
    assert_eq!(host, host.clone());
    let elsewhere = LinuxClaims::new(
        std::path::Path::new("/a"),
        std::path::Path::new("/b"),
        std::path::Path::new("/c"),
        std::path::Path::new("/d"),
    );
    assert_ne!(elsewhere, host);
    assert!(format!("{host:?}").contains("ipmi: false"));
    assert!(format!("{with_first:?}").contains("ipmi: true"));
}
