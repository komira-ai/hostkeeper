//! Router mode: what is learned from the clients' broadcasts, and that nothing is ever sent.

use std::net::Ipv4Addr;

use hostkeeper_pxe_packets::ClientArch;
use hostkeeper_pxe_packets::dhcp::{Ignored, Lease, MAX_LEARNED, MessageType, Mode, Outcome, Port};

use crate::build::{
    ClientPacket, ENROLLED, HTTP_CLASS, INFORM, OFFERED, PXE_CLASS, RECORDED, REQUEST, answers,
    both_sets, service,
};

/// What router mode makes of `packets`, in order, on the DHCP port.
fn learn(packets: &[ClientPacket]) -> (Vec<Outcome>, hostkeeper_pxe_packets::dhcp::DhcpService) {
    let mut dhcp = service(Mode::Router);
    let outcomes = packets
        .iter()
        .map(|p| dhcp.on_packet(Port::Dhcp, &p.bytes(), &answers(None), &both_sets()))
        .collect();
    (outcomes, dhcp)
}

#[test]
fn a_selecting_request_yields_its_address_mac_and_architecture() {
    let request = ClientPacket::selecting(RECORDED).with(93, &[0, 11]);
    let (outcomes, dhcp) = learn(&[request]);
    let lease = Lease {
        address: OFFERED,
        mac: RECORDED,
        arch: Some(ClientArch::ARM64_UEFI),
    };
    assert_eq!(outcomes, [Outcome::Lease(lease)]);
    assert_eq!(dhcp.lease(OFFERED), Some(lease));
    assert_eq!(dhcp.lease(Ipv4Addr::new(192, 0, 2, 78)), None);
}

/// A REQUEST with no option 93 takes the architecture of the last DISCOVER with its `chaddr`
/// that stated one; another MAC's DISCOVER does not count.
#[test]
fn a_request_with_no_option_93_takes_the_architecture_of_its_discover() {
    let (outcomes, dhcp) = learn(&[
        ClientPacket::discover(RECORDED, PXE_CLASS, 9),
        ClientPacket::discover(RECORDED, PXE_CLASS, 11),
        ClientPacket::discover(RECORDED, PXE_CLASS, 7).without(93),
        ClientPacket::discover(ENROLLED, PXE_CLASS, 9),
        ClientPacket::selecting(RECORDED),
    ]);
    assert_eq!(
        outcomes[..4],
        [
            Outcome::Discover {
                mac: RECORDED,
                arch: Some(ClientArch::X86_64_UEFI)
            },
            Outcome::Discover {
                mac: RECORDED,
                arch: Some(ClientArch::ARM64_UEFI)
            },
            Outcome::Discover {
                mac: RECORDED,
                arch: None
            },
            Outcome::Discover {
                mac: ENROLLED,
                arch: Some(ClientArch::X86_64_UEFI)
            },
        ]
    );
    let want = Lease {
        address: OFFERED,
        mac: RECORDED,
        arch: Some(ClientArch::ARM64_UEFI),
    };
    assert_eq!(outcomes[4], Outcome::Lease(want));
    assert_eq!(dhcp.lease(OFFERED), Some(want));
}

/// A REQUEST's own option 93 wins over its DISCOVER's.
#[test]
fn a_request_option_93_wins_over_the_discover() {
    let (outcomes, _) = learn(&[
        ClientPacket::discover(RECORDED, HTTP_CLASS, 16),
        ClientPacket::selecting(RECORDED).with(93, &[0, 19]),
    ]);
    assert_eq!(
        outcomes[1],
        Outcome::Lease(Lease {
            address: OFFERED,
            mac: RECORDED,
            arch: Some(ClientArch::ARM64_UEFI_HTTP),
        })
    );
}

#[test]
fn a_request_with_no_option_93_and_no_discover_yields_no_architecture() {
    let (outcomes, dhcp) = learn(&[ClientPacket::selecting(RECORDED)]);
    let want = Lease {
        address: OFFERED,
        mac: RECORDED,
        arch: None,
    };
    assert_eq!(outcomes, [Outcome::Lease(want)]);
    assert_eq!(dhcp.lease(OFFERED), Some(want));
}

/// A REQUEST with no option 50 (a renewal from its own address) teaches nothing.
#[test]
fn a_request_with_no_option_50_yields_nothing() {
    let mut renewal = ClientPacket::new(REQUEST, RECORDED).with(93, &[0, 7]);
    renewal.ciaddr = OFFERED;
    let (outcomes, dhcp) = learn(&[renewal]);
    assert_eq!(
        outcomes,
        [Outcome::Ignored(Ignored::NoAddress { mac: RECORDED })]
    );
    assert_eq!(dhcp.lease(OFFERED), None);
}

/// An address the router gives again to another client is that client's.
#[test]
fn the_last_request_for_an_address_holds_it() {
    let (_, dhcp) = learn(&[
        ClientPacket::selecting(RECORDED).with(93, &[0, 7]),
        ClientPacket::selecting(ENROLLED),
    ]);
    assert_eq!(
        dhcp.lease(OFFERED),
        Some(Lease {
            address: OFFERED,
            mac: ENROLLED,
            arch: None
        })
    );
}

/// A run keeps at most [`MAX_LEARNED`] addresses and MACs: past that the one learned longest
/// ago goes, the newest stays, and one learned again counts as new.
/// Catches: memory that grows without bound under a flood of made-up addresses, and a flood
/// that stops a real client being learned.
#[test]
fn learning_is_bounded_and_drops_the_oldest_first() {
    let address = |n: usize| {
        let n = u32::try_from(n).expect("a small index");
        Ipv4Addr::from(0xc000_0200 + n)
    };
    let mac = |n: usize| {
        let [_, _, _, _, a, b, c, d] = (n as u64).to_be_bytes();
        hostkeeper_pxe_packets::Mac::new([0x02, 0, a, b, c, d])
    };
    let mut dhcp = service(Mode::Router);
    let send = |dhcp: &mut hostkeeper_pxe_packets::dhcp::DhcpService, p: ClientPacket| {
        dhcp.on_packet(Port::Dhcp, &p.bytes(), &answers(None), &both_sets())
    };
    for n in 0..MAX_LEARNED {
        let discover = ClientPacket::discover(mac(n), PXE_CLASS, 11);
        send(&mut dhcp, discover);
        let mut request = ClientPacket::selecting(mac(n)).without(50);
        request = request.with(50, &address(n).octets());
        send(&mut dhcp, request);
    }
    // Learned again, the first is now the newest; one more pushes out the second.
    send(&mut dhcp, ClientPacket::discover(mac(0), PXE_CLASS, 11));
    send(
        &mut dhcp,
        ClientPacket::selecting(mac(0))
            .without(50)
            .with(50, &address(0).octets()),
    );
    let last = MAX_LEARNED;
    send(&mut dhcp, ClientPacket::discover(mac(last), PXE_CLASS, 11));
    send(
        &mut dhcp,
        ClientPacket::selecting(mac(last))
            .without(50)
            .with(50, &address(last).octets()),
    );
    assert_eq!(
        dhcp.lease(address(1)),
        None,
        "the oldest address is dropped"
    );
    for n in [0, 2, last] {
        assert_eq!(
            dhcp.lease(address(n)).map(|lease| lease.mac),
            Some(mac(n)),
            "address {n} is kept"
        );
    }
    // The second MAC's DISCOVER was dropped too: a REQUEST from it now learns no architecture.
    let got = send(
        &mut dhcp,
        ClientPacket::selecting(mac(1))
            .without(50)
            .with(50, &address(1).octets()),
    );
    assert_eq!(
        got,
        Outcome::Lease(Lease {
            address: address(1),
            mac: mac(1),
            arch: None
        })
    );
    let got = send(
        &mut dhcp,
        ClientPacket::selecting(mac(3))
            .without(50)
            .with(50, &address(3).octets()),
    );
    assert_eq!(
        got,
        Outcome::Lease(Lease {
            address: address(3),
            mac: mac(3),
            arch: Some(ClientArch::ARM64_UEFI)
        })
    );
}

/// Every input of the proxy-mode and router-mode tests, on each port, in router mode: none is
/// ever answered. The proxyDHCP port, which router mode does not use, teaches nothing either.
/// Catches: a router-mode server that sends DHCP packets, which guarding drops at best and
/// which fight the router at worst.
#[test]
fn router_mode_never_sends_a_packet() {
    let mut relayed = ClientPacket::discover(RECORDED, PXE_CLASS, 7);
    relayed.giaddr = Ipv4Addr::new(198, 51, 100, 1);
    let mut inputs = vec![
        ClientPacket::discover(RECORDED, PXE_CLASS, 7),
        ClientPacket::discover(RECORDED, HTTP_CLASS, 16),
        ClientPacket::discover(RECORDED, PXE_CLASS, 0),
        ClientPacket::discover(RECORDED, PXE_CLASS, 10),
        ClientPacket::discover(ENROLLED, PXE_CLASS, 11),
        ClientPacket::discover(RECORDED, PXE_CLASS, 7).without(60),
        ClientPacket::proxy_request(RECORDED, PXE_CLASS, 7),
        ClientPacket::selecting(RECORDED),
        ClientPacket::selecting(RECORDED).with(93, &[0, 11]),
        ClientPacket::new(REQUEST, RECORDED),
        ClientPacket::new(INFORM, RECORDED),
        relayed,
    ]
    .into_iter()
    .map(|p| p.bytes())
    .collect::<Vec<_>>();
    inputs.extend([Vec::new(), vec![0; 239], vec![0; 2000]]);
    for port in [Port::Dhcp, Port::ProxyDhcp] {
        let mut dhcp = service(Mode::Router);
        for bytes in &inputs {
            let got = dhcp.on_packet(port, bytes, &answers(None), &both_sets());
            assert!(!matches!(got, Outcome::Reply(_)), "{got:?}");
            if port == Port::ProxyDhcp {
                assert!(matches!(got, Outcome::Ignored(_)), "{got:?}");
            }
        }
    }
    let request = ClientPacket::selecting(RECORDED).bytes();
    assert_eq!(
        service(Mode::Router).on_packet(Port::ProxyDhcp, &request, &answers(None), &both_sets()),
        Outcome::Ignored(Ignored::NotAnswered {
            mac: RECORDED,
            kind: MessageType::Request,
            port: Port::ProxyDhcp,
            mode: Mode::Router,
        })
    );
    let inform = ClientPacket::new(INFORM, RECORDED).bytes();
    assert_eq!(
        service(Mode::Router).on_packet(Port::Dhcp, &inform, &answers(None), &both_sets()),
        Outcome::Ignored(Ignored::NotAnswered {
            mac: RECORDED,
            kind: MessageType::Other(INFORM),
            port: Port::Dhcp,
            mode: Mode::Router,
        })
    );
}
