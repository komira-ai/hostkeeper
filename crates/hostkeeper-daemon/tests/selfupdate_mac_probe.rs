//! hostkeeper's own update on a Mac, through the daemon, as far as its probe: the protocol of
//! the new binary asked through the Mac's step launcher (`LaunchdLauncher`, one launchd job per
//! probe), over a stand-in for `launchctl` that runs the job as launchd would, writing what the
//! binary prints to the job's standard output file. The Linux probe through `SystemdLauncher` is
//! in `hostkeeper-steps`' tests; the run under the real launchd is the macOS device job's.

mod support;

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_daemon::selfupdate::{LauncherProbe, REASON_PROTOCOL_UNREADABLE, binary_dir};
use hostkeeper_hook::Class;
use hostkeeper_launchd::launchctl::{Launchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_launchd::launcher::STDOUT_FILE;
use hostkeeper_launchd::{LaunchdLauncher, Pace};
use support::selfupdate::{HK, NEW, SelfRig, awaiting, refused_before_commit};
use support::snapshot;

/// How the stand-in's job of a probe ends.
#[derive(Clone, Copy)]
enum Ends {
    /// It exits 0 having printed this.
    Prints(&'static str),
    /// It is killed by a signal, having printed nothing.
    Killed,
}

#[derive(Default)]
struct Jobs {
    /// The labels loaded now.
    loaded: Vec<String>,
    /// Every argument list asked, in order.
    calls: Vec<Vec<String>>,
}

/// `launchctl` over a launchd whose jobs end at once, as `ends` says.
struct StandIn {
    ends: Ends,
    jobs: Arc<Mutex<Jobs>>,
}

impl Launchctl for StandIn {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        let mut jobs = self.jobs.lock().expect("the jobs");
        jobs.calls.push(args.to_vec());
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(match words.as_slice() {
            ["print", target] => {
                let label = target.trim_start_matches("system/");
                match (jobs.loaded.iter().any(|l| l == label), self.ends) {
                    (false, _) => LaunchctlOutput::not_loaded(),
                    (true, Ends::Prints(_)) => LaunchctlOutput::success(&format!(
                        "{target} = {{\n\tstate = not running\n\tlast exit code = 0\n}}\n"
                    )),
                    (true, Ends::Killed) => LaunchctlOutput::success(&format!(
                        "{target} = {{\n\tstate = not running\n\tlast terminating signal = \
                         Killed: 9\n}}\n"
                    )),
                }
            }
            ["bootstrap", "system", plist] => {
                let plist = Path::new(plist);
                let label = plist
                    .file_stem()
                    .expect("<label>.plist")
                    .to_string_lossy()
                    .into_owned();
                if let Ends::Prints(text) = self.ends {
                    let out = plist.with_file_name(STDOUT_FILE);
                    fs::write(out, text).expect("the job's output");
                }
                jobs.loaded.push(label);
                LaunchctlOutput::success("")
            }
            ["bootout", target] => {
                let label = target.trim_start_matches("system/");
                jobs.loaded.retain(|l| l != label);
                LaunchctlOutput::success("")
            }
            other => panic!("launchd has no command {other:?}"),
        })
    }
}

/// A clock that moves only when the launcher waits.
#[derive(Default)]
struct StillPace(Mutex<Duration>);

impl Pace for StillPace {
    fn elapsed(&self) -> Duration {
        *self.0.lock().expect("the pace")
    }

    fn sleep(&self, wait: Duration) {
        *self.0.lock().expect("the pace") += wait;
    }
}

/// The daemon of `s`, its probe the host's (`LauncherProbe`) over a `LaunchdLauncher` whose
/// jobs end as `ends` says; and what that `launchctl` was asked.
fn mac_daemon(s: &SelfRig, ends: Ends) -> (hostkeeper_daemon::Daemon, Arc<Mutex<Jobs>>) {
    let jobs = Arc::new(Mutex::new(Jobs::default()));
    let launcher = LaunchdLauncher::new(
        StandIn {
            ends,
            jobs: jobs.clone(),
        },
        StillPace::default(),
        &s.rig.scratch.path().join("step-jobs"),
    );
    let daemon = s
        .daemon()
        .with_probe(Arc::new(LauncherProbe::new(Arc::new(launcher))));
    (daemon, jobs)
}

const PROBE_LABEL: &str = "dev.hostkeeper.hostkeeper.probe.hostkeeper-self-update-protocol";

/// Catches the Mac's self-update refused before the commit because its step launcher runs no
/// probe (the trait's default answered every Mac `protocol_unreadable`), and a probe that is not
/// the release's binary run as one launchd job: the daemon of the old version ordered the update
/// to [`NEW`] asks the binary of the release its protocol as the job of the probe's label, which
/// prints `1`; the update goes on, the new binary is staged and switched to, the reply is
/// `Deferred` `awaiting_restart`, and the probe's job is unloaded.
#[test]
fn a_mac_self_update_probes_the_new_binary_as_a_launchd_job_and_goes_on() {
    let s = SelfRig::new("su-mac-probe");
    let (daemon, jobs) = mac_daemon(&s, Ends::Prints("1\n"));

    assert_eq!(daemon.order(&s.order(NEW, 1)), awaiting(false));
    assert!(daemon.ctx().restart().requested());
    assert_eq!(s.current().as_deref(), Some(NEW));
    let jobs = jobs.lock().expect("the jobs");
    let bootstrapped: Vec<&Vec<String>> = jobs
        .calls
        .iter()
        .filter(|args| args[0] == "bootstrap")
        .collect();
    assert_eq!(bootstrapped.len(), 1, "{:?}", jobs.calls);
    assert!(
        bootstrapped[0][2].ends_with(&format!("{PROBE_LABEL}/{PROBE_LABEL}.plist")),
        "{:?}",
        jobs.calls
    );
    assert!(jobs.loaded.is_empty(), "the probe's job is unloaded");
    let rec = s.rig.record(HK).expect("the record");
    assert_eq!(
        rec.progress.awaiting_restart.map(|a| a.version).as_deref(),
        Some(NEW)
    );
}

/// Catches a Mac probe that did not answer read as one that did: a probe job killed by a signal,
/// or one that printed something else than one line of digits, refuses the update `Failed`
/// before the commit, of class environment, `protocol_unreadable`, with the binary directory
/// as it was and no record written.
#[test]
fn a_mac_probe_that_does_not_answer_refuses_the_update_protocol_unreadable() {
    for (n, ends) in [Ends::Killed, Ends::Prints("one\n")]
        .into_iter()
        .enumerate()
    {
        let s = SelfRig::new(&format!("su-mac-probe-unanswered-{n}"));
        let (daemon, _) = mac_daemon(&s, ends);
        let bin = binary_dir(&s.rig.state);
        let before = snapshot(&bin);
        assert_eq!(
            daemon.order(&s.order(NEW, 1)),
            refused_before_commit(Class::Environment, REASON_PROTOCOL_UNREADABLE),
            "row {n}"
        );
        assert_eq!(snapshot(&bin), before, "row {n}");
        assert!(s.rig.record(HK).is_none(), "row {n}");
    }
}
