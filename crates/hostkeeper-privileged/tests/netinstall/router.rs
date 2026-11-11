//! A minimal fake of the farm router's DHCP: it gives each client an address of its pool and, as
//! each step of the test sets it, either no boot file (the router of proxy mode) or
//! `pxe`'s address and `hostkeeper/boot.efi` (the router of router mode, as the
//! UniFi "Network Boot" setting is designed to: the `siaddr` and `file` fields and options 66 and
//! 67).
//!
//! [`Router`] is the answer, a pure function of a packet and the leases given so far; it is run
//! without root by `tests/net_install_rig.rs`. [`RouterDhcp`] serves it on UDP 67 inside the
//! router's network namespace, from a thread of the test, and logs one line per packet.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use hostkeeper_pxe_packets::Mac;
use hostkeeper_pxe_packets::dhcp::{MessageType, Packet};

use super::events::{LineSink, Source};

/// The port a DHCP server listens on.
pub const SERVER_PORT: u16 = 67;
/// The port a DHCP client listens on.
pub const CLIENT_PORT: u16 = 68;
/// How long a lease the router gives lasts, in seconds: longer than any test.
const LEASE_SECS: u32 = 86_400;
/// The fixed part of a BOOTP packet: the header up to and with `file`.
const HEADER: usize = 236;
/// The magic cookie of RFC 2131, after the header.
const COOKIE: [u8; 4] = [99, 130, 83, 99];
/// How often the serving thread looks at its stop flag while no packet comes.
const STOP_LOOK: Duration = Duration::from_millis(200);

/// Where the router names the boot server, in router mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Boot {
    /// `pxe`'s address: `siaddr` and option 66.
    pub server: Ipv4Addr,
    /// The boot file: the `file` field and option 67.
    pub file: String,
}

/// The router's segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// The router's own address: its server identifier, and the clients' router (option 3).
    pub router: Ipv4Addr,
    /// The prefix length of the segment.
    pub prefix: u8,
    /// The first address of the pool.
    pub first: Ipv4Addr,
    /// How many addresses the pool has.
    pub size: u8,
}

impl Segment {
    fn mask(&self) -> Ipv4Addr {
        Ipv4Addr::from(
            u32::MAX
                .checked_shl(32 - u32::from(self.prefix))
                .unwrap_or(0),
        )
    }
}

/// What the router does with one packet: the line it logs, and the reply it broadcasts, if any.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    pub line: String,
    pub reply: Option<Vec<u8>>,
}

/// The router's DHCP: the segment, the boot server it names (router mode) or none (proxy mode),
/// and the address it gave each MAC, in the order the MACs came.
pub struct Router {
    segment: Segment,
    boot: Option<Boot>,
    leases: BTreeMap<Mac, Ipv4Addr>,
}

/// DHCP's message types this router sends.
#[derive(Clone, Copy)]
enum Kind {
    Offer = 2,
    Ack = 5,
    Nak = 6,
}

impl Router {
    pub fn new(segment: Segment, boot: Option<Boot>) -> Router {
        Router {
            segment,
            boot,
            leases: BTreeMap::new(),
        }
    }

    /// The address of `mac`: the one it was given, or the next of the pool.
    fn lease(&mut self, mac: Mac) -> Option<Ipv4Addr> {
        if let Some(address) = self.leases.get(&mac) {
            return Some(*address);
        }
        let given = u32::try_from(self.leases.len()).ok()?;
        if given >= u32::from(self.segment.size) {
            return None;
        }
        let address = Ipv4Addr::from(u32::from(self.segment.first) + given);
        self.leases.insert(mac, address);
        Some(address)
    }

    /// The router's answer to the datagram `bytes`.
    pub fn answer(&mut self, bytes: &[u8]) -> Answer {
        let packet = match Packet::parse(bytes) {
            Ok(packet) if packet.is_request() => packet,
            Ok(_) => return silent("not answered: a BOOTREPLY".to_owned()),
            Err(why) => return silent(format!("not answered: {why}")),
        };
        let (mac, kind) = (packet.mac(), packet.message_type());
        let said = format!("{kind} from {mac}");
        match kind {
            MessageType::Discover => match self.lease(mac) {
                Some(address) => self.give(bytes, Kind::Offer, &said, "offer", address),
                None => silent(format!(
                    "{said}: not answered: the pool of {} addresses is used up",
                    self.segment.size
                )),
            },
            MessageType::Request => {
                if let Some(server) = packet.server_id().filter(|s| *s != self.segment.router) {
                    return silent(format!(
                        "{said}: not answered: it is for the server {server}"
                    ));
                }
                let asked = packet.requested_address().or_else(|| {
                    Some(Ipv4Addr::new(bytes[12], bytes[13], bytes[14], bytes[15]))
                        .filter(|ciaddr| !ciaddr.is_unspecified())
                });
                match (self.lease(mac), asked) {
                    (Some(address), Some(asked)) if asked == address => {
                        self.give(bytes, Kind::Ack, &said, "ack", address)
                    }
                    (lease, asked) => {
                        let why = match (lease, asked) {
                            (None, _) => {
                                format!("the pool of {} addresses is used up", self.segment.size)
                            }
                            (Some(address), Some(asked)) => {
                                format!("it asks for {asked}, and its address is {address}")
                            }
                            (Some(_), None) => "it names no address".to_owned(),
                        };
                        Answer {
                            line: format!("{said}: nak: {why}"),
                            reply: Some(self.reply(bytes, Kind::Nak, Ipv4Addr::UNSPECIFIED)),
                        }
                    }
                }
            }
            _ => silent(format!("{said}: not answered")),
        }
    }

    /// An offer or an ack of `address`, and its line.
    fn give(&self, bytes: &[u8], kind: Kind, said: &str, verb: &str, address: Ipv4Addr) -> Answer {
        let boot = match &self.boot {
            Some(boot) => format!("boot file {} on {}", boot.file, boot.server),
            None => "no boot file".to_owned(),
        };
        Answer {
            line: format!("{said}: {verb} {address}, {boot}"),
            reply: Some(self.reply(bytes, kind, address)),
        }
    }

    /// A BOOTREPLY of `kind` to the request `bytes`, giving `yiaddr`.
    fn reply(&self, bytes: &[u8], kind: Kind, yiaddr: Ipv4Addr) -> Vec<u8> {
        let mut out = vec![0u8; HEADER];
        out[0] = 2;
        out[1] = 1;
        out[2] = 6;
        // xid and flags as the client sent them; chaddr too.
        out[4..8].copy_from_slice(&bytes[4..8]);
        out[10..12].copy_from_slice(&bytes[10..12]);
        out[16..20].copy_from_slice(&yiaddr.octets());
        out[28..44].copy_from_slice(&bytes[28..44]);
        let given = !matches!(kind, Kind::Nak);
        let boot = self.boot.as_ref().filter(|_| given);
        if let Some(boot) = boot {
            out[20..24].copy_from_slice(&boot.server.octets());
            // `file` is 128 bytes at 108, NUL-padded.
            let file = boot.file.as_bytes();
            out[108..108 + file.len()].copy_from_slice(file);
        }
        out.extend_from_slice(&COOKIE);
        let mut option = |code: u8, value: &[u8]| {
            out.push(code);
            out.push(u8::try_from(value.len()).expect("a short option"));
            out.extend_from_slice(value);
        };
        option(53, &[kind as u8]);
        option(54, &self.segment.router.octets());
        if given {
            option(51, &LEASE_SECS.to_be_bytes());
            option(1, &self.segment.mask().octets());
            option(3, &self.segment.router.octets());
            if let Some(boot) = boot {
                option(66, boot.server.to_string().as_bytes());
                option(67, boot.file.as_bytes());
            }
        }
        out.push(255);
        out
    }
}

fn silent(line: String) -> Answer {
    Answer { line, reply: None }
}

/// The router's DHCP serving: a thread of the test with a socket made in the router's network
/// namespace on UDP 67, which broadcasts each reply to UDP 68 on `interface`. Dropping it stops
/// the thread.
pub struct RouterDhcp {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RouterDhcp {
    pub fn start(socket: UdpSocket, mut router: Router, sink: Arc<LineSink>) -> RouterDhcp {
        socket
            .set_read_timeout(Some(STOP_LOOK))
            .expect("a read timeout");
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let to = SocketAddrV4::new(Ipv4Addr::BROADCAST, CLIENT_PORT);
        let thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 1500];
            while !stopped.load(Ordering::SeqCst) {
                // A timeout is the look at the stop flag; any other error ends nothing either.
                let Ok((read, _)) = socket.recv_from(&mut buffer) else {
                    continue;
                };
                let answer = router.answer(&buffer[..read]);
                if let Some(reply) = &answer.reply
                    && let Err(e) = socket.send_to(reply, to)
                {
                    sink.line(Source::Router, format!("{}: not sent: {e}", answer.line));
                    continue;
                }
                sink.line(Source::Router, answer.line);
            }
        });
        RouterDhcp {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for RouterDhcp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
