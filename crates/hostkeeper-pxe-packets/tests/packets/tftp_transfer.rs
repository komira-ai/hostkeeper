//! The transfer of one file: block numbering and the final short block (RFC 1350), option
//! negotiation (RFC 2347), duplicate ACKs, timeouts and the client's errors.

use hostkeeper_pxe_packets::tftp::{
    Aborted, DEFAULT_BLKSIZE, MAX_BLOCKS, MAX_RETRANSMITS, Options, Refused, Send, Step, Transfer,
    data_packet,
};

fn ack(block: u16) -> Vec<u8> {
    let mut out = vec![0, 4];
    out.extend_from_slice(&block.to_be_bytes());
    out
}

fn data(block: u16, start: u64, end: u64) -> Send {
    Send::Data { block, start, end }
}

fn oack(pairs: &[(&str, &str)]) -> Send {
    let mut out = vec![0, 6];
    for (name, value) in pairs {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }
    Send::Packet(out)
}

fn start(options: Options, size: u64) -> (Transfer, Send) {
    Transfer::start(options, size).expect("a file that fits")
}

/// Every DATA a client acknowledging each block in turn receives, and the bytes they carry
/// out of `file`; the transfer ends with [`Step::Done`] on the last ACK.
fn receive(options: Options, file: &[u8]) -> (Vec<Send>, Vec<u8>) {
    let size = u64::try_from(file.len()).expect("a small file");
    let (mut transfer, first) = start(options, size);
    let mut sent = vec![first.clone()];
    let mut got = Vec::new();
    let mut next = first;
    loop {
        let reply = match &next {
            Send::Packet(_) => ack(0),
            Send::Data { block, start, end } => {
                let payload = &file[usize::try_from(*start).expect("small")
                    ..usize::try_from(*end).expect("small")];
                let packet = data_packet(*block, payload);
                assert_eq!(packet[..4], [0, 3, (block >> 8) as u8, *block as u8]);
                got.extend_from_slice(&packet[4..]);
                ack(*block)
            }
        };
        match transfer.on_packet(&reply) {
            Step::Send(send) => {
                sent.push(send.clone());
                next = send;
            }
            Step::Done => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    (sent, got)
}

/// With no options, blocks of 512 numbered from 1, the last one short.
/// Catches: a block numbered from 0, and a transfer that ends without a short block, which the
/// client waits on until it times out.
#[test]
fn blocks_are_numbered_from_one_and_the_last_is_short() {
    let file: Vec<u8> = (0..1300u32).map(|n| (n % 251) as u8).collect();
    let (sent, got) = receive(Options::default(), &file);
    assert_eq!(
        sent,
        [data(1, 0, 512), data(2, 512, 1024), data(3, 1024, 1300)]
    );
    assert_eq!(got, file);
}

/// A file whose size is a multiple of the block size ends with an empty block, and an empty
/// file is one empty block.
#[test]
fn a_whole_number_of_blocks_ends_with_an_empty_block() {
    let file = vec![7u8; 1024];
    let (sent, got) = receive(Options::default(), &file);
    assert_eq!(
        sent,
        [data(1, 0, 512), data(2, 512, 1024), data(3, 1024, 1024)]
    );
    assert_eq!(got, file);
    let (sent, got) = receive(Options::default(), &[]);
    assert_eq!(sent, [data(1, 0, 0)]);
    assert!(got.is_empty());
}

/// Options asked for are answered with an OACK of the values taken, block 1 follows its ACK
/// 0, and blocks are of the negotiated size.
#[test]
fn options_are_answered_with_an_oack_before_block_one() {
    let file = vec![1u8; 3000];
    let both = Options {
        blksize: Some(1468),
        tsize: true,
    };
    let (sent, got) = receive(both, &file);
    assert_eq!(
        sent,
        [
            oack(&[("blksize", "1468"), ("tsize", "3000")]),
            data(1, 0, 1468),
            data(2, 1468, 2936),
            data(3, 2936, 3000),
        ]
    );
    assert_eq!(got, file);
    let tsize = Options {
        blksize: None,
        tsize: true,
    };
    let (sent, _) = receive(tsize, &file);
    assert_eq!(sent[0], oack(&[("tsize", "3000")]));
    assert_eq!(sent[1], data(1, 0, u64::from(DEFAULT_BLKSIZE)));
    let blksize = Options {
        blksize: Some(1024),
        tsize: false,
    };
    let (transfer, first) = start(blksize, 3000);
    assert_eq!(first, oack(&[("blksize", "1024")]));
    assert_eq!(transfer.blksize(), 1024);
}

/// A duplicate ACK, an ACK of a block not yet sent and an ACK 0 after the OACK was answered
/// send nothing; the awaited ACK moves the transfer on.
/// Catches: the Sorcerer's Apprentice syndrome of RFC 1123, every block sent twice.
#[test]
fn a_duplicate_or_stray_ack_sends_nothing() {
    let options = Options {
        blksize: Some(512),
        tsize: false,
    };
    let (mut transfer, _) = start(options, 2000);
    assert_eq!(
        transfer.on_packet(&ack(1)),
        Step::Wait,
        "an ACK of a block not sent"
    );
    assert_eq!(transfer.on_packet(&ack(0)), Step::Send(data(1, 0, 512)));
    assert_eq!(transfer.on_packet(&ack(0)), Step::Wait, "ACK 0 again");
    assert_eq!(transfer.on_packet(&ack(1)), Step::Send(data(2, 512, 1024)));
    assert_eq!(transfer.on_packet(&ack(1)), Step::Wait, "ACK 1 again");
    assert_eq!(transfer.on_packet(&ack(3)), Step::Wait, "an ACK ahead");
    assert_eq!(transfer.on_packet(&ack(2)), Step::Send(data(3, 1024, 1536)));
}

/// A timeout sends the last packet again, up to [`MAX_RETRANSMITS`] times, then gives up; an
/// acknowledged block starts the count again.
#[test]
fn a_timeout_sends_the_last_packet_again_until_it_gives_up() {
    let options = Options {
        blksize: None,
        tsize: true,
    };
    let (mut transfer, first) = start(options, 600);
    for _ in 0..MAX_RETRANSMITS {
        assert_eq!(transfer.on_timeout(), Step::Send(first.clone()));
    }
    assert_eq!(transfer.on_packet(&ack(0)), Step::Send(data(1, 0, 512)));
    for _ in 0..MAX_RETRANSMITS {
        assert_eq!(transfer.on_timeout(), Step::Send(data(1, 0, 512)));
    }
    assert_eq!(transfer.on_timeout(), Step::Aborted(Aborted::GaveUp));
    assert_eq!(transfer.on_timeout(), Step::Wait, "over");
    assert_eq!(transfer.on_packet(&ack(1)), Step::Wait, "over");
}

/// The client's ERROR ends the transfer with its code and message (what is not printable
/// ASCII replaced); a packet that is neither ACK nor ERROR ends it with illegal operation.
#[test]
fn the_client_error_or_an_illegal_packet_ends_the_transfer() {
    let (mut transfer, _) = start(Options::default(), 100);
    let mut error = vec![0, 5, 0, 8];
    error.extend_from_slice(b"no\x07thanks\0trailing");
    assert_eq!(
        transfer.on_packet(&error),
        Step::Aborted(Aborted::ClientError {
            code: 8,
            message: "no?thanks".to_owned()
        })
    );
    assert_eq!(transfer.on_packet(&ack(1)), Step::Wait, "over");
    let (mut transfer, _) = start(Options::default(), 100);
    assert_eq!(
        transfer.on_packet(&[0, 5, 0, 0]),
        Step::Aborted(Aborted::ClientError {
            code: 0,
            message: String::new()
        }),
        "an ERROR with no message"
    );
    let mut illegal = vec![0, 5, 0, 4];
    illegal.extend_from_slice(b"illegal TFTP operation\0");
    for packet in [
        vec![0, 3, 0, 1, 9],
        vec![0, 4, 0],
        ack(1).repeat(2),
        vec![],
        vec![0, 1],
    ] {
        let (mut transfer, _) = start(Options::default(), 100);
        assert_eq!(
            transfer.on_packet(&packet),
            Step::Aborted(Aborted::Illegal(illegal.clone())),
            "{packet:?}"
        );
    }
}

/// The last ACK ends the transfer, and nothing is sent after it.
#[test]
fn nothing_is_sent_after_the_last_ack() {
    let (mut transfer, _) = start(Options::default(), 10);
    assert_eq!(transfer.on_packet(&ack(1)), Step::Done);
    assert_eq!(transfer.on_packet(&ack(1)), Step::Wait);
    assert_eq!(transfer.on_timeout(), Step::Wait);
}

/// A file of [`MAX_BLOCKS`] blocks (the last short) is served; one byte more would need block
/// 65536, and is refused rather than numbered past 65535.
#[test]
fn a_file_needing_more_than_65535_blocks_is_refused() {
    let blksize = u64::from(DEFAULT_BLKSIZE);
    let fits = u64::from(MAX_BLOCKS) * blksize - 1;
    let (mut transfer, _) = start(Options::default(), fits);
    let mut last = Step::Wait;
    for block in 1..=MAX_BLOCKS {
        last = transfer.on_packet(&ack(block));
        if block < MAX_BLOCKS {
            let start = u64::from(block) * blksize;
            assert_eq!(
                last,
                Step::Send(data(block + 1, start, (start + blksize).min(fits)))
            );
        }
    }
    assert_eq!(last, Step::Done);
    let too_large = fits + 1;
    assert_eq!(
        Transfer::start(Options::default(), too_large).map(|(_, send)| send),
        Err(Refused::TooLarge {
            size: too_large,
            blksize: DEFAULT_BLKSIZE
        })
    );
}

#[test]
fn a_data_packet_is_opcode_block_and_payload() {
    assert_eq!(data_packet(0x0102, b"abc"), [0, 3, 1, 2, b'a', b'b', b'c']);
    assert_eq!(data_packet(65535, &[]), [0, 3, 0xff, 0xff]);
}
