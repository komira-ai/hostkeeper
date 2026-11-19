//! What the console's tests share: a coordinator over its own enrollment and an ops store on the
//! fake, with a Linux server and a Mac enrolled against their records, the console's three files,
//! the scripted BMC tool, a neighbour table the test fills, and session ids drawn from a counter.

use std::collections::{BTreeMap, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;

use hostkeeper_console::ipmi::Settings;
use hostkeeper_console::scripted::Scripted;
use hostkeeper_console::session::{Bounds, Sessions};
use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_testkit::Scratch;

use super::Console;
use super::files::{ConsoleFiles, Paths};
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enroll_request};
use crate::marks_support::{NEW_DAEMON, ask, beat_of, operator, start, who};
use crate::operator::Caller;
use crate::ops::{AuditEntry, OpsState, OpsStore};
use crate::store::Fake;
use crate::testutil::Rig;

/// The Linux server `node-02`: its data MAC, its BMC's MAC and the BMC's address.
pub(crate) const NODE: &str = "node-02";
pub(crate) const NODE_DATA: &str = "0c:c4:7a:00:00:02";
pub(crate) const NODE_BMC: &str = "0c:c4:7a:00:00:01";
pub(crate) const NODE_BMC_ADDRESS: &str = "192.0.2.12";
/// What the BMC of `node-02` answers to the MAC read.
pub(crate) const NODE_BMC_ANSWER: &str = " 11 0c c4 7a 00 00 01\n";

/// The Mac `mac-01`: its serial, data MAC and address.
pub(crate) const MAC: &str = "mac-01";
pub(crate) const MAC_SERIAL: &str = "C02X1";
pub(crate) const MAC_DATA: &str = "a8:5e:45:00:00:01";
pub(crate) const MAC_ADDRESS: &str = "mac-01.lab.example";

/// The operators file: `alice` may change power, `bob` may not.
pub(crate) const OPERATORS: &str =
    "[[operator]]\nname = \"alice\"\n\n[[operator]]\nname = \"bob\"\npower = false\n";

/// The BMC password of the credential file.
pub(crate) const PASSWORD: &str = "a long random pass";

/// A coordinator with a console, and what its tests look at.
pub(crate) struct Fixture {
    pub(crate) rig: Rig,
    pub(crate) enrollment: EnrollmentRig,
    pub(crate) fake: Fake,
    pub(crate) tool: Scripted,
    pub(crate) files: Paths,
    pub(crate) dials: Dials,
    pub(crate) neighbors: NeighborTable,
}

/// The neighbour table double: the address of each MAC a test put in it.
#[derive(Clone, Default)]
pub(crate) struct NeighborTable(Arc<Mutex<BTreeMap<MacAddress, IpAddr>>>);

impl NeighborTable {
    /// The table holds `ip` for `mac`.
    pub(crate) fn put(&self, mac: &str, ip: &str) {
        self.0.lock().expect("lock").insert(
            MacAddress::parse(mac).expect("a MAC"),
            ip.parse().expect("an address"),
        );
    }

    /// The lookup of the console.
    pub(crate) fn lookup(&self) -> super::Neighbors {
        let table = self.clone();
        Arc::new(move |mac| table.0.lock().expect("lock").get(mac).copied())
    }
}

/// The dial double: each dial of a Mac takes the next scripted outcome, and is recorded; with
/// none scripted it is refused as a closed port.
#[derive(Clone, Default)]
pub(crate) struct Dials {
    outcomes: Arc<Mutex<VecDeque<std::io::Result<tokio::io::DuplexStream>>>>,
    dialled: Arc<Mutex<Vec<String>>>,
}

impl Dials {
    /// The next dial connects: the Mac's end of the connection is answered.
    pub(crate) fn connect(&self) -> tokio::io::DuplexStream {
        let (coordinator, mac) = tokio::io::duplex(1 << 16);
        self.outcomes
            .lock()
            .expect("lock")
            .push_back(Ok(coordinator));
        mac
    }

    /// The next dial fails as `kind`.
    pub(crate) fn fail(&self, kind: std::io::ErrorKind) {
        self.outcomes
            .lock()
            .expect("lock")
            .push_back(Err(kind.into()));
    }

    /// The addresses dialled, in order.
    pub(crate) fn dialled(&self) -> Vec<String> {
        self.dialled.lock().expect("lock").clone()
    }

    /// The dial of the console.
    pub(crate) fn dial(&self) -> super::Dial {
        let dials = self.clone();
        Arc::new(move |address| {
            dials
                .dialled
                .lock()
                .expect("lock")
                .push(address.to_string());
            let next = dials.outcomes.lock().expect("lock").pop_front();
            async move {
                let stream =
                    next.unwrap_or_else(|| Err(std::io::ErrorKind::ConnectionRefused.into()))?;
                Ok(Box::new(stream) as Box<dyn super::Duplex>)
            }
            .boxed()
        })
    }
}

/// The caller `name`.
pub(crate) fn caller(name: &str) -> Caller {
    Caller::Operator(name.parse().expect("a token name"))
}

impl Fixture {
    /// The coordinator `name` with the operators, targets and credential files written in its
    /// directory, `node-02` and `mac-01` recorded and enrolled, a window of `window`, at most
    /// `max` sessions, and the tool scripted by the test.
    pub(crate) fn with(name: &str, max: usize, window: Duration) -> Self {
        let enrollment = EnrollmentRig::new(&format!("{name}-enrollment"));
        let mut rig = Rig::new(name);
        let fake = Fake::new();
        rig.coordinator = start(&rig.dir, &rig.clock, "")
            .with_ops(OpsStore::on_fake(&fake, OpsState::default()))
            .with_enrollment(Arc::clone(&enrollment.enrollment));
        rig.at(2_000);
        let files = write_files(&rig.dir, OPERATORS);
        let tool = Scripted::new();
        let dials = Dials::default();
        let neighbors = NeighborTable::default();
        let console = console(&files, &tool, max, window)
            .with_dial(dials.dial())
            .with_neighbors(neighbors.lookup());
        let fixture = Self {
            rig,
            enrollment,
            fake,
            tool,
            files,
            dials,
            neighbors,
        };
        fixture.record(NODE, &[NODE_DATA], NODE_BMC, "");
        fixture.record(MAC, &[MAC_DATA], "", MAC_SERIAL);
        fixture.enroll("m-node", &[NODE_DATA], NODE_BMC, "", NODE);
        fixture.enroll("m-mac", &[], "", MAC_SERIAL, MAC);
        let mut fixture = fixture;
        let coordinator = std::mem::replace(
            &mut fixture.rig.coordinator,
            start(&fixture.rig.dir, &fixture.rig.clock, ""),
        );
        fixture.rig.coordinator = coordinator.with_console(console);
        fixture
    }

    /// As [`Self::with`], with the default bounds.
    pub(crate) fn new(name: &str) -> Self {
        Self::with(name, 8, Duration::from_secs(30))
    }

    /// As [`Self::new`], with no targets file: every BMC's address is found.
    pub(crate) fn without_targets(name: &str) -> Self {
        let mut fixture = Self::new(name);
        fixture.files.targets = None;
        let console = console(&fixture.files, &fixture.tool, 8, Duration::from_secs(30))
            .with_dial(fixture.dials.dial())
            .with_neighbors(fixture.neighbors.lookup());
        let coordinator = std::mem::replace(
            &mut fixture.rig.coordinator,
            start(&fixture.rig.dir, &fixture.rig.clock, ""),
        );
        fixture.rig.coordinator = coordinator.with_console(console);
        fixture
    }

    /// A heartbeat of `host` over gRPC, answered now, that reports its BMC at `ip` with `mac`.
    pub(crate) fn report_bmc(&self, host: &str, ip: &str, mac: &str) {
        ask(&self.rig, host, "storage", beat_of(NEW_DAEMON, Vec::new()));
        let request = agent::HeartbeatRequest {
            bmc: Some(agent::BmcReport {
                ip: ip.to_owned(),
                mac: mac.to_owned(),
            }),
            ..agent::HeartbeatRequest::default()
        };
        self.rig
            .coordinator
            .hear_bmc(&who(host, "storage"), &request);
    }

    /// Records `host` with `data`, `bmc` and `serial`.
    pub(crate) fn record(&self, host: &str, data: &[&str], bmc: &str, serial: &str) {
        self.rig
            .coordinator
            .add_record(
                &operator(),
                proto::AddRecordRequest {
                    host_id: host.to_owned(),
                    data_macs: data.iter().map(|m| (*m).to_owned()).collect(),
                    bmc_mac: bmc.to_owned(),
                    serial: serial.to_owned(),
                    ..proto::AddRecordRequest::default()
                },
            )
            .expect("recorded");
    }

    /// Enrolls the machine `machine` claiming `addresses`, the BMC MAC `bmc` and the Mac serial
    /// `serial`, approved as `host`, then heard from.
    pub(crate) fn enroll(
        &self,
        machine: &str,
        addresses: &[&str],
        bmc: &str,
        serial: &str,
        host: &str,
    ) {
        let key = HostKey::new();
        let mut request = enroll_request(&key, machine);
        let claims = request.claims.as_mut().expect("claims");
        claims.addresses = addresses.iter().map(|a| (*a).to_owned()).collect();
        claims.bmc_mac = bmc.to_owned();
        claims.mac_serial = serial.to_owned();
        self.enrollment.enrollment.enroll(request).expect("posted");
        let approved = self
            .rig
            .coordinator
            .enrollment_approve(
                &operator(),
                proto::ApproveRequest {
                    request_id: key.key_hash().to_hex(),
                    key_sha256: key.key_hash().to_hex(),
                    host_id: host.to_owned(),
                    role: "storage".to_owned(),
                    reason: "built today".to_owned(),
                    ..proto::ApproveRequest::default()
                },
            )
            .expect("approved");
        ask(
            &self.rig,
            &approved.host_id,
            "storage",
            beat_of(NEW_DAEMON, Vec::new()),
        );
    }

    /// The console's audit lines, oldest first.
    pub(crate) fn console_lines(&self) -> Vec<AuditEntry> {
        let all = self.rig.coordinator.ops().audit_log().expect("the log");
        all.into_iter()
            .filter(|entry| crate::audit_actions::AuditOnly::is_audit_only(&entry.action))
            .collect()
    }

    /// The console's audit lines as `(by, action, node, reason)`, oldest first.
    pub(crate) fn lines(&self) -> Vec<(String, String, String, String)> {
        self.console_lines()
            .into_iter()
            .map(|entry| {
                (
                    entry.by,
                    entry.action,
                    entry.node_id.unwrap_or_default(),
                    entry.reason.unwrap_or_default(),
                )
            })
            .collect()
    }
}

/// The answer's account of `address`, found in the targets file.
pub(crate) fn file_address(address: &str) -> proto::ConsoleAddress {
    proto::ConsoleAddress {
        address: address.to_owned(),
        source: proto::ConsoleAddressSource::TargetsFile.into(),
        stale: false,
        reported_unix: 0,
    }
}

/// One expected line of [`Fixture::lines`].
pub(crate) fn line(
    by: &str,
    action: &str,
    node: &str,
    reason: &str,
) -> (String, String, String, String) {
    (
        by.to_owned(),
        action.to_owned(),
        node.to_owned(),
        reason.to_owned(),
    )
}

/// The three files in `dir`, the operators file holding `operators`.
pub(crate) fn write_files(dir: &Scratch, operators: &str) -> Paths {
    use std::os::unix::fs::PermissionsExt;
    let put = |name: &str, text: &str, mode: u32| {
        let path = dir.path().join(name);
        std::fs::write(&path, text).expect("written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("its mode");
        path
    };
    let targets = format!(
        "[[target]]\nmac = \"{NODE_BMC}\"\naddress = \"{NODE_BMC_ADDRESS}\"\n\n[[target]]\nmac = \"{MAC_DATA}\"\naddress = \"{MAC_ADDRESS}\"\n"
    );
    Paths {
        operators: put("console-operators.toml", operators, 0o644),
        targets: Some(put("console-targets.toml", &targets, 0o644)),
        credential: put(
            "bmc.toml",
            &format!("user = \"ADMIN\"\npassword = \"{PASSWORD}\"\n"),
            0o600,
        ),
    }
}

/// A console over `files` and `tool`, with session ids from a counter.
pub(crate) fn console(files: &Paths, tool: &Scripted, max: usize, window: Duration) -> Console {
    let counter = AtomicU8::new(0);
    Console::new(
        ConsoleFiles::read(files.clone()).expect("the files"),
        Arc::new(tool.clone()),
        Settings::default(),
        Sessions::new(Bounds {
            max,
            window,
            idle: super::DEFAULT_IDLE,
            total: super::DEFAULT_TOTAL,
        }),
    )
    .with_random(Box::new(move |buffer| {
        let next = counter.fetch_add(1, Ordering::SeqCst) + 1;
        buffer.fill(next);
        true
    }))
}

/// The id of the `n`th session the counter draws.
pub(crate) fn session_id(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}
