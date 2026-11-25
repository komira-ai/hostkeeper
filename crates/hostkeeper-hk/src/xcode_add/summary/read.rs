//! What the summary reads from the coordinator's gateway, through [`crate::call::ask`]: every
//! page of `Hosts.List` and `HostGroups.List`, the open runs of `Maintenance.List` with
//! `Maintenance.Get` of each, and the preview of `Maintenance.Schedule`. The answers are the
//! gateway's JSON; a field that is absent is empty (the gateway leaves a default out), a number
//! of 64 bits is a string or a number, and a field this does not read is ignored. A call that
//! fails is an `Err` that says which call and why.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::json;

use crate::Stop;
use crate::args::Globals;
use crate::call::{Refused, ask};
use crate::{PROGRAM, World};

const HOSTS: &str = "/hostkeeper.operator.v1.Hosts/List";
const GROUPS: &str = "/hostkeeper.operator.v1.HostGroups/List";
const RUNS: &str = "/hostkeeper.operator.v1.Maintenance/List";
const RUN: &str = "/hostkeeper.operator.v1.Maintenance/Get";
const SCHEDULE: &str = "/hostkeeper.operator.v1.Maintenance/Schedule";

/// The platform of a Mac, as the gateway names it.
const DARWIN: &str = "PLATFORM_DARWIN_ARM64";

/// The runs that have not ended: not yet in their window, taking hosts, or halted in a group.
const OPEN_STATES: [&str; 3] = [
    "MAINTENANCE_STATE_SCHEDULED",
    "MAINTENANCE_STATE_RUNNING",
    "MAINTENANCE_STATE_HALTED",
];

/// A pages' size: the most the coordinator gives.
const PAGE: u32 = 256;

/// An Xcode a Mac reports installed.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct Installed {
    pub(super) version: String,
    pub(super) build: String,
    pub(super) selected: bool,
    pub(super) foreign: bool,
}

/// A host of the platform `darwin-arm64`.
#[derive(Debug, Clone, Default)]
pub(super) struct Mac {
    pub(super) id: String,
    /// Empty: in no host group.
    pub(super) group: String,
    /// The Xcodes of its maintenance report; `None` when the coordinator keeps no report.
    pub(super) installed: Option<Vec<Installed>>,
}

/// A group's maintenance settings, as far as Xcode reads them.
#[derive(Debug, Clone)]
pub(super) struct Policy {
    pub(super) keep: usize,
    /// The version `xcode_default` names; `None`: `latest`.
    pub(super) default: Option<String>,
    pub(super) batch: u32,
}

impl Default for Policy {
    /// What a group with no policy has: it keeps no Xcode, one host at a time.
    fn default() -> Self {
        Policy {
            keep: 0,
            default: None,
            batch: 1,
        }
    }
}

/// A maintenance run that has not ended.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Run {
    pub(super) id: String,
    pub(super) state: String,
    #[serde(deserialize_with = "number")]
    pub(super) window_start_unix: u64,
    #[serde(deserialize_with = "number")]
    pub(super) window_end_unix: u64,
    pub(super) halted_groups: Vec<String>,
    pub(super) batch: Option<u32>,
}

/// One host's turn of a run.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Turn {
    pub(super) host_id: String,
    pub(super) step: String,
    pub(super) kinds: Vec<String>,
    pub(super) skipped_because: String,
}

/// An open run and its hosts' turns.
#[derive(Debug, Clone, Default)]
pub(super) struct Open {
    pub(super) run: Run,
    pub(super) turns: Vec<Turn>,
}

/// What `Maintenance.Schedule` without `confirm` answers: the run as it would be.
#[derive(Debug, Clone)]
pub(super) enum Preview {
    Run {
        batch: Option<u32>,
        turns: Vec<Turn>,
    },
    /// Refused as the stored run would be: the gateway's code and reason.
    Refused { code: String, reason: String },
}

/// A 64-bit number the gateway writes as a string, or as a number.
fn number<'de, D: Deserializer<'de>>(from: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Number(u64),
        Text(String),
    }
    match Either::deserialize(from)? {
        Either::Number(n) => Ok(n),
        Either::Text(text) => text.parse().map_err(serde::de::Error::custom),
    }
}

/// What a stopped call says.
pub(super) fn why(stop: &Stop) -> String {
    match stop {
        Stop::Usage(text) | Stop::Fail(text) => String::from_utf8_lossy(text).into_owned(),
        Stop::Exit(code) => format!("it exited {code}"),
    }
}

/// What a refusal says: its reason and its code.
fn refused_text(refused: &Refused) -> String {
    format!(
        "refused: {} ({})",
        String::from_utf8_lossy(&refused.reason),
        String::from_utf8_lossy(&refused.code)
    )
}

/// The call `name` of `path` with the JSON `body`, its answer read as a `T`; or the refusal.
fn call<T: DeserializeOwned>(
    w: &mut World<'_>,
    globals: &Globals,
    name: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<Result<T, Refused>, String> {
    let answer = ask(w, globals, path, body.to_string().as_bytes())
        .map_err(|stop| format!("{name}: {}", why(&stop)))?;
    match answer {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Ok)
            .map_err(|error| format!("{name}: the answer is not the coordinator's JSON ({error})")),
        Err(refused) => Ok(Err(refused)),
    }
}

/// [`call`] where a refusal is a failure of the summary.
fn must<T: DeserializeOwned>(
    w: &mut World<'_>,
    globals: &Globals,
    name: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<T, String> {
    call(w, globals, name, path, body)?
        .map_err(|refused| format!("{name}: {}", refused_text(&refused)))
}

/// Every page of the list at `path`: `split` takes a page apart into its items and the token of
/// the next page (empty on the last). A token that does not change would ask for ever.
fn pages<P: DeserializeOwned, T>(
    w: &mut World<'_>,
    globals: &Globals,
    name: &str,
    path: &str,
    split: fn(P) -> (Vec<T>, String),
) -> Result<Vec<T>, String> {
    let mut all = Vec::new();
    let mut token = String::new();
    loop {
        let body = json!({"limit": PAGE, "pageToken": token});
        let (items, next) = split(must(w, globals, name, path, &body)?);
        all.extend(items);
        if next.is_empty() {
            return Ok(all);
        }
        if next == token {
            return Err(format!("{name}: the page after {next} is {next} again"));
        }
        token = next;
    }
}

/// The Macs: every host of the platform `darwin-arm64`, by node id.
pub(super) fn macs(w: &mut World<'_>, globals: &Globals) -> Result<Vec<Mac>, String> {
    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct Page {
        hosts: Vec<Row>,
        next_page_token: String,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct Row {
        node_id: String,
        host_group: String,
        heartbeat: Beat,
        maintenance: Option<Report>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Beat {
        platform: String,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Report {
        xcodes: Vec<Installed>,
    }
    let rows = pages(w, globals, "Hosts.List", HOSTS, |page: Page| {
        (page.hosts, page.next_page_token)
    })?;
    let mut macs: Vec<Mac> = rows
        .into_iter()
        .filter(|row| row.heartbeat.platform == DARWIN)
        .map(|row| Mac {
            id: row.node_id,
            group: row.host_group,
            installed: row.maintenance.map(|report| report.xcodes),
        })
        .collect();
    macs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(macs)
}

/// Each host group's maintenance settings, by name.
pub(super) fn policies(
    w: &mut World<'_>,
    globals: &Globals,
) -> Result<BTreeMap<String, Policy>, String> {
    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct Page {
        host_groups: Vec<State>,
        next_page_token: String,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct State {
        group: Group,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Group {
        name: String,
        policy: Rollout,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Rollout {
        maintenance: Settings,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct Settings {
        xcode_keep: usize,
        xcode_default: Option<String>,
        batch: Option<u32>,
    }
    let states = pages(w, globals, "HostGroups.List", GROUPS, |page: Page| {
        (page.host_groups, page.next_page_token)
    })?;
    Ok(states
        .into_iter()
        .map(|state| {
            let settings = state.group.policy.maintenance;
            let policy = Policy {
                keep: settings.xcode_keep,
                default: settings.xcode_default.filter(|text| text != "latest"),
                batch: settings.batch.unwrap_or(1),
            };
            (state.group.name, policy)
        })
        .collect())
}

/// The runs that have not ended, newest first, each with its hosts' turns.
pub(super) fn open_runs(w: &mut World<'_>, globals: &Globals) -> Result<Vec<Open>, String> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Listed {
        runs: Vec<Run>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Got {
        run: Run,
        hosts: Vec<Turn>,
    }
    let body = json!({"states": OPEN_STATES, "limit": PAGE});
    let listed: Listed = must(w, globals, "Maintenance.List", RUNS, &body)?;
    let mut open = Vec::new();
    for listed_run in listed.runs {
        let body = json!({"runId": listed_run.id});
        let got: Got = must(w, globals, "Maintenance.Get", RUN, &body)?;
        open.push(Open {
            run: got.run,
            turns: got.hosts,
        });
    }
    Ok(open)
}

/// What a run of the Xcode of `group`, named for `version`, would be: `Maintenance.Schedule` with
/// the kind `xcode` and no `confirm`, which stores nothing and orders no host.
pub(super) fn preview(
    w: &mut World<'_>,
    globals: &Globals,
    group: &str,
    version: &str,
) -> Result<Preview, String> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Answer {
        run: Run,
        hosts: Vec<Turn>,
    }
    let body = json!({
        "kinds": ["MAINTENANCE_KIND_XCODE"],
        "hostGroup": group,
        "reason": format!("Xcode {version}"),
    });
    let answer = call::<Answer>(w, globals, "Maintenance.Schedule", SCHEDULE, &body)?;
    Ok(match answer {
        Ok(answer) => Preview::Run {
            batch: answer.run.batch,
            turns: answer.hosts,
        },
        Err(refused) => Preview::Refused {
            code: String::from_utf8_lossy(&refused.code).into_owned(),
            reason: String::from_utf8_lossy(&refused.reason).into_owned(),
        },
    })
}

/// The command that stores the run of `group` for `version`, with `extra` flags after it.
pub(super) fn schedule_line(group: &str, version: &str, extra: &str) -> String {
    format!(
        "{PROGRAM} maintenance schedule --host-group {group} --kinds xcode --reason \"Xcode \
         {version}\" --confirm true{extra}"
    )
}
