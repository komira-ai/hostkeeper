//! The bridge of a console stream (`docs/design/console-broker.md`, decisions D6 and D12, and
//! section 17.3): the bytes between the browser's WebSocket and the device, both ways, until one
//! side ends.
//!
//! Each side is a [`Pipe`] of bounded channels, which the coordinator fills from the WebSocket's
//! messages and the device's reads, and drains to the WebSocket's frames and the device's writes.
//! The bridge takes a message from one side only once the other side has room for it, so a side
//! that does not take what it is sent stops the bridge reading the other: the bytes held are at
//! most what the channels hold, whatever either side does.
//!
//! For a serial console the bridge also drops, from what the browser sends, the escape byte of the
//! BMC tool ([`SOL_ESCAPE`]), which any WebSocket client could send and which would let it end
//! the tool's session; and from what the device sends, the tool's own banner line
//! (`[SOL Session operational. ...]`), which on a pipe comes out with the host's first bytes and
//! holds the escape byte. A screen's bytes cross unchanged.
//!
//! No socket, process or clock: the caller owns the channels' other ends and decides when the
//! bridge stops, by dropping the future.

use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;

use crate::ipmi::SOL_ESCAPE;
use crate::session::Kind;

/// The start of the BMC tool's banner line.
const BANNER: &[u8] = b"[SOL Session operational.";

/// The most bytes of a first line looked at for the banner: past them, the line is the host's.
const MAX_BANNER: usize = 128;

/// One side of the bridge: what it sends, and where what it is sent goes.
pub struct Pipe {
    /// The side's messages, in order; the end of the channel is the end of the side.
    pub from: mpsc::Receiver<Vec<u8>>,
    /// Where the other side's messages go; a closed channel is the end of the side.
    pub to: mpsc::Sender<Vec<u8>>,
}

/// The bytes the bridge has passed each way, which the caller reads while it runs.
#[derive(Debug, Default)]
pub struct Moved {
    to_device: AtomicU64,
    to_browser: AtomicU64,
}

impl Moved {
    /// The bytes passed from the browser to the device, after the escape byte was dropped.
    #[must_use]
    pub fn to_device(&self) -> u64 {
        self.to_device.load(Ordering::Relaxed)
    }

    /// The bytes passed from the device to the browser, after the banner was dropped.
    #[must_use]
    pub fn to_browser(&self) -> u64 {
        self.to_browser.load(Ordering::Relaxed)
    }

    /// The bytes passed both ways together.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.to_device().saturating_add(self.to_browser())
    }
}

/// The side that ended the bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// The browser's messages ended, or it takes no more.
    Browser,
    /// The device's bytes ended, or it takes no more.
    Device,
}

/// Bridges `browser` and `device` for a session of `kind`, as the module says, counting the bytes
/// in `moved`, until a side ends.
pub async fn bridge(kind: Kind, browser: Pipe, device: Pipe, moved: &Moved) -> Ended {
    let Pipe {
        from: mut typed,
        to: shown,
    } = browser;
    let Pipe {
        from: mut said,
        to: told,
    } = device;
    let up = async {
        loop {
            let Ok(room) = told.reserve().await else {
                return Ended::Device;
            };
            let Some(mut bytes) = typed.recv().await else {
                return Ended::Browser;
            };
            if kind == Kind::Serial {
                bytes.retain(|byte| *byte != SOL_ESCAPE);
            }
            if !bytes.is_empty() {
                moved.to_device.fetch_add(len(&bytes), Ordering::Relaxed);
                room.send(bytes);
            }
        }
    };
    let down = async {
        let mut banner = Banner::new(kind);
        loop {
            let Ok(room) = shown.reserve().await else {
                return Ended::Browser;
            };
            let Some(bytes) = said.recv().await else {
                return Ended::Device;
            };
            let bytes = banner.strip(bytes);
            if !bytes.is_empty() {
                moved.to_browser.fetch_add(len(&bytes), Ordering::Relaxed);
                room.send(bytes);
            }
        }
    };
    tokio::select! {
        ended = up => ended,
        ended = down => ended,
    }
}

/// The length of `bytes` as a count of bytes moved.
fn len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

/// The dropping of the BMC tool's banner line from the start of a serial console's output.
struct Banner {
    /// What came so far while it may still be the banner; `None` once it is known.
    held: Option<Vec<u8>>,
}

impl Banner {
    /// Looks for the banner in a serial console's output only.
    fn new(kind: Kind) -> Self {
        Self {
            held: (kind == Kind::Serial).then(Vec::new),
        }
    }

    /// What of `bytes` goes on to the browser: nothing while the start of the output may still be
    /// the banner; then all of it, but for the banner's line when it was the banner.
    fn strip(&mut self, bytes: Vec<u8>) -> Vec<u8> {
        let Some(mut held) = self.held.take() else {
            return bytes;
        };
        held.extend(bytes);
        let compared = held.len().min(BANNER.len());
        if held[..compared] != BANNER[..compared] {
            return held;
        }
        match held.iter().position(|byte| *byte == b'\n') {
            Some(end) => held.split_off(end + 1),
            None if held.len() > MAX_BANNER => held,
            None => {
                self.held = Some(held);
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
#[path = "bridge_tests.rs"]
mod tests;
