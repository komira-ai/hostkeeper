//! The host inventory's generated commands, `hostkeeper ctl enrollment add-record`,
//! `list-records` and `remove-record`, run as an operator types them: parsed from the generated
//! command table, encoded, sent with the operator's token over the in-memory gRPC route to the
//! generated `hostkeeper.operator.v1.Enrollment` server, and the answer printed. The server here
//! keeps the records in memory; the coordinator's own calls behind it are tested in
//! `hostkeeper-coordinator` (`inventory_tests`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hostkeeper_ctl::transport::InMemory;
use hostkeeper_ctl::{Io, PeerIdentity, exit};
use hostkeeper_proto::hostkeeper::agent::v1::Platform;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::enrollment_server::{Enrollment, EnrollmentServer};
use serde_json::{Value, json};
use tonic::{Code, Request, Response, Status};

/// The token the operator's file holds.
const TOKEN: &str = "operator-token-1";

/// What the server saw and keeps.
#[derive(Default)]
struct Seen {
    records: BTreeMap<String, proto::InventoryRecord>,
    adds: Vec<proto::AddRecordRequest>,
    lists: Vec<proto::ListRecordsRequest>,
    removes: Vec<proto::RemoveRecordRequest>,
    opens: Vec<proto::OpenRecordRequest>,
    rejects: Vec<proto::RejectRequest>,
    bearers: Vec<String>,
}

/// An `Enrollment` server whose record calls keep records in memory, refusing as the coordinator
/// does (`record_exists`, `no_record`), and which serves no pending request.
#[derive(Clone, Default)]
struct Records(Arc<Mutex<Seen>>);

impl Records {
    fn seen<T>(&self, read: impl FnOnce(&mut Seen) -> T) -> T {
        read(&mut self.0.lock().expect("lock"))
    }

    fn bearer<T>(&self, request: &Request<T>) {
        let bearer = request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("none")
            .to_owned();
        self.seen(|s| s.bearers.push(bearer));
    }
}

#[tonic::async_trait]
impl Enrollment for Records {
    async fn list(
        &self,
        request: Request<proto::ListPendingRequest>,
    ) -> Result<Response<proto::ListPendingResponse>, Status> {
        self.bearer(&request);
        Ok(Response::new(proto::ListPendingResponse::default()))
    }

    async fn approve(
        &self,
        request: Request<proto::ApproveRequest>,
    ) -> Result<Response<proto::ApproveResponse>, Status> {
        self.bearer(&request);
        Err(Status::new(Code::NotFound, "unknown_request"))
    }

    async fn reject(
        &self,
        request: Request<proto::RejectRequest>,
    ) -> Result<Response<proto::RejectResponse>, Status> {
        self.bearer(&request);
        let reject = request.into_inner();
        self.seen(|s| s.rejects.push(reject));
        Err(Status::new(Code::NotFound, "unknown_request"))
    }

    async fn add_record(
        &self,
        request: Request<proto::AddRecordRequest>,
    ) -> Result<Response<proto::AddRecordResponse>, Status> {
        self.bearer(&request);
        let add = request.into_inner();
        self.seen(|s| {
            s.adds.push(add.clone());
            if s.records.contains_key(&add.host_id) {
                return Err(Status::new(Code::AlreadyExists, "record_exists"));
            }
            let record = proto::InventoryRecord {
                host_id: add.host_id.clone(),
                role: add.role.clone(),
                data_macs: add.data_macs.clone(),
                bmc_mac: add.bmc_mac.clone(),
                serial: add.serial.clone(),
                notes: add.notes.clone(),
                added_by: "operator".to_owned(),
                added_unix: 1_800_000_000,
                enrolled: false,
                host_held: proto::HostHeld::None.into(),
                approve_until_unix: if add.role.is_empty() {
                    0
                } else {
                    add.approve_until_unix
                },
                state: proto::RecordState::Open.into(),
                platform: add.platform,
                ..proto::InventoryRecord::default()
            };
            s.records.insert(add.host_id, record.clone());
            Ok(Response::new(proto::AddRecordResponse {
                record: Some(record),
                entry: None,
            }))
        })
    }

    async fn list_records(
        &self,
        request: Request<proto::ListRecordsRequest>,
    ) -> Result<Response<proto::ListRecordsResponse>, Status> {
        self.bearer(&request);
        let list = request.into_inner();
        let records = self.seen(|s| {
            s.lists.push(list.clone());
            s.records
                .values()
                .filter(|r| list.enrolled.is_none_or(|want| r.enrolled == want))
                .cloned()
                .collect()
        });
        Ok(Response::new(proto::ListRecordsResponse {
            records,
            next_page_token: String::new(),
        }))
    }

    async fn remove_record(
        &self,
        request: Request<proto::RemoveRecordRequest>,
    ) -> Result<Response<proto::RemoveRecordResponse>, Status> {
        self.bearer(&request);
        let remove = request.into_inner();
        self.seen(|s| {
            s.removes.push(remove.clone());
            match s.records.remove(&remove.host_id) {
                Some(_) => Ok(Response::new(proto::RemoveRecordResponse::default())),
                None => Err(Status::new(Code::NotFound, "no_record")),
            }
        })
    }

    async fn open_record(
        &self,
        request: Request<proto::OpenRecordRequest>,
    ) -> Result<Response<proto::OpenRecordResponse>, Status> {
        self.bearer(&request);
        let open = request.into_inner();
        self.seen(|s| {
            s.opens.push(open.clone());
            let Some(record) = s.records.get_mut(&open.host_id) else {
                return Err(Status::new(Code::NotFound, "no_record"));
            };
            record.approve_until_unix = open.approve_until_unix;
            record.state = proto::RecordState::Open.into();
            Ok(Response::new(proto::OpenRecordResponse {
                record: Some(record.clone()),
                entry: None,
            }))
        })
    }
}

/// One run of `hostkeeper ctl`: its exit code and streams.
struct Ran {
    code: u8,
    stdout: String,
    stderr: String,
}

/// Runs `hostkeeper ctl <args>` with the operator's token file, against `memory`.
fn ctl(memory: &InMemory, token_file: &str, args: &[&str]) -> Ran {
    ctl_as("json", memory, token_file, args)
}

/// The same run printing in the `--output text` form.
fn ctl_text(memory: &InMemory, token_file: &str, args: &[&str]) -> Ran {
    ctl_as("text", memory, token_file, args)
}

fn ctl_as(output: &str, memory: &InMemory, token_file: &str, args: &[&str]) -> Ran {
    let mut stdin: &[u8] = b"";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let globals = [
        "--endpoint",
        "https://coordinator.example",
        "--ca-file",
        "ca.pem",
        "--token-file",
        token_file,
        "--output",
        output,
    ];
    let words = std::iter::once("hostkeeper-ctl")
        .chain(args.iter().copied())
        .chain(globals);
    let code = hostkeeper_ctl::run(
        words,
        memory,
        Io {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    );
    Ran {
        code,
        stdout: String::from_utf8(stdout).expect("UTF-8"),
        stderr: String::from_utf8(stderr).expect("UTF-8"),
    }
}

fn json_of(ran: &Ran) -> Value {
    assert_eq!(ran.code, exit::SUCCESS, "{}", ran.stderr);
    serde_json::from_str(&ran.stdout).expect("JSON on stdout")
}

/// Catches: an inventory command missing from the generated table, a flag that does not reach
/// its field (the repeated `--data-macs` above all), a call routed to another method or sent
/// without the operator's token, a reply not printed, and a refusal not ending the run with its
/// token on stderr. Add, list, a second add of the host id, remove and list again run in order
/// against one server; then the pending view, an approval and a rejection (its request id and
/// reason reaching the server as given) on the same service.
#[test]
fn the_inventory_commands_add_list_and_remove_a_record_through_the_grpc_route() {
    let dir = hostkeeper_testkit::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "ctl-inventory",
    )
    .expect("a scratch directory");
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).expect("written");
    let token_file = token_file.to_str().expect("UTF-8 path");
    let records = Records::default();
    let memory = InMemory::new(PeerIdentity::Operator("op".to_owned()))
        .serve(EnrollmentServer::new(records.clone()));

    let added = ctl(
        &memory,
        token_file,
        &[
            "enrollment",
            "add-record",
            "--host-id",
            "db-1",
            "--role",
            "storage",
            "--data-macs",
            "0C-C4-7A-00-00-02",
            "--data-macs",
            "0c:c4:7a:00:00:03",
            "--bmc-mac",
            "0c:c4:7a:00:00:01",
            "--serial",
            "SN-1",
            "--notes",
            "rack 4",
        ],
    );
    assert_eq!(
        json_of(&added)["record"],
        json!({"hostId": "db-1", "role": "storage",
               "dataMacs": ["0C-C4-7A-00-00-02", "0c:c4:7a:00:00:03"],
               "bmcMac": "0c:c4:7a:00:00:01", "serial": "SN-1", "notes": "rack 4",
               "addedBy": "operator", "addedUnix": "1800000000",
               "hostHeld": "HOST_HELD_NONE", "state": "RECORD_STATE_OPEN"})
    );
    assert_eq!(
        records.seen(|s| s.adds.clone()),
        vec![proto::AddRecordRequest {
            host_id: "db-1".to_owned(),
            role: "storage".to_owned(),
            data_macs: vec![
                "0C-C4-7A-00-00-02".to_owned(),
                "0c:c4:7a:00:00:03".to_owned()
            ],
            bmc_mac: "0c:c4:7a:00:00:01".to_owned(),
            serial: "SN-1".to_owned(),
            notes: "rack 4".to_owned(),
            ..proto::AddRecordRequest::default()
        }]
    );

    let listed = json_of(&ctl(&memory, token_file, &["enrollment", "list-records"]));
    let hosts: Vec<&str> = listed["records"]
        .as_array()
        .expect("records")
        .iter()
        .filter_map(|r| r["hostId"].as_str())
        .collect();
    assert_eq!(hosts, ["db-1"]);

    let again = ctl(
        &memory,
        token_file,
        &[
            "enrollment",
            "add-record",
            "--host-id",
            "db-1",
            "--serial",
            "SN-9",
        ],
    );
    assert_eq!(again.code, exit::FAILURE);
    assert!(again.stderr.contains("record_exists"), "{}", again.stderr);
    assert_eq!(again.stdout, "");

    let removed = ctl(
        &memory,
        token_file,
        &[
            "enrollment",
            "remove-record",
            "--host-id",
            "db-1",
            "--reason",
            "rebuilt",
        ],
    );
    assert_eq!(removed.code, exit::SUCCESS, "{}", removed.stderr);
    assert_eq!(
        records.seen(|s| s.removes.clone()),
        vec![proto::RemoveRecordRequest {
            host_id: "db-1".to_owned(),
            reason: "rebuilt".to_owned(),
        }]
    );
    let gone = ctl(
        &memory,
        token_file,
        &[
            "enrollment",
            "remove-record",
            "--host-id",
            "db-1",
            "--reason",
            "again",
        ],
    );
    assert_eq!(gone.code, exit::FAILURE);
    assert!(gone.stderr.contains("no_record"), "{}", gone.stderr);
    let empty = json_of(&ctl(&memory, token_file, &["enrollment", "list-records"]));
    assert_eq!(empty["records"].as_array().map_or(0, Vec::len), 0);

    // The pending view and approval are on the same service and route.
    let pending = ctl(&memory, token_file, &["enrollment", "list"]);
    assert_eq!(pending.code, exit::SUCCESS, "{}", pending.stderr);
    let approve = ctl(
        &memory,
        token_file,
        &["enrollment", "approve", "--request-id", "ab"],
    );
    assert!(
        approve.stderr.contains("unknown_request"),
        "{}",
        approve.stderr
    );
    let reject = ctl(
        &memory,
        token_file,
        &[
            "enrollment",
            "reject",
            "--request-id",
            "cd",
            "--reason",
            "not ours",
        ],
    );
    assert_eq!(reject.code, exit::FAILURE);
    assert!(
        reject.stderr.contains("unknown_request"),
        "{}",
        reject.stderr
    );
    let rejects = records.seen(|s| s.rejects.clone());
    assert_eq!(
        rejects,
        [proto::RejectRequest {
            request_id: "cd".to_owned(),
            reason: "not ours".to_owned(),
        }]
    );

    let bearers = records.seen(|s| s.bearers.clone());
    assert_eq!(bearers.len(), 9);
    assert!(
        bearers.iter().all(|b| *b == format!("Bearer {TOKEN}")),
        "{bearers:?}"
    );
}

/// Catches: the generated `--enrolled` flag of `list-records` not reaching
/// `ListRecordsRequest.enrolled` as typed (an absent flag sent as `false`, or `false` dropped
/// as if absent, would hide the records a host is enrolled against), and a record's `enrolled`
/// not printed.
#[test]
fn list_records_sends_the_enrolled_filter_as_typed() {
    let dir = hostkeeper_testkit::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "ctl-inventory-enrolled",
    )
    .expect("a scratch directory");
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).expect("written");
    let token_file = token_file.to_str().expect("UTF-8 path");
    let records = Records::default();
    records.seen(|s| {
        for (host, enrolled) in [("db-1", true), ("db-2", false)] {
            let record = proto::InventoryRecord {
                host_id: host.to_owned(),
                serial: format!("SN-{host}"),
                enrolled,
                ..proto::InventoryRecord::default()
            };
            s.records.insert(host.to_owned(), record);
        }
    });
    let memory = InMemory::new(PeerIdentity::Operator("op".to_owned()))
        .serve(EnrollmentServer::new(records.clone()));
    let hosts = |args: &[&str]| -> Vec<(String, bool)> {
        let words: Vec<&str> = ["enrollment", "list-records"]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        json_of(&ctl(&memory, token_file, &words))["records"]
            .as_array()
            .expect("records")
            .iter()
            .map(|r| {
                let host = r["hostId"].as_str().expect("a host id").to_owned();
                (host, r["enrolled"].as_bool().unwrap_or(false))
            })
            .collect()
    };

    let all = [("db-1".to_owned(), true), ("db-2".to_owned(), false)];
    assert_eq!(hosts(&[]), all);
    assert_eq!(hosts(&["--enrolled", "true"]), all[..1]);
    assert_eq!(hosts(&["--enrolled", "false"]), all[1..]);
    assert_eq!(
        records.seen(|s| s.lists.iter().map(|l| l.enrolled).collect::<Vec<_>>()),
        [None, Some(true), Some(false)]
    );
}

/// A scratch token file and a server holding no records, for the platform tests.
fn platform_setup(name: &str) -> (hostkeeper_testkit::Scratch, String, Records, InMemory) {
    let dir =
        hostkeeper_testkit::Scratch::under(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")), name)
            .expect("a scratch directory");
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).expect("written");
    let token_file = token_file.to_str().expect("UTF-8 path").to_owned();
    let records = Records::default();
    let memory = InMemory::new(PeerIdentity::Operator("op".to_owned()))
        .serve(EnrollmentServer::new(records.clone()));
    (dir, token_file, records, memory)
}

/// Proves: `add-record --platform` reaches `AddRecordRequest.platform` as typed for both Linux
/// values, and a record added without the flag is sent with the platform unspecified.
#[test]
fn add_record_sends_the_platform_as_typed() {
    let (_dir, token_file, records, memory) = platform_setup("ctl-inventory-platform");
    for (host, flag) in [
        ("arm-1", Some("linux_arm64")),
        ("x86-1", Some("linux_x86_64")),
        ("none-1", None),
    ] {
        let mut args = vec![
            "enrollment",
            "add-record",
            "--host-id",
            host,
            "--serial",
            host,
        ];
        if let Some(flag) = flag {
            args.extend(["--platform", flag]);
        }
        let ran = ctl(&memory, &token_file, &args);
        assert_eq!(ran.code, exit::SUCCESS, "{}", ran.stderr);
    }
    let sent: Vec<(String, Platform)> = records.seen(|s| {
        s.adds
            .iter()
            .map(|a| (a.host_id.clone(), a.platform()))
            .collect()
    });
    assert_eq!(
        sent,
        [
            ("arm-1".to_owned(), Platform::LinuxArm64),
            ("x86-1".to_owned(), Platform::LinuxX8664),
            ("none-1".to_owned(), Platform::Unspecified),
        ]
    );
}

/// Proves: a `--platform` outside the list is a usage error (exit code 2) that names the
/// possible values, and no call is made.
#[test]
fn add_record_refuses_a_platform_outside_the_list_without_calling() {
    let (_dir, token_file, records, memory) = platform_setup("ctl-inventory-platform-bad");
    let ran = ctl(
        &memory,
        &token_file,
        &[
            "enrollment",
            "add-record",
            "--host-id",
            "db-1",
            "--serial",
            "SN-1",
            "--platform",
            "windows",
        ],
    );
    assert_eq!(ran.code, exit::USAGE, "{}", ran.stderr);
    for value in ["linux_x86_64", "linux_arm64", "darwin_arm64"] {
        assert!(ran.stderr.contains(value), "{}", ran.stderr);
    }
    assert_eq!(ran.stdout, "");
    assert_eq!(records.seen(|s| s.adds.len() + s.bearers.len()), 0);
}

/// Proves: `list-records` prints a record's platform in the JSON output (as the enum's name) and
/// in the text output, and leaves it out for a record with none.
#[test]
fn list_records_prints_the_platform_in_json_and_text() {
    let (_dir, token_file, records, memory) = platform_setup("ctl-inventory-platform-list");
    records.seen(|s| {
        for (host, platform) in [
            ("arm-1", Platform::LinuxArm64),
            ("none-1", Platform::Unspecified),
        ] {
            let record = proto::InventoryRecord {
                host_id: host.to_owned(),
                serial: host.to_owned(),
                platform: platform.into(),
                ..proto::InventoryRecord::default()
            };
            s.records.insert(host.to_owned(), record);
        }
    });
    let listed = json_of(&ctl(&memory, &token_file, &["enrollment", "list-records"]));
    let platforms: Vec<Option<&str>> = listed["records"]
        .as_array()
        .expect("records")
        .iter()
        .map(|r| r["platform"].as_str())
        .collect();
    assert_eq!(platforms, [Some("PLATFORM_LINUX_ARM64"), None]);

    let text = ctl_text(&memory, &token_file, &["enrollment", "list-records"]);
    assert_eq!(text.code, exit::SUCCESS, "{}", text.stderr);
    assert!(
        text.stdout.contains("PLATFORM_LINUX_ARM64"),
        "{}",
        text.stdout
    );
}

/// Proves: `add-record --approve-until-unix` and `--approve-for-secs` reach
/// `AddRecordRequest.approve_until_unix` and `approve_for_secs` as typed (an absent flag stays 0,
/// which is "none given"); `open-record` sends its host id, window and reason as
/// `OpenRecordRequest` over the operator's route, prints the record the server answers with its
/// state, and ends a run against an unknown host id with `no_record` on stderr.
#[test]
fn add_record_carries_its_window_and_open_record_reopens_one() {
    let dir = hostkeeper_testkit::Scratch::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "ctl-inventory-window",
    )
    .expect("a scratch directory");
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).expect("written");
    let token_file = token_file.to_str().expect("UTF-8 path");
    let records = Records::default();
    let memory = InMemory::new(PeerIdentity::Operator("op".to_owned()))
        .serve(EnrollmentServer::new(records.clone()));

    for (host, flag, value) in [
        ("db-1", "--approve-until-unix", "1800086400"),
        ("db-2", "--approve-for-secs", "3600"),
    ] {
        let added = ctl(
            &memory,
            token_file,
            &[
                "enrollment",
                "add-record",
                "--host-id",
                host,
                "--role",
                "storage",
                flag,
                value,
            ],
        );
        assert_eq!(added.code, exit::SUCCESS, "{}", added.stderr);
    }
    let adds = records.seen(|s| s.adds.clone());
    assert_eq!(
        adds.iter()
            .map(|a| (a.approve_until_unix, a.approve_for_secs))
            .collect::<Vec<_>>(),
        [(1_800_086_400, 0), (0, 3_600)]
    );

    let opened = json_of(&ctl(
        &memory,
        token_file,
        &[
            "enrollment",
            "open-record",
            "--host-id",
            "db-2",
            "--approve-until-unix",
            "1800090000",
            "--reason",
            "rack was late",
        ],
    ));
    assert_eq!(opened["record"]["hostId"], "db-2");
    assert_eq!(opened["record"]["approveUntilUnix"], "1800090000");
    assert_eq!(opened["record"]["state"], "RECORD_STATE_OPEN");
    assert_eq!(
        records.seen(|s| s.opens.clone()),
        [proto::OpenRecordRequest {
            host_id: "db-2".to_owned(),
            approve_until_unix: 1_800_090_000,
            approve_for_secs: 0,
            reason: "rack was late".to_owned(),
        }]
    );

    let missing = ctl(
        &memory,
        token_file,
        &["enrollment", "open-record", "--host-id", "nobody"],
    );
    assert_eq!(missing.code, exit::FAILURE);
    assert!(missing.stderr.contains("no_record"), "{}", missing.stderr);
    assert_eq!(missing.stdout, "");
}
