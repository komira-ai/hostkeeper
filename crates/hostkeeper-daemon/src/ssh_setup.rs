//! The daemon sets sshd up to trust the coordinator's SSH user CA (`ssh-v1`;
//! docs/design/ssh-through-coordinator.md sections 3 and 6): the [`SshSink`] of the heartbeat
//! loop, on a Linux host over [`crate::ssh_host::SystemHost`] and on a Mac over
//! [`crate::ssh_host_mac::MacHost`]; the steps are the same, and what differs is behind
//! [`SshHost`].
//!
//! A record ([`PushedSsh`]) is taken up when its generation differs from the one the daemon last
//! took up, or when the files of an applied record are no longer what it asks for (a drift is
//! put right, nothing else is re-run); a record that was refused is taken up again at its next
//! generation or when the daemon starts. Taking one up adds trust and removes none, by the
//! design's eight rules, each at the step that keeps it:
//!
//! 1. Only the drop-in, the CA keys, the principals file of the login user and the revoked list
//!    are written ([`crate::ssh_files`]); nothing of sshd's own is.
//! 2. The drop-in is checked with `sshd -t` in a place no configuration includes, before any file
//!    is written.
//! 3. `sshd -T` is read for the login user and every account with keys before the change and after
//!    it: the login user must be able to log in by certificate alone and its trust files must be
//!    the drop-in's; every other account's settings must be what they were
//!    ([`crate::ssh_effective`]). A failure puts the files back.
//! 4. A reload (never a restart) follows a change, and the first record applied since the
//!    daemon started, and then a connection reads the banner; without one the files are put back and
//!    sshd reloaded again. Under socket activation the reload is the triggered service's, when
//!    the socket hands its connections to one listener that runs ([`SshHost::reload`]); a Mac's
//!    sshd, which launchd starts per connection, has no reload, only the check.
//! 5. Passwords stay as found, unless the record says `off` and every account with a password
//!    has a key path too (or is the login user). A Mac has no shadow file to read, so `off` is
//!    `unsupported` there and nothing is written.
//! 6. A stopped sshd is started and a disabled unit enabled (a Mac's Remote Login is loaded with
//!    `launchctl`, `remote_login` in the detail when that fails); nothing is stopped, and the unit
//!    that listens is never switched.
//! 7. `enabled = false` removes the files and reloads; sshd is left running (a Mac's Remote Login
//!    stays on).
//! 8. An allow or deny list that excludes the login user stops the setup before any change.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_link::{Log, PASSWORD_OFF, PushedSsh, SshReport, SshSink};

use crate::ssh_effective::{Effective, Failing, Ours, PASSWORD_KEYWORDS};
use crate::ssh_files::{
    Plan, Refused, Snapshot, SshPaths, SshState, current, install, plan, uninstall, wanted,
};
use crate::ssh_host::{SshHost, SshdUnit, UnitError};

/// What the setup holds between heartbeats.
#[derive(Default)]
struct Held {
    /// The last record taken up, whatever it came to.
    taken: Option<Taken>,
    fingerprints: Vec<String>,
    /// The serials the revoked list holds, when it was written by this setup.
    serials: Option<Vec<u64>>,
    /// The login user the last enabled record was planned for: the record's own, or the one the
    /// host resolved when the record named none (a Mac's single administrator).
    login_user: Option<String>,
    /// Whether a record was applied since the daemon started, so the listening sshd has read the
    /// files: a reload made it, or nothing changed after one that did.
    listener_read: bool,
}

/// A record taken up, and what came of it.
#[derive(Clone, Copy)]
struct Taken {
    generation: u64,
    state: SshState,
    detail: &'static str,
}

/// The daemon's sshd setup.
pub struct SshSetup {
    host: Arc<dyn SshHost>,
    paths: SshPaths,
    owner: u32,
    log: Arc<dyn Log>,
    held: Mutex<Held>,
}

/// An account's auth settings: keyword to the lines of its value.
type Settings = BTreeMap<&'static str, Vec<Vec<String>>>;

impl SshSetup {
    /// The setup of `host`, writing `paths` as the user `owner`, saying what it did in `log`.
    #[must_use]
    pub fn new(host: Arc<dyn SshHost>, paths: SshPaths, owner: u32, log: Arc<dyn Log>) -> SshSetup {
        SshSetup {
            host,
            paths,
            owner,
            log,
            held: Mutex::new(Held::default()),
        }
    }

    /// The effective configuration for `user`.
    fn effective(&self, user: &str) -> Result<Effective, Refused> {
        self.host
            .effective(user)
            .map_err(|_| Refused::new(SshState::ConfigInvalid, "sshd_config"))
    }

    /// The auth settings of every account with keys but the login user, by name.
    fn others(
        &self,
        accounts: &[crate::ssh_host::Account],
        login: &str,
    ) -> Result<BTreeMap<String, Settings>, Refused> {
        let mut recorded = BTreeMap::new();
        for account in accounts.iter().filter(|a| a.keyed && a.name != login) {
            recorded.insert(
                account.name.clone(),
                self.effective(&account.name)?.auth_settings(),
            );
        }
        Ok(recorded)
    }

    /// What rule 3 requires of the effective configuration after the change.
    fn verify(&self, plan: &Plan, before: &BTreeMap<String, Settings>) -> Result<(), Refused> {
        let login = self.effective(&plan.login_user)?;
        let principals = self.paths.principals_template();
        let expanded = self
            .paths
            .principals(&plan.login_user)
            .display()
            .to_string();
        let (ca, revoked) = (
            self.paths.ca_keys().display().to_string(),
            self.paths.revoked().display().to_string(),
        );
        let ours = Ours {
            ca_keys: &ca,
            principals: &principals,
            principals_expanded: &expanded,
            revoked: &revoked,
        };
        login
            .cert_login(&plan.login_user, Some(&ours))
            .map_err(|Failing(keyword)| {
                let shadowed = Failing(keyword).is_shadowed();
                let state = if shadowed {
                    SshState::ShadowedByStock
                } else {
                    SshState::CertLoginNotPossible
                };
                Refused::new(state, keyword)
            })?;
        if plan.passwords_off {
            for keyword in PASSWORD_KEYWORDS {
                if login.first(keyword) != Some("no") {
                    return Err(Refused::new(SshState::ShadowedByStock, keyword));
                }
            }
        }
        for (name, was) in before {
            // The password keywords are the one setting the drop-in may change (rule 5).
            let mut now = self.effective(name)?.auth_settings();
            let mut was = was.clone();
            for keyword in PASSWORD_KEYWORDS {
                now.remove(keyword);
                was.remove(keyword);
            }
            if now != was {
                return Err(Refused::new(SshState::ReloadFailed, "account_settings"));
            }
        }
        self.host
            .banner(&login.listening())
            .map_err(|_| Refused::new(SshState::ReloadFailed, "banner"))
    }

    /// Takes up an enabled record.
    fn enable(&self, access: &PushedSsh, held: &mut Held) -> Result<(), Refused> {
        let login_user = self
            .host
            .prepare(&access.login_user, access.password_auth == PASSWORD_OFF)?;
        let plan = plan(&PushedSsh {
            login_user,
            ..access.clone()
        })?;
        held.login_user = Some(plan.login_user.clone());
        let unit = self.host.unit().map_err(|why| match why {
            UnitError::Missing => Refused::new(SshState::SshdMissing, "unit"),
            UnitError::Masked => Refused::new(SshState::Unsupported, "unit_masked"),
            UnitError::NoManager => Refused::new(SshState::Unsupported, "service_manager"),
        })?;
        let login = self.effective(&plan.login_user)?;
        let groups = self.host.groups(&plan.login_user);
        if let Some(keyword) = login.excluded(&plan.login_user, &groups) {
            return Err(Refused::new(SshState::BlockedByAllowList, keyword));
        }
        login
            .cert_login(&plan.login_user, None)
            .map_err(|Failing(keyword)| Refused::new(SshState::CertLoginNotPossible, keyword))?;
        let accounts = self.host.accounts();
        let before = self.others(&accounts, &plan.login_user)?;
        if plan.passwords_off
            && accounts
                .iter()
                .any(|a| a.password && !a.keyed && a.name != plan.login_user)
        {
            return Err(Refused::new(SshState::WouldLockOut, "account_without_key"));
        }
        let step = match unit {
            SshdUnit::Launchd => "remote_login",
            _ => "start",
        };
        self.host
            .start(&unit)
            .map_err(|_| Refused::new(SshState::ReloadFailed, step))?;
        let revoked =
            if held.serials.as_ref() == Some(&plan.serials) && self.paths.revoked().is_file() {
                None
            } else {
                let made = self
                    .host
                    .revocations(&plan.ca_keys, &plan.serials)
                    .map_err(|_| Refused::new(SshState::ConfigInvalid, "revocations"))?;
                Some(made)
            };
        let files = wanted(&self.paths, &plan, revoked);
        self.check_candidate(&plan)?;
        let snapshot = Snapshot::of(&self.paths);
        // Until a record is applied after the daemon starts, a reload follows even when no file
        // changes: the files may have been written by an earlier start that did not reload (or
        // stopped before it did), and sshd -T reads the files, not what the listening sshd holds.
        // A record refused before this point does not count.
        let first = !held.listener_read;
        let applied = self.put(&files, &unit, &snapshot, first, || {
            self.verify(&plan, &before)
        });
        if applied.is_ok() {
            held.serials = Some(plan.serials);
            held.listener_read = true;
        }
        applied
    }

    /// `sshd -t` over the drop-in, written where no configuration includes it.
    fn check_candidate(&self, plan: &Plan) -> Result<(), Refused> {
        let candidate = self.paths.candidate();
        let bytes = crate::ssh_files::render_drop_in(&self.paths, plan.passwords_off);
        let written = crate::ssh_files::write(&candidate, &bytes, self.owner);
        let checked = written
            .map_err(|_| ())
            .and_then(|_| self.host.check_config(&candidate).map_err(|_| ()));
        let _ = std::fs::remove_file(&candidate);
        checked.map_err(|()| Refused::new(SshState::ConfigInvalid, "sshd_t"))
    }

    /// Installs `files`, reloads when something changed or `always` is set, and runs `verify`;
    /// any failure puts `snapshot` back and reloads again.
    fn put(
        &self,
        files: &crate::ssh_files::Wanted,
        unit: &SshdUnit,
        snapshot: &Snapshot,
        always: bool,
        verify: impl Fn() -> Result<(), Refused>,
    ) -> Result<(), Refused> {
        let step = || -> Result<(), Refused> {
            let changed = install(&self.paths, files, self.owner)
                .map_err(|_| Refused::new(SshState::ConfigInvalid, "write"))?;
            if changed || always {
                self.host
                    .reload(unit)
                    .map_err(|_| Refused::new(SshState::ReloadFailed, "reload"))?;
            }
            verify()
        };
        step().inspect_err(|_| self.roll_back(snapshot, unit))
    }

    /// Puts the files of `snapshot` back and has sshd read them.
    fn roll_back(&self, snapshot: &Snapshot, unit: &SshdUnit) {
        if let Err(why) = snapshot.restore(self.owner, &self.paths) {
            self.log.line(&format!("error: ssh: {why}"));
        }
        if let Err(why) = self.host.reload(unit) {
            self.log.line(&format!(
                "error: ssh: sshd is not reloaded after the files were put back: {why}"
            ));
        }
    }

    /// Takes up a record that turns the setup off.
    fn disable(&self, held: &mut Held) -> Result<(), Refused> {
        let unit = self.host.unit().ok();
        let snapshot = Snapshot::of(&self.paths);
        let changed =
            uninstall(&self.paths).map_err(|_| Refused::new(SshState::ConfigInvalid, "write"))?;
        held.serials = None;
        let (true, Some(unit)) = (changed, unit) else {
            return Ok(());
        };
        let reloaded = self
            .host
            .reload(&unit)
            .map_err(|_| Refused::new(SshState::ReloadFailed, "reload"))
            .and_then(|()| {
                let at = self.effective("root")?.listening();
                self.host
                    .banner(&at)
                    .map_err(|_| Refused::new(SshState::ReloadFailed, "banner"))
            });
        reloaded.inspect_err(|_| self.roll_back(&snapshot, &unit))
    }

    /// Whether the files of the held record are what it asks for, planned for the login user the
    /// apply used (the host's choice when the record names none).
    fn drifted(&self, access: &PushedSsh, held: &Held) -> bool {
        match (held.taken.map(|taken| taken.state), access.enabled) {
            (Some(SshState::Applied), true) => held.login_user.as_ref().is_some_and(|login_user| {
                let record = PushedSsh {
                    login_user: login_user.clone(),
                    ..access.clone()
                };
                plan(&record).is_ok_and(|plan| {
                    let files = wanted(&self.paths, &plan, None);
                    !(current(&self.paths, &files) && held.serials.as_ref() == Some(&plan.serials))
                })
            }),
            (Some(SshState::Disabled), false) => self.paths.drop_in().exists(),
            _ => false,
        }
    }
}

impl SshSink for SshSetup {
    fn report(&self) -> Option<SshReport> {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let taken = held.taken?;
        Some(SshReport {
            applied_generation: taken.generation,
            state: taken.state.as_str().to_owned(),
            host_key_fingerprints: held.fingerprints.clone(),
            detail: taken.detail.to_owned(),
        })
    }

    fn apply(&self, access: &PushedSsh) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let same = held
            .taken
            .is_some_and(|taken| taken.generation == access.generation);
        if same && !self.drifted(access, &held) {
            return;
        }
        let outcome = if access.enabled {
            self.enable(access, &mut held)
                .map(|()| (SshState::Applied, ""))
        } else {
            self.disable(&mut held).map(|()| (SshState::Disabled, ""))
        };
        let (state, detail) = outcome.unwrap_or_else(|refused| (refused.state, refused.detail));
        held.taken = Some(Taken {
            generation: access.generation,
            state,
            detail,
        });
        held.fingerprints = self.host.host_key_fingerprints();
        let note = if detail.is_empty() {
            String::new()
        } else {
            format!(" ({detail})")
        };
        let level = if matches!(state, SshState::Applied | SshState::Disabled) {
            ""
        } else {
            "error: "
        };
        self.log.line(&format!(
            "{level}ssh generation {}: {}{note}",
            access.generation,
            state.as_str()
        ));
    }
}

#[cfg(test)]
#[path = "ssh_setup_tests.rs"]
mod tests;
