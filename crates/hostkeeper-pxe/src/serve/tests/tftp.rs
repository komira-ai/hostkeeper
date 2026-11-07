//! TFTP, end to end: the boot loaders and the fixed `grub.cfg` are served by their names, in
//! blocks, with the options of RFC 2347; every other request is refused with its TFTP error and
//! logged; a transfer sends a block again until it is acknowledged, and ends when the client
//! stops, errs or sends a packet that is not an ACK.

use std::net::{SocketAddr, UdpSocket};

use hostkeeper_pxe_seed::inputs::LinuxPlatform;

use super::kit::{Kit, NOW, Running, SHIM_BYTES, ack, client, content, receive, rrq, tftp_read};

/// A run in proxy mode whose offers go nowhere the tests read.
fn start(name: &str) -> (Kit, Running) {
    let kit = Kit::new(name);
    let sink = client();
    let run = kit.start(kit.args("proxy", sink.local_addr().expect("its address"), &[]));
    (kit, run)
}

/// The opcode and the first word (block or error code) of a TFTP packet.
fn head(packet: &[u8]) -> (u16, u16) {
    (
        u16::from_be_bytes([packet[0], packet[1]]),
        u16::from_be_bytes([packet[2], packet[3]]),
    )
}

/// Sends a read request of `name` from `socket` and returns the first packet and its sender
/// (the transfer's port).
fn first(socket: &UdpSocket, server: SocketAddr, name: &str) -> (Vec<u8>, SocketAddr) {
    socket.send_to(&rrq(name, &[]), server).expect("send");
    receive(socket)
}

/// The shim, read by its name with the default block size, is its exact bytes: two full blocks
/// of 512 and a short third. With `blksize 1024` and `tsize 0` the OACK carries both, the size
/// being the file's, and the file comes in a full block and a short one. Each transfer is
/// logged with its size once its last block is acknowledged.
///
/// Catches a transfer that loses or repeats a block, one that ignores the block size asked for,
/// and an OACK that names another size than the file's.
#[test]
fn a_boot_loader_is_sent_whole_with_or_without_options() {
    let (_kit, mut run) = start("tftp-whole");
    let shim = content(LinuxPlatform::X86_64, "shim", SHIM_BYTES);
    assert_eq!(
        tftp_read(run.tftp, "hostkeeper/shimx64.efi", &[]),
        Ok(shim.clone())
    );
    let line = run.wait("sent hostkeeper/shimx64.efi");
    assert!(
        line.starts_with(&format!("{NOW} tftp 127.0.0.1:")),
        "{line}"
    );
    assert!(
        line.ends_with(": sent hostkeeper/shimx64.efi (1500 bytes)"),
        "{line}"
    );

    let socket = client();
    socket
        .send_to(
            &rrq(
                "hostkeeper/shimx64.efi",
                &[("blksize", "1024"), ("tsize", "0")],
            ),
            run.tftp,
        )
        .expect("send");
    let (oack, transfer) = receive(&socket);
    assert_eq!(oack, b"\0\x06blksize\x001024\0tsize\x001500\0");
    socket.send_to(&ack(0), transfer).expect("send");
    let (one, _) = receive(&socket);
    assert_eq!(head(&one), (3, 1));
    assert_eq!(&one[4..], &shim[..1024]);
    socket.send_to(&ack(1), transfer).expect("send");
    let (two, _) = receive(&socket);
    assert_eq!(head(&two), (3, 2));
    assert_eq!(&two[4..], &shim[1024..]);
    socket.send_to(&ack(2), transfer).expect("send");
    run.wait("sent hostkeeper/shimx64.efi");
    let arm = content(LinuxPlatform::Arm64, "grub", 700);
    assert_eq!(
        tftp_read(run.tftp, "/hostkeeper/grubaa64.efi", &[]),
        Ok(arm)
    );
    assert_eq!(run.stop().0, Ok(()));
}

/// A write request is refused "access violation" (2), a name that climbs out of the tree is
/// refused as unsafe (2), an unknown name and the kernel (served by HTTP only) "file not found"
/// (1); each refusal is logged with the reader's address and the reason.
///
/// Catches a TFTP server that accepts a write, follows `..` out of its tree, or serves by TFTP a
/// file only HTTP serves.
#[test]
fn every_other_request_is_refused_and_logged() {
    let (_kit, mut run) = start("tftp-refused");
    let socket = client();
    let me = socket.local_addr().expect("its address");
    let mut write = rrq("hostkeeper/shimx64.efi", &[]);
    write[1] = 2;
    socket.send_to(&write, run.tftp).expect("send");
    let (error, _) = receive(&socket);
    assert_eq!(error, b"\0\x05\0\x02access violation\0");
    assert_eq!(
        run.wait("refused"),
        format!("{NOW} tftp {me}: refused: a write request for \"hostkeeper/shimx64.efi\"")
    );
    for (name, code, said) in [
        (
            "../etc/passwd",
            2,
            "the name \"../etc/passwd\" is not a relative path of plain components".to_owned(),
        ),
        ("missing.efi", 1, "\"missing.efi\" is not served".to_owned()),
        (
            "vmlinuz-linux-x86_64",
            1,
            "\"vmlinuz-linux-x86_64\" is not served".to_owned(),
        ),
    ] {
        let (error, _) = first(&socket, run.tftp, name);
        assert_eq!(head(&error), (5, code), "{name}");
        assert_eq!(
            run.wait("refused"),
            format!("{NOW} tftp {me}: refused: {said}"),
            "{name}"
        );
    }
    assert_eq!(run.stop().0, Ok(()));
}

/// A block that is not acknowledged is sent again, the same bytes; after the retransmits the
/// transfer gives up, and logs that the client stopped acknowledging.
///
/// Catches a transfer that sends a block once and waits for ever on a lost packet, and one that
/// retransmits without end.
#[test]
fn an_unacknowledged_block_is_sent_again_until_the_transfer_gives_up() {
    let (_kit, mut run) = start("tftp-again");
    let socket = client();
    let (once, transfer) = first(&socket, run.tftp, "hostkeeper/shimx64.efi");
    let (again, from) = receive(&socket);
    assert_eq!(head(&once), (3, 1));
    assert_eq!((again, from), (once, transfer));
    let line = run.wait("not sent");
    assert!(
        line.ends_with(": hostkeeper/shimx64.efi not sent: the client stopped acknowledging"),
        "{line}"
    );
    assert_eq!(run.stop().0, Ok(()));
}

/// A packet to a transfer's port from another socket is answered "unknown transfer ID" (5) and
/// changes nothing: the transfer goes on with its client and completes.
///
/// Catches a transfer another host on the LAN can steer or end.
#[test]
fn a_stranger_to_a_transfer_gets_unknown_transfer_id() {
    let (_kit, mut run) = start("tftp-stranger");
    let socket = client();
    let (one, transfer) = first(&socket, run.tftp, "hostkeeper/shimx64.efi");
    let stranger = client();
    stranger.send_to(&ack(1), transfer).expect("send");
    let (error, from) = receive(&stranger);
    assert_eq!(from, transfer);
    assert_eq!(error, b"\0\x05\0\x05unknown transfer ID\0");
    let mut file = one[4..].to_vec();
    socket.send_to(&ack(1), transfer).expect("send");
    loop {
        let (data, _) = receive(&socket);
        let (op, block) = head(&data);
        assert_eq!(op, 3);
        file.extend_from_slice(&data[4..]);
        socket.send_to(&ack(block), transfer).expect("send");
        if data.len() - 4 < 512 {
            break;
        }
    }
    assert_eq!(file, content(LinuxPlatform::X86_64, "shim", SHIM_BYTES));
    run.wait("sent hostkeeper/shimx64.efi (1500 bytes)");
    assert_eq!(run.stop().0, Ok(()));
}

/// A client's ERROR ends its transfer, logged with its code and message; a packet that is
/// neither an ACK nor an ERROR is answered "illegal TFTP operation" (4) and ends it too.
///
/// Catches a transfer that keeps sending to a client that gave up, and one that takes any
/// packet as an acknowledgement.
#[test]
fn a_client_error_or_an_illegal_packet_ends_the_transfer() {
    let (_kit, mut run) = start("tftp-abort");
    let socket = client();
    let (_, transfer) = first(&socket, run.tftp, "hostkeeper/shimx64.efi");
    socket
        .send_to(b"\0\x05\0\x00stop\0", transfer)
        .expect("send");
    let line = run.wait("not sent");
    assert!(
        line.ends_with(": hostkeeper/shimx64.efi not sent: the client sent error 0: \"stop\""),
        "{line}"
    );
    let other = client();
    let (_, transfer) = first(&other, run.tftp, "hostkeeper/grubx64.efi");
    other
        .send_to(&rrq("hostkeeper/grubx64.efi", &[]), transfer)
        .expect("send");
    let (error, _) = loop {
        let (packet, from) = receive(&other);
        if head(&packet).0 == 5 {
            break (packet, from);
        }
    };
    assert_eq!(error, b"\0\x05\0\x04illegal TFTP operation\0");
    let line = run.wait("not sent");
    assert!(
        line.ends_with(
            ": hostkeeper/grubx64.efi not sent: the client sent a packet that is neither an ACK nor an ERROR"
        ),
        "{line}"
    );
    assert_eq!(run.stop().0, Ok(()));
}
