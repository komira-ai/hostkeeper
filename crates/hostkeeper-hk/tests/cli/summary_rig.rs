//! What `xcode_add_summary` shares with the tests of its own: a fake gateway whose answers are the
//! real messages of `hostkeeper-proto`, written as the coordinator's gateway writes them.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use prost_reflect::{DescriptorPool, DynamicMessage, SerializeOptions};
use serde_json::Value;

use crate::add_rig::{Add, FLEET, Store, archive_bytes};
use crate::rig::{Ran, Reply, Request, Server};

pub const HOSTS: &str = FLEET[0];
pub const GROUPS: &str = FLEET[1];
pub const RUNS: &str = FLEET[2];
pub const GET: &str = "/hostkeeper.operator.v1.Maintenance/Get";
pub const SCHEDULE: &str = "/hostkeeper.operator.v1.Maintenance/Schedule";

pub const XCODE: agent::MaintenanceKind = agent::MaintenanceKind::Xcode;

/// The line a store that already holds the release makes the run say first.
pub const HELD: &str = "already in the store: Xcode 27.2 (27B52)\n";

/// The line `hk maintenance schedule ...` that stores the run of `group`.
pub fn command(group: &str) -> String {
    format!(
        "    hk maintenance schedule --host-group {group} --kinds xcode --reason \"Xcode 27.2\" \
         --confirm true\n"
    )
}

/// What a run that would be stored says before the line that stores it: the line of `no run takes
/// it yet` (design section 1) and then [`command`].
pub fn stores(group: &str) -> String {
    format!(
        "    no run takes it yet. Press Update on the Maintenance page, or:\n{}",
        command(group)
    )
}

/// The JSON the coordinator's gateway answers for `message`, the answer of the method `method` of
/// `service`: the message is encoded, decoded as the method's output type from the real
/// descriptors, and written with the options the gateway writes with (`SerializeOptions::new()`
/// in `hostkeeper-coordinator`'s `grpc/gateway.rs`, `to_json`): the names of fields in
/// lowerCamelCase, enums by name, 64-bit integers as strings, a field with its default value left
/// out.
pub fn gateway_json<M: prost_reflect::prost::Message>(
    service: &str,
    method: &str,
    message: &M,
) -> Value {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("descriptors");
    let method = pool
        .get_service_by_name(&format!("hostkeeper.operator.v1.{service}"))
        .expect("a service")
        .methods()
        .find(|m| m.name() == method)
        .expect("a method");
    let answer = DynamicMessage::decode(method.output(), message.encode_to_vec().as_slice())
        .expect("the method's answer");
    answer
        .serialize_with_options(serde_json::value::Serializer, &SerializeOptions::new())
        .expect("JSON")
}

/// What the fake gateway answers; a path in `broken` answers that reply in place of its own.
#[derive(Clone, Default)]
pub struct Fleet {
    /// The pages of `Hosts.List`.
    pub hosts: Vec<Vec<proto::Host>>,
    /// The pages of `HostGroups.List`.
    pub groups: Vec<Vec<proto::HostGroupState>>,
    /// The open runs `Maintenance.List` names.
    pub runs: Vec<proto::MaintenanceRun>,
    /// The answer of `Maintenance.Get`, by run id.
    pub turns: BTreeMap<String, proto::GetMaintenanceRunResponse>,
    /// The answer of `Maintenance.Schedule`, by group: its status and body.
    pub schedule: BTreeMap<String, (u16, String)>,
    pub broken: Option<(&'static str, Reply)>,
}

/// A host of the platform `platform`.
pub fn host(id: &str, group: &str, platform: agent::Platform) -> proto::Host {
    proto::Host {
        node_id: id.to_owned(),
        host_group: group.to_owned(),
        heartbeat: Some(agent::HeartbeatRequest {
            platform: platform as i32,
            ..agent::HeartbeatRequest::default()
        }),
        ..proto::Host::default()
    }
}

/// A host that is a Mac with no maintenance report.
pub fn mac_without_report(id: &str, group: &str) -> proto::Host {
    host(id, group, agent::Platform::DarwinArm64)
}

/// A host that is a Linux host.
pub fn linux(id: &str, group: &str) -> proto::Host {
    host(id, group, agent::Platform::LinuxX8664)
}

/// A host that is a Mac with a maintenance report listing `xcodes`: `(version, build, selected,
/// foreign)`.
pub fn mac(id: &str, group: &str, xcodes: &[(&str, &str, bool, bool)]) -> proto::Host {
    let xcodes = xcodes
        .iter()
        .map(
            |(version, build, selected, foreign)| agent::InstalledXcode {
                version: (*version).to_owned(),
                build: (*build).to_owned(),
                path: format!("/Applications/Xcode-{version}.app"),
                selected: *selected,
                foreign: *foreign,
            },
        )
        .collect();
    proto::Host {
        maintenance: Some(agent::MaintenanceReport {
            xcodes,
            ..agent::MaintenanceReport::default()
        }),
        ..mac_without_report(id, group)
    }
}

/// A group whose Macs keep `keep` Xcodes, in batches of `batch`, with `default` selected.
pub fn group(
    name: &str,
    keep: u32,
    batch: Option<u32>,
    default: Option<&str>,
) -> proto::HostGroupState {
    proto::HostGroupState {
        group: Some(proto::HostGroup {
            name: name.to_owned(),
            policy: Some(proto::RolloutPolicy {
                maintenance: Some(proto::MaintenancePolicy {
                    xcode_keep: keep,
                    xcode_default: default.map(str::to_owned),
                    batch,
                    ..proto::MaintenancePolicy::default()
                }),
                ..proto::RolloutPolicy::default()
            }),
            ..proto::HostGroup::default()
        }),
        generation: 1,
        ..proto::HostGroupState::default()
    }
}

/// A run, as `Maintenance.List` and `Maintenance.Get` name it.
pub fn run(id: &str, state: proto::MaintenanceState) -> proto::MaintenanceRun {
    proto::MaintenanceRun {
        id: id.to_owned(),
        state: state as i32,
        ..proto::MaintenanceRun::default()
    }
}

/// A host's turn of a run.
pub fn turn(
    id: &str,
    step: proto::MaintenanceHostStep,
    kinds: &[agent::MaintenanceKind],
) -> proto::MaintenanceHost {
    proto::MaintenanceHost {
        host_id: id.to_owned(),
        step: step as i32,
        kinds: kinds.iter().map(|kind| *kind as i32).collect(),
        ..proto::MaintenanceHost::default()
    }
}

/// A preview of a run that would take `taken` and skip `skipped` `(host, why)`.
pub fn preview(
    taken: &[&str],
    skipped: &[(&str, proto::MaintenanceSkip)],
    batch: Option<u32>,
) -> (u16, String) {
    let mut hosts: Vec<proto::MaintenanceHost> = taken
        .iter()
        .map(|id| turn(id, proto::MaintenanceHostStep::Waiting, &[XCODE]))
        .collect();
    for (id, why) in skipped {
        hosts.push(proto::MaintenanceHost {
            skipped_because: *why as i32,
            ..turn(id, proto::MaintenanceHostStep::Skipped, &[])
        });
    }
    let answer = proto::ScheduleMaintenanceResponse {
        run: Some(proto::MaintenanceRun {
            kinds: vec![XCODE as i32],
            batch,
            ..proto::MaintenanceRun::default()
        }),
        hosts,
        entry: None,
    };
    (
        200,
        gateway_json("Maintenance", "Schedule", &answer).to_string(),
    )
}

/// The refusal the gateway answers.
pub fn refusal(code: &str, reason: &str) -> Reply {
    Reply::new(
        400,
        format!("{{\"code\":\"{code}\",\"reason\":\"{reason}\"}}"),
    )
}

/// The same refusal as a preview's answer: its status and body.
pub fn refused(code: &str, reason: &str) -> (u16, String) {
    let reply = refusal(code, reason);
    (reply.status, String::from_utf8(reply.body).expect("text"))
}

impl Fleet {
    /// The page `request` asks for of a list of `count` pages, `build` making the answer of page
    /// `at` with the token of the next: page `n` names page `n + 1` as the next.
    pub fn page(count: usize, request: &Request, build: impl Fn(usize, String) -> Value) -> Reply {
        let asked: Value = serde_json::from_slice(&request.body).expect("a JSON body");
        let at = match asked["pageToken"].as_str().unwrap_or_default() {
            "" => 0,
            token => token
                .strip_prefix("page-")
                .and_then(|n| n.parse().ok())
                .expect("a token this fleet gave"),
        };
        let next = if at + 1 < count {
            format!("page-{}", at + 1)
        } else {
            String::new()
        };
        Reply::new(200, build(at, next).to_string())
    }

    pub fn answer(&self, request: &Request) -> Option<Reply> {
        if let Some((path, reply)) = &self.broken
            && request.target == *path
        {
            return Some(reply.clone());
        }
        let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
        match request.target.as_str() {
            HOSTS => Some(Fleet::page(self.hosts.len(), request, |at, next| {
                let page = proto::ListHostsResponse {
                    hosts: self.hosts[at].clone(),
                    next_page_token: next,
                };
                gateway_json("Hosts", "List", &page)
            })),
            GROUPS => Some(Fleet::page(self.groups.len(), request, |at, next| {
                let page = proto::ListHostGroupsResponse {
                    host_groups: self.groups[at].clone(),
                    next_page_token: next,
                };
                gateway_json("HostGroups", "List", &page)
            })),
            RUNS => {
                let listed = proto::ListMaintenanceRunsResponse {
                    runs: self.runs.clone(),
                };
                Some(Reply::new(
                    200,
                    gateway_json("Maintenance", "List", &listed).to_string(),
                ))
            }
            GET => {
                let id = body["runId"].as_str().expect("a run id");
                Some(Reply::new(
                    200,
                    gateway_json("Maintenance", "Get", &self.turns[id]).to_string(),
                ))
            }
            SCHEDULE => {
                let group = body["hostGroup"].as_str().expect("a group");
                let (status, text) = self.schedule[group].clone();
                Some(Reply::new(status, text))
            }
            _ => None,
        }
    }

    /// `run` and its turns, as `Maintenance.List` and `Maintenance.Get` answer them.
    pub fn with_run(
        mut self,
        run: proto::MaintenanceRun,
        turns: Vec<proto::MaintenanceHost>,
    ) -> Fleet {
        self.turns.insert(
            run.id.clone(),
            proto::GetMaintenanceRunResponse {
                run: Some(run.clone()),
                hosts: turns,
            },
        );
        self.runs.push(run);
        self
    }

    pub fn with_preview(mut self, group: &str, answer: (u16, String)) -> Fleet {
        self.schedule.insert(group.to_owned(), answer);
        self
    }

    pub fn broken(mut self, path: &'static str, reply: Reply) -> Fleet {
        self.broken = Some((path, reply));
        self
    }
}

/// The store of most tests: it holds the archive of the tests as 27.2 (27B52) and, besides, the
/// releases `others` as `(version, build)`.
pub fn store(others: &[(&str, &str)]) -> Store {
    let mut store = Store::default().holding(&archive_bytes(), "27.2", "27B52");
    for (n, (version, build)) in others.iter().enumerate() {
        store = store.holding_sha(&format!("{n:064x}"), version, build);
    }
    store
}

/// `hk xcode add <archive>` against `store` behind `fleet`.
pub fn add(name: &str, store: Store, fleet: &Fleet) -> (Add, Server, Ran) {
    let add = Add::new(&format!("summary-{name}"));
    let path = add.file("Xcode_27.2.xip", &archive_bytes(), 100);
    let fleet = fleet.clone();
    let server = store.serve_with(&add.rig, move |request| fleet.answer(request));
    let ran = add.add(&server, &[&path]);
    (add, server, ran)
}

/// The JSON bodies the server saw on `path`.
pub fn bodies(server: &Server, path: &str) -> Vec<Value> {
    server
        .seen()
        .iter()
        .filter(|seen| seen.target == path)
        .map(|seen| serde_json::from_slice(&seen.body).expect("a JSON body"))
        .collect()
}

/// What `Maintenance.Get` answers for `run` and its `turns`, as the gateway writes it.
pub fn answer_of(run: &proto::MaintenanceRun, turns: &[proto::MaintenanceHost]) -> Value {
    let answer = proto::GetMaintenanceRunResponse {
        run: Some(run.clone()),
        hosts: turns.to_vec(),
    };
    gateway_json("Maintenance", "Get", &answer)
}

/// The fleet of the first test: mac-build's Macs and mac-proof's, in two pages of each list, with
/// a Linux host among them.
pub fn two_groups() -> Fleet {
    Fleet {
        hosts: vec![
            vec![
                mac("mac-01", "mac-build", &[]),
                mac("mac-02", "mac-build", &[("27.1", "27A10", true, false)]),
                linux("build-1", "linux"),
            ],
            vec![
                mac("mac-03", "mac-build", &[]),
                mac("mac-04", "mac-proof", &[]),
            ],
        ],
        groups: vec![
            vec![group("mac-build", 2, Some(1), None)],
            vec![group("mac-proof", 0, None, None)],
        ],
        ..Fleet::default()
    }
    .with_preview(
        "mac-build",
        preview(&["mac-01", "mac-02", "mac-03"], &[], None),
    )
}
