//! The runner: carries a plan out against a coordinator, with `hkctl` alone, and records each
//! event it sees.
//!
//! Each repetition starts with the plan's hosts, read from `hkctl hosts list` (refused when they
//! are not as many as the plan says), and the newest audit sequence number, after which the
//! audit is read. Then each step runs in order:
//! - a call runs `hkctl` once; a call that fails ends the repetition. With `audit`, the entry of
//!   that action the call wrote stamps the event with the coordinator's time;
//! - an operator step prints its instruction and waits for a line on standard input;
//! - a watch polls until its condition holds. Each poll that `hkctl` fails is a `call-failed`
//!   event and the watch goes on; at the timeout the watch records `timed-out` and ends the
//!   repetition, or goes on to the next step when the plan says so.
//!
//! An event observed by a poll carries the coordinator's stamp where the answer has one (the
//! host's last heartbeat, an audit entry's time, a host's first heartbeat or serving, a Mac's
//! enrollment); the runner's clock stamps every event too.
//!
//! [`crate::dry::dry_run`] prints what [`run`] would do, from the same plan and parameters, and
//! calls nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use serde_json::Value;
use thiserror::Error;

use crate::clock::Clock;
use crate::coordinator::{Answer, CallError, Coordinator};
use crate::fill::{check_files, render, render_json, render_until};
use crate::fleet::{self, HostView};
use crate::params::{ParamError, Params};
use crate::plan::{Action, Health, OnTimeout, Pick, Plan, REPETITION, Step, Until, Watch};
use crate::record::Record;
use crate::template::{self, TemplateError};

/// The gateway's name of a successful update.
const SUCCEEDED: &str = "OUTCOME_SUCCEEDED";
/// The most entries or hosts one page of `hkctl` answers.
const PAGE: &str = "256";

/// Why a run stopped before its end. The records written until then stay.
#[derive(Debug, Error)]
pub enum RunError {
    #[error(transparent)]
    Params(#[from] ParamError),
    #[error(transparent)]
    Call(#[from] CallError),
    #[error(transparent)]
    Template(#[from] TemplateError),
    #[error("{{{0}}} is not a slot of a step")]
    Slot(String),
    #[error("parameter {param:?}: reading {path}: {source}")]
    File {
        param: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`hkctl {command}` answered what is not the gateway's JSON: {why}")]
    Unreadable { command: String, why: String },
    #[error("`hkctl {command}` failed (exit {code}{reason}), and the run needs its answer")]
    Needed {
        command: String,
        code: i32,
        reason: String,
    },
    #[error("the plan's hosts: {want}, but the coordinator lists {got}")]
    Hosts { want: String, got: usize },
    #[error("step {step:?} names {{host}}, and no host is known yet")]
    NoHost { step: String },
    #[error("standard input ended while waiting for the operator")]
    NoOperator,
    #[error("reading standard input: {0}")]
    Input(#[source] io::Error),
    #[error("writing to the console: {0}")]
    Console(#[source] io::Error),
    #[error("writing a record: {0}")]
    Record(#[source] io::Error),
}

/// What a run talks to.
pub struct Io<'a> {
    pub coordinator: &'a mut dyn Coordinator,
    pub clock: &'a dyn Clock,
    /// Where the operator's lines come from.
    pub operator: &'a mut dyn BufRead,
    /// Where progress and instructions go.
    pub console: &'a mut dyn Write,
    /// Takes each record as it is made.
    pub records: &'a mut dyn FnMut(&Record) -> io::Result<()>,
    /// The names the records give the hosts; the caller keeps them whether the run ends or fails.
    pub names: &'a mut Names,
}

/// The names `host-<n>` the records give the hosts, in the order the run first saw them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names {
    order: Vec<String>,
}

impl Names {
    /// The name of `node`, given a new one on first sight.
    pub fn name(&mut self, node: &str) -> String {
        let index = match self.order.iter().position(|n| n == node) {
            Some(i) => i,
            None => {
                self.order.push(node.to_owned());
                self.order.len() - 1
            }
        };
        format!("host-{}", index + 1)
    }

    /// `hosts.tsv`: each node id and its name, a line each.
    pub fn tsv(&self) -> String {
        self.order
            .iter()
            .enumerate()
            .map(|(i, node)| format!("{node}\thost-{}\n", i + 1))
            .collect()
    }
}

/// How a run went.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub complete: u32,
    /// Each repetition that ended early, and why.
    pub ended: Vec<(u32, String)>,
}

/// Runs `plan` with `params`, handing each record to `io.records` as it is made.
pub fn run(plan: &Plan, params: &Params, io: Io<'_>) -> Result<Summary, RunError> {
    params.check(plan)?;
    check_files(plan, params)?;
    let mut runner = Runner { plan, params, io };
    let mut summary = Summary::default();
    for k in 1..=plan.repetitions {
        match runner.repetition(k)? {
            None => summary.complete += 1,
            Some(why) => summary.ended.push((k, why)),
        }
    }
    Ok(summary)
}

/// One repetition's state.
struct Rep {
    k: u32,
    scope: Vec<String>,
    host: Option<String>,
    /// The newest audit sequence number when the repetition started; its audit watches read
    /// the entries after it.
    start_seq: u64,
}

/// What a step leaves the repetition to do.
enum Flow {
    Next,
    End(String),
}

/// What a watch has seen so far.
#[derive(Default)]
struct State {
    /// The (node, event) pairs already recorded, each recorded once.
    seen: BTreeSet<(String, &'static str)>,
    last_beat: BTreeMap<String, u64>,
    /// The last heartbeats before the coordinator went away, once it has.
    gone: Option<BTreeMap<String, u64>>,
    failing: bool,
    devices: Option<BTreeMap<String, Option<u64>>>,
    /// The audit sequence number an audit watch reads after: its repetition's start, then each
    /// entry it has read.
    cursor: Option<u64>,
}

struct Runner<'p, 'a> {
    plan: &'p Plan,
    params: &'p Params,
    io: Io<'a>,
}

impl Runner<'_, '_> {
    fn emit(
        &mut self,
        rep: u32,
        step: &str,
        event: &str,
        node: Option<&str>,
        coordinator_unix: Option<u64>,
        detail: BTreeMap<String, String>,
    ) -> Result<(), RunError> {
        let host = node.map(|n| self.io.names.name(n));
        let record = Record {
            rep,
            step: step.to_owned(),
            event: event.to_owned(),
            host,
            runner_ms: self.io.clock.stamp_ms(),
            coordinator_unix,
            detail,
        };
        writeln!(
            self.io.console,
            "[repetition {rep}] {step}: {event}{}",
            record
                .host
                .as_ref()
                .map(|h| format!(" {h}"))
                .unwrap_or_default()
        )
        .map_err(RunError::Console)?;
        (self.io.records)(&record).map_err(RunError::Record)
    }

    fn call(&mut self, args: &[String], request: Option<&Value>) -> Result<Answer, RunError> {
        Ok(self.io.coordinator.call(args, request)?)
    }

    fn json(args: &[String], answer: &Answer) -> Result<Value, RunError> {
        serde_json::from_str(&answer.stdout).map_err(|e| RunError::Unreadable {
            command: args.join(" "),
            why: e.to_string(),
        })
    }

    /// A call whose answer the run needs: its failure stops the run.
    fn needed(&mut self, args: &[String]) -> Result<Value, RunError> {
        let answer = self.call(args, None)?;
        if answer.code != 0 {
            return Err(RunError::Needed {
                command: args.join(" "),
                code: answer.code,
                reason: answer
                    .refused()
                    .map(|r| format!(", refused: {r}"))
                    .unwrap_or_default(),
            });
        }
        Self::json(args, &answer)
    }

    /// Every page of a listing, or the answer of the call that failed.
    fn pages(&mut self, base: &[&str]) -> Result<Result<Vec<Value>, Answer>, RunError> {
        let mut out = Vec::new();
        let mut token = String::new();
        loop {
            let mut args: Vec<String> = base.iter().map(|s| (*s).to_owned()).collect();
            args.extend(["--limit".to_owned(), PAGE.to_owned()]);
            if !token.is_empty() {
                args.extend(["--page-token".to_owned(), token.clone()]);
            }
            let answer = self.call(&args, None)?;
            if answer.code != 0 {
                return Ok(Err(answer));
            }
            let page = Self::json(&args, &answer)?;
            token = page
                .get("nextPageToken")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            out.push(page);
            if token.is_empty() {
                return Ok(Ok(out));
            }
        }
    }

    fn hosts(&mut self) -> Result<Result<Vec<HostView>, Answer>, RunError> {
        Ok(self
            .pages(&["hosts", "list"])?
            .map(|pages| pages.iter().flat_map(|p| fleet::hosts_page(p).0).collect()))
    }

    fn newest_seq(&mut self) -> Result<u64, RunError> {
        let args = strings(&["audit", "list", "--newest", "true", "--limit", "1"]);
        Ok(fleet::audit_page(&self.needed(&args)?).1)
    }

    fn failed(&mut self, rep: u32, step: &str, answer: &Answer) -> Result<(), RunError> {
        let mut detail = BTreeMap::from([("code".to_owned(), answer.code.to_string())]);
        if let Some(reason) = answer.refused() {
            detail.insert("refused".to_owned(), reason.to_owned());
        }
        self.emit(rep, step, "call-failed", None, None, detail)
    }

    /// Runs repetition `k`; `Some(why)` when it ended early.
    fn repetition(&mut self, k: u32) -> Result<Option<String>, RunError> {
        let mut rep = Rep {
            k,
            scope: Vec::new(),
            host: None,
            start_seq: 0,
        };
        if self.plan.hosts.pick != Pick::None {
            rep.scope = self.scope()?;
            if self.plan.hosts.exactly == Some(1) {
                rep.host = rep.scope.first().cloned();
            }
        }
        if self.plan.reads_audit() {
            rep.start_seq = self.newest_seq()?;
        }
        let hosts = BTreeMap::from([("hosts".to_owned(), rep.scope.len().to_string())]);
        self.emit(k, REPETITION, "started", None, None, hosts)?;
        let mut ended = None;
        for step in &self.plan.steps {
            if let Flow::End(why) = self.step(step, &mut rep)? {
                ended = Some(why);
                break;
            }
        }
        let detail = BTreeMap::from([("complete".to_owned(), ended.is_none().to_string())]);
        let mut detail = detail;
        if let Some(why) = &ended {
            detail.insert("why".to_owned(), why.clone());
        }
        self.emit(k, REPETITION, "ended", None, None, detail)?;
        Ok(ended)
    }

    /// The node ids of the repetition's hosts.
    fn scope(&mut self) -> Result<Vec<String>, RunError> {
        let hosts = match self.hosts()? {
            Ok(hosts) => hosts,
            Err(answer) => {
                return Err(RunError::Needed {
                    command: "hosts list".to_owned(),
                    code: answer.code,
                    reason: answer
                        .refused()
                        .map(|r| format!(", refused: {r}"))
                        .unwrap_or_default(),
                });
            }
        };
        let scope: Vec<String> = hosts
            .into_iter()
            .filter(|h| match &self.plan.hosts.service {
                Some(s) => h.service(s).is_some(),
                None => h.live,
            })
            .map(|h| h.node)
            .collect();
        let want = self.plan.hosts.exactly;
        if scope.is_empty() || want.is_some_and(|w| usize::try_from(w).ok() != Some(scope.len())) {
            return Err(RunError::Hosts {
                want: want.map_or_else(|| "at least one".to_owned(), |w| format!("exactly {w}")),
                got: scope.len(),
            });
        }
        Ok(scope)
    }

    fn step(&mut self, step: &Step, rep: &mut Rep) -> Result<Flow, RunError> {
        let host = rep.host.clone();
        let host = host.as_deref();
        if host.is_none() && step_names_host(step) {
            return Err(RunError::NoHost {
                step: step.id.clone(),
            });
        }
        match &step.action {
            Action::Call {
                args,
                request,
                audit,
            } => {
                let args = args
                    .iter()
                    .map(|a| render(self.params, a, rep.k, host, false))
                    .collect::<Result<Vec<_>, _>>()?;
                let request = request
                    .as_ref()
                    .map(|r| render_json(self.params, r, rep.k, host, false))
                    .transpose()?;
                self.call_step(&step.id, &args, request.as_ref(), audit.as_deref(), rep)
            }
            Action::Operator { says } => {
                let says = render(self.params, says, rep.k, host, false)?;
                write!(
                    self.io.console,
                    "[repetition {}] {}: {says}\nPress Enter when it is done.\n",
                    rep.k, step.id
                )
                .map_err(RunError::Console)?;
                self.io.console.flush().map_err(RunError::Console)?;
                let mut line = String::new();
                if self
                    .io
                    .operator
                    .read_line(&mut line)
                    .map_err(RunError::Input)?
                    == 0
                {
                    return Err(RunError::NoOperator);
                }
                self.emit(rep.k, &step.id, "done", None, None, BTreeMap::new())?;
                Ok(Flow::Next)
            }
            Action::Watch(w) => {
                let until = render_until(self.params, &w.until, rep.k, host, false)?;
                self.watch(&step.id, w, &until, rep)
            }
        }
    }

    fn call_step(
        &mut self,
        step: &str,
        args: &[String],
        request: Option<&Value>,
        audit: Option<&str>,
        rep: &mut Rep,
    ) -> Result<Flow, RunError> {
        let before = match audit {
            Some(_) => Some(self.newest_seq()?),
            None => None,
        };
        let answer = self.call(args, request)?;
        if answer.code != 0 {
            self.failed(rep.k, step, &answer)?;
            return Ok(Flow::End(format!("{step} failed")));
        }
        let mut detail = BTreeMap::new();
        let mut stamp = None;
        if let (Some(action), Some(before)) = (audit, before) {
            let list = strings(&[
                "audit",
                "list",
                "--since-seq",
                &before.to_string(),
                "--action",
                action,
                "--limit",
                PAGE,
            ]);
            match fleet::audit_page(&self.needed(&list)?).0.first() {
                Some(entry) => {
                    stamp = entry.at_unix;
                    detail.insert("audit_seq".to_owned(), entry.seq.to_string());
                }
                None => {
                    detail.insert("audit".to_owned(), "not found".to_owned());
                }
            }
        }
        self.emit(rep.k, step, "called", None, stamp, detail)?;
        Ok(Flow::Next)
    }

    fn watch(
        &mut self,
        step: &str,
        w: &Watch,
        until: &Until,
        rep: &mut Rep,
    ) -> Result<Flow, RunError> {
        let mut state = State::default();
        let start = self.io.clock.mono_ms();
        let limit = w.timeout_secs.saturating_mul(1000);
        loop {
            if self.poll(step, until, &mut state, rep)? {
                return Ok(Flow::Next);
            }
            if self.io.clock.mono_ms().saturating_sub(start) >= limit {
                self.emit(rep.k, step, "timed-out", None, None, BTreeMap::new())?;
                return Ok(match w.on_timeout {
                    OnTimeout::EndRepetition => Flow::End(format!("{step} timed out")),
                    OnTimeout::Continue => Flow::Next,
                });
            }
            self.io.clock.sleep_ms(w.poll_secs.saturating_mul(1000));
        }
    }

    /// One poll of a watch; true when its condition holds.
    fn poll(
        &mut self,
        step: &str,
        until: &Until,
        st: &mut State,
        rep: &mut Rep,
    ) -> Result<bool, RunError> {
        match until {
            Until::Back => self.poll_back(step, st, rep),
            Until::Audit { actions, by_name } => {
                self.poll_audit(step, actions, by_name.as_deref(), st, rep)
            }
            Until::Beating => self.poll_beating(step, st, rep),
            Until::MdmEnrolled => self.poll_devices(step, st, rep),
            Until::Updated { .. } | Until::Health { .. } | Until::Live => {
                let hosts = match self.hosts()? {
                    Ok(hosts) => hosts,
                    Err(answer) => {
                        self.failed(rep.k, step, &answer)?;
                        return Ok(false);
                    }
                };
                let (events, goal) = host_events(until, &rep.scope, &hosts, st);
                for (node, event, stamp) in events {
                    self.emit(rep.k, step, event, Some(&node), stamp, BTreeMap::new())?;
                }
                Ok(rep
                    .scope
                    .iter()
                    .all(|n| st.seen.contains(&(n.clone(), goal))))
            }
        }
    }

    fn poll_back(&mut self, step: &str, st: &mut State, rep: &Rep) -> Result<bool, RunError> {
        let hosts = match self.hosts()? {
            Ok(hosts) => hosts,
            Err(answer) => {
                self.failed(rep.k, step, &answer)?;
                if st.gone.is_none() {
                    st.gone = Some(st.last_beat.clone());
                    self.emit(rep.k, step, "restart-seen", None, None, BTreeMap::new())?;
                }
                st.failing = true;
                return Ok(false);
            }
        };
        if st.failing {
            st.failing = false;
            self.emit(rep.k, step, "answered", None, None, BTreeMap::new())?;
        }
        let find = |node: &str| hosts.iter().find(|h| h.node == node);
        if st.gone.is_none() && rep.scope.iter().any(|n| !find(n).is_some_and(|h| h.live)) {
            st.gone = Some(st.last_beat.clone());
            self.emit(rep.k, step, "restart-seen", None, None, BTreeMap::new())?;
        }
        for node in &rep.scope {
            let Some(host) = find(node) else { continue };
            let Some(beat) = host.last_heartbeat else {
                continue;
            };
            if st.last_beat.get(node) != Some(&beat) {
                st.last_beat.insert(node.clone(), beat);
                self.emit(
                    rep.k,
                    step,
                    "heartbeat",
                    Some(node),
                    Some(beat),
                    BTreeMap::new(),
                )?;
            }
            let before = st.gone.as_ref().map(|g| g.get(node).copied().unwrap_or(0));
            if before.is_some_and(|b| host.live && beat > b)
                && st.seen.insert((node.clone(), "back"))
            {
                self.emit(rep.k, step, "back", Some(node), Some(beat), BTreeMap::new())?;
            }
        }
        Ok(st.gone.is_some()
            && rep
                .scope
                .iter()
                .all(|n| st.seen.contains(&(n.clone(), "back"))))
    }

    fn poll_audit(
        &mut self,
        step: &str,
        actions: &[String],
        by_name: Option<&str>,
        st: &mut State,
        rep: &mut Rep,
    ) -> Result<bool, RunError> {
        let cursor = *st.cursor.get_or_insert(rep.start_seq);
        let args = strings(&[
            "audit",
            "list",
            "--since-seq",
            &cursor.to_string(),
            "--limit",
            PAGE,
        ]);
        let answer = self.call(&args, None)?;
        if answer.code != 0 {
            self.failed(rep.k, step, &answer)?;
            return Ok(false);
        }
        let (entries, next) = fleet::audit_page(&Self::json(&args, &answer)?);
        for entry in entries {
            st.cursor = Some(cursor.max(entry.seq));
            let by = by_name.is_none_or(|b| entry.by_name.as_deref() == Some(b));
            if by && actions.contains(&entry.action) {
                if entry.node.is_some() {
                    rep.host.clone_from(&entry.node);
                }
                let detail = BTreeMap::from([("action".to_owned(), entry.action.clone())]);
                self.emit(
                    rep.k,
                    step,
                    "seen",
                    entry.node.as_deref(),
                    entry.at_unix,
                    detail,
                )?;
                return Ok(true);
            }
        }
        st.cursor = Some(cursor.max(next));
        Ok(false)
    }

    fn poll_beating(&mut self, step: &str, st: &mut State, rep: &Rep) -> Result<bool, RunError> {
        let node = rep.host.clone().unwrap_or_default();
        let args = strings(&["hosts", "get", "--node-id", &node]);
        let answer = self.call(&args, None)?;
        if answer.code != 0 {
            self.failed(rep.k, step, &answer)?;
            return Ok(false);
        }
        let host = HostView::read(
            Self::json(&args, &answer)?
                .get("host")
                .unwrap_or(&Value::Null),
        );
        for (event, stamp) in [
            ("first-heartbeat", host.first_heartbeat),
            ("first-serving", host.first_serving),
        ] {
            if stamp.is_some() && st.seen.insert((node.clone(), event)) {
                self.emit(rep.k, step, event, Some(&node), stamp, BTreeMap::new())?;
            }
        }
        Ok(st.seen.len() == 2)
    }

    fn poll_devices(&mut self, step: &str, st: &mut State, rep: &Rep) -> Result<bool, RunError> {
        let pages = match self.pages(&["mdm", "list-devices"])? {
            Ok(pages) => pages,
            Err(answer) => {
                self.failed(rep.k, step, &answer)?;
                return Ok(false);
            }
        };
        let devices: BTreeMap<String, Option<u64>> = pages
            .iter()
            .flat_map(|p| fleet::devices_page(p).0)
            .map(|d| (d.id, d.enrolled_since))
            .collect();
        let Some(known) = &st.devices else {
            st.devices = Some(devices);
            return Ok(false);
        };
        let new = devices
            .iter()
            .find(|(id, since)| since.is_some() && known.get(*id) != Some(since))
            .map(|(_, since)| *since);
        match new {
            Some(since) => {
                self.emit(rep.k, step, "enrolled", None, since, BTreeMap::new())?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// A host's event seen by a poll: the node, the event and the coordinator's stamp.
type Sighting = (String, &'static str, Option<u64>);

/// Whether a step's texts name `{host}`.
fn step_names_host(step: &Step) -> bool {
    crate::plan::step_texts(&step.action)
        .iter()
        .any(|t| template::slots(t).is_ok_and(|slots| slots.iter().any(|s| s == "host")))
}

/// The events one listing of the hosts shows for a watch of updates, health or liveness, each
/// (node, event) once, and the event that marks a host done.
fn host_events(
    until: &Until,
    scope: &[String],
    hosts: &[HostView],
    st: &mut State,
) -> (Vec<Sighting>, &'static str) {
    let mut out = Vec::new();
    let goal = match until {
        Until::Updated { .. } => "updated",
        Until::Health { .. } => "health",
        _ => "live",
    };
    for node in scope {
        let Some(host) = hosts.iter().find(|h| h.node == *node) else {
            continue;
        };
        let mut found: Vec<&'static str> = Vec::new();
        match until {
            Until::Updated {
                service,
                version,
                healthy,
            } => {
                if let Some(s) = host.service(service) {
                    if s.holds_slot || s.updating.as_deref() == Some(version) {
                        found.push("ordered");
                    }
                    let at = |v: &Option<String>| v.as_deref() == Some(version.as_str());
                    if at(&s.installed)
                        && at(&s.last_target)
                        && s.last_outcome.as_deref() == Some(SUCCEEDED)
                        && (!healthy || s.health == Health::Healthy.gateway())
                    {
                        found.push("updated");
                    }
                }
            }
            Until::Health { service, health } => {
                if host
                    .service(service)
                    .is_some_and(|s| s.health == health.gateway())
                {
                    found.push("health");
                }
            }
            _ => {
                if host.live {
                    found.push("live");
                }
            }
        }
        for event in found {
            if st.seen.insert((node.clone(), event)) {
                out.push((node.clone(), event, host.last_heartbeat));
            }
        }
    }
    (out, goal)
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}
