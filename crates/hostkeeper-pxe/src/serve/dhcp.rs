//! The DHCP sockets of a run: each datagram is answered by [`Server::on_dhcp`] in a task of its
//! own (a proxy answer reads the coordinator first), and its answer is sent from the socket it
//! came in on: a proxy offer to `--dhcp-reply-to` (the limited broadcast address and the client
//! port, 68, by default), a proxy ACK to the address and port the REQUEST came from. Router
//! mode sends nothing.

use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;

use hostkeeper_pxe_packets::dhcp::{MAX_PACKET, Port, ReplyTo};
use tokio::net::UdpSocket;
use tokio::sync::watch;

use crate::serve::server::Server;

/// Answers the datagrams of `socket`, which is the `port` socket, until `stop` changes.
pub async fn serve(
    server: Arc<Server>,
    socket: Arc<UdpSocket>,
    port: Port,
    reply_to: SocketAddrV4,
    mut stop: watch::Receiver<bool>,
) {
    let mut buffer = vec![0; MAX_PACKET + 1];
    loop {
        let received = tokio::select! {
            received = socket.recv_from(&mut buffer) => received,
            _ = stop.changed() => return,
        };
        let (n, from) = match received {
            Ok(received) => received,
            Err(_) => continue,
        };
        let bytes = buffer[..n].to_vec();
        let (server, socket) = (Arc::clone(&server), Arc::clone(&socket));
        tokio::spawn(async move {
            if let Some((reply, to)) = server.on_dhcp(port, &bytes).await {
                let to = match to {
                    ReplyTo::Broadcast => SocketAddr::V4(reply_to),
                    ReplyTo::Sender => from,
                };
                match socket.send_to(&reply, to).await {
                    Ok(_) => {}
                    Err(error) => server.log().line(format_args!(
                        "dhcp: the answer to {to} could not be sent: {error}"
                    )),
                }
            }
        });
    }
}
