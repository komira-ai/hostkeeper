#![cfg(all(target_os = "linux", target_env = "gnu", feature = "privileged"))]
//! The hostkeeper daemon on Alpine under OpenRC, as root, in privileged.yml's job `openrc-device`
//! (and pr-device.yml's copy of it): each test boots an Alpine container whose init is OpenRC
//! (`openrc` says how it is made and what is a fixture). The tests are built on the job's glibc
//! runner (hence the `target_env` above, which the workflow lint ties this file to the job by),
//! and what runs in the container is the static musl build.
//!
//! What each test proves:
//! - hostkeeper's Alpine package, built by nfpm from `packaging/alpine/`, installs with `apk`,
//!   enables its init script in the default runlevel, and `rc-service hostkeeper start` returns
//!   once the daemon is ready (its ready file holds the pid supervise-daemon runs); the daemon is
//!   the packaged binary's seed of the binary directory, exec'd from `current`, its heartbeat
//!   loop runs; killed, supervise-daemon starts it again; stopped, it is gone; a reboot of the
//!   container starts it from the runlevel; and `apk del` takes it out of the runlevel and its
//!   files away, and stops nothing;
//! - a daemon whose config does not load makes `rc-service hostkeeper start` return, with the
//!   error said, rather than wait for good, and once the config is fixed the daemon supervise-daemon
//!   keeps starting becomes ready with no command;
//! - the daemon of version `1.0.0`, under the shipped init script, ordered the update to `1.0.1`
//!   stages it as `next` and exits; supervise-daemon starts the packaged binary again, which
//!   execs `current`, the old binary, which hands over to the new one (two-programs.md, 4.3);
//!   the new one finishes the switch, the update settles `Succeeded`, the heartbeats report it;
//! - a new binary that cannot start is handed over to three times, each start of supervise-daemon
//!   through the packaged binary and the old one; then the old binary settles the update
//!   `Failed` after the commit, `self_update_fallback`, the version is on the list of bad ones,
//!   `next` is gone, `current` never moved, and the old binary runs.

mod openrc;

use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_config::platform::Platform;
use hostkeeper_hook::Verdict;
use hostkeeper_steps::record::{Loaded, Record, State};
use hostkeeper_steps::{BadVersions, RecordStore, Seal, tree_digest};
use openrc::{
    Alpine, HK_ROOT, READY, SERVICE, copy, dir, file, musl_program, nfpm, repo, text, wait,
};
use serde_json::Value;

/// The version of this build: what the Alpine package and its binary are.
const OWN: &str = env!("CARGO_PKG_VERSION");
/// The service of hostkeeper itself.
const HK: &str = "hostkeeper";
/// The versions of the self-update tests.
const N: &str = "1.0.0";
const NEXT: &str = "1.0.1";
/// The package digest the orders name.
const DIGEST: &str = "abababababababababababababababababababababababababababababababab";
/// The order id of the self-update.
const ORDER: &str = "self-update-1";
/// The reason of the verdict of an update whose new binary has not run yet.
const AWAITING_REASON: &str = "awaiting_restart";
/// The shipped init script.
const INIT_SCRIPT: &str = include_str!("../../../packaging/alpine/hostkeeper.initd");
/// The line of the init script with the daemon's arguments, as shipped.
const SHIPPED_ARGS: &str = "command_args=\"daemon --config /etc/hostkeeper/hostkeeper.toml \
    --state-dir /var/lib/updater/state --packages-dir /var/lib/hostkeeper/packages \
    --ready-file /run/hostkeeper/ready\"";

/// The daemon config: every key the loader requires. The installed daemon never reaches its
/// addresses (it waits for its enrollment); the fixture's link, package store and enroller
/// replace what would.
const CONFIG: &str = r#"coordinator = "https://coordinator.example"
ca_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow = ".github/workflows/release.yml"
refs = ["refs/tags/*"]
hosted_runner = true
"#;

/// The file of `version` in the binary directory under the state directory `state` (a path in
/// the container).
fn version_file(state: &str, version: &str) -> String {
    format!("{state}/hostkeeper/bin/versions/{version}")
}

/// hostkeeper's Alpine package of this build, made by the pinned nfpm from the committed
/// `packaging/alpine/` with the musl daemon, in `<base>/apk/`; its file.
fn build_apk(base: &Path) -> PathBuf {
    let stage = base.join("stage");
    let root = repo();
    for (from, to, mode) in [
        (
            "packaging/alpine/nfpm.yaml",
            "packaging/alpine/nfpm.yaml",
            0o644,
        ),
        (
            "packaging/alpine/hostkeeper.initd",
            "packaging/alpine/hostkeeper.initd",
            0o755,
        ),
        (
            "packaging/alpine/scripts/postinstall.sh",
            "packaging/alpine/scripts/postinstall.sh",
            0o755,
        ),
        (
            "packaging/alpine/scripts/preremove.sh",
            "packaging/alpine/scripts/preremove.sh",
            0o755,
        ),
        (
            "packaging/linux/hostkeeper.toml.template",
            "packaging/linux/hostkeeper.toml.template",
            0o644,
        ),
        (
            "packaging/sigstore/trusted_root.json",
            "target/nfpm/trusted_root.json",
            0o644,
        ),
    ] {
        let to = stage.join(to);
        dir(to.parent().expect("a parent"), 0o755);
        copy(&root.join(from), &to, mode);
    }
    // The daemon never runs `gh` while it waits for its enrollment: any executable stands in.
    file(&stage.join("target/nfpm/gh"), b"#!/bin/sh\nexit 1\n", 0o755);
    copy(
        &musl_program("hostkeeper"),
        &stage.join("target/nfpm/hostkeeper"),
        0o755,
    );
    let out = base.join("apk");
    dir(&out, 0o755);
    let built = std::process::Command::new(nfpm())
        .current_dir(&stage)
        .env("VERSION", OWN)
        .env("ARCH", "amd64")
        .args([
            "package",
            "--config",
            "packaging/alpine/nfpm.yaml",
            "--packager",
            "apk",
        ])
        .arg("--target")
        .arg(&out)
        .output()
        .expect("nfpm runs");
    assert!(built.status.success(), "nfpm: {}", text(&built));
    let mut apks: Vec<PathBuf> = fs::read_dir(&out)
        .expect("the package directory")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "apk"))
        .collect();
    assert_eq!(apks.len(), 1, "one package: {apks:?}");
    apks.remove(0)
}

/// `host` with hostkeeper's Alpine package installed from `apk` and the daemon config placed (by
/// whoever installs the package, as on a host).
fn install(host: &Alpine, apk: &Path, config: &str) {
    let name = apk.file_name().expect("a name").to_string_lossy();
    host.exec_ok(&[
        "apk",
        "add",
        "--no-network",
        "--allow-untrusted",
        &format!("{HK_ROOT}/apk/{name}"),
    ]);
    place_config(host, config, 0o644);
}

/// The daemon config `text` placed at `/etc/hostkeeper/hostkeeper.toml`, mode `mode`.
fn place_config(host: &Alpine, config: &str, mode: u32) {
    file(
        &host.base().join("hostkeeper.toml"),
        config.as_bytes(),
        0o644,
    );
    host.exec_ok(&["mkdir", "-p", "/etc/hostkeeper"]);
    host.exec_ok(&[
        "cp",
        &format!("{HK_ROOT}/hostkeeper.toml"),
        "/etc/hostkeeper/hostkeeper.toml",
    ]);
    host.exec_ok(&[
        "chmod",
        &format!("{mode:o}"),
        "/etc/hostkeeper/hostkeeper.toml",
    ]);
}

/// Catches an Alpine package that does not install, does not enable its service, or whose start
/// returns before the daemon is ready (or never); a daemon that is not the packaged binary's seed
/// exec'd from `current`; a supervisor that does not restart it, or a stop that leaves it; a
/// service that a reboot does not start; and a removal that fails, leaves the service in the
/// runlevel, or stops the running daemon.
#[test]
fn the_alpine_package_installs_and_runs_the_daemon_under_openrc() {
    let host = Alpine::boot("install");
    let apk = build_apk(host.base());
    install(&host, &apk, CONFIG);
    assert!(
        in_default_runlevel(&host),
        "the package enables the service in the default runlevel"
    );

    let started = host.exec(&["rc-service", SERVICE, "start"]);
    assert!(
        started.status.success(),
        "{}\n{}",
        text(&started),
        host.logs()
    );
    assert!(
        !text(&started).contains("ended before it was ready"),
        "{}",
        text(&started)
    );
    let exe = version_file("/var/lib/updater/state", OWN);
    let pid = host.runs(&exe, None);
    assert_eq!(
        host.ready(),
        Some(format!("{pid}\n")),
        "the start returned once this daemon was ready"
    );
    assert_eq!(host.status(), Some(0));
    assert_eq!(
        host.exec_ok(&["readlink", "/var/lib/updater/state/hostkeeper/bin/current"])
            .trim(),
        format!("versions/{OWN}")
    );
    // Past its start: its heartbeat loop runs, and waits for the enrollment no test gives it.
    wait("the heartbeat loop of the daemon runs", || {
        host.log().contains("not enrolled: waiting for")
    });

    host.exec_ok(&["kill", "-KILL", &pid.to_string()]);
    let again = host.runs(&exe, Some(pid));
    wait("the restarted daemon is ready", || {
        host.ready() == Some(format!("{again}\n"))
    });
    assert_eq!(host.status(), Some(0));

    host.exec_ok(&["rc-service", SERVICE, "stop"]);
    assert_eq!(host.status(), Some(3), "stopped");
    assert_eq!(host.exe_of(again), None, "the daemon is gone");

    host.reboot();
    wait("the default runlevel starts the daemon at boot", || {
        host.status() == Some(0) && host.ready().is_some()
    });
    let booted = host.runs(&exe, None);

    // Removed, the package takes the service out of the default runlevel (its pre-remove) and
    // its files away, and stops nothing: the daemon runs on from its binary directory, as the
    // deb's and the rpm's removal leave a running daemon.
    host.exec_ok(&["apk", "del", "--no-network", "hostkeeper"]);
    assert!(
        !in_default_runlevel(&host),
        "the removal takes the service out of the default runlevel"
    );
    assert!(
        !host
            .exec(&["test", "-e", "/etc/init.d/hostkeeper"])
            .status
            .success()
    );
    assert!(
        !host
            .exec(&["test", "-e", "/usr/bin/hostkeeper"])
            .status
            .success()
    );
    assert_eq!(
        host.exe_of(booted).as_deref(),
        Some(exe.as_str()),
        "the removal stops nothing"
    );
}

/// Whether the service is in the default runlevel, as `rc-update show default` lists it.
fn in_default_runlevel(host: &Alpine) -> bool {
    let shown = host.exec_ok(&["rc-update", "show", "default"]);
    shown
        .lines()
        .any(|line| line.split('|').next().map(str::trim) == Some(SERVICE))
}

/// Catches a start that waits for good on a daemon that cannot become ready (the runlevel, and
/// the boot with it, would wait too): with a config others may write, which the daemon refuses,
/// `rc-service hostkeeper start` returns with the error said; supervise-daemon goes on starting
/// it, and once the config is fixed the daemon becomes ready with no command.
#[test]
fn a_daemon_that_cannot_become_ready_does_not_hold_its_start() {
    let host = Alpine::boot("not-ready");
    let apk = build_apk(host.base());
    install(&host, &apk, CONFIG);
    host.exec_ok(&["chmod", "666", "/etc/hostkeeper/hostkeeper.toml"]);
    let started = host.exec(&["rc-service", SERVICE, "start"]);
    assert!(
        text(&started).contains("hostkeeper ended before it was ready"),
        "{}\n{}",
        text(&started),
        host.logs()
    );
    assert_eq!(host.ready(), None);
    assert!(
        host.log()
            .contains("the daemon config is not a file the host trusts")
    );

    host.exec_ok(&["chmod", "644", "/etc/hostkeeper/hostkeeper.toml"]);
    let pid = host.runs(&version_file("/var/lib/updater/state", OWN), None);
    wait("the daemon becomes ready once its config loads", || {
        host.ready() == Some(format!("{pid}\n"))
    });
    assert_eq!(host.status(), Some(0));
}

/// The manifest of hostkeeper at `version`: its hook the workload fixture in mode `ok`, which
/// ends every step `0` with a valid last line.
fn manifest(version: &str) -> String {
    let platform = Platform::current();
    format!(
        r#"schema = 1
name = "{HK}"
version = "{version}"
platform = "{platform}"
min_hostkeeper = "0.1.0"

[hook]
command = ["bin/hook"]
verify_deadline_secs = 1600

[hook.timeout_secs]
prepare = 600
cutover = 300
verify = 1000
rollback = 600
cleanup = 1300

[params]
"fixture.mode" = "ok"
"#
    )
}

/// Where a self-update test's packaged binary is, in the container: a copy of the musl
/// `hostkeeper-selfupdate-daemon` named by its version, as the program's `--fixture-packaged`
/// takes it, which `/usr/bin/hostkeeper` links to.
const PACKAGED_DIR: &str = "/usr/lib/hostkeeper-fixture";

/// The packaged binary of a self-update test, version [`N`].
fn packaged() -> String {
    format!("{PACKAGED_DIR}/{N}")
}

/// The Alpine host of a self-update test: the musl `hostkeeper-selfupdate-daemon` as the
/// packaged binary (version [`N`], `--fixture-packaged`; [`packaged`], which the init script's
/// `/usr/bin/hostkeeper` links to), under the
/// shipped init script with only its daemon arguments changed to the test's tree; the releases of
/// [`N`] and [`NEXT`] sealed in the package store with [`N`] installed; the coordinator files the
/// test writes.
struct SelfUpdateHost {
    host: Alpine,
}

impl SelfUpdateHost {
    /// The host, the daemon started and running [`N`]; `fail_next` makes [`NEXT`] refuse to
    /// start (`--fixture-fail-start`).
    fn start(tag: &str, fail_next: bool) -> SelfUpdateHost {
        let host = Alpine::boot(tag);
        let base = host.base().to_path_buf();
        dir(&base.join("state"), 0o700);
        dir(&base.join("state").join(HK), 0o700);
        dir(&base.join("packages"), 0o755);
        for sub in ["link", "units", "placed"] {
            dir(&base.join("fixture").join(sub), 0o755);
        }
        file(&base.join("hostkeeper.toml"), CONFIG.as_bytes(), 0o644);
        let service = base.join("packages").join(HK);
        dir(&service, 0o755);
        for version in [N, NEXT] {
            release(&service, version);
        }
        std::os::unix::fs::symlink(N, service.join("current")).expect("link current");

        copy(
            &musl_program("hostkeeper-selfupdate-daemon"),
            &base.join("hostkeeper"),
            0o755,
        );
        host.exec_ok(&["mkdir", "-p", PACKAGED_DIR]);
        host.exec_ok(&["cp", &format!("{HK_ROOT}/hostkeeper"), &packaged()]);
        host.exec_ok(&["chmod", "755", &packaged()]);
        host.exec_ok(&["ln", "-s", &packaged(), "/usr/bin/hostkeeper"]);
        let fail = if fail_next {
            format!(" --fixture-fail-start {NEXT}")
        } else {
            String::new()
        };
        let args = format!(
            "command_args=\"--fixture-packaged {packaged} --fixture-dir {HK_ROOT}/fixture{fail} daemon \
             --config {HK_ROOT}/hostkeeper.toml --state-dir {HK_ROOT}/state --packages-dir \
             {HK_ROOT}/packages --ready-file {READY} \
             --heartbeat-secs 1\"",
            packaged = packaged(),
        );
        let shipped: String = SHIPPED_ARGS
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            INIT_SCRIPT
                .lines()
                .filter(|line| line.starts_with("command_args="))
                .collect::<Vec<_>>(),
            [shipped.as_str()],
            "the shipped init script has the daemon arguments this test replaces"
        );
        let script = INIT_SCRIPT.replace(&shipped, &args);
        file(&base.join("hostkeeper.initd"), script.as_bytes(), 0o755);
        host.exec_ok(&[
            "cp",
            &format!("{HK_ROOT}/hostkeeper.initd"),
            "/etc/init.d/hostkeeper",
        ]);
        host.exec_ok(&["chmod", "755", "/etc/init.d/hostkeeper"]);

        let started = host.exec(&["rc-service", SERVICE, "start"]);
        assert!(
            started.status.success(),
            "{}\n{}",
            text(&started),
            host.logs()
        );
        let su = SelfUpdateHost { host };
        su.runs(N, None);
        su
    }

    /// The pid of the daemon once it runs the binary of `version`, other than `not`.
    fn runs(&self, version: &str, not: Option<u32>) -> u32 {
        self.check_not_stuck();
        self.host
            .runs(&version_file(&format!("{HK_ROOT}/state"), version), not)
    }

    /// The coordinator orders the `update` of hostkeeper to `version`, order [`ORDER`] try 1.
    fn offer(&self, version: &str) {
        let link = self.host.base().join("fixture/link");
        let order = serde_json::json!({
            "id": ORDER,
            "try": 1,
            "verb": "update",
            "service": HK,
            "version": version,
            "digest": DIGEST,
        });
        let tmp = link.join("order.json.tmp");
        file(&tmp, order.to_string().as_bytes(), 0o644);
        fs::rename(&tmp, link.join("order.json")).expect("place the order");
    }

    fn hk_state(&self) -> PathBuf {
        self.host.base().join("state").join(HK)
    }

    /// The version `current` of the binary directory names.
    fn current(&self) -> Option<String> {
        fs::read_link(self.hk_state().join("bin/current"))
            .ok()
            .map(|target| {
                target
                    .to_string_lossy()
                    .trim_start_matches("versions/")
                    .to_owned()
            })
    }

    /// The version `previous` of the binary directory names.
    fn previous(&self) -> Option<String> {
        fs::read_link(self.hk_state().join("bin/previous"))
            .ok()
            .map(|target| {
                target
                    .to_string_lossy()
                    .trim_start_matches("versions/")
                    .to_owned()
            })
    }

    fn record(&self) -> Option<Record> {
        match RecordStore::new(&self.hk_state()).load() {
            Ok(Loaded::Current(rec)) => Some(*rec),
            Ok(Loaded::Absent | Loaded::Newer { .. }) => None,
            Err(e) => panic!("the step record cannot be read: {e}"),
        }
    }

    /// Every heartbeat the daemon sent so far.
    fn beats(&self) -> Vec<Value> {
        let text = fs::read_to_string(self.host.base().join("fixture/link/beats.jsonl"))
            .unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Fails at once when a heartbeat reports the order refused (any failure but the wait for
    /// the restart), unless `refusal` is allowed.
    fn check_not_stuck(&self) {
        for beat in self.beats() {
            let refused = beat["services"].as_array().into_iter().flatten().find(|s| {
                let last = &s["last_update"];
                s["name"] == HK
                    && last["order_id"] == ORDER
                    && last["outcome"] == "failed"
                    && last["reason"] != AWAITING_REASON
                    && last["reason"] != "self_update_fallback"
            });
            if let Some(service) = refused {
                panic!("the order was refused: {service}\n{}", self.host.logs());
            }
        }
    }

    /// Waits until the step record of hostkeeper is finished, and gives it.
    fn settled(&self) -> Record {
        let mut found = None;
        wait("the step record of hostkeeper is finished", || {
            self.check_not_stuck();
            found = self
                .record()
                .filter(|rec| matches!(rec.state, State::Finished { .. }));
            found.is_some()
        });
        found.expect("a finished record")
    }

    /// Waits until a heartbeat of the daemon at `version` reports hostkeeper at `version` with
    /// the order's `last_update` of `outcome`.
    fn reports(&self, version: &str, outcome: &str) {
        wait(&format!("a heartbeat reports the order {outcome}"), || {
            self.check_not_stuck();
            self.beats().iter().any(|beat| {
                beat["agent"]["version"] == version
                    && beat["services"].as_array().is_some_and(|services| {
                        services.iter().any(|service| {
                            service["name"] == HK
                                && service["last_update"]["order_id"] == ORDER
                                && service["last_update"]["outcome"] == outcome
                        })
                    })
            })
        });
    }
}

/// The release of hostkeeper at `version` in `service`, as the real store leaves it: the tree
/// (manifest, the hook, the binary, each musl), then its seal beside it.
fn release(service: &Path, version: &str) {
    let tree = service.join(version);
    dir(&tree.join("bin"), 0o755);
    fs::set_permissions(&tree, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("its mode");
    file(
        &tree.join("manifest.toml"),
        manifest(version).as_bytes(),
        0o644,
    );
    copy(
        &musl_program("hostkeeper-fixture-hook"),
        &tree.join("bin/hook"),
        0o755,
    );
    copy(
        &musl_program("hostkeeper-selfupdate-daemon"),
        &tree.join("bin/hostkeeper"),
        0o755,
    );
    let digest = tree_digest(&tree).expect("the tree digest");
    let seal = Seal::new(&digest, DIGEST).expect("a seal");
    file(
        &service.join(format!("{version}.seal")),
        seal.to_json().as_bytes(),
        0o644,
    );
}

/// Catches an OpenRC self-update whose new binary never runs (the clean exit not respawned, the
/// packaged binary execing another file than `current`, no hand-over at the old binary's start):
/// the daemon of [`N`] ordered the update to [`NEXT`] restarts into it under supervise-daemon,
/// the update settles `Succeeded`, `current` names [`NEXT`] and `previous` [`N`], and the new
/// daemon reports the order succeeded and is ready.
#[test]
fn a_daemon_under_openrc_updates_itself_and_supervise_daemon_restarts_it_into_the_next_binary() {
    let su = SelfUpdateHost::start("update", false);
    let old = su.runs(N, None);
    su.offer(NEXT);
    let rec = su.settled();
    assert_eq!(rec.target_version, NEXT);
    assert_eq!(
        rec.state,
        State::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None,
        },
        "{rec:?}\n{}",
        su.host.logs()
    );
    let new = su.runs(NEXT, Some(old));
    assert_ne!(new, old);
    assert_eq!(su.current().as_deref(), Some(NEXT));
    assert_eq!(su.previous().as_deref(), Some(N));
    su.reports(NEXT, "succeeded");
    wait("the new daemon is ready", || {
        su.host.ready() == Some(format!("{new}\n"))
    });
}

/// Catches an OpenRC host that crash-loops on a new binary for good (nothing ends the hand-over),
/// or gives up before its three tries: [`NEXT`] refuses to start; after it was handed over to
/// three times, the old binary settles the update `Failed` after the commit
/// (`self_update_fallback`), [`NEXT`] is on the list of bad versions, `next` is removed,
/// `current` still names [`N`], and the daemon of [`N`] runs.
#[test]
fn a_next_binary_that_cannot_start_is_fallen_back_from_by_the_old_binary() {
    let su = SelfUpdateHost::start("fallback", true);
    let old = su.runs(N, None);
    su.offer(NEXT);
    let rec = su.settled();
    assert_eq!(rec.target_version, NEXT);
    assert!(
        matches!(
            &rec.state,
            State::Finished { reason: Some(reason), .. } if reason == "self_update_fallback"
        ),
        "{rec:?}\n{}",
        su.host.logs()
    );
    su.runs(N, Some(old));
    assert_eq!(su.current().as_deref(), Some(N));
    assert!(
        BadVersions::load(&su.hk_state())
            .expect("the list of bad versions")
            .contains(NEXT)
    );
    let log = su.host.log();
    assert_eq!(
        log.matches(&format!("version {NEXT} refuses to start"))
            .count(),
        3,
        "three starts of the new binary\n{log}"
    );
    assert!(
        fs::symlink_metadata(su.hk_state().join("bin/next")).is_err(),
        "next is removed"
    );
}
