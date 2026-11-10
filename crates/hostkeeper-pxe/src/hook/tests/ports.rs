//! The host's ports: the real `systemctl` program and the real fetch, over a loopback listener
//! that closes each connection so nothing leaves the machine.

use std::cell::RefCell;
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::thread;

use hostkeeper_hook::{Class, Outcome, Step};
use hostkeeper_package::gh::{Ran, Runner, Spawn};

use super::kit::{FakeSystemctl, SPAWN, Setup, step};
use crate::hook::prepare::verifier_of;
use crate::hook::systemctl::HostSystemctl;
use crate::hook::{FetchJob, Systemctl, fetch_with};

/// The program is run with the arguments given, its exit code is returned, a program that ends
/// by a signal is `-1`, and one that is not there is an error. The environment is empty.
///
/// Catches a `systemctl` run with the caller's environment, or whose exit code is lost.
#[test]
fn the_host_systemctl_runs_a_program_with_an_empty_environment() {
    let _alone = SPAWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let run = |program: &str, args: &[&str]| HostSystemctl::at(PathBuf::from(program)).run(args);
    assert_eq!(run("/bin/sh", &["-c", "exit 0"]).expect("ran"), 0);
    assert_eq!(run("/bin/sh", &["-c", "exit 3"]).expect("ran"), 3);
    assert_eq!(run("/bin/sh", &["-c", "kill -9 $$"]).expect("ran"), -1);
    assert_eq!(
        run("/bin/sh", &["-c", "test -z \"$HOME$CARGO_MANIFEST_DIR\""]).expect("ran"),
        0
    );
    assert_eq!(
        run(
            "/bin/sh",
            &[
                "-c",
                "test \"$1\" = restart && test \"$2\" = pxe.service",
                "x",
                "restart",
                "pxe.service"
            ]
        )
        .expect("ran"),
        0
    );
    let missing = run("/nonexistent/systemctl", &[]).expect_err("not there");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
}

/// A `gh` that records its runs and answers yes.
struct Recording(Rc<RefCell<Vec<Spawn>>>);

impl Runner for Recording {
    fn run(&self, spawn: &Spawn) -> io::Result<Ran> {
        self.0.borrow_mut().push(spawn.clone());
        Ok(Ran {
            code: Some(0),
            ..Default::default()
        })
    }
}

/// The verifier a job makes runs the `gh` of the job, passes its trusted root as
/// `--custom-trusted-root`, and runs in `gh`'s home under the new directory.
///
/// Catches a `gh` or trusted root of `[prepare]` that is parsed and then ignored, so the
/// attestations would be checked by another `gh` or against another root than the operator
/// named.
#[test]
fn the_verifier_runs_the_gh_and_trusted_root_the_job_names() {
    use hostkeeper_package::ports::{Attest, CommitId, SourceRef, Verifier};
    for (gh, root) in [
        ("/opt/pinned/gh", "/opt/pinned/root.json"),
        (crate::cli::GH, crate::cli::TRUSTED_ROOT),
    ] {
        let job = FetchJob {
            request: crate::testing::release::request(Path::new("/srv/boot/1.2.3"), &[]),
            roots: Vec::new(),
            gh: gh.into(),
            trusted_root: root.into(),
        };
        let runs = Rc::new(RefCell::new(Vec::new()));
        let verifier = verifier_of(
            &job,
            Box::new(Recording(Rc::clone(&runs))),
            Box::new(crate::testing::bundle::TrustAll),
        );
        let clock = crate::testing::FakeClock::at(5);
        let signers = crate::testing::bundle::signers();
        let sha = hostkeeper_config::daemon::Sha256Hex::parse(&"a".repeat(64)).expect("a digest");
        let commit = CommitId::new(crate::testing::bundle::COMMIT).expect("a commit");
        let source_ref = SourceRef::new("refs/heads/main").expect("a ref");
        let deadline = hostkeeper_steps::Stamp::now(&clock);
        let request = Attest {
            archive: Path::new("/srv/boot/1.2.3/netboot.json"),
            bundle: Path::new("/srv/boot/1.2.3/.verify/netboot.json.sigstore.json"),
            sha256: &sha,
            commit: &commit,
            source_ref: &source_ref,
            signers: &signers,
            deadline: &deadline,
        };
        verifier
            .verify(&request)
            .expect("the recording gh says yes");
        let runs = runs.borrow();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].program, Path::new(gh));
        assert_eq!(runs[0].cwd, Path::new("/srv/boot/1.2.3/.verify/gh"));
        let args: Vec<String> = runs[0]
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = args
            .iter()
            .position(|a| a == "--custom-trusted-root")
            .expect("the root flag");
        assert_eq!(args[at + 1], root);
    }
}

/// With a loadable config and CA bundle, the host's fetch asks the release for `netboot.json`
/// over HTTPS; a release that closes the connection is refused as `boot_set_refused` naming the
/// URL, and the new directory is gone; with and without `[prepare] platform`.
///
/// Catches settings that never reach the real HTTPS client, and a refused run that leaves a
/// half-made directory.
#[test]
fn the_host_ports_ask_the_release_and_a_refusal_keeps_nothing() {
    let setup = Setup::new("prepare-ports");
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener.local_addr().expect("its address").port();
    let closer = thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            drop(stream);
        }
    });
    let release = format!("https://127.0.0.1:{port}/v1.2.3");
    let params = setup.params(Some("1.2.3"), None, "");
    let text = std::fs::read_to_string(&params).expect("params");
    setup.params_text(&text.replace("https://releases.example/v1.2.3", &release));
    for extra in ["", "platform = [\"linux-arm64\"]\n"] {
        setup.settings_with(extra);
        let ended = step(
            &setup,
            Step::Prepare,
            &params,
            &FakeSystemctl::default(),
            &fetch_with,
        );
        let detail = ended.refused(Outcome::Failure, Class::Release, "boot_set_refused");
        assert_eq!(
            detail,
            format!("{release}/netboot.json: could not connect, or the request was not answered")
        );
        assert_eq!(setup.listing(), Vec::<String>::new());
    }
    closer.join().expect("the listener");
}
