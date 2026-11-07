//! The planner and the executor.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};

use crate::trust;
use crate::{
    Cmd, DRY_RUN_NOTE, GETENT, Host, Kind, NOTHING_TO_DO, Output, SYSTEMCTL, Trustee, USERADD,
};

/// Whether a plan is printed or carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Print each step; change nothing.
    DryRun,
    /// Carry each step out, printing it first.
    Apply,
}

/// A user and a group, by name; the ids are looked up when they are needed, so a plan can
/// name a user it is about to create.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// The user.
    pub user: String,
    /// The group.
    pub group: String,
}

impl Account {
    /// `user` and `group`.
    pub fn new(user: &str, group: &str) -> Account {
        Account {
            user: user.to_owned(),
            group: group.to_owned(),
        }
    }

    /// `root:root`.
    pub fn root() -> Account {
        Account::new("root", "root")
    }
}

/// What a systemctl step does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitVerb {
    /// `daemon-reload`, no units.
    DaemonReload,
    /// `enable`.
    Enable,
    /// `start`.
    Start,
    /// `restart`.
    Restart,
    /// `disable --now`.
    DisableNow,
}

impl UnitVerb {
    fn words(self) -> &'static [&'static str] {
        match self {
            UnitVerb::DaemonReload => &["daemon-reload"],
            UnitVerb::Enable => &["enable"],
            UnitVerb::Start => &["start"],
            UnitVerb::Restart => &["restart"],
            UnitVerb::DisableNow => &["disable", "--now"],
        }
    }
}

/// One change to a host. `Debug` prints the length of a file's bytes, never the bytes: a
/// plan that is formatted by an assertion, a panic or a log line cannot show a secret.
#[derive(Clone, PartialEq, Eq)]
pub enum Step {
    /// A directory with this mode and owner.
    Dir {
        /// Where.
        path: String,
        /// The permission bits.
        mode: u32,
        /// The owner.
        owner: Account,
    },
    /// A file with these bytes, mode and owner. The bytes are never printed.
    File {
        /// Where.
        path: String,
        /// The permission bits.
        mode: u32,
        /// The owner.
        owner: Account,
        /// The content.
        bytes: Vec<u8>,
    },
    /// No file at `path`.
    Absent {
        /// Where.
        path: String,
    },
    /// A system user (and a group of its name), with no login and no home made.
    User {
        /// The name.
        name: String,
        /// The home directory it is recorded with.
        home: String,
    },
    /// A systemctl action.
    Unit {
        /// The action.
        verb: UnitVerb,
        /// The units, none for a reload.
        units: Vec<String>,
    },
    /// A program.
    Run(Cmd),
}

impl fmt::Debug for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::Dir { path, mode, owner } => f
                .debug_struct("Dir")
                .field("path", path)
                .field("mode", &format_args!("{mode:04o}"))
                .field("owner", owner)
                .finish(),
            Step::File {
                path,
                mode,
                owner,
                bytes,
            } => f
                .debug_struct("File")
                .field("path", path)
                .field("mode", &format_args!("{mode:04o}"))
                .field("owner", owner)
                .field("bytes", &format_args!("{} bytes", bytes.len()))
                .finish(),
            Step::Absent { path } => f.debug_struct("Absent").field("path", path).finish(),
            Step::User { name, home } => f
                .debug_struct("User")
                .field("name", name)
                .field("home", home)
                .finish(),
            Step::Unit { verb, units } => f
                .debug_struct("Unit")
                .field("verb", verb)
                .field("units", units)
                .finish(),
            Step::Run(cmd) => f.debug_tuple("Run").field(cmd).finish(),
        }
    }
}

impl Step {
    /// The step's line in a plan: names, sizes and modes only.
    pub fn describe(&self) -> String {
        match self {
            Step::Dir { path, mode, owner } => format!(
                "mkdir {path} mode {mode:04o} owner {}:{}",
                owner.user, owner.group
            ),
            Step::File {
                path,
                mode,
                owner,
                bytes,
            } => format!(
                "write {path} mode {mode:04o} owner {}:{} ({} bytes)",
                owner.user,
                owner.group,
                bytes.len()
            ),
            Step::Absent { path } => format!("remove {path}"),
            Step::User { name, home } => {
                format!("useradd {name} (system user and group, home {home}, no login)")
            }
            Step::Unit { verb, units } => {
                format!("systemctl {}", unit_cmd(*verb, units).args.join(" "))
            }
            Step::Run(cmd) => format!("run {}", cmd.line()),
        }
    }
}

/// The systemctl command of a unit step; the units come after `--`, so a name that starts
/// with a hyphen is not read as an option.
fn unit_cmd(verb: UnitVerb, units: &[String]) -> Cmd {
    let cmd = Cmd::new(SYSTEMCTL).args(verb.words());
    match units {
        [] => cmd,
        _ => cmd.arg("--").args(units),
    }
}

/// The steps a host needs, in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// The steps.
    pub steps: Vec<Step>,
}

/// Why a plan could not be made.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// Looking at the host failed: a stat, a read, or a lookup command that could not be
    /// started. `what` names the operation and its path or name.
    #[error("{what}: {source}")]
    Host {
        /// The operation, as `stat PATH`, `read PATH` or `getent DATABASE NAME`.
        what: String,
        /// The host's error.
        #[source]
        source: io::Error,
    },
    /// The account a service is to run as exists and resolves to uid 0, so its services would
    /// run as root.
    #[error("refused: the user {user} exists with uid 0; a service does not run as root")]
    RootAccount {
        /// The user's name.
        user: String,
    },
}

/// `result`, with the operation `what` it came from.
fn looked<T>(what: impl FnOnce() -> String, result: io::Result<T>) -> Result<T, PlanError> {
    result.map_err(|source| PlanError::Host {
        what: what(),
        source,
    })
}

/// The ids of `account`; `None` when the user or the group does not exist.
///
/// # Errors
/// A lookup command could not be started ([`PlanError::Host`]).
pub fn ids(host: &dyn Host, account: &Account) -> Result<Option<(u32, u32)>, PlanError> {
    let uid = id(host, "passwd", &account.user)?;
    let gid = id(host, "group", &account.group)?;
    Ok(uid.zip(gid))
}

fn id(host: &dyn Host, database: &str, name: &str) -> Result<Option<u32>, PlanError> {
    // `getent passwd NAME` prints `name:x:uid:gid:...`, `getent group NAME` `name:x:gid:...`.
    let found = looked(
        || format!("getent {database} {name}"),
        host.run(&Cmd::new(GETENT).arg(database).arg(name)),
    )?;
    Ok(trust::id_field(&found))
}

/// The kind, the permission bits and the owner's ids (uid, gid) of a path.
type Shape = (Kind, u32, (u32, u32));

/// Looks at a host and records what would change it.
pub struct Planner<'h> {
    host: &'h dyn Host,
    steps: Vec<Step>,
}

impl<'h> Planner<'h> {
    /// A planner for `host` with no steps.
    pub fn new(host: &'h dyn Host) -> Planner<'h> {
        Planner {
            host,
            steps: Vec::new(),
        }
    }

    /// The plan made.
    pub fn finish(self) -> Plan {
        Plan { steps: self.steps }
    }

    /// The service user `name`, unless it exists. Whether a step was added. A user that exists
    /// is taken as it is, unless it resolves to uid 0: whatever its name, a service run as it
    /// would run as root.
    ///
    /// # Errors
    /// [`PlanError::RootAccount`]: `name` exists with uid 0. [`PlanError::Host`]: a lookup
    /// command could not be started.
    pub fn user(&mut self, name: &str, home: &str) -> Result<bool, PlanError> {
        let uid = id(self.host, "passwd", name)?;
        match uid {
            Some(0) => Err(PlanError::RootAccount {
                user: name.to_owned(),
            }),
            Some(_) => Ok(false),
            None => {
                self.steps.push(Step::User {
                    name: name.to_owned(),
                    home: home.to_owned(),
                });
                Ok(true)
            }
        }
    }

    /// What is at `path` now.
    fn seen(&self, path: &str) -> Result<Option<Shape>, PlanError> {
        let meta = looked(|| format!("stat {path}"), self.host.stat(path))?;
        Ok(meta.map(|m| (m.kind, m.mode, (m.uid, m.gid))))
    }

    /// The ids `owner` must have; ids no account has when it does not exist yet, so a path of
    /// a user the plan is about to make is planned.
    fn wanted(&self, owner: &Account) -> Result<(u32, u32), PlanError> {
        Ok(ids(self.host, owner)?.unwrap_or((u32::MAX, u32::MAX)))
    }

    /// The directory, unless it is there with this mode and owner. Whether a step was added.
    ///
    /// # Errors
    /// A stat or a lookup failed ([`PlanError::Host`]).
    pub fn dir(&mut self, path: &str, mode: u32, owner: &Account) -> Result<bool, PlanError> {
        let have = self.seen(path)?;
        let want = Some((Kind::Dir, mode, self.wanted(owner)?));
        let same = have == want;
        if !same {
            self.steps.push(Step::Dir {
                path: path.to_owned(),
                mode,
                owner: owner.clone(),
            });
        }
        Ok(!same)
    }

    /// The file, unless it is there with these bytes, this mode and owner. Whether a step
    /// was added.
    ///
    /// # Errors
    /// A stat, a read or a lookup failed ([`PlanError::Host`]).
    pub fn file(
        &mut self,
        path: &str,
        mode: u32,
        owner: &Account,
        bytes: &[u8],
    ) -> Result<bool, PlanError> {
        let have = self.seen(path)?;
        let want = Some((Kind::File, mode, self.wanted(owner)?));
        let content = looked(|| format!("read {path}"), self.host.read(path))?;
        let same = (have, content) == (want, Some(bytes.to_vec()));
        if !same {
            self.steps.push(Step::File {
                path: path.to_owned(),
                mode,
                owner: owner.clone(),
                bytes: bytes.to_vec(),
            });
        }
        Ok(!same)
    }

    /// No file at `path`, if there is one. Whether a step was added.
    ///
    /// # Errors
    /// The stat failed ([`PlanError::Host`]).
    pub fn absent(&mut self, path: &str) -> Result<bool, PlanError> {
        let there = self.seen(path)?.is_some();
        if there {
            self.steps.push(Step::Absent {
                path: path.to_owned(),
            });
        }
        Ok(there)
    }

    /// A systemctl action.
    pub fn unit(&mut self, verb: UnitVerb, units: &[&str]) {
        self.steps.push(Step::Unit {
            verb,
            units: units.iter().map(|u| (*u).to_owned()).collect(),
        });
    }

    /// A program.
    pub fn run(&mut self, cmd: Cmd) {
        self.steps.push(Step::Run(cmd));
    }

    /// Whether `systemctl <query> <unit>` succeeds (`is-enabled`, `is-active`).
    ///
    /// # Errors
    /// systemctl could not be started ([`PlanError::Host`]).
    pub fn unit_is(&self, query: &str, unit: &str) -> Result<bool, PlanError> {
        let out = looked(
            || format!("systemctl {query} {unit}"),
            self.host.run(&Cmd::new(SYSTEMCTL).arg(query).arg(unit)),
        )?;
        Ok(out.code == 0)
    }

    /// `unit` enabled, and restarted when `again`, else started when it is not active.
    /// Whether it is (re)started.
    ///
    /// # Errors
    /// systemctl could not be started ([`PlanError::Host`]).
    pub fn service(&mut self, unit: &str, again: bool) -> Result<bool, PlanError> {
        if !self.unit_is("is-enabled", unit)? {
            self.unit(UnitVerb::Enable, &[unit]);
        }
        let idle = !self.unit_is("is-active", unit)?;
        if again {
            self.unit(UnitVerb::Restart, &[unit]);
        } else if idle {
            self.unit(UnitVerb::Start, &[unit]);
        }
        Ok(again || idle)
    }
}

/// Why one step of a plan failed.
#[derive(Debug, thiserror::Error)]
pub enum StepError {
    /// The host refused the change, or the program could not be started.
    #[error(transparent)]
    Host(#[from] io::Error),
    /// The owner of a directory or a file could not be looked up.
    #[error(transparent)]
    Lookup(#[from] PlanError),
    /// The owner of a directory or a file does not exist.
    #[error("no user {user} or group {group}")]
    NoAccount {
        /// The user.
        user: String,
        /// The group.
        group: String,
    },
    /// The program ran and failed. Its output is not kept: a program may print a value it
    /// should not.
    #[error("exit status {0}")]
    Exit(i32),
}

/// Why a plan could not be carried out.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// Applying needs root.
    #[error("applying a plan needs root (effective uid is {0})")]
    NotRoot(u32),
    /// A step would be refused when it is carried out (a directory on its way is owned by a
    /// user not trusted for its path, as the steps before it leave the host); no step was
    /// carried out or printed.
    #[error("step {index} ({what}) would fail, so no step was carried out: {source}")]
    Refused {
        /// The step's position, from 1.
        index: usize,
        /// Its line in the plan.
        what: String,
        /// What is wrong.
        #[source]
        source: StepError,
    },
    /// A step failed; the steps before it were done.
    #[error("step {index} ({what}) failed: {source}")]
    Step {
        /// The step's position, from 1.
        index: usize,
        /// Its line in the plan.
        what: String,
        /// What went wrong.
        #[source]
        source: StepError,
    },
    /// The plan could not be written out.
    #[error("cannot write the plan out: {0}")]
    Write(#[from] io::Error),
}

/// Checks `plan`, then prints it, and with [`Mode::Apply`] carries it out step by step,
/// stopping at the first that fails.
///
/// The check is the same for a dry run and an apply, and comes before the first step is
/// printed or carried out: every directory, file and removal step is judged against the
/// owners the directories on its way will have when it runs ([`Host::way`] says who owns them
/// now; an earlier directory step of the plan sets the owner of its directory, and makes each
/// missing directory above it as the effective user's), so a plan that an apply would stop
/// part way is refused whole ([`ExecError::Refused`]) by both.
///
/// # Errors
/// [`ExecError`].
pub fn execute(
    plan: &Plan,
    mode: Mode,
    host: &dyn Host,
    out: &mut dyn Write,
) -> Result<(), ExecError> {
    if mode == Mode::Apply && host.euid() != 0 {
        return Err(ExecError::NotRoot(host.euid()));
    }
    check(plan, host)?;
    if plan.steps.is_empty() {
        writeln!(out, "{NOTHING_TO_DO}")?;
    }
    for (i, step) in plan.steps.iter().enumerate() {
        let what = step.describe();
        match mode {
            Mode::DryRun => writeln!(out, "DRY-RUN: {what}")?,
            Mode::Apply => {
                writeln!(out, "+ {what}")?;
                apply(host, step).map_err(|source| ExecError::Step {
                    index: i + 1,
                    what,
                    source,
                })?;
            }
        }
    }
    if mode == Mode::DryRun && !plan.steps.is_empty() {
        writeln!(out, "{DRY_RUN_NOTE}")?;
    }
    Ok(())
}

/// Who a directory on the way to a path is owned by when a step runs.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Who {
    /// The user with this id.
    Uid(u32),
    /// The user of this name, which does not exist yet: the plan makes it before the step.
    Made(String),
}

impl Who {
    /// The owner `account` is, by id when its user exists.
    fn of(host: &dyn Host, account: &Account) -> Result<Who, PlanError> {
        let uid = id(host, "passwd", &account.user)?;
        Ok(uid.map_or_else(|| Who::Made(account.user.clone()), Who::Uid))
    }

    /// Whether a directory this user owns may be on the way to a path that an operation
    /// asking for `owner` changes, when `trusted` are trusted there.
    fn trusted(&self, trusted: &[Trustee], owner: Option<&Who>) -> bool {
        owner == Some(self)
            || trusted.iter().any(|t| match (self, t) {
                (Who::Uid(a), Trustee::Uid(b)) => a == b,
                (Who::Made(a), Trustee::User(b)) => a == b,
                _ => false,
            })
    }

    /// The refusal of the directory `label`, owned by this user, on the way to `path`: the
    /// words of the walk that would refuse it.
    fn refusal(&self, path: &str, label: &str) -> io::Error {
        match self {
            Who::Uid(uid) => trust::untrusted(path, label, *uid),
            Who::Made(user) => trust::refused_at(
                path,
                &format!(
                    "{label} is to be owned by {user}, a user this plan makes, which is not trusted for this path"
                ),
            ),
        }
    }
}

/// Every step of `plan` judged as the host will judge it when it runs, before any runs.
fn check(plan: &Plan, host: &dyn Host) -> Result<(), ExecError> {
    // The owner each directory a step of the plan makes or changes has after that step.
    let mut planned: BTreeMap<String, Who> = BTreeMap::new();
    for (i, step) in plan.steps.iter().enumerate() {
        let judged = match step {
            Step::Dir { path, owner, .. } => judge(host, &mut planned, path, Some(owner), true)
                .map(|who| {
                    if let Some(who) = who {
                        planned.insert(path.clone(), who);
                    }
                }),
            Step::File { path, owner, .. } => {
                judge(host, &mut planned, path, Some(owner), false).map(drop)
            }
            Step::Absent { path } => judge(host, &mut planned, path, None, false).map(drop),
            Step::User { .. } | Step::Unit { .. } | Step::Run(_) => Ok(()),
        };
        judged.map_err(|source| ExecError::Refused {
            index: i + 1,
            what: step.describe(),
            source,
        })?;
    }
    Ok(())
}

/// The directories on the way to `path` judged for an operation asking for `owner`: each
/// owned as `planned` says, else as the host says, else (when `create`, as a directory step
/// makes the missing ones) by the effective user, which `planned` then records. The walk ends
/// at a directory that is neither there nor made: the step itself fails there. The owner, as
/// judged; `None` when the host judges no owner (then nothing is looked up).
fn judge(
    host: &dyn Host,
    planned: &mut BTreeMap<String, Who>,
    path: &str,
    owner: Option<&Account>,
    create: bool,
) -> Result<Option<Who>, StepError> {
    let Some(way) = host.way(path)? else {
        return Ok(None);
    };
    let owner = owner.map(|a| Who::of(host, a)).transpose()?;
    let names: Vec<&str> = path.split('/').filter(|n| !n.is_empty()).collect();
    let root = way.owners.first().map(|uid| Who::Uid(*uid));
    let mut on_way = vec![(way.root.clone(), String::new(), root)];
    let mut at = String::new();
    for (k, name) in names.iter().enumerate().take(names.len().saturating_sub(1)) {
        at = format!("{at}/{name}");
        let there = way.owners.get(k + 1).map(|uid| Who::Uid(*uid));
        on_way.push(((*name).to_owned(), at.clone(), there));
    }
    for (label, dir, there) in on_way {
        let made = (create && !dir.is_empty()).then(|| Who::Uid(host.euid()));
        let who = match planned.get(&dir).cloned().or(there) {
            Some(who) => who,
            None => match made {
                Some(who) => {
                    planned.insert(dir, who.clone());
                    who
                }
                None => break,
            },
        };
        if !who.trusted(&way.trusted, owner.as_ref()) {
            return Err(StepError::Host(who.refusal(path, &label)));
        }
    }
    Ok(owner)
}

fn apply(host: &dyn Host, step: &Step) -> Result<(), StepError> {
    match step {
        Step::Dir { path, mode, owner } => Ok(host.mkdir(path, *mode, owner_ids(host, owner)?)?),
        Step::File {
            path,
            mode,
            owner,
            bytes,
        } => Ok(host.write(path, *mode, owner_ids(host, owner)?, bytes)?),
        Step::Absent { path } => Ok(host.remove(path)?),
        Step::User { name, home } => {
            let cmd = Cmd::new(USERADD).args(&[
                "--system",
                "--user-group",
                "--home-dir",
                home,
                "--no-create-home",
                "--shell",
                "/usr/sbin/nologin",
                name,
            ]);
            succeeded(host.run(&cmd)?)
        }
        Step::Unit { verb, units } => succeeded(host.run(&unit_cmd(*verb, units))?),
        Step::Run(cmd) => succeeded(host.run(cmd)?),
    }
}

fn owner_ids(host: &dyn Host, owner: &Account) -> Result<(u32, u32), StepError> {
    ids(host, owner)?.ok_or_else(|| StepError::NoAccount {
        user: owner.user.clone(),
        group: owner.group.clone(),
    })
}

/// A program's exit status as a result. Its output is not repeated: a program may print a
/// value it should not.
fn succeeded(out: Output) -> Result<(), StepError> {
    match out.code {
        0 => Ok(()),
        code => Err(StepError::Exit(code)),
    }
}
