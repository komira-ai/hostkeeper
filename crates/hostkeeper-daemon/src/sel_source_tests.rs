//! The tests of `sel_source`: a [`SelReader`] over a fake BMC ([`FakeBmc`]) that answers the
//! requests of the table from the SEL and SDR fixtures of `tests/fixtures/sel` and
//! `tests/fixtures/sdr` (entries written byte for byte from the IPMI 2.0 tables, with generic
//! sensor names), and a real [`HealthKeeper`] over a scratch state directory: no device, no BMC,
//! no real clock; every time is a number the test gives. This file holds the fake and the rig;
//! the records each entry gives are tested in `sel_normalize_tests.rs`, and how the reader reads
//! (what it asks, its marks, its failures, its bounds, its thread) in `sel_reads_tests.rs`.

#[path = "sel_normalize_tests.rs"]
mod normalize;
#[path = "sel_reads_tests.rs"]
mod reads;

use std::collections::BTreeMap;
use std::sync::Mutex;

use hostkeeper_link::HostHealthSource;
use hostkeeper_link::testing::{RecordingLog, ScriptedSleeper};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::health_state::{self, Read};
use crate::host_health::BootFacts;
use crate::shutdown_probe::{GoingDown, ShutdownProbe};

const INCIDENT_SEL: &str = include_str!("../tests/fixtures/sel/incident.sel");
const KINDS_SEL: &str = include_str!("../tests/fixtures/sel/kinds.sel");
const NAMES_SDR: &str = include_str!("../tests/fixtures/sdr/names.sdr");

/// The incident's SEL was last erased at this second.
const INCIDENT_ERASED: u32 = 1_790_000_000;
/// The incident host's boot after the fault.
const INCIDENT_BOOT: u64 = 1_795_662_100;
/// When the incident host's SEL is read, after that boot.
const INCIDENT_READ: u64 = 1_795_662_540;
/// The kinds fixture's SEL was last erased at this second.
const KINDS_ERASED: u32 = 1_795_000_000;
/// The first second of the kinds fixture's entries, and the boot it is read in (15 seconds in).
const KINDS_T: u64 = 1_795_600_800;
const KINDS_BOOT: u64 = KINDS_T + 15;

/// The records of a fixture: one per line of hex bytes, `#` lines are comments.
fn fixture(text: &str) -> Vec<Vec<u8>> {
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            line.split_whitespace()
                .map(|byte| u8::from_str_radix(byte, 16).expect("a hex byte"))
                .collect()
        })
        .collect()
}

fn record_id(record: &[u8]) -> u16 {
    u16::from_le_bytes([record[0], record[1]])
}

/// The answer to a `Get SEL Entry` or `Get SDR` of `id` in `records`: the next id and the record,
/// or "not present".
fn walk(records: &[Vec<u8>], id: u16) -> Vec<u8> {
    let at = if id == FIRST_RECORD {
        (!records.is_empty()).then_some(0)
    } else {
        records.iter().position(|r| record_id(r) == id)
    };
    at.map_or_else(
        || vec![NOT_PRESENT],
        |i| {
            let next = records.get(i + 1).map_or(LAST_RECORD, |r| record_id(r));
            let mut answer = vec![0];
            answer.extend(next.to_le_bytes());
            answer.extend(&records[i]);
            answer
        },
    )
}

/// A BMC a test sets.
#[derive(Default)]
struct Bmc {
    /// What every request fails with, when set: no device, a BMC that does not answer.
    down: Option<io::ErrorKind>,
    /// An answer given in place of the BMC's, by command (a short or an empty one).
    answer: BTreeMap<(u8, u8), Vec<u8>>,
    /// The completion code a command is refused with.
    refuse: BTreeMap<(u8, u8), u8>,
    /// The completion code a SEL entry, or an SDR record, is refused with, by record id.
    refuse_entry: BTreeMap<u16, u8>,
    refuse_sdr: BTreeMap<u16, u8>,
    sel: Vec<Vec<u8>>,
    added: u32,
    erased: u32,
    sdr: Vec<Vec<u8>>,
    sdr_added: u32,
    asked: Vec<BmcRequest>,
}

#[derive(Default)]
struct FakeBmc(Mutex<Bmc>);

impl FakeBmc {
    /// A BMC whose SEL holds `sel`, last erased at `erased`, and whose SDR the fixture's.
    fn holding(sel: &str, erased: u32) -> Arc<FakeBmc> {
        let bmc = FakeBmc::default();
        bmc.with(|b| {
            b.sel = fixture(sel);
            b.added = erased + 1;
            b.erased = erased;
            b.sdr = fixture(NAMES_SDR);
            b.sdr_added = 1_794_000_000;
        });
        Arc::new(bmc)
    }

    fn with(&self, change: impl FnOnce(&mut Bmc)) {
        change(&mut self.0.lock().expect("the BMC"));
    }

    /// The requests asked since the last call.
    fn asked(&self) -> Vec<BmcRequest> {
        std::mem::take(&mut self.0.lock().expect("the BMC").asked)
    }
}

impl BmcPort for FakeBmc {
    fn ask(&self, request: BmcRequest) -> io::Result<Vec<u8>> {
        let mut bmc = self.0.lock().expect("the BMC");
        bmc.asked.push(request);
        if let Some(kind) = bmc.down {
            return Err(io::Error::from(kind));
        }
        let command = request.command();
        if let Some(answer) = bmc.answer.get(&command) {
            return Ok(answer.clone());
        }
        if let Some(code) = bmc.refuse.get(&command) {
            return Ok(vec![*code]);
        }
        Ok(match request {
            BmcRequest::DeviceId => vec![
                0, 0x20, 0x81, 0x03, 0x45, 0x02, 0xbf, 0x57, 0x01, 0, 0x23, 0,
            ],
            BmcRequest::SelInfo => {
                let count = u16::try_from(bmc.sel.len()).expect("a count");
                let mut answer = vec![0, 0x51];
                answer.extend(count.to_le_bytes());
                answer.extend([0xff, 0xff]);
                answer.extend(bmc.added.to_le_bytes());
                answer.extend(bmc.erased.to_le_bytes());
                answer.push(0x02);
                answer
            }
            BmcRequest::SelEntry(id) => match bmc.refuse_entry.get(&id) {
                Some(code) => vec![*code],
                None => walk(&bmc.sel, id),
            },
            BmcRequest::SdrInfo => {
                let count = u16::try_from(bmc.sdr.len()).expect("a count");
                let mut answer = vec![0, 0x51];
                answer.extend(count.to_le_bytes());
                answer.extend([0xff, 0xff]);
                answer.extend(bmc.sdr_added.to_le_bytes());
                answer.extend([0, 0, 0, 0, 0]);
                answer
            }
            BmcRequest::Sdr(id) => match bmc.refuse_sdr.get(&id) {
                Some(code) => vec![*code],
                None => walk(&bmc.sdr, id),
            },
        })
    }
}

struct Running;

impl ShutdownProbe for Running {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("the system is running".to_owned())
    }
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// A reader of `bmc` and its keeper over `dir`, in the boot begun at `boot_unix`.
struct Rig {
    reader: SelReader,
    keeper: Arc<HealthKeeper>,
    log: Arc<RecordingLog>,
    dir: Scratch,
}

impl Rig {
    fn new(name: &str, bmc: &Arc<FakeBmc>, boot_unix: u64) -> Rig {
        let dir = scratch(name);
        let log = Arc::new(RecordingLog::default());
        let keeper = Arc::new(HealthKeeper::open(
            dir.path(),
            BootFacts {
                boot_id: Some("b7c1".to_owned()),
                boot_unix,
                machine_id: "m-1".to_owned(),
            },
            Box::new(Running),
            log.clone(),
        ));
        let reader = SelReader::new(bmc.clone(), Arc::clone(&keeper), log.clone());
        Rig {
            reader,
            keeper,
            log,
            dir,
        }
    }

    /// Every record held, in the order held.
    fn held(&self) -> Vec<pb::HardwareEvent> {
        match health_state::read(self.dir.path(), "m-1") {
            Read::Read(file) => file.held.records.iter().map(|r| r.event()).collect(),
            other => panic!("no state file: {other:?}"),
        }
    }

    /// The records held, then each written, so the next read's are the new ones.
    fn take_held(&self) -> Vec<pb::HardwareEvent> {
        let held = self.held();
        let ids: Vec<String> = held.iter().map(|e| e.event_id.clone()).collect();
        self.keeper.written(&ids);
        held
    }

    /// What the source last said: its status and reason.
    fn state(&self) -> (pb::SourceStatus, String) {
        let sources = self.keeper.health().sources;
        assert_eq!(sources.len(), 1, "{sources:?}");
        (sources[0].status(), sources[0].reason.clone())
    }

    fn mark(&self) -> SelMark {
        serde_json::from_str(&self.keeper.mark(SOURCE).expect("a mark")).expect("the mark")
    }
}

/// A record of the SEL, as the reader makes it.
#[expect(
    clippy::too_many_arguments,
    reason = "a record is its eleven fields, each named where it is compared"
)]
fn sel_record(
    id: &str,
    time_unix: u64,
    of_previous_boot: bool,
    component: pb::HardwareComponent,
    subject: &str,
    severity: pb::HardwareSeverity,
    state: pb::HardwareEventState,
    code: &str,
    message: &str,
    raw: &str,
    about: &str,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: id.to_owned(),
        time_unix,
        of_previous_boot,
        component: component.into(),
        subject: subject.to_owned(),
        severity: severity.into(),
        state: state.into(),
        source: "sel".to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        raw: raw.to_owned(),
        about: about.to_owned(),
    }
}

use hostkeeper_proto::hostkeeper::agent::v1::{
    HardwareComponent as C, HardwareEventState as St, HardwareSeverity as Sv,
};
