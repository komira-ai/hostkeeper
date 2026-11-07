//! DHCP on the farm LAN, in each of `hostkeeper-pxe`'s two modes (network-install.md section 3).
//!
//! [`DhcpService::on_packet`] takes one datagram, the port it came in on, the answer set and
//! the boot sets, and gives one [`Outcome`]:
//!
//! - **Proxy mode.** A DISCOVER on the DHCP port, and a REQUEST on the proxyDHCP port (4011),
//!   from a client whose vendor class is `PXEClient` or `HTTPClient`, whose MAC is in the answer
//!   set, and whose platform has a boot set, gets a [`Reply`]: an offer, or an ACK, that gives no
//!   address and names the boot file of its platform (a TFTP name for `PXEClient`, an
//!   `http://` URL for `HTTPClient`). Every other packet is [`Outcome::Ignored`], with the reason.
//! - **Router mode.** Nothing is ever sent. A DISCOVER's architecture is kept for its MAC
//!   ([`Outcome::Discover`]); a REQUEST that names the address the router offered (option 50)
//!   teaches the address, its client's MAC and its architecture ([`Outcome::Lease`]), which
//!   [`DhcpService::lease`] then gives for that address. A REQUEST that states no architecture
//!   takes the one of the last DISCOVER with its MAC.
//!
//! The platform a client is installed as is that of its architecture (option 93, first value),
//! or, when it states none, the platform of its record in the answer set. A record whose
//! platform disagrees with the firmware's is answered with nothing.

mod learn;
mod packet;

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use hostkeeper_wire::Platform;

use crate::{BootSets, ClientArch, Mac};
use learn::Learner;
use packet::ReplyKind;

pub use learn::{Lease, MAX_LEARNED};
pub use packet::{BootClass, DhcpError, MAX_PACKET, MessageType, Packet};

/// The MACs that may be installed, each with its record's platform (`None`: the record states
/// none). `serve` (item N9) makes it from the records, hosts and pending requests it read for
/// this answer; an empty set answers nothing.
pub type AnswerSet = BTreeMap<Mac, Option<Platform>>;

/// How `hostkeeper-pxe` offers network boot (its flag `--mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A proxyDHCP server: the router gives the address, this answers with the boot file.
    Proxy,
    /// The router names the boot server and `hostkeeper/boot.efi`; this sends no DHCP packet.
    Router,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Proxy => "proxy",
            Self::Router => "router",
        })
    }
}

/// The socket a datagram came in on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Port {
    /// The DHCP server port (67 by default): broadcasts from clients.
    Dhcp,
    /// The proxyDHCP port (4011 by default): a PXE client's REQUEST to the proxy after its offer.
    /// Router mode does not use it.
    ProxyDhcp,
}

impl std::fmt::Display for Port {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Dhcp => "DHCP",
            Self::ProxyDhcp => "proxyDHCP",
        })
    }
}

/// This server, as its replies name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Server {
    /// The address on the farm LAN: `siaddr` and option 54 of each reply, the TFTP server, and
    /// the host of each `http://` boot file.
    pub address: Ipv4Addr,
    /// The port of the HTTP server, in each `http://` boot file.
    pub http_port: u16,
}

/// Where a [`Reply`] is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyTo {
    /// The limited broadcast address, to the client port 68: the answer to a DISCOVER, whose
    /// client has no address yet.
    Broadcast,
    /// The address and port the request came from: the answer to a REQUEST on UDP 4011, sent
    /// from the address the router gave.
    Sender,
}

/// A packet to send, in proxy mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    /// The client it answers.
    pub mac: Mac,
    /// The platform whose boot loader it names.
    pub platform: Platform,
    /// The boot file it names: a TFTP name, or an `http://` URL.
    pub boot_file: String,
    /// The packet.
    pub bytes: Vec<u8>,
    /// Where to send it.
    pub to: ReplyTo,
}

/// What a datagram came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Proxy mode: send this.
    Reply(Reply),
    /// Router mode: a DISCOVER; its architecture, when it states one, is kept for its MAC.
    Discover {
        /// The client.
        mac: Mac,
        /// Its option 93, if any.
        arch: Option<ClientArch>,
    },
    /// Router mode: a REQUEST taught the address its client takes.
    Lease(Lease),
    /// Nothing is sent or learned, for this reason.
    Ignored(Ignored),
}

/// Why a datagram got no reply, or taught nothing.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Ignored {
    /// Not a DHCP packet this crate reads.
    #[error(transparent)]
    Malformed(#[from] DhcpError),
    /// A BOOTREPLY (`op` 2): from a server, not a client.
    #[error("a BOOTREPLY from {mac}, not a client's request")]
    NotRequest {
        /// Its `chaddr`.
        mac: Mac,
    },
    /// A message type this mode does not answer or learn from on this port.
    #[error("a {kind} from {mac} on the {port} port is not answered in {mode} mode")]
    NotAnswered {
        /// The client.
        mac: Mac,
        /// The message type.
        kind: MessageType,
        /// The port it came in on.
        port: Port,
        /// The mode.
        mode: Mode,
    },
    /// Forwarded by a relay agent: this server answers only its own broadcast domain.
    #[error("a packet from {mac} forwarded by the relay {relay}")]
    Relayed {
        /// The client.
        mac: Mac,
        /// The relay's address (`giaddr`).
        relay: Ipv4Addr,
    },
    /// No `PXEClient` or `HTTPClient` vendor class: an ordinary client, or the installer's own.
    #[error("{mac} is not a network-boot client")]
    NotNetworkBoot {
        /// The client.
        mac: Mac,
    },
    /// The MAC is not in the answer set.
    #[error("{mac} is not in the answer set")]
    NotInAnswerSet {
        /// The client.
        mac: Mac,
    },
    /// The client states no architecture and its record no platform.
    #[error("{mac} states no architecture and its record no platform")]
    NoPlatform {
        /// The client.
        mac: Mac,
    },
    /// The architecture is one this design does not install.
    #[error("{mac} states the architecture {arch}, which is not installed")]
    UnsupportedArch {
        /// The client.
        mac: Mac,
        /// Its option 93.
        arch: ClientArch,
    },
    /// The record's platform is not the firmware's.
    #[error("the record of {mac} is {record}, and its firmware states {firmware}")]
    PlatformDisagrees {
        /// The client.
        mac: Mac,
        /// The record's platform.
        record: Platform,
        /// The platform of the architecture the firmware states.
        firmware: Platform,
    },
    /// The platform has no boot set.
    #[error("{mac} is {platform}, which has no boot set")]
    NoBootSet {
        /// The client.
        mac: Mac,
        /// Its platform.
        platform: Platform,
    },
    /// A REQUEST to UDP 4011 that names another server in option 54.
    #[error("the request of {mac} names the server {server}")]
    OtherServer {
        /// The client.
        mac: Mac,
        /// The server it names.
        server: Ipv4Addr,
    },
    /// Router mode: a REQUEST with no requested address (option 50), such as a renewal.
    #[error("the request of {mac} names no address")]
    NoAddress {
        /// The client.
        mac: Mac,
    },
}

/// The DHCP side of one run of `hostkeeper-pxe`. It holds what router mode learned in this
/// run, in memory only: a run starts with nothing learned.
#[derive(Clone, Debug)]
pub struct DhcpService {
    mode: Mode,
    server: Server,
    learner: Learner,
}

impl DhcpService {
    /// A service of `mode` that names `server` in its replies, having learned nothing.
    #[must_use]
    pub fn new(mode: Mode, server: Server) -> Self {
        Self {
            mode,
            server,
            learner: Learner::default(),
        }
    }

    /// What the datagram `bytes`, which came in on `port`, comes to. `answers` and `boot` are
    /// those read for this answer; router mode reads neither.
    pub fn on_packet(
        &mut self,
        port: Port,
        bytes: &[u8],
        answers: &AnswerSet,
        boot: &BootSets,
    ) -> Outcome {
        let packet = match Packet::parse(bytes) {
            Ok(packet) => packet,
            Err(error) => return Outcome::Ignored(error.into()),
        };
        let mac = packet.mac();
        if !packet.is_request() {
            return Outcome::Ignored(Ignored::NotRequest { mac });
        }
        let kind = packet.message_type();
        match (self.mode, port, kind) {
            (Mode::Proxy, Port::Dhcp, MessageType::Discover)
            | (Mode::Proxy, Port::ProxyDhcp, MessageType::Request) => {
                match self.proxy(&packet, port, answers, boot) {
                    Ok(reply) => Outcome::Reply(reply),
                    Err(ignored) => Outcome::Ignored(ignored),
                }
            }
            (Mode::Router, Port::Dhcp, MessageType::Discover) => {
                let arch = packet.arch();
                self.learner.discover(mac, arch);
                Outcome::Discover { mac, arch }
            }
            (Mode::Router, Port::Dhcp, MessageType::Request) => {
                match self
                    .learner
                    .request(mac, packet.requested_address(), packet.arch())
                {
                    Some(lease) => Outcome::Lease(lease),
                    None => Outcome::Ignored(Ignored::NoAddress { mac }),
                }
            }
            _ => Outcome::Ignored(Ignored::NotAnswered {
                mac,
                kind,
                port,
                mode: self.mode,
            }),
        }
    }

    /// What router mode learned for `address` in this run: the client that took it last, and
    /// its architecture; `None` when no REQUEST named it.
    #[must_use]
    pub fn lease(&self, address: Ipv4Addr) -> Option<Lease> {
        self.learner.lease(address)
    }

    /// The proxy reply to a DISCOVER on the DHCP port or a REQUEST on the proxyDHCP port.
    fn proxy(
        &self,
        packet: &Packet,
        port: Port,
        answers: &AnswerSet,
        boot: &BootSets,
    ) -> Result<Reply, Ignored> {
        let mac = packet.mac();
        let relay = packet.relay();
        if !relay.is_unspecified() {
            return Err(Ignored::Relayed { mac, relay });
        }
        let class = packet.boot_class().ok_or(Ignored::NotNetworkBoot { mac })?;
        let record = *answers.get(&mac).ok_or(Ignored::NotInAnswerSet { mac })?;
        let platform = platform_of(mac, packet.arch(), record)?;
        let loaders = boot
            .loaders(platform)
            .ok_or(Ignored::NoBootSet { mac, platform })?;
        let (kind, to) = match port {
            Port::Dhcp => (ReplyKind::Offer, ReplyTo::Broadcast),
            Port::ProxyDhcp => {
                if let Some(server) = packet.server_id()
                    && server != self.server.address
                {
                    return Err(Ignored::OtherServer { mac, server });
                }
                (ReplyKind::Ack, ReplyTo::Sender)
            }
        };
        let boot_file = match class {
            BootClass::Pxe => loaders.shim.to_owned(),
            BootClass::Http => format!(
                "http://{}:{}/{}",
                self.server.address, self.server.http_port, loaders.shim
            ),
        };
        Ok(Reply {
            mac,
            platform,
            bytes: packet.reply(kind, self.server.address, class, &boot_file),
            boot_file,
            to,
        })
    }
}

/// The platform `mac` is installed as: its firmware's (from `arch`), which must agree with the
/// record's when the record has one; the record's when the firmware states none.
fn platform_of(
    mac: Mac,
    arch: Option<ClientArch>,
    record: Option<Platform>,
) -> Result<Platform, Ignored> {
    match (arch, record) {
        (None, None) => Err(Ignored::NoPlatform { mac }),
        (None, Some(record)) => Ok(record),
        (Some(arch), record) => {
            let firmware = arch
                .platform()
                .ok_or(Ignored::UnsupportedArch { mac, arch })?;
            match record {
                Some(record) if record != firmware => Err(Ignored::PlatformDisagrees {
                    mac,
                    record,
                    firmware,
                }),
                _ => Ok(firmware),
            }
        }
    }
}
