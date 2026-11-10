//! The rig of the network-install tests (`tests/net_install.rs`, `tests/net_install_arm64.rs`,
//! docs/design/network-install.md section 11): a farm LAN with no route out ([`lan`]), the
//! router's DHCP faked by the test ([`router`]), the real `pxe` with a boot set its own
//! `update-hook prepare` sealed ([`pxe`]), the real coordinator with `--lan-listen` on the LAN
//! (`support::pair`), a release channel on the LAN (`support::channel`) that also serves the
//! signed apt repository of the fixture service, and a QEMU guest
//! ([`guest`]). Every wait is on a progress event ([`events`]): a line of the guest's serial
//! console, of `pxe`'s log or of the router's DHCP, or an answer of the operator API;
//! each ends on its event or on the end of the guest, and the job's `timeout-minutes` is the
//! only bound.
//!
//! What the tests keep, passed or failed, is under `target/privileged-artifacts/<test>/`: the
//! guest's console, the logs of `pxe`, of the router's DHCP and of the coordinator,
//! and what a test keeps with [`Rig::keep`].
//! [`router::Router`] and [`events::Lines`] need no root; `tests/net_install_rig.rs` runs them in
//! the pull-request set.

#![allow(
    dead_code,
    reason = "each test file uses part of the network-install rig"
)]

pub mod events;
pub mod guest;
pub mod heartbeat;
pub mod lan;
pub mod program;
pub mod pxe;
pub mod repositories;
pub mod router;

use std::fs;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use socket2::{Domain, Protocol, Socket, Type};

use crate::e2e::aptrepo::{self, AptRepo, Deb, File, Service};
use crate::e2e::signer::FixtureSigner;
use crate::support::channel::Channel;
use crate::support::operator_api::must;
use crate::support::pair::{BEACON, Coordinator, ROLE, TestCa, put};
use crate::support::{Bench, FIXTURE};
use events::{Bus, LineSink, Mark, Source};
use guest::{Guest, Machine, Ssh};
use hostkeeper_proto::hostkeeper::release::v1::{StepBounds, Target};
use lan::{LAN_LINK, Lan};
use program::Program;
use pxe::{Mode, ServeInputs};
use router::{Boot, Router, RouterDhcp, SERVER_PORT, Segment};

/// The fixture service the guest is placed by its group, the group its record names by its role.
pub const SERVICE: &str = "hostkeeper-ci-fixture";
/// The version of the fixture service the guest is ordered.
pub const VERSION: &str = "1.0.0";
/// The last byte of the guest's address: the first of the router's pool.
const GUEST: u8 = 100;
/// The file the router names in router mode.
const BOOT_EFI: &str = "hostkeeper/boot.efi";

/// One network-install test's world. The fields are dropped in their order: the programs
/// first, the LAN and the scratch directory last.
pub struct Rig {
    pub guest: Guest,
    pxe: Option<Program>,
    router: Option<RouterDhcp>,
    pub coordinator: Coordinator,
    pub channel: Channel,
    /// The apt repository of the fixture service, which the channel serves to the guest.
    pub repo: AptRepo,
    pub bus: Bus,
    pub ssh: Ssh,
    pub host_id: &'static str,
    pub signer: FixtureSigner,
    pub channel_ca: String,
    /// The files the seed gives the installed host's repository directory.
    pub repository_files: Vec<(String, Vec<u8>)>,
    sink: Arc<LineSink>,
    inputs: ServeInputs,
    artifacts: PathBuf,
    pub lan: Lan,
    bench: Bench,
}

impl Rig {
    /// The world of the test `test`, on the LAN `<net>.0/24`, for `machine` recorded as
    /// `host_id`: the coordinator, the channel, the apt repository (empty, with the host's entry of
    /// it given to the seed), the boot set of the machine's platform sealed, and the inventory
    /// record of the machine (its data MAC, serial and platform). Nothing serves network boot yet,
    /// and the guest is off.
    pub fn new(
        test: &str,
        tag: &str,
        net: [u8; 3],
        machine: Machine,
        host_id: &'static str,
    ) -> Rig {
        let bench = Bench::new(test);
        let scratch = bench.scratch().to_path_buf();
        let artifacts = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("../privileged-artifacts")
            .join(test);
        fs::create_dir_all(&artifacts).expect("the artifacts directory");
        let lan = Lan::new(tag, net);
        let bus = Bus::default();
        let sink = Arc::new(LineSink::new(&bus, &artifacts.join("router-dhcp.log")));

        let coordinator = Coordinator::start_on_lan(&scratch, &[], lan.host());
        // The group the guest's record names by its role lists the fixture service, which comes
        // from the registry's repository `komira`: the default repository of a package.
        coordinator.put_group(ROLE, &[BEACON, SERVICE]);
        let ca = TestCa::new("release channel CA");
        let channel = Channel::start_at(&ca, lan.host());
        let signer = FixtureSigner::new();

        // The config every installed host gets: the coordinator's LAN listener and its CA, the
        // channel the loader requires, and the signer of the release `prepare` checks.
        let daemon_config = scratch.join("hostkeeper.toml");
        put(
            &daemon_config,
            format!(
                "coordinator = \"{}\"\nca_sha256 = \"{}\"\nrelease_channel = \"{}\"\n\n{}",
                coordinator.lan_url(),
                coordinator.ca_sha256(),
                channel.url(),
                signer.trusted_signers_toml()
            )
            .as_bytes(),
            0o600,
        );
        let platform = machine.arch.platform();
        let ssh = Ssh::new(&scratch.join("ssh"), lan.address(GUEST));
        // The repository entry every installed host gets: the apt repository of the fixture
        // service on the channel, signed by a key of the test's, whose keyring the entry names.
        let repo = AptRepo::new(&scratch, "komira-testing");
        let keyring = scratch.join("repository-keyring.gpg");
        repo.export_keyring(&keyring);
        let keyring_bytes = fs::read(&keyring).expect("the repository's keyring");
        let entry = repo.sources_text(
            &channel.url(),
            &Path::new(repositories::HOST_DIR).join(repositories::KEYRING),
        );
        let repository_files = repositories::files(&entry, &keyring_bytes);
        // The registry holds the repository as the channel serves it: the coordinator refuses a
        // set of `apt` entries for a repository it does not hold, and pushes it to the host,
        // whose directory (the files above) is what the registry renders, so the daemon adopts it.
        let registered = repo.registry_entry_at(repositories::NAME, &channel.url());
        assert_eq!(registered.keyring, keyring_bytes, "the registry's keyring");
        must(
            "repositories put",
            coordinator.api().put_repository(registered),
        );
        let repositories = scratch.join("repositories");
        repositories::write(&repositories, &repository_files);
        let serial_console = match machine.arch {
            guest::Arch::X86_64 => "ttyS0",
            guest::Arch::Arm64 => "ttyAMA0",
        };
        // The boot set, fetched and sealed by `pxe update-hook prepare` against the settings
        // `serve` runs with (in proxy mode: `prepare` checks the settings, not the mode).
        let sealed = pxe::prepare(
            &scratch,
            platform,
            &channel,
            &ca.pem(),
            &signer,
            &pxe::ServeSettings {
                mode: Mode::Proxy,
                address: lan.pxe(),
                coordinator: &coordinator,
                daemon_config: &daemon_config,
                ssh_keys: &ssh.public,
                serial_console: Some(serial_console),
                repositories: Some(&repositories),
                loopback: false,
            },
        );

        let guest = Guest::new(machine, &scratch.join("guest"), &lan.bridge_ns, &artifacts);
        let rig = Rig {
            pxe: None,
            router: None,
            coordinator,
            channel,
            repo,
            bus,
            host_id,
            channel_ca: ca.pem(),
            repository_files,
            signer,
            sink,
            inputs: ServeInputs {
                sealed,
                daemon_config,
                repositories,
                ssh_keys: ssh.public.clone(),
                serial_console,
                log_dir: artifacts.clone(),
            },
            ssh,
            guest,
            artifacts,
            lan,
            bench,
        };
        let machine = rig.guest.machine();
        must(
            "enrollment add-record",
            rig.coordinator.api().add_record(
                host_id,
                ROLE,
                machine.mac,
                machine.serial,
                machine.arch.proto_platform(),
                "the network-install test's guest",
            ),
        );
        rig
    }

    /// The test's scratch directory.
    pub fn scratch(&self) -> &Path {
        self.bench.scratch()
    }

    /// Keeps `bytes` as `name` beside the logs, under `target/privileged-artifacts/<test>/`, mode
    /// 0644 so that the job's later steps, which do not run as root, read it.
    pub fn keep(&self, name: &str, bytes: &[u8]) {
        put(&self.artifacts.join(name), bytes, 0o644);
    }

    /// The guest's address, the first of the router's pool.
    pub fn guest_address(&self) -> Ipv4Addr {
        self.lan.address(GUEST)
    }

    /// The router's DHCP and `pxe` in `mode`, each started afresh: in proxy mode the
    /// router names no boot file, in router mode it names `pxe` and
    /// `hostkeeper/boot.efi`; `serve` gets `--reinstall` for each of `reinstall`.
    pub fn serve(&mut self, mode: Mode, reinstall: &[&str]) {
        if let Some(pxe) = self.pxe.take() {
            pxe.stop();
        }
        self.router = None;
        let boot = (mode == Mode::Router).then(|| Boot {
            server: self.lan.pxe(),
            file: BOOT_EFI.to_owned(),
        });
        let segment = Segment {
            router: self.lan.router(),
            prefix: 24,
            first: self.lan.address(GUEST),
            size: 50,
        };
        let socket = Lan::within(&self.lan.router_ns, || {
            let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
            socket.set_reuse_address(true)?;
            socket.bind_device(Some(LAN_LINK.as_bytes()))?;
            socket.set_broadcast(true)?;
            socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, SERVER_PORT).into())?;
            std::io::Result::<UdpSocket>::Ok(socket.into())
        })
        .expect("the router's DHCP socket");
        self.sink.line(
            Source::Router,
            format!(
                "started: {}",
                match &boot {
                    Some(boot) =>
                        format!("router mode, boot file {} on {}", boot.file, boot.server),
                    None => "proxy mode, no boot file".to_owned(),
                }
            ),
        );
        self.router = Some(RouterDhcp::start(
            socket,
            Router::new(segment, boot),
            Arc::clone(&self.sink),
        ));
        self.pxe = Some(pxe::serve(
            &mut self.bus,
            &self.lan,
            &self.coordinator,
            &self.inputs,
            mode,
            reinstall,
        ));
    }

    /// The MAC as `pxe` logs it.
    pub fn mac(&self) -> &'static str {
        self.guest.machine().mac
    }

    /// Waits for a line of `pxe` after `from` that holds every one of `parts`.
    pub fn pxe_says(&mut self, from: Mark, what: &str, parts: &[&str]) -> (Mark, String) {
        let parts: Vec<String> = parts.iter().map(|p| (*p).to_owned()).collect();
        self.bus.wait(from, Source::Pxe, what, move |line| {
            parts.iter().all(|part| line.contains(part.as_str()))
        })
    }

    /// Waits for the login prompt of the installed system on the guest's console after `from`:
    /// its host name is the record's host id.
    pub fn login_prompt(&mut self, from: Mark) -> Mark {
        let prompt = format!("{} login:", self.host_id);
        self.bus
            .wait(
                from,
                Source::Guest,
                "the installed system's login prompt",
                move |line| line.ends_with(&prompt),
            )
            .0
    }

    /// Waits for the network boot of the guest after `from` to end in its install: in proxy
    /// mode the proxy offer; in router mode its REQUEST learned and `hostkeeper/boot.efi` served
    /// as the platform's shim; in either mode the install menu of its MAC and its seed fetched.
    /// Gives the mark after the seed's fetch.
    pub fn wait_install(&mut self, from: Mark, mode: Mode) -> Mark {
        let mac = self.mac();
        let cpu = self.guest.machine().arch.grub_cpu();
        let platform = self.guest.machine().arch.platform();
        let host = self.host_id;
        let from = match mode {
            Mode::Proxy => {
                let shim = self.guest.machine().arch.shim();
                self.pxe_says(
                    from,
                    "the proxy offer to the guest's DISCOVER",
                    &[
                        &format!("dhcp: DHCPDISCOVER from {mac}, "),
                        &format!("proxy offer sent, boot file {shim} ({platform})"),
                    ],
                )
                .0
            }
            Mode::Router => {
                let (at, _) = self.pxe_says(
                    from,
                    "the guest's REQUEST, learned",
                    &[
                        &format!("dhcp: DHCPREQUEST from {mac}, "),
                        &format!("it takes {}", self.guest_address()),
                    ],
                );
                let shim = self.guest.machine().arch.shim();
                self.pxe_says(
                    at,
                    "hostkeeper/boot.efi served as the platform's shim",
                    &[&format!(
                        "hostkeeper/boot.efi is {shim} for {mac} ({platform})"
                    )],
                )
                .0
            }
        };
        let (from, _) = self.pxe_says(
            from,
            "the install menu of the guest's MAC",
            &[
                &format!("menu grub/hostkeeper/{cpu}/{mac}: "),
                &format!("install {platform} as host {host}"),
            ],
        );
        self.pxe_says(
            from,
            "the guest's seed fetched",
            &[&format!(
                "fetched by {mac}, host {host}: its install started"
            )],
        )
        .0
    }

    /// The pending request that matches the guest's record and claims the guest's UUID and
    /// serial, once there is one other than those of `seen` (their key hashes).
    pub fn wait_request(&mut self, seen: &[String]) -> Value {
        let host = self.host_id;
        let (uuid, serial) = (self.guest.machine().uuid, self.guest.machine().serial);
        let coordinator = &self.coordinator;
        self.bus
            .until("a pending request matched to the record", || {
                let pending = coordinator.pending()?;
                pending
                    .iter()
                    .find(|r| {
                        r["recordHostId"] == host
                            && !seen.iter().any(|s| r["keySha256"] == s.as_str())
                    })
                    .cloned()
                    .map(|request| {
                        assert_eq!(
                            request["inventoryMatch"], "INVENTORY_MATCH_MATCHED",
                            "{request}"
                        );
                        assert_eq!(
                            request["claims"]["productUuid"]
                                .as_str()
                                .map(str::to_ascii_lowercase)
                                .as_deref(),
                            Some(uuid),
                            "the guest's UUID: {request}"
                        );
                        assert_eq!(request["claims"]["dmiSerial"], serial, "{request}");
                        request
                    })
                    .ok_or_else(|| format!("{pending:?}"))
            })
    }

    /// Waits for the guest's console to show the key hash `key` of its request after `from`.
    pub fn console_shows(&mut self, from: Mark, key: &str) -> Mark {
        let line = format!("key sha256:  {key}");
        self.bus
            .wait(
                from,
                Source::Guest,
                "the request's key hash on the guest's console",
                move |l| l == line,
            )
            .0
    }

    /// Gives the installed guest what the fixture service's install needs of a host: the channel's
    /// CA in its CA bundle, which apt's `https` reads when the daemon's tools fetch the apt
    /// repository the channel serves, by the operator account over SSH.
    pub fn trust_channel(&mut self) {
        let ssh = &self.ssh;
        let ca = self.channel_ca.clone();
        self.bus.until("the guest's SSH server", || {
            ssh.output("true", b"").map(|_| ())
        });
        ssh.output(
            "sudo tee -a /etc/ssl/certs/ca-certificates.crt",
            ca.as_bytes(),
        )
        .unwrap_or_else(|e| panic!("on the guest: {e}"));
    }

    /// The guest's `/etc/machine-id`: which installed system is running.
    pub fn machine_id(&mut self) -> String {
        let ssh = &self.ssh;
        let id = self.bus.until("the guest's machine id over SSH", || {
            ssh.output("cat /etc/machine-id", b"")
        });
        String::from_utf8_lossy(&id).trim().to_owned()
    }

    /// The guest's record, as `Enrollment.ListRecords` gives it.
    pub fn record(&self) -> Value {
        let records = must(
            "enrollment list-records",
            self.coordinator.api().list_records(),
        );
        records["records"]
            .as_array()
            .and_then(|all| all.iter().find(|r| r["hostId"] == self.host_id).cloned())
            .unwrap_or_else(|| panic!("no record of {}: {records}", self.host_id))
    }

    /// The host as `Hosts.Get` gives it, or why not.
    pub fn host(&self) -> Result<Value, String> {
        self.coordinator
            .api()
            .get_host(self.host_id)
            .map(|got| got["host"].clone())
            .map_err(|e| e.to_string())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let dir = self.bench.scratch().join("coordinator");
        for file in ["coordinator.out", "coordinator.err"] {
            let _ = fs::copy(dir.join(file), self.artifacts.join(file));
        }
    }
}

/// The unit the fixture package runs: the fixture's `serve`, as its main process.
const SERVICE_UNIT: &str = "[Unit]\nDescription=the network-install test's fixture service\n\n\
     [Service]\nType=exec\nExecStart=/usr/bin/hostkeeper-ci-fixture serve\n\n[Install]\n\
     WantedBy=multi-user.target\n";

/// Publishes the fixture package in `repo` and gives the entry that orders it: its program the
/// fixture of this build (also its hook), its file under `/etc/hostkeeper-ci-fixture`, its unit in
/// `/usr/lib/systemd/system`, for the host's architecture (the machine's of the x86-64 test).
/// Serve the repository's files ([`Channel::serve_tree`]) after it.
pub fn fixture(repo: &AptRepo) -> Target {
    let program = fs::read(FIXTURE).expect("the fixture program");
    let published = repo.publish(&Deb {
        package: SERVICE.to_owned(),
        version: VERSION.to_owned(),
        files: vec![
            File::new("usr/bin/hostkeeper-ci-fixture", program.clone(), 0o755),
            File::new(
                &format!("usr/lib/{SERVICE}/hostkeeper-hook"),
                program,
                0o755,
            ),
            File::new(
                "usr/lib/systemd/system/hostkeeper-ci-fixture.service",
                SERVICE_UNIT,
                0o644,
            ),
            File::new(
                "etc/hostkeeper-ci-fixture/fixture.conf",
                format!("fixture {VERSION}\n"),
                0o644,
            ),
        ],
    });
    let service = Service {
        repository: repositories::ENTRY.to_owned(),
        run: vec![format!("{SERVICE}.service")],
        health_url: None,
        params: [("fixture.mode".to_owned(), aptrepo::text("ok"))]
            .into_iter()
            .collect(),
        hook_command: vec![format!("/usr/lib/{SERVICE}/hostkeeper-hook")],
        steps: StepBounds {
            prepare: 60,
            cutover: 60,
            verify: 60,
            rollback: 60,
            cleanup: 60,
            uninstall: Some(60),
        },
        verify_deadline_secs: 300,
    };
    aptrepo::entry(&service, &published)
}
