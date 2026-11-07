//! The TFTP socket of a run (RFC 1350, the options of RFC 2347): each read request is taken by
//! `hostkeeper-pxe-packets` ([`on_request`]) against the names served, what router mode learned
//! for the reader's address and, for `hostkeeper/boot.efi`, the record platforms read now; and
//! each transfer runs in a task of its own, from a new socket at `--address` and a port of its
//! own (the server's transfer id), as RFC 1350 has it.
//!
//! A refused request is answered with the TFTP error of its refusal, from that new socket, and
//! logged with the reader's address. A transfer sends each packet again when its ACK does not
//! come within `--tftp-timeout-ms`, up to [`MAX_RETRANSMITS`] times; a packet from another
//! address or port is answered "unknown transfer ID" and changes nothing.
//!
//! [`MAX_RETRANSMITS`]: hostkeeper_pxe_packets::tftp::MAX_RETRANSMITS

use std::net::{IpAddr, SocketAddr, SocketAddrV4};
use std::sync::Arc;

use hostkeeper_pxe_packets::tftp::{
    Aborted, BOOT_EFI, ErrorCode, MAX_BLKSIZE, MAX_REQUEST, RecordPlatforms, Send, Step, Target,
    Transfer, data_packet, on_request, parse_request,
};
use tokio::net::UdpSocket;
use tokio::sync::watch;

use crate::serve::server::{Content, Protocol, Server};

/// The most bytes read of one packet: a request, or a client's packet in a transfer.
const MAX_DATAGRAM: usize = MAX_REQUEST + MAX_BLKSIZE as usize;

/// Takes the read requests of `socket` until `stop` changes.
pub async fn serve(server: Arc<Server>, socket: UdpSocket, mut stop: watch::Receiver<bool>) {
    let mut buffer = vec![0; MAX_DATAGRAM];
    loop {
        let received = tokio::select! {
            received = socket.recv_from(&mut buffer) => received,
            _ = stop.changed() => return,
        };
        let Ok((n, from)) = received else { continue };
        let bytes = buffer[..n].to_vec();
        let server = Arc::clone(&server);
        tokio::spawn(async move { request(&server, &bytes, from).await });
    }
}

/// Answers one read request from `from`.
async fn request(server: &Server, bytes: &[u8], from: SocketAddr) {
    let address = SocketAddrV4::new(server.settings().address, 0);
    let socket = match UdpSocket::bind(address).await {
        Ok(socket) => socket,
        Err(error) => {
            server.log().line(format_args!(
                "tftp {from}: no transfer socket at {address}: {error}"
            ));
            return;
        }
    };
    let lease = match from.ip() {
        IpAddr::V4(ip) => server.lease(ip),
        IpAddr::V6(_) => None,
    };
    let boot_efi = parse_request(bytes)
        .is_ok_and(|request| request.name.strip_prefix('/').unwrap_or(&request.name) == BOOT_EFI);
    let records: RecordPlatforms = match boot_efi {
        true => server.answers().await.record_platforms,
        false => RecordPlatforms::new(),
    };
    let read = match on_request(
        bytes,
        server.tftp_files(),
        lease.as_ref(),
        &records,
        server.boot(),
    ) {
        Ok(read) => read,
        Err(refused) => {
            server
                .log()
                .line(format_args!("tftp {from}: refused: {refused}"));
            let _ = socket.send_to(&refused.packet(), from).await;
            return;
        }
    };
    let (name, content) = match read.target {
        Target::File(name) => {
            let content = server.content(&name, Protocol::Tftp).await;
            (name, content)
        }
        Target::Generated { dir, name } => {
            let name = format!("{dir}/{name}");
            let content = server.content(&name, Protocol::Tftp).await;
            (name, content)
        }
        Target::BootEfi {
            mac,
            platform,
            shim,
        } => {
            server.log().line(format_args!(
                "tftp {from}: {BOOT_EFI} is {shim} for {mac} ({platform})"
            ));
            (shim.to_owned(), server.shim(shim))
        }
    };
    let Some(content) = content else {
        server
            .log()
            .line(format_args!("tftp {from}: refused: {name:?} is not served"));
        let _ = socket
            .send_to(&ErrorCode::FileNotFound.packet(), from)
            .await;
        return;
    };
    let (transfer, first) = match Transfer::start(read.options, content.len()) {
        Ok(started) => started,
        Err(refused) => {
            server
                .log()
                .line(format_args!("tftp {from}: refused: {refused}"));
            let _ = socket.send_to(&refused.packet(), from).await;
            return;
        }
    };
    let said = match send_file(server, &socket, from, transfer, first, &content).await {
        Ok(()) => format!("sent {name} ({} bytes)", content.len()),
        Err(why) => format!("{name} not sent: {why}"),
    };
    server.log().line(format_args!("tftp {from}: {said}"));
}

/// Runs `transfer` of `content` to `client`, whose first packet is `first`, to its end.
async fn send_file(
    server: &Server,
    socket: &UdpSocket,
    client: SocketAddr,
    mut transfer: Transfer,
    first: Send,
    content: &Content,
) -> Result<(), String> {
    let timeout = server.settings().tftp_timeout;
    let mut buffer = vec![0; MAX_DATAGRAM];
    let mut step = Step::Send(first);
    loop {
        match step {
            Step::Send(send) => {
                let packet = match send {
                    Send::Packet(packet) => packet,
                    Send::Data { block, start, end } => {
                        let payload = content
                            .read(start, (end - start) as usize)
                            .map_err(|e| format!("the file could not be read: {e}"))?;
                        data_packet(block, &payload)
                    }
                };
                socket
                    .send_to(&packet, client)
                    .await
                    .map_err(|e| format!("a packet could not be sent: {e}"))?;
            }
            Step::Wait => {}
            Step::Done => return Ok(()),
            Step::Aborted(Aborted::ClientError { code, message }) => {
                return Err(format!("the client sent error {code}: {message:?}"));
            }
            Step::Aborted(Aborted::Illegal(packet)) => {
                let _ = socket.send_to(&packet, client).await;
                return Err(
                    "the client sent a packet that is neither an ACK nor an ERROR".to_owned(),
                );
            }
            Step::Aborted(Aborted::GaveUp) => {
                return Err("the client stopped acknowledging".to_owned());
            }
        }
        step = loop {
            match tokio::time::timeout(timeout, socket.recv_from(&mut buffer)).await {
                Err(_) => break transfer.on_timeout(),
                Ok(Ok((n, from))) if from == client => break transfer.on_packet(&buffer[..n]),
                Ok(Ok((_, stranger))) => {
                    let _ = socket
                        .send_to(&ErrorCode::UnknownTid.packet(), stranger)
                        .await;
                }
                // A failed receive counts as a timeout, so a socket that keeps failing ends the
                // transfer after its retransmits.
                Ok(Err(_)) => break transfer.on_timeout(),
            }
        };
    }
}
