//! The step runner: one update of one service, from `prepare` to its verdict, through a
//! [`StepLauncher`], with every move written to the step record first.
//!
//! The runner runs inside the daemon, the one root process of the host
//! (docs/design/hostkeeper-design.md, 6.2). It asks [`hostkeeper_hook::next`] what follows each
//! step, it runs nothing that is not in a staged release it has checked against the digest the
//! updater verified, and a crash at any point leaves a record that [`StepRunner::run`] picks up: a step found `running` has its unit
//! stopped by name and is run again, a step found `done` is not.
//!
//! The daemon's own moves around the hook's steps, `current` and the placed links switched to
//! the target immediately before `cutover` and back immediately before `rollback` (design
//! section 5.2, steps 5 and 6), come in through [`StepRunner::with_switch`]: the runner calls the
//! [`Switch`] before every run of those two steps, resumed ones included, so the switch must be
//! idempotent. A third call, [`Toward::Confirmed`], comes once a `cleanup` has succeeded and
//! before the verdict is written (and before the post-success watcher begins): if it fails the
//! runner stops with the record still at `cleanup` done, so the next run calls it again.
//! A `force` of the service may flag the record `forced` while an update runs (design section
//! 5.6, steps 4 to 6). The runner reads the flag on disk, under the record lock, before it
//! launches any step (a resumed one included) and in every write after a step returns: once it
//! is set, the update is closed `Failed`, `forced_over` ([`Record::close_forced_over`]), its
//! params file is removed, and nothing more is launched, `rollback` and `cleanup` included.

use std::io;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_config::names::{MAX_UNIT_NAME, is_service_name};
use hostkeeper_hook::{
    ARTIFACT_DIGEST_KEY, Context, Invocation, Next, Outcome, PREVIOUS_RELEASE_KEY,
    PREVIOUS_VERSION_KEY, Params, Phase, Report, STAGED_RELEASE_KEY, Scalar, Step,
    TARGET_VERSION_KEY, Verdict, next,
};

use crate::artifact::{ArtifactError, Staged};
use crate::clock::{Clock, Stamp};
use crate::deadman::Watch;
use crate::launcher::{Bound, LaunchError, StepCall, StepExit, StepLauncher};
use crate::record::{HookSel, Loaded, Progress, Record, RecordError, RecordStore, SCHEMA, State};
use crate::systemd::unit_name;

/// The protocol version of this updater: an order's `min_updater_protocol` above it is
/// `Deferred` with the reason `updater_old` until the updater has updated itself.
pub const UPDATER_PROTOCOL: u32 = 1;
/// How often a deferred `rollback` is run again before it is read as a failure.
pub const ROLLBACK_ATTEMPTS: u32 = 3;
/// The wait before a deferred step is run again when the hook gave no `retry_after_secs`.
pub const DEFAULT_RETRY_SECS: u64 = 60;
/// The wait between two runs of `verify` by the post-success watcher when the hook gave none.
pub const WATCH_POLL_SECS: u64 = 30;
/// The reason of a `halt` for a staged release that no longer has its digest.
pub const REASON_ARTIFACT_CHANGED: &str = "artifact_changed";
/// The longest wait a hook can ask for between two runs of a step.
pub const MAX_RETRY_SECS: u64 = 3600;
/// The reason a cleared halt is recorded with.
pub const REASON_HALT_CLEARED: &str = "halt_cleared";
/// The reason of a `Deferred` for a hook that needs a newer updater.
pub const REASON_UPDATER_OLD: &str = "updater_old";
/// The reason of a `rollback` that declines a first install: there is no previous release to go
/// back to (design section 5.7, step 6).
pub const REASON_NO_PREVIOUS_RELEASE: &str = "no_previous_release";

/// Which way the daemon switches a service around a step of the hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toward {
    /// To the target, immediately before `cutover`.
    Target,
    /// Back, immediately before `rollback`.
    Back,
    /// After a `cleanup` that succeeded, before the verdict is written: the daemon commits the
    /// placed paths and unlinks what the version no longer lists. It is called again by a run
    /// that finds the `cleanup` done, so it must be idempotent.
    Confirmed,
}

/// The daemon's switch of `current` and the placed links around the hook's steps
/// ([`StepRunner::with_switch`]): called with [`Toward::Target`] before each run of `cutover`,
/// [`Toward::Back`] before each run of `rollback` and [`Toward::Confirmed`] after a `cleanup`
/// that succeeded, with the order. `Err` says why it could not switch: the step does not run (for
/// `Confirmed`, the verdict is not written), and the runner stops with [`RunError::Switch`].
pub type Switch = Arc<dyn Fn(Toward, &UpdateOrder) -> Result<(), String> + Send + Sync>;

/// The bounds of the manifest's `hook.timeout_secs` and the other hook keys the runner reads (and
/// no other key of it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// `0` is no bound ([`Bound::Unbounded`]); `0` for any other step is a step not started.
    pub prepare_secs: u64,
    pub cutover_secs: u64,
    pub verify_secs: u64,
    pub rollback_secs: u64,
    pub cleanup_secs: u64,
    /// After `cutover` succeeded, `verify` must succeed by then or the update rolls back.
    pub verify_deadline_secs: u64,
    /// The ceiling on a pending promotion; also written to the params file.
    pub promotion_ceiling_secs: u64,
    /// How often a failed `cleanup` is run again.
    pub cleanup_retries: u32,
}

impl Bounds {
    /// The bound of one step.
    pub fn timeout(&self, step: Step) -> u64 {
        match step {
            Step::Prepare => self.prepare_secs,
            Step::Cutover => self.cutover_secs,
            Step::Verify => self.verify_secs,
            Step::Rollback => self.rollback_secs,
            Step::Cleanup => self.cleanup_secs,
            // Not a step of an update: the runner never runs it, and it has no bound here.
            Step::Uninstall => 0,
        }
    }

    /// The bound of one step as it runs: [`Bound::Unbounded`] for a `prepare` of 0, the seconds
    /// otherwise, and `None` for 0 of any other step (a step with nothing left of its bound is
    /// not started).
    pub fn bound(&self, step: Step) -> Option<Bound> {
        match (step, NonZeroU64::new(self.timeout(step))) {
            (_, Some(secs)) => Some(Bound::Secs(secs)),
            (Step::Prepare, None) => Some(Bound::Unbounded),
            (_, None) => None,
        }
    }
}

/// One update of one service.
#[derive(Clone, Debug)]
pub struct UpdateOrder {
    /// The service, a name that keeps [`hostkeeper_config::names::is_service_name`].
    pub service: String,
    pub target_version: String,
    pub previous_version: Option<String>,
    /// The sha256 of the release artifact the updater verified (its attested tarball): 64
    /// lower-case hex digits, the value in the order and in the release's SHA256SUMS. It is
    /// written to every step's params file as `updater.artifact_digest` = `sha256:<hex>`, and an
    /// order with another artifact digest is another update even for the same tree.
    ///
    /// **This crate does not bind the digest to the unpacked bytes.** The step that unpacks the
    /// tarball must hash the bytes as it reads them and unpack from that same stream, so that the
    /// digest is of what was unpacked, then seal the tree with [`Staged::seal`].
    pub artifact_digest: String,
    /// The verified staged release (the unpacked tree of that artifact).
    pub staged: Staged,
    /// The release being replaced, kept staged until `cleanup` has succeeded; `None` on the first
    /// install.
    pub previous: Option<Staged>,
    /// The hook executable within a release: the program of the manifest's `[hook] command`.
    pub hook_binary: PathBuf,
    pub bounds: Bounds,
    /// The manifest's `[params]` table.
    pub declared_params: Params,
    /// The lowest updater protocol the release asks for; `None` asks for none.
    pub min_updater_protocol: Option<u32>,
    /// The coordinator's try of the order (`Order.try`), kept in the record as `order_try`;
    /// `None` for an update that came without one.
    pub order_try: Option<u64>,
}

/// How an update ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub verdict: Verdict,
    /// The hook's `reason` token, or the updater's own ([`REASON_ARTIFACT_CHANGED`],
    /// [`REASON_UPDATER_OLD`]).
    pub reason: Option<String>,
}

/// Why the runner stopped without a verdict. The record says where it was; calling
/// [`StepRunner::run`] again resumes.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Record(#[from] RecordError),
    #[error(transparent)]
    Launch(#[from] LaunchError),
    #[error("the order is not valid: {0}")]
    BadOrder(&'static str),
    /// The service name keeps its rule, but a unit a step of it runs in,
    /// `updater-<service>-<step>-<generation>.service`, would be longer than
    /// [`MAX_UNIT_NAME`] bytes for some step and generation. Nothing ran.
    #[error(
        "the service name {service:?} is too long for the units its steps run in: \
         `updater-<service>-<step>-<generation>.service` would pass {MAX_UNIT_NAME} bytes"
    )]
    UnitNameTooLong { service: String },
    #[error("an update of {service} to {target} is not finished")]
    Busy { service: String, target: String },
    #[error("the step record has schema {0}, newer than this updater writes")]
    RecordNewer(u64),
    /// The staged release could not be read (out of descriptors, an I/O error): nothing ran and
    /// the step is still `pending` in the record. Calling [`StepRunner::run`] again checks the
    /// release again.
    #[error(
        "the staged release cannot be read now; nothing ran and the step is still pending: {0}"
    )]
    Unreadable(ArtifactError),
    /// The switch before `cutover` or `rollback` failed ([`StepRunner::with_switch`]): nothing of
    /// the step ran and it is still `pending` in the record. Calling [`StepRunner::run`] again
    /// switches again.
    #[error("the switch before the step failed; nothing ran and the step is still pending: {0}")]
    Switch(String),
    /// The dead-man could not stop the step at its bound, and the step ended later: it is left
    /// `running` in the record, so that [`StepRunner::run`] again stops its unit by name before
    /// anything else and runs the step again. `error` is the launcher's, as its stop gave it.
    #[error("the dead-man could not stop {unit} at its bound ({error}); the step is left running")]
    Unstopped {
        unit: String,
        #[source]
        error: Arc<LaunchError>,
    },
}

/// A test's way to make the check of the staged release fail with an I/O error before a step:
/// `Some` for the step whose check fails. It can only make a check fail, never pass.
pub type CheckFault = Arc<dyn Fn(Step) -> Option<io::Error> + Send + Sync>;

enum Ran {
    Exit(StepExit),
    NoStart,
}

/// Runs the updates of one service.
pub struct StepRunner {
    launcher: Arc<dyn StepLauncher>,
    clock: Arc<dyn Clock>,
    store: RecordStore,
    watch: Arc<Watch>,
    fault: Option<CheckFault>,
    switch: Option<Switch>,
    /// [`StepRunner::with_held_success`].
    held_success: bool,
}

impl StepRunner {
    pub fn new(
        launcher: Arc<dyn StepLauncher>,
        clock: Arc<dyn Clock>,
        store: RecordStore,
    ) -> StepRunner {
        StepRunner {
            launcher,
            clock,
            store,
            watch: Watch::new(),
            fault: None,
            switch: None,
            held_success: false,
        }
    }

    /// The same runner, the `Succeeded` of an update that replaces a release (one with a
    /// previous release) returned and never written: the record stays at the step that reached
    /// it (`done` at `cleanup`), unfinished, for the caller to finish. This is the updater's own
    /// update (design section 6.6), which only its new binary may record as succeeded, once it
    /// has run ([`crate::selfupdate::follow_on`], [`crate::selfupdate::settle`]). A run of the
    /// same order again recomputes the same verdict from the record and launches nothing.
    pub fn with_held_success(mut self) -> StepRunner {
        self.held_success = true;
        self
    }

    /// The same runner, `switch` called before every run of `cutover` and of `rollback`.
    pub fn with_switch(mut self, switch: Switch) -> StepRunner {
        self.switch = Some(switch);
        self
    }

    /// The switch before `step`, if the runner has one and the step has one.
    fn switch_for(&self, order: &UpdateOrder, step: Step) -> Result<(), RunError> {
        let toward = match step {
            Step::Cutover => Toward::Target,
            Step::Rollback => Toward::Back,
            _ => return Ok(()),
        };
        match &self.switch {
            Some(switch) => switch(toward, order).map_err(RunError::Switch),
            None => Ok(()),
        }
    }

    /// The confirming call of the switch, if the runner has one.
    fn confirm(&self, order: &UpdateOrder) -> Result<(), RunError> {
        match &self.switch {
            Some(switch) => switch(Toward::Confirmed, order).map_err(RunError::Switch),
            None => Ok(()),
        }
    }

    /// The same runner, its check of the staged release failed wherever `fault` says: the seam of
    /// the tests of a release that cannot be read for a moment.
    pub fn with_check_fault(mut self, fault: CheckFault) -> StepRunner {
        self.fault = Some(fault);
        self
    }

    /// What the dead-man watches: give it to [`crate::DeadMan::new`].
    pub fn watch(&self) -> Arc<Watch> {
        Arc::clone(&self.watch)
    }

    /// Clears a `halt`: the next order runs. `false` when the record holds no halt.
    pub fn clear_halt(&self) -> Result<bool, RunError> {
        let mut rec = match self.store.load()? {
            Loaded::Newer { schema } => return Err(RunError::RecordNewer(schema)),
            Loaded::Absent => return Ok(false),
            Loaded::Current(rec) => rec,
        };
        let halted = matches!(
            rec.state,
            State::Finished {
                verdict: Verdict::Halt,
                ..
            }
        );
        if !halted {
            return Ok(false);
        }
        rec.state = State::Finished {
            verdict: Verdict::Deferred {
                retry_after_secs: None,
            },
            reason: Some(REASON_HALT_CLEARED.to_owned()),
        };
        self.store.save(&rec)?;
        Ok(true)
    }

    /// How the last update of this service ended, `None` when none has or one is under way.
    pub fn last_result(&self) -> Result<Option<Finished>, RunError> {
        Ok(match self.store.load()? {
            Loaded::Current(rec) => match rec.state {
                State::Finished { verdict, reason } => Some(Finished { verdict, reason }),
                _ => None,
            },
            Loaded::Absent => None,
            Loaded::Newer { schema } => return Err(RunError::RecordNewer(schema)),
        })
    }

    /// Runs the update to its verdict, or resumes the unfinished one the record holds.
    ///
    /// An order for a different update while one is unfinished is [`RunError::Busy`], except
    /// that a post-success watcher is stopped by a new order (3.2). A record that is finished
    /// starts the next generation: the verdict of a finished update is read with
    /// [`StepRunner::last_result`].
    pub fn run(&self, order: &UpdateOrder) -> Result<Finished, RunError> {
        self.run_with(order, false)
    }

    /// As [`StepRunner::run`], but the post-success watcher is not run here: once the update has
    /// reached it (or at once, when the record is already in it) this returns
    /// `Succeeded { promotion_pending: true }` and leaves the record in the watcher, for a later
    /// [`StepRunner::run`] of the same order to watch. The daemon's start pass runs each
    /// unfinished update with it, so that a watcher, which may run for as long as the promotion
    /// ceiling, does not hold up the start (design section 6.5).
    pub fn run_until_watching(&self, order: &UpdateOrder) -> Result<Finished, RunError> {
        self.run_with(order, true)
    }

    fn run_with(&self, order: &UpdateOrder, handoff: bool) -> Result<Finished, RunError> {
        let staged = path_str(order.staged.dir())?;
        let previous = order
            .previous
            .as_ref()
            .map(|p| path_str(p.dir()))
            .transpose()?;
        if !is_service_name(&order.service) {
            return Err(RunError::BadOrder("the service name"));
        }
        if longest_step_unit(&order.service) > MAX_UNIT_NAME {
            return Err(RunError::UnitNameTooLong {
                service: order.service.clone(),
            });
        }
        let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        if order.artifact_digest.len() != 64 || !order.artifact_digest.bytes().all(hex) {
            return Err(RunError::BadOrder("the artifact digest"));
        }
        if order
            .min_updater_protocol
            .is_some_and(|needed| needed > UPDATER_PROTOCOL)
        {
            return Ok(Finished {
                verdict: Verdict::Deferred {
                    retry_after_secs: None,
                },
                reason: Some(REASON_UPDATER_OLD.to_owned()),
            });
        }
        self.store.check_private()?;
        let generation = match self.store.load()? {
            Loaded::Newer { schema } => return Err(RunError::RecordNewer(schema)),
            Loaded::Absent => 1,
            Loaded::Current(rec) => {
                // A verdict stands: the same order again (the same update, and the same try of
                // it) is answered with it and runs nothing, and a halt holds the service for every
                // order until `clear_halt`. A `Deferred` is run again, because the order comes
                // back after its wait; so is an update the coordinator orders again with another
                // try, a new order of the same package.
                if let State::Finished { verdict, reason } = &rec.state {
                    let ended =
                        matches!(verdict, Verdict::Succeeded { .. } | Verdict::Failed { .. });
                    let again = same_update(&rec, order) & (rec.order_try == order.order_try);
                    if matches!(verdict, Verdict::Halt) || (ended && again) {
                        // The updater may have died between the verdict and the removal.
                        self.store.remove_params()?;
                        return Ok(Finished {
                            verdict: verdict.clone(),
                            reason: reason.clone(),
                        });
                    }
                }
                let unfinished = !matches!(rec.state, State::Finished { .. });
                if unfinished && same_update(&rec, order) {
                    return self.advance(order, *rec, handoff);
                }
                if unfinished && rec.progress.watching.is_none() {
                    return Err(RunError::Busy {
                        service: rec.service,
                        target: rec.target_version,
                    });
                }
                // A watcher that is stopped by a new order may have a `verify` still alive.
                if matches!(rec.state, State::Running { .. }) {
                    self.launcher.stop(&rec.unit(rec.step))?;
                }
                rec.generation + 1
            }
        };
        let rec = self.begin(order, generation, staged, previous)?;
        self.advance(order, rec, handoff)
    }

    /// Writes the params file and the first record of an update.
    fn begin(
        &self,
        order: &UpdateOrder,
        generation: u64,
        staged: &str,
        previous: Option<&str>,
    ) -> Result<Record, RunError> {
        let mut params = order
            .declared_params
            .clone()
            .with_promotion_ceiling_secs(order.bounds.promotion_ceiling_secs);
        let text = |s: &str| Scalar::Text(s.to_owned());
        params.insert(STAGED_RELEASE_KEY, text(staged));
        params.insert(TARGET_VERSION_KEY, text(&order.target_version));
        params.insert(
            ARTIFACT_DIGEST_KEY,
            text(&format!("sha256:{}", order.artifact_digest)),
        );
        if let Some(previous) = previous {
            params.insert(PREVIOUS_RELEASE_KEY, text(previous));
        }
        if let Some(version) = &order.previous_version {
            params.insert(PREVIOUS_VERSION_KEY, text(version));
        }
        self.store.save_params(&params.to_toml())?;
        let rec = Record {
            schema: SCHEMA,
            service: order.service.clone(),
            target_version: order.target_version.clone(),
            artifact_digest: order.staged.digest().to_owned(),
            tarball_digest: order.artifact_digest.clone(),
            previous_digest: order.previous.as_ref().map(|p| p.digest().to_owned()),
            generation,
            params: self.store.params_path(),
            step: Step::Prepare,
            attempt: 1,
            state: State::Pending,
            hook: HookSel::New,
            progress: Progress::default(),
            order_try: order.order_try,
            forced: false,
            force: None,
        };
        self.store.save(&rec)?;
        Ok(rec)
    }

    fn advance(
        &self,
        order: &UpdateOrder,
        mut rec: Record,
        handoff: bool,
    ) -> Result<Finished, RunError> {
        loop {
            let finished = matches!(rec.state, State::Finished { .. });
            // A force flagged the update: it ends here, before anything else is launched.
            if !finished && self.forced(&mut rec)? {
                continue;
            }
            // The watcher is left to a later run ([`StepRunner::run_until_watching`]).
            if handoff & rec.progress.watching.is_some() & !finished {
                return Ok(Finished {
                    verdict: Verdict::Succeeded {
                        promotion_pending: true,
                    },
                    reason: None,
                });
            }
            match rec.state.clone() {
                State::Finished { verdict, reason } => {
                    self.store.remove_params()?;
                    return Ok(Finished { verdict, reason });
                }
                State::Running { .. } => {
                    // The updater died with the step's unit possibly alive: stop it by name and
                    // wait for it to end before the step is started again (3.5).
                    self.launcher.stop(&rec.unit(rec.step))?;
                    rec.attempt = rec.attempt.saturating_add(1);
                    rec.state = State::Pending;
                    self.write(&mut rec)?;
                }
                State::Pending => self.start(order, &mut rec)?,
                State::Done { code, last_line } => self.after(order, &mut rec, code, last_line)?,
            }
        }
    }

    /// Checks the release, records the step as running, runs it, records how it ended.
    fn start(&self, order: &UpdateOrder, rec: &mut Record) -> Result<(), RunError> {
        let release = match rec.hook {
            HookSel::New => &order.staged,
            HookSel::Previous => order.previous.as_ref().unwrap_or(&order.staged),
        };
        let checked = match self.fault.as_ref().and_then(|fault| fault(rec.step)) {
            Some(error) => Err(ArtifactError::Io(error)),
            None => release.check(&order.hook_binary),
        };
        let ran = match checked {
            Ok(executable) => match self.launch(order, rec, &executable)? {
                Some(ran) => ran,
                // A force flagged the update before its step was launched: it is closed.
                None => return Ok(()),
            },
            // The hook cannot be started: a failed step, and for `rollback` the one case to go
            // to the previous release's hook (3.3).
            Err(ArtifactError::BadHookPath(_) | ArtifactError::HookMissing(_)) => Ran::NoStart,
            // The tree could not be read (out of descriptors, an I/O error): that passes, and it
            // says nothing about the tree. Nothing runs, nothing is written: the step is still
            // pending, and the next run checks again.
            Err(error @ ArtifactError::Io(_)) => return Err(RunError::Unreadable(error)),
            // The tree is not what was verified: nothing from it runs.
            Err(
                ArtifactError::Changed { .. }
                | ArtifactError::Untrusted(_)
                | ArtifactError::Unsupported(_),
            ) => {
                rec.state = State::Finished {
                    verdict: Verdict::Halt,
                    reason: Some(REASON_ARTIFACT_CHANGED.to_owned()),
                };
                return self.write(rec);
            }
        };
        rec.state = match ran {
            Ran::Exit(exit) => State::Done {
                code: exit.code,
                last_line: exit.last_line,
            },
            Ran::NoStart => {
                if rec.step == Step::Rollback
                    && rec.hook == HookSel::New
                    && order.previous.is_some()
                {
                    rec.hook = HookSel::Previous;
                    State::Pending
                } else {
                    State::Done {
                        code: None,
                        last_line: None,
                    }
                }
            }
        };
        self.write(rec)
    }

    /// Launches the step `rec` is at. `None`, and nothing launched, when the write of the record
    /// as `running` found it flagged by a force: the record is closed instead.
    fn launch(
        &self,
        order: &UpdateOrder,
        rec: &mut Record,
        executable: &Path,
    ) -> Result<Option<Ran>, RunError> {
        // `current` and the links are switched before the step starts; a failed switch leaves it
        // pending, and nothing of it runs.
        self.switch_for(order, rec.step)?;
        let started = Stamp::now(&*self.clock);
        // Nothing is left of the bound (the verify window has passed): the step is not started,
        // and never as unbounded, which only a `prepare` whose manifest timeout is 0
        // is. It ends as a failure.
        let Some(bound) = self.bound(order, rec) else {
            return Ok(Some(Ran::Exit(StepExit {
                code: None,
                last_line: None,
            })));
        };
        let (deadline_secs, unbounded) = match bound {
            Bound::Secs(secs) => (started.mono_secs.saturating_add(secs.get()), false),
            Bound::Unbounded => (started.mono_secs, true),
        };
        // Recorded `running` before the launch either way, so that a crash is resumed by
        // stopping the unit by name.
        rec.state = State::Running {
            started,
            deadline_secs,
            unbounded,
        };
        self.write(rec)?;
        if rec.forced {
            return Ok(None);
        }
        let unit = rec.unit(rec.step);
        let artifact = (rec.step == Step::Prepare).then(|| order.staged.dir().to_path_buf());
        // An unbounded step has no second to be stopped at: the dead-man is not armed for it.
        if !unbounded {
            self.watch.arm(&unit, deadline_secs);
        }
        let result = self.launcher.run(&StepCall {
            service: &order.service,
            invocation: Invocation {
                step: rec.step,
                artifact,
                params: Some(rec.params.clone()),
            },
            executable,
            unit: &unit,
            bound,
        });
        // A step the dead-man could not stop at its bound ran past it: what it reports is not
        // read. It stays `running` in the record, and the next run stops its unit by name first.
        if let Some(error) = self.watch.disarm() {
            return Err(RunError::Unstopped { unit, error });
        }
        match result {
            Ok(exit) => Ok(Some(Ran::Exit(exit))),
            Err(LaunchError::NoStart(_)) => Ok(Some(Ran::NoStart)),
            Err(e) => Err(e.into()),
        }
    }

    /// Whether the record on disk is this update's and a force flagged it `forced`. The caller
    /// holds the record lock.
    fn flagged(&self, rec: &Record) -> Result<bool, RunError> {
        Ok(match self.store.load()? {
            Loaded::Current(disk) => {
                disk.forced & disk.force.is_none() & (disk.generation == rec.generation)
            }
            Loaded::Absent | Loaded::Newer { .. } => false,
        })
    }

    /// Writes `rec`, under the record lock; when a force flagged the update meanwhile, `rec` is
    /// closed `forced_over` first, so that the write never clears the flag.
    fn write(&self, rec: &mut Record) -> Result<(), RunError> {
        let _held = self.store.hold();
        if self.flagged(rec)? {
            rec.close_forced_over();
        }
        let succeeded = matches!(
            rec.state,
            State::Finished {
                verdict: Verdict::Succeeded { .. },
                ..
            }
        );
        // The updater's own update: its success is the new binary's to write.
        if self.held_success & succeeded & rec.previous_digest.is_some() {
            return Ok(());
        }
        Ok(self.store.save(rec)?)
    }

    /// When a force flagged the update, closes `rec` `forced_over` and writes it: `true`.
    fn forced(&self, rec: &mut Record) -> Result<bool, RunError> {
        let _held = self.store.hold();
        if !self.flagged(rec)? {
            return Ok(false);
        }
        rec.close_forced_over();
        self.store.save(rec)?;
        Ok(true)
    }

    /// The bound of the step about to run: its own, and for `verify` before the update has
    /// succeeded no more than the time left of the verify window. `None` when nothing is left.
    fn bound(&self, order: &UpdateOrder, rec: &Record) -> Option<Bound> {
        let own = order.bounds.bound(rec.step)?;
        match (&rec.progress.verify_started, own) {
            (Some(began), Bound::Secs(secs)) if rec.step == Step::Verify => {
                let left = order
                    .bounds
                    .verify_deadline_secs
                    .saturating_sub(began.elapsed_secs(&*self.clock));
                NonZeroU64::new(secs.get().min(left)).map(Bound::Secs)
            }
            _ => Some(own),
        }
    }

    /// Decides from a step that ended what comes next.
    fn after(
        &self,
        order: &UpdateOrder,
        rec: &mut Record,
        code: Option<i32>,
        last_line: Option<String>,
    ) -> Result<(), RunError> {
        let step = rec.step;
        let report = Report::read(last_line.as_deref().unwrap_or(""));
        let outcome = Outcome::from_exit(code);
        if rec.progress.watching.is_some() {
            self.watching(order, rec, outcome, &report);
            return self.write(rec);
        }
        let mut ctx = Context {
            committed: rec.progress.committed,
            promotion_pending: rec.progress.promotion_pending,
            then: rec.progress.then.clone(),
            verify_deadline_passed: self.verify_window_passed(order, rec, &report),
            retries_left: match step {
                Step::Rollback => rec.progress.retry < ROLLBACK_ATTEMPTS,
                Step::Cleanup => rec.progress.retry < order.bounds.cleanup_retries,
                _ => false,
            },
        };
        ctx.observe(step, &report);
        rec.progress.committed = ctx.committed;
        rec.progress.promotion_pending = ctx.promotion_pending;
        // A first install has no release to go back to: a `rollback` that fails saying so
        // declines the install, which ends `Failed` rather than in a halt (C3).
        let declined = (step == Step::Rollback)
            & order.previous.is_none()
            & (outcome == Outcome::Failure)
            & (report.reason.as_deref() == Some(REASON_NO_PREVIOUS_RELEASE));
        let then = match declined {
            true => Next::Report(Verdict::Failed {
                phase: match ctx.committed {
                    true => Phase::AfterCommit,
                    false => Phase::BeforeCommit,
                },
                class: report.class,
            }),
            false => next(step, outcome, &report, &ctx),
        };
        match then {
            Next::Run(then) => {
                if step == Step::Cutover {
                    rec.progress.verify_started = Some(Stamp::now(&*self.clock));
                }
                rec.progress.retry = 0;
                self.move_to(rec, then);
            }
            Next::Again { step, after_secs } => {
                self.clock
                    .sleep_secs(wait_secs(after_secs, DEFAULT_RETRY_SECS));
                if step != Step::Verify {
                    rec.progress.retry += 1;
                }
                rec.attempt = rec.attempt.saturating_add(1);
                rec.state = State::Pending;
            }
            Next::Rollback { then } => {
                rec.progress.then = Some(then);
                rec.progress.reason = report.reason;
                rec.progress.retry = 0;
                self.move_to(rec, Step::Rollback);
            }
            Next::Report(verdict) => {
                // A `cleanup` succeeded: the switch is confirmed before the verdict is written
                // or the watcher begins. On `Err` the record on disk is still `cleanup` done.
                if matches!(verdict, Verdict::Succeeded { .. }) {
                    self.confirm(order)?;
                }
                let reason = report.reason.or(rec.progress.reason.clone());
                self.report(rec, verdict, reason);
            }
        }
        self.write(rec)
    }

    fn move_to(&self, rec: &mut Record, step: Step) {
        rec.step = step;
        rec.attempt = 1;
        rec.state = State::Pending;
    }

    /// Whether another run of `verify` after the wait would fall outside the verify window.
    fn verify_window_passed(&self, order: &UpdateOrder, rec: &Record, report: &Report) -> bool {
        let Some(began) = &rec.progress.verify_started else {
            return false;
        };
        let wait = wait_secs(report.retry_after_secs, DEFAULT_RETRY_SECS);
        began.elapsed_secs(&*self.clock).saturating_add(wait) >= order.bounds.verify_deadline_secs
    }

    /// The update is over, or (a pending promotion) the watcher begins.
    fn report(&self, rec: &mut Record, verdict: Verdict, reason: Option<String>) {
        match verdict {
            Verdict::Succeeded {
                promotion_pending: true,
            } => {
                rec.progress.watching = Some(Stamp::now(&*self.clock));
                // The verify window is over: the watcher is bounded by the promotion ceiling.
                rec.progress.verify_started = None;
                rec.progress.retry = 0;
                self.move_to(rec, Step::Verify);
            }
            verdict => rec.state = State::Finished { verdict, reason },
        }
    }

    /// The post-success watcher: `verify` again until it exits `0` without the flag. It is
    /// outside the table of the contract and never runs `rollback`: the commit has happened.
    fn watching(&self, order: &UpdateOrder, rec: &mut Record, outcome: Outcome, report: &Report) {
        let after_commit = Verdict::Failed {
            phase: Phase::AfterCommit,
            class: report.class,
        };
        let over = rec.progress.watching.as_ref().is_some_and(|began| {
            began.elapsed_secs(&*self.clock) >= order.bounds.promotion_ceiling_secs
        });
        let verdict = match outcome {
            Outcome::Success if !report.promotion_pending => Verdict::Succeeded {
                promotion_pending: false,
            },
            Outcome::Success if !over => {
                self.clock
                    .sleep_secs(wait_secs(report.retry_after_secs, WATCH_POLL_SECS));
                rec.attempt = rec.attempt.saturating_add(1);
                rec.state = State::Pending;
                return;
            }
            Outcome::Halt => Verdict::Halt,
            _ => after_commit,
        };
        rec.state = State::Finished {
            verdict,
            reason: report.reason.clone(),
        };
    }
}

/// How long to wait: what the hook asked for, or `default`, never more than [`MAX_RETRY_SECS`].
fn wait_secs(asked: Option<u64>, default: u64) -> u64 {
    asked.unwrap_or(default).min(MAX_RETRY_SECS)
}

/// Whether `rec` is the update `order` asks for.
fn same_update(rec: &Record, order: &UpdateOrder) -> bool {
    rec.service == order.service
        && rec.target_version == order.target_version
        && rec.artifact_digest == order.staged.digest()
        && rec.tarball_digest == order.artifact_digest
        && rec.previous_digest.as_deref() == order.previous.as_ref().map(Staged::digest)
}

fn path_str(path: &Path) -> Result<&str, RunError> {
    path.to_str()
        .ok_or(RunError::BadOrder("a release path that is not UTF-8"))
}

/// The bytes of the longest unit a step of `service` can run in: the longest step name, the
/// largest generation and the `.service` systemd adds.
fn longest_step_unit(service: &str) -> usize {
    Step::ALL
        .iter()
        .map(|step| unit_name(service, *step, u64::MAX).len())
        .max()
        .unwrap_or_default()
        + ".service".len()
}
