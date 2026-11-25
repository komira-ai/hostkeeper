//! The TCP dial of a Mac's Screen Sharing, against a listener on loopback with a port the kernel
//! chose, and how a failed dial closes the stream.

use std::io::ErrorKind;
use std::net::Ipv4Addr;
use std::time::Duration;

use hostkeeper_console::close::Close;
use hostkeeper_console::targets::Address;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::device::{dial_failure, tcp_dial};

/// Proves: the dial connects to the given port at the address, and the connection carries the
/// dialler's bytes to the listener.
///
/// Catches: a dial to another port than the one it was made for.
#[tokio::test]
async fn the_dial_connects_to_its_port_at_the_address() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bound");
    let port = listener.local_addr().expect("an address").port();
    let dial = tcp_dial(port, Duration::from_secs(3_600));
    let address = Address::Ip(Ipv4Addr::LOCALHOST.into());
    let (dialled, accepted) = tokio::join!(dial(address.clone()), listener.accept());
    let mut dialled = dialled.expect("connected");
    let (mut mac, _) = accepted.expect("accepted");
    dialled.write_all(b"RFB").await.expect("written");
    let mut got = [0u8; 3];
    mac.read_exact(&mut got).await.expect("read");
    assert_eq!(&got, b"RFB");
}

/// Proves: a refused connection closes the stream `mac_screen_sharing_off`, and every other
/// failure (the timeout, an unreachable host or network, a reset) `mac_unreachable_or_asleep`.
///
/// Catches: a Mac whose Screen Sharing is off told to the operator as asleep, or the reverse.
#[test]
fn a_refused_connection_is_mac_screen_sharing_off() {
    assert_eq!(
        dial_failure(&ErrorKind::ConnectionRefused.into()),
        Close::MacScreenSharingOff
    );
    for kind in [
        ErrorKind::TimedOut,
        ErrorKind::HostUnreachable,
        ErrorKind::NetworkUnreachable,
        ErrorKind::ConnectionReset,
    ] {
        assert_eq!(
            dial_failure(&kind.into()),
            Close::MacUnreachableOrAsleep,
            "{kind:?}"
        );
    }
}
