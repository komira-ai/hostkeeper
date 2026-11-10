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
//!   keeps starting becomes ready with no command.
//!
//! hostkeeper's update of itself is not run here: an OpenRC host has no installer the daemon
//! announces (docs/design/packages-native.md, section 10.7), so it updates hostkeeper by
//! reinstall.

mod openrc;

use std::fs;
use std::path::{Path, PathBuf};

use openrc::{Alpine, HK_ROOT, SERVICE, copy, dir, file, musl_program, nfpm, repo, text, wait};

/// The version of this build: what the Alpine package and its binary are.
const OWN: &str = env!("CARGO_PKG_VERSION");

/// The daemon config: every key the loader requires. The installed daemon never reaches its
/// addresses: it waits for its enrollment.
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
