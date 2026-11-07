//! What one run of `serve` decides, apart from its sockets: the answer of a DHCP packet, and the
//! content of a name read by TFTP or HTTP.
//!
//! - **The answer set is read for each answer** ([`Server::answers`]): a proxy offer or ACK, a
//!   boot menu, a seed's first fetch and the `hostkeeper/boot.efi` of a client that stated no
//!   architecture each read the coordinator first. A read that fails is logged, and that answer
//!   is made from the empty set: no offer, the local-disk menu, no seed, no record platform.
//! - **A menu** is chosen by `hostkeeper-pxe-seed` from the answer set and the mode; the install
//!   chosen is kept for the MAC, because its seed is made from it.
//! - **A seed** is served only to a MAC that was given an install menu in this run. Its first
//!   fetch reads the answer set again and is refused unless the MAC is still in it, as the
//!   record of the same host; that fetch takes the MAC out of the set for the rest of the run,
//!   and the later fetches of the same seed are served from the install kept.
//! - **A sealed file** is served by its name: by TFTP only the boot loaders (the shim and GRUB of
//!   each set), by HTTP every sealed file.
//!
//! The fixed `grub.cfg` and the menus are served under [`GRUB_PREFIX`], by TFTP (GRUB's network
//! `$prefix` on the TFTP server, network-install.md section 4) and by HTTP alike (the same path
//! below the server, for a GRUB loaded by UEFI HTTP boot).

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_pxe_packets::BootSets;
use hostkeeper_pxe_packets::dhcp::{
    DhcpService, Ignored, Lease, Mode, Outcome, Packet, Port, ReplyTo,
};
use hostkeeper_pxe_packets::tftp::FileSet;
use hostkeeper_pxe_seed::inputs::{
    BootServer, DaemonConfigText, InstallDisk, LinuxPlatform, SerialConsole,
};
use hostkeeper_pxe_seed::keys::AuthorizedKeys;
use hostkeeper_pxe_seed::layout::MENU_DIR;
use hostkeeper_pxe_seed::menu::{GRUB_CFG, Install, LOCAL_DISK_MENU, MenuChoice, MenuRequest};
use hostkeeper_pxe_seed::seed::{SeedInputs, seed};
use hostkeeper_wire::HostId;

use crate::serve::answers::{Answers, RunFacts, answers};
use crate::serve::inventory::Coordinator;
use crate::serve::log::Log;
use crate::serve::sealed::{OpenFile, Sealed};

/// The directory, below the root of the TFTP and HTTP trees, that GRUB's `$prefix` names: it
/// holds `grub.cfg` and, under [`MENU_DIR`], the menus.
pub const GRUB_PREFIX: &str = "grub";

/// What the flags and the files they name fix for the run.
#[derive(Clone, Debug)]
pub struct Settings {
    /// `--mode`.
    pub mode: Mode,
    /// `--address`: what the machines reach this server at.
    pub address: Ipv4Addr,
    /// `--daemon-config`, checked by the daemon's parser.
    pub daemon_config: DaemonConfigText,
    /// `--ssh-authorized-keys`.
    pub keys: AuthorizedKeys,
    /// `--install-disk`.
    pub install_disk: InstallDisk,
    /// `--serial-console`.
    pub serial_console: Option<SerialConsole>,
    /// `--reinstall`.
    pub reinstall: BTreeSet<HostId>,
    /// `--tftp-timeout-ms`: how long a TFTP transfer waits for an ACK before it sends again.
    pub tftp_timeout: Duration,
}

/// Which protocol a name is read by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Tftp,
    /// An HTTP `GET`.
    Http,
    /// An HTTP `HEAD`: it reads a name's size, and is not a seed's fetch.
    Head,
}

/// What a name reads.
#[derive(Clone, Debug)]
pub enum Content {
    /// A sealed file.
    File(Arc<OpenFile>),
    /// A generated text: `grub.cfg`, a menu, a seed.
    Text(String),
}

impl Content {
    /// Its size in bytes.
    pub fn len(&self) -> u64 {
        match self {
            Content::File(file) => file.bytes(),
            Content::Text(text) => text.len() as u64,
        }
    }

    /// The `len` bytes from `start`.
    ///
    /// # Errors
    /// The sealed file's read error.
    pub fn read(&self, start: u64, len: usize) -> std::io::Result<Vec<u8>> {
        match self {
            Content::File(file) => file.read(start, len),
            Content::Text(text) => {
                let bytes = text.as_bytes();
                let from = usize::try_from(start)
                    .unwrap_or(bytes.len())
                    .min(bytes.len());
                let to = from.saturating_add(len).min(bytes.len());
                Ok(bytes[from..to].to_vec())
            }
        }
    }
}

/// The two files of a seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeedFile {
    UserData,
    MetaData,
}

/// What this run learned: router mode's leases, the installs given, the seeds fetched.
struct RunState {
    dhcp: DhcpService,
    installs: BTreeMap<MacAddress, Install>,
    seeded: BTreeSet<MacAddress>,
}

/// One run of `serve`.
pub struct Server {
    settings: Settings,
    http: BootServer,
    sealed: Sealed,
    platforms: BTreeSet<LinuxPlatform>,
    boot: BootSets,
    tftp: FileSet,
    coordinator: Coordinator,
    log: Log,
    state: Mutex<RunState>,
}

impl Server {
    /// The run of `settings` over `sealed`, whose HTTP server the machines reach at `http`,
    /// reading `coordinator`, writing `log`; it has learned nothing yet.
    pub fn new(
        settings: Settings,
        http: BootServer,
        sealed: Sealed,
        coordinator: Coordinator,
        log: Log,
    ) -> Server {
        let platforms: BTreeSet<LinuxPlatform> = sealed.platforms().collect();
        let boot = crate::serve::platforms::boot_sets(platforms.iter().copied());
        let mut tftp = FileSet::new();
        let grub_cfg = format!("{GRUB_PREFIX}/grub.cfg");
        let mut names = vec![grub_cfg];
        for (_, set) in sealed.sets() {
            names.push(set.shim.clone());
            names.push(set.grub.clone());
        }
        for name in &names {
            // Every name here is a constant of this crate or of the packet crate's table, all
            // of which `add_file` takes.
            let _ = tftp.add_file(name);
        }
        for cpu in LinuxPlatform::ALL.map(LinuxPlatform::grub_cpu) {
            let _ = tftp.add_generated_dir(&format!("{GRUB_PREFIX}/{MENU_DIR}/{cpu}"));
        }
        let dhcp = DhcpService::new(
            settings.mode,
            hostkeeper_pxe_packets::dhcp::Server {
                address: settings.address,
                http_port: http.address().port(),
            },
        );
        Server {
            settings,
            http,
            sealed,
            platforms,
            boot,
            tftp,
            coordinator,
            log,
            state: Mutex::new(RunState {
                dhcp,
                installs: BTreeMap::new(),
                seeded: BTreeSet::new(),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, RunState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The settings of the run.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The log of the run.
    pub fn log(&self) -> &Log {
        &self.log
    }

    /// The boot sets, as the packet crate takes them.
    pub fn boot(&self) -> &BootSets {
        &self.boot
    }

    /// The names TFTP serves.
    pub fn tftp_files(&self) -> &FileSet {
        &self.tftp
    }

    /// What router mode learned for `address` in this run.
    pub fn lease(&self, address: Ipv4Addr) -> Option<Lease> {
        self.state().dhcp.lease(address)
    }

    /// The answer set, read now; the empty set, logged, when the read fails.
    pub async fn answers(&self) -> Answers {
        match self.coordinator.snapshot().await {
            Ok(snapshot) => {
                let seeded = self.state().seeded.clone();
                answers(
                    &snapshot,
                    &RunFacts {
                        mode: self.settings.mode,
                        platforms: &self.platforms,
                        reinstall: &self.settings.reinstall,
                        seeded: &seeded,
                    },
                )
            }
            Err(error) => {
                self.log.line(format_args!(
                    "the coordinator could not be read ({error}): this answer is made from the empty answer set"
                ));
                Answers::default()
            }
        }
    }

    /// The answer to the datagram `bytes` that came in on `port`, and where it goes; `None`
    /// when nothing is sent. Each packet of a network-boot client is logged with what it came
    /// to; no other packet is.
    pub async fn on_dhcp(&self, port: Port, bytes: &[u8]) -> Option<(Vec<u8>, ReplyTo)> {
        let client = Packet::parse(bytes)
            .ok()
            .filter(Packet::is_request)
            .and_then(|p| {
                p.boot_class()
                    .map(|class| (p.mac(), p.message_type(), p.arch(), class))
            });
        let answers = match (self.settings.mode, client) {
            (Mode::Proxy, Some(_)) => self.answers().await,
            _ => Answers::default(),
        };
        let outcome = self
            .state()
            .dhcp
            .on_packet(port, bytes, &answers.dhcp, &self.boot);
        let (mac, kind, arch, class) = client?;
        let arch = arch.map_or_else(|| "none".to_owned(), |arch| arch.to_string());
        let (said, reply) = match outcome {
            Outcome::Reply(reply) => (
                format!(
                    "{} sent, boot file {} ({})",
                    match reply.to {
                        ReplyTo::Broadcast => "proxy offer",
                        ReplyTo::Sender => "proxy ACK",
                    },
                    reply.boot_file,
                    reply.platform
                ),
                Some((reply.bytes, reply.to)),
            ),
            Outcome::Discover { .. } => {
                ("its architecture is kept for its REQUEST".to_owned(), None)
            }
            Outcome::Lease(lease) => (format!("it takes {}", lease.address), None),
            Outcome::Ignored(Ignored::NotInAnswerSet { mac }) => (
                format!(
                    "no answer: not in the answer set: {}",
                    answers.why_not(mac.octets())
                ),
                None,
            ),
            Outcome::Ignored(why) => (format!("no answer: {why}"), None),
        };
        self.log.line(format_args!(
            "dhcp: {kind} from {mac}, architecture {arch}, {}, on the {port} port: {said}",
            class.name()
        ));
        reply
    }

    /// What the relative name `name` reads by `protocol`, or `None` when it is not served (a
    /// refused seed included, which is logged).
    pub async fn content(&self, name: &str, protocol: Protocol) -> Option<Content> {
        if name == format!("{GRUB_PREFIX}/grub.cfg") {
            return Some(Content::Text(GRUB_CFG.to_owned()));
        }
        if let Some(request) = menu_request(name) {
            return Some(Content::Text(self.menu(name, request).await));
        }
        if protocol != Protocol::Tftp
            && let Some((mac, file)) = seed_request(name)
        {
            return self
                .seed(name, mac, file, protocol == Protocol::Head)
                .await
                .map(Content::Text);
        }
        let tftp_served = self
            .sealed
            .sets()
            .any(|(_, set)| set.shim == name || set.grub == name);
        self.sealed
            .file(name)
            .filter(|_| protocol != Protocol::Tftp || tftp_served)
            .map(Content::File)
    }

    /// The sealed shim `hostkeeper/boot.efi` stands for.
    pub fn shim(&self, name: &str) -> Option<Content> {
        self.sealed.file(name).map(Content::File)
    }

    /// The menu GRUB asked for at `name`, chosen from the answer set read now.
    async fn menu(&self, name: &str, request: MenuRequest<'_>) -> String {
        let answers = self.answers().await;
        let choice = request.choose(
            crate::serve::platforms::menu_mode(self.settings.mode),
            &|mac| answers.menus.get(&mac),
        );
        match choice {
            MenuChoice::Install(install) => match self.sealed.set(install.platform()) {
                Some(set) => {
                    let menu = install.menu(&self.http, &set.iso);
                    self.log.line(format_args!(
                        "menu {name}: install {} as host {}",
                        install.platform(),
                        install.record().host_id
                    ));
                    self.state().installs.insert(install.mac(), install);
                    menu
                }
                None => {
                    self.log.line(format_args!(
                        "menu {name}: local disk: {} has no boot set here",
                        install.platform()
                    ));
                    LOCAL_DISK_MENU.to_owned()
                }
            },
            MenuChoice::LocalDisk(reason) => {
                let detail = match &reason {
                    hostkeeper_pxe_seed::menu::LocalDiskReason::NotInAnswerSet { mac } => {
                        format!(": {}", answers.why_not(mac.octets()))
                    }
                    _ => String::new(),
                };
                self.log
                    .line(format_args!("menu {name}: local disk: {reason}{detail}"));
                LOCAL_DISK_MENU.to_owned()
            }
        }
    }

    /// The seed file `file` of `mac`, or `None` when it is refused. A `probe` (an HTTP `HEAD`)
    /// is answered as a fetch is, but does not count as the seed's fetch.
    async fn seed(
        &self,
        name: &str,
        mac: MacAddress,
        file: SeedFile,
        probe: bool,
    ) -> Option<String> {
        let (install, seeded) = {
            let state = self.state();
            (
                state.installs.get(&mac).cloned(),
                state.seeded.contains(&mac),
            )
        };
        let Some(install) = install else {
            self.log.line(format_args!(
                "seed {name}: refused: {mac} was given no install menu in this run"
            ));
            return None;
        };
        if !seeded {
            let answers = self.answers().await;
            let same = answers
                .menus
                .get(&mac)
                .is_some_and(|record| record.host_id == install.record().host_id);
            // A fetch of the same seed that came in meanwhile may have taken the MAC out of the
            // set already: that is the same install, not a refusal.
            let refused = !same & !self.state().seeded.contains(&mac);
            if refused {
                self.log.line(format_args!(
                    "seed {name}: refused: {mac} is no longer in the answer set as host {}: {}",
                    install.record().host_id,
                    answers.why_not(mac.octets())
                ));
                return None;
            }
            let first = match probe {
                true => false,
                false => self.state().seeded.insert(mac),
            };
            if first {
                self.log.line(format_args!(
                    "seed {name}: fetched by {mac}, host {}: its install started, and it leaves the answer set for the rest of the run",
                    install.record().host_id
                ));
            }
        }
        let made = seed(&SeedInputs {
            install: &install,
            server: self.http,
            daemon_config: &self.settings.daemon_config,
            keys: &self.settings.keys,
            install_disk: &self.settings.install_disk,
            serial_console: self.settings.serial_console.as_ref(),
        });
        Some(match file {
            SeedFile::UserData => made.user_data,
            SeedFile::MetaData => made.meta_data,
        })
    }
}

/// The menu request of `name` when it is `grub/hostkeeper/<cpu>/<mac>`, one component each.
fn menu_request(name: &str) -> Option<MenuRequest<'_>> {
    name.strip_prefix(&format!("{GRUB_PREFIX}/{MENU_DIR}/"))
        .filter(|rest| rest.split('/').count() == 2)
        .and_then(|_| MenuRequest::parse(name))
}

/// The seed file `name` names: `seed/<mac>/user-data` or `seed/<mac>/meta-data`.
fn seed_request(name: &str) -> Option<(MacAddress, SeedFile)> {
    let rest = name.strip_prefix("seed/")?;
    let (mac, file) = rest.split_once('/')?;
    let file = match file {
        "user-data" => SeedFile::UserData,
        "meta-data" => SeedFile::MetaData,
        _ => return None,
    };
    MacAddress::parse(mac).ok().map(|mac| (mac, file))
}
