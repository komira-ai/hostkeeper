//! The sockets of a run as the kernel binds them: a socket bound to port 0 has a port no other
//! socket can be bound to, and a socket bound to a port its flag names still shares it.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use socket2::{Domain, Protocol, Socket, Type};

use crate::serve::sockets::{Addresses, Bound};

/// Loopback at `port`.
fn loopback(port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)
}

/// A socket of `kind` with `SO_REUSEADDR`, not bound yet.
fn sharing(kind: Type, protocol: Protocol) -> Socket {
    let socket = Socket::new(Domain::IPV4, kind, Some(protocol)).expect("a socket");
    socket.set_reuse_address(true).expect("SO_REUSEADDR");
    socket
}

/// The IPv4 address `address` is.
fn v4(address: std::io::Result<SocketAddr>) -> SocketAddrV4 {
    match address.expect("its address") {
        SocketAddr::V4(address) => address,
        SocketAddr::V6(address) => panic!("an IPv6 address {address}"),
    }
}

/// Every socket of a run whose flags name port 0 takes a port no socket with `SO_REUSEADDR` can
/// be bound to as well: the DHCP, proxyDHCP and TFTP sockets, and the HTTP listener (which Linux
/// shares with no other socket while it listens, `SO_REUSEADDR` or not: that part holds the
/// listener to the same rule, it does not tell the fix from the code before it).
///
/// Catches a port-0 socket that shares its port: the kernel may then give it a port another run
/// holds, and a DISCOVER or a TFTP read sent to this run reaches the other one, so the client
/// waits for an answer that never comes.
#[test]
fn a_socket_bound_to_port_zero_shares_its_port_with_no_other_socket() {
    let bound = Bound::bind(&Addresses {
        dhcp: loopback(0),
        proxy: Some(loopback(0)),
        tftp: loopback(0),
        http: loopback(0),
        interface: None,
    })
    .expect("the sockets");
    let proxy = bound.proxy.as_ref().expect("the proxyDHCP socket");
    for (what, address) in [
        ("DHCP", v4(bound.dhcp.local_addr())),
        ("proxyDHCP", v4(proxy.local_addr())),
        ("TFTP", v4(bound.tftp.local_addr())),
    ] {
        assert_ne!(address.port(), 0, "{what}");
        let other = sharing(Type::DGRAM, Protocol::UDP);
        let Err(error) = other.bind(&address.into()) else {
            panic!("the {what} port {address} is shared");
        };
        assert_eq!(error.kind(), ErrorKind::AddrInUse, "{what}: {error}");
    }
    let http = v4(bound.http.local_addr());
    assert_ne!(http.port(), 0);
    let other = sharing(Type::STREAM, Protocol::TCP);
    let Err(error) = other.bind(&http.into()) else {
        panic!("the HTTP port {http} is shared");
    };
    assert_eq!(error.kind(), ErrorKind::AddrInUse, "HTTP: {error}");
}

/// A socket bound to a port its flag names still shares that port with a socket another
/// program holds with `SO_REUSEADDR` (a host's own DHCP client): each of the DHCP, proxyDHCP
/// and TFTP sockets is bound to the port such a socket holds, the kernel's choice for it.
///
/// Catches a fix of the port-0 case that takes `SO_REUSEADDR` from every socket, and a DHCP
/// port held by the host's DHCP client that then refuses the start.
#[test]
fn a_socket_bound_to_a_named_port_shares_it_with_a_socket_that_allows_it() {
    let held: Vec<(Socket, SocketAddrV4)> = (0..3)
        .map(|_| {
            let socket = sharing(Type::DGRAM, Protocol::UDP);
            socket
                .bind(&SocketAddr::V4(loopback(0)).into())
                .expect("a held port");
            let address = socket
                .local_addr()
                .expect("its address")
                .as_socket_ipv4()
                .expect("IPv4");
            (socket, address)
        })
        .collect();
    let bound = Bound::bind(&Addresses {
        dhcp: held[0].1,
        proxy: Some(held[1].1),
        tftp: held[2].1,
        http: loopback(0),
        interface: None,
    })
    .expect("the sockets share the held ports");
    assert_eq!(v4(bound.dhcp.local_addr()), held[0].1);
    assert_eq!(
        v4(bound.proxy.as_ref().expect("proxyDHCP").local_addr()),
        held[1].1
    );
    assert_eq!(v4(bound.tftp.local_addr()), held[2].1);
}
