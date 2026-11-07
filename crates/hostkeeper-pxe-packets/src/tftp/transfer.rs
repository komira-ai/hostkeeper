//! The transfer of one file to one client (RFC 1350, with the options of RFC 2347).

use super::Refused;
use super::packet::{ClientPacket, DEFAULT_BLKSIZE, ErrorCode, Options, oack};

/// The most blocks a transfer sends: a block number is 16 bits, and RFC 1350 says nothing of
/// what follows block 65535, so a file that needs more is refused rather than numbered past it.
/// At the default block size that is 32 MiB less 512 bytes; the boot loaders and menus served
/// by TFTP are a few MiB at most.
pub const MAX_BLOCKS: u16 = u16::MAX;

/// How many times a packet is sent again, each on a timeout the caller's clock gives, before
/// the transfer gives up.
pub const MAX_RETRANSMITS: u32 = 5;

/// A packet a transfer sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Send {
    /// This packet (the OACK).
    Packet(Vec<u8>),
    /// The DATA of `block`: the bytes `start..end` of the file, put in [`super::data_packet`].
    Data {
        /// The block number.
        block: u16,
        /// The offset of its first byte.
        start: u64,
        /// The offset after its last byte; `end - start` is less than the block size only in
        /// the last block.
        end: u64,
    },
}

/// How a transfer ended before its last block was acknowledged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Aborted {
    /// The client sent an ERROR, with this code and message.
    ClientError {
        /// Its code.
        code: u16,
        /// Its message.
        message: String,
    },
    /// The client sent a packet that is neither an ACK nor an ERROR; this ERROR (illegal TFTP
    /// operation) is the last packet to send it.
    Illegal(Vec<u8>),
    /// A packet went unacknowledged after [`MAX_RETRANSMITS`] sends again.
    GaveUp,
}

/// What a transfer does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Send this, and wait for its acknowledgement.
    Send(Send),
    /// Send nothing: a duplicate or stray ACK (RFC 1123, the TFTP section: a duplicate ACK is
    /// not answered, so no block is sent twice for one timeout), or any packet or timeout after
    /// the transfer is over.
    Wait,
    /// The last block was acknowledged.
    Done,
    /// The transfer ended.
    Aborted(Aborted),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// The OACK was sent; ACK 0 is awaited.
    Oack,
    /// The DATA of this block was sent; its ACK is awaited.
    Block(u16),
    /// Finished, either way.
    Over,
}

/// The transfer of a file of a known size.
#[derive(Clone, Debug)]
pub struct Transfer {
    blksize: u16,
    size: u64,
    last_block: u16,
    state: State,
    last_sent: Send,
    retransmits: u32,
}

impl Transfer {
    /// The transfer of a file of `size` bytes for a request with `options`, and its first
    /// packet: an OACK when the request asked for an option this server takes (block 1 follows
    /// its ACK 0), DATA of block 1 otherwise.
    ///
    /// # Errors
    /// [`Refused::TooLarge`] when the file needs more than [`MAX_BLOCKS`] blocks.
    pub fn start(options: Options, size: u64) -> Result<(Self, Send), Refused> {
        let blksize = options.blksize.unwrap_or(DEFAULT_BLKSIZE);
        // Every whole block, then the short one (empty when `size` is a multiple of `blksize`).
        let last_block = u16::try_from(size / u64::from(blksize) + 1)
            .map_err(|_| Refused::TooLarge { size, blksize })?;
        let mut transfer = Self {
            blksize,
            size,
            last_block,
            state: State::Oack,
            last_sent: Send::Packet(Vec::new()),
            retransmits: 0,
        };
        let first = if options.blksize.is_some() || options.tsize {
            Send::Packet(oack(options.blksize, options.tsize.then_some(size)))
        } else {
            transfer.data(1)
        };
        transfer.last_sent = first.clone();
        Ok((transfer, first))
    }

    /// The block size of this transfer.
    #[must_use]
    pub fn blksize(&self) -> u16 {
        self.blksize
    }

    /// The DATA of `block`, now the block awaited.
    fn data(&mut self, block: u16) -> Send {
        let start = u64::from(block - 1) * u64::from(self.blksize);
        let end = (start + u64::from(self.blksize)).min(self.size);
        self.state = State::Block(block);
        self.retransmits = 0;
        Send::Data { block, start, end }
    }

    /// The step for the packet `bytes` from the client.
    pub fn on_packet(&mut self, bytes: &[u8]) -> Step {
        if self.state == State::Over {
            return Step::Wait;
        }
        match ClientPacket::parse(bytes) {
            ClientPacket::Ack(acked) => {
                let awaited = match self.state {
                    State::Block(block) => block,
                    _ => 0,
                };
                if acked != awaited {
                    Step::Wait
                } else if acked == self.last_block {
                    self.state = State::Over;
                    Step::Done
                } else {
                    let next = self.data(acked + 1);
                    self.last_sent = next.clone();
                    Step::Send(next)
                }
            }
            ClientPacket::Error { code, message } => {
                self.state = State::Over;
                Step::Aborted(Aborted::ClientError { code, message })
            }
            ClientPacket::Illegal => {
                self.state = State::Over;
                Step::Aborted(Aborted::Illegal(ErrorCode::IllegalOperation.packet()))
            }
        }
    }

    /// The step when the last packet sent went unacknowledged for the caller's timeout: that
    /// packet again, or [`Aborted::GaveUp`] after [`MAX_RETRANSMITS`].
    pub fn on_timeout(&mut self) -> Step {
        if self.state == State::Over {
            return Step::Wait;
        }
        if self.retransmits == MAX_RETRANSMITS {
            self.state = State::Over;
            return Step::Aborted(Aborted::GaveUp);
        }
        self.retransmits += 1;
        Step::Send(self.last_sent.clone())
    }
}
