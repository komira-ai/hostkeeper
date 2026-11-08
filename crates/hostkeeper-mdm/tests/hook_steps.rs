//! The `mdm` package hook's `cutover`, `verify`, `rollback`, `cleanup` and `uninstall`, and what
//! every step refuses, on the fake host: the `install`, `tls` and `verify` tests of the kit
//! re-pointed at the package's program.

mod common;
mod hook_support;

use std::ffi::OsString;

use common::SVC;
use hook_support::*;
use hostkeeper_hook::Class;
use hostkeeper_mdm_core::package::{self, CERT_SERVICE, CERT_TIMER, DEPSYNCER, PROXY, RUN};
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{Host, SYSTEMCTL};

/// `ss -Hltn` with the three services on their loopback ports, and the proxy on the LAN
/// address when `proxy`.
fn listening(host: &FakeHost, proxy: bool) {
    let mut text = String::new();
    for port in [9004, 9005, 9001] {
        text.push_str(&format!("LISTEN 0 4096 127.0.0.1:{port} 0.0.0.0:*\n"));
    }
    if proxy {
        text.push_str(&format!("LISTEN 0 4096 {}:443 0.0.0.0:*\n", common::bind()));
    }
    host.reply(&["/usr/bin/ss", "-Hltn"], 0, &text);
}

/// `units` active.
fn active(host: &FakeHost, units: &[&str]) {
    for unit in units {
        host.reply(&[SYSTEMCTL, "is-active", unit], 0, "active\n");
    }
}

/// A host on which `prepare` ran (an update when `previous`), with no command recorded since.
fn prepared(run: &Run, previous: bool) -> FakeHost {
    let host = package_host();
    let ended = run.prepare(&host, previous);
    assert_eq!(ended.code, 0, "{}", ended.err);
    host.clear_calls();
    host
}

/// Catches a `cutover` that starts the new version in another order, leaves a unit of the old
/// version running, or starts the proxy without its certificate: it reloads systemd, enables
/// the proxy and the timer, restarts the run units and the timer, leaves the proxy waiting,
/// restarts depsyncer only if it runs, and starts no certificate job without the DNS
/// credentials.
#[test]
fn cutover_reloads_enables_and_restarts_the_new_version() {
    let run = Run::new("cutover");
    let host = prepared(&run, true);
    let ended = run.step(&host, &["cutover"], true);
    assert_eq!(ended.code, 0, "{}", ended.err);
    assert_eq!(
        systemctl(&host),
        [
            "daemon-reload".to_owned(),
            format!("enable -- {PROXY} {CERT_TIMER}"),
            format!("restart -- {} {CERT_TIMER}", RUN.join(" ")),
            format!("try-restart -- {DEPSYNCER}"),
        ]
    );
    assert!(
        ended
            .err
            .contains(&format!("{PROXY}: waits for a TLS certificate in {CERTS}"))
    );
}

/// Catches a `cutover` that leaves the proxy on the old binary once it has its certificate, or
/// never asks for the first certificate: with a certificate there the proxy is restarted; with
/// none and the DNS credentials there, the certificate job is started without waiting.
#[test]
fn cutover_restarts_the_proxy_with_its_certificate_and_asks_for_the_first_one() {
    let run = Run::new("cutover-tls");
    let host = prepared(&run, true);
    with_certificate(&host);
    with_dns_env(&host);
    assert_eq!(run.step(&host, &["cutover"], true).code, 0);
    let ran = systemctl(&host);
    assert!(ran.contains(&format!("restart -- {PROXY}")), "{ran:?}");
    assert!(!ran.iter().any(|c| c.contains(CERT_SERVICE)), "{ran:?}");

    let run = Run::new("cutover-first-cert");
    let host = prepared(&run, true);
    with_dns_env(&host);
    assert_eq!(run.step(&host, &["cutover"], true).code, 0);
    let ran = systemctl(&host);
    assert!(!ran.contains(&format!("restart -- {PROXY}")), "{ran:?}");
    assert_eq!(
        ran.last(),
        Some(&format!("start --no-block -- {CERT_SERVICE}"))
    );
}

/// Catches steps run out of order: `cutover` before `prepare`, `verify` before `cutover` and
/// `cleanup` before `verify` each fail with their reason and change nothing; `cleanup` says it
/// is reversible, so the daemon rolls back rather than calling it committed (the others leave
/// it out: they come before the commit anyway).
#[test]
fn a_step_before_the_one_it_needs_fails_and_changes_nothing() {
    let run = Run::new("order");
    let host = package_host();
    for (step, reason) in [
        ("cutover", "not_prepared"),
        ("verify", "not_cut_over"),
        ("cleanup", "not_verified"),
    ] {
        let ended = run.step(&host, &[step], true);
        assert_eq!(ended.code, 1, "{step}");
        let report = ended.report();
        assert_eq!(report.reason.as_deref(), Some(reason), "{step}");
        assert_eq!(report.class, Class::Release, "{step}");
        assert_eq!(report.reversible_stated, step == "cleanup", "{step}");
        assert!(report.reversible, "{step}");
    }
    assert_eq!(systemctl(&host), Vec::<String>::new());
}

/// Catches a `verify` that passes a stack that does not serve, fails one that is still coming
/// up, or judges what no update changes: with the run units active on their ports it
/// succeeds (the proxy, the certificate, the DNS credentials and the enrollment profile, all
/// absent, are printed as notes); a run unit not active, or a port not listened on, defers it
/// (75, `not_ready`, retry after 5 s); a listener on port 80 or a drop-in that differs from the
/// render fails it (1, `check_failed`).
#[test]
fn verify_passes_a_serving_stack_defers_one_coming_up_and_fails_a_broken_one() {
    let run = Run::new("verify");
    let host = prepared(&run, true);
    assert_eq!(run.step(&host, &["cutover"], true).code, 0);

    let ended = run.step(&host, &["verify"], true);
    assert_eq!(ended.code, 75, "{}", ended.err);
    let report = ended.report();
    assert_eq!(report.reason.as_deref(), Some("not_ready"));
    assert_eq!(report.retry_after_secs, Some(5));

    active(&host, &RUN);
    let ended = run.step(&host, &["verify"], true);
    assert_eq!(ended.code, 75, "{}", ended.err);
    assert!(
        ended
            .err
            .contains("FAIL  NanoHUB listens on 127.0.0.1:9004 only: nothing listens there"),
        "{}",
        ended.err
    );

    listening(&host, false);
    let ended = run.step(&host, &["verify"], true);
    assert_eq!(ended.code, 0, "{}", ended.err);
    for line in [
        "note  the TLS certificate: none yet in /var/lib/mdm/lego/certificates",
        "note  /etc/mdm/dns.env is a 0600 file of root: /etc/mdm/dns.env is not there",
        "ok    /etc/mdm/Caddyfile is as the settings render it",
    ] {
        assert!(
            ended.err.contains(line),
            "missing {line:?} in {}",
            ended.err
        );
    }

    let mut text = String::from_utf8(host.read(&drop_in(PROXY)).unwrap().unwrap()).unwrap();
    text.push_str("IPAddressAllow=any\n");
    host.put_file(&drop_in(PROXY), 0o644, (0, 0), text.as_bytes());
    let ended = run.step(&host, &["verify"], true);
    assert_eq!(ended.code, 1, "{}", ended.err);
    assert_eq!(ended.report().reason.as_deref(), Some("check_failed"));
    assert_eq!(
        ended.report().detail.as_deref(),
        Some(
            "/etc/systemd/system/mdm-proxy.service.d/settings.conf is as the settings render it: differs from the render"
        )
    );
}

/// Catches a `verify` that does not check the proxy once it has its certificate: with the
/// certificate there, the proxy and the timer must be active and the proxy must listen on the
/// LAN address; until they are, the step is deferred.
#[test]
fn verify_with_a_certificate_waits_for_the_proxy_and_the_timer() {
    let run = Run::new("verify-tls");
    let host = prepared(&run, true);
    assert_eq!(run.step(&host, &["cutover"], true).code, 0);
    with_certificate(&host);
    active(&host, &RUN);
    listening(&host, false);
    host.reply(
        &[
            "/usr/bin/openssl",
            "x509",
            "-noout",
            "-checkend",
            "1814400",
            "-in",
            CERT,
        ],
        0,
        "",
    );
    let ended = run.step(&host, &["verify"], true);
    assert_eq!(ended.code, 75, "{}", ended.err);
    let detail = ended.report().detail.unwrap_or_default();
    assert!(
        detail.contains("mdm-proxy.service is active: is inactive"),
        "{detail}"
    );
    assert!(
        detail.contains("mdm-cert.timer is active: is inactive"),
        "{detail}"
    );
    active(&host, &[PROXY, CERT_TIMER]);
    listening(&host, true);
    let ended = run.step(&host, &["verify"], true);
    assert_eq!(ended.code, 0, "{}", ended.err);
    assert!(
        ended
            .err
            .contains("note  the certificate is valid for 21 more days: ok")
    );
}

/// Catches a `rollback` of an update that leaves the new version's rendered files, or does not
/// start what the daemon linked back: each file `prepare` changed is put back byte for byte
/// (and one it made is removed), systemd is reloaded and the run units restarted; the record is
/// forgotten, so a second rollback changes nothing more.
#[test]
fn rollback_of_an_update_puts_the_files_back_and_restarts_the_old_version() {
    let run = Run::new("rollback-update");
    let host = package_host();
    // An earlier version left a Caddyfile and a drop-in that differ from this one's render.
    host.add_user("mdm", SVC);
    host.put_dir(ETC, 0o750, (0, SVC));
    host.put_file(CADDYFILE, 0o640, (0, SVC), b"THE OLD CADDYFILE\n");
    host.put_dir(&format!("{UNITS}/mdm.slice.d"), 0o755, (0, 0));
    host.put_file(
        &drop_in("mdm.slice"),
        0o644,
        (0, 0),
        b"[Slice]\nMemoryMax=2G\n",
    );
    assert_eq!(run.prepare(&host, true).code, 0);
    assert_ne!(
        host.read(CADDYFILE).unwrap().unwrap(),
        b"THE OLD CADDYFILE\n"
    );
    assert_eq!(run.step(&host, &["cutover"], true).code, 0);
    host.clear_calls();
    let ended = run.step(&host, &["rollback"], true);
    assert_eq!(ended.code, 0, "{}", ended.err);
    assert!(ended.report().reversible);
    assert_eq!(
        host.read(CADDYFILE).unwrap().unwrap(),
        b"THE OLD CADDYFILE\n"
    );
    assert_eq!(
        host.read(&drop_in("mdm.slice")).unwrap().unwrap(),
        b"[Slice]\nMemoryMax=2G\n"
    );
    for unit in [PROXY, DEPSYNCER, CERT_SERVICE, package::CERT_RENEW] {
        assert_eq!(host.read(&drop_in(unit)).unwrap(), None, "{unit}");
    }
    assert_eq!(
        systemctl(&host),
        [
            "daemon-reload".to_owned(),
            format!("restart -- {} {CERT_TIMER}", RUN.join(" ")),
            format!("try-restart -- {DEPSYNCER}"),
        ]
    );
    host.clear_calls();
    let before = host.paths();
    let again = run.step(&host, &["rollback"], true);
    assert_eq!(again.code, 0, "{}", again.err);
    assert_eq!(host.paths(), before);
    assert_eq!(systemctl(&host), Vec::<String>::new());
}

/// Catches a `rollback` of a first install that leaves units running or enabled: it stops
/// every unit of the package (a unit the daemon already unlinked, which systemctl answers 5
/// for, is nothing to stop), removes the links the hook's `enable` made and the files it
/// rendered.
#[test]
fn rollback_of_a_first_install_stops_the_units_and_removes_what_it_enabled() {
    let run = Run::new("rollback-first");
    let host = prepared(&run, false);
    assert_eq!(run.step(&host, &["cutover"], false).code, 0);
    for link in [
        "/etc/systemd/system/multi-user.target.wants/mdm-proxy.service",
        "/etc/systemd/system/timers.target.wants/mdm-cert.timer",
    ] {
        host.put_symlink(link, "/etc/systemd/system/unit");
    }
    let all: Vec<&str> = package::UNITS.iter().map(|u| u.name).collect();
    host.reply(&[&[SYSTEMCTL, "stop", "--"][..], &all[..]].concat(), 5, "");
    host.clear_calls();
    let ended = run.step(&host, &["rollback"], false);
    assert_eq!(ended.code, 0, "{}", ended.err);
    assert_eq!(systemctl(&host), [format!("stop -- {}", all.join(" "))]);
    assert!(
        ended.err.contains("not loaded, nothing to stop"),
        "{}",
        ended.err
    );
    let paths = host.paths();
    for gone in rendered_files().iter().map(String::as_str).chain([
        "/etc/systemd/system/multi-user.target.wants/mdm-proxy.service",
        "/etc/systemd/system/timers.target.wants/mdm-cert.timer",
    ]) {
        assert!(!paths.iter().any(|p| p == gone), "{gone} is still there");
    }
    // The state and the secrets stay: a later install keeps the CA and the keys.
    assert!(host.read(CA).unwrap().is_some());
    assert!(host.read(&format!("{ETC}/scep.env")).unwrap().is_some());
}

/// Catches a `rollback` that fails a step it cannot undo silently: a restart the old version's
/// units refuse fails the rollback with `unit_failed`.
#[test]
fn a_restart_the_units_refuse_fails_the_step() {
    let run = Run::new("unit-not-loaded");
    let host = prepared(&run, true);
    // A unit systemctl does not know is a failure of a restart, not nothing to do.
    host.reply_prefix(&[SYSTEMCTL, "enable"], 5, "");
    let ended = run.step(&host, &["cutover"], true);
    assert_eq!(
        (ended.code, ended.report().reason.as_deref()),
        (1, Some("unit_failed"))
    );

    let run = Run::new("unit-failed");
    let host = prepared(&run, true);
    host.reply_prefix(&[SYSTEMCTL, "restart"], 1, "");
    let ended = run.step(&host, &["cutover"], true);
    assert_eq!(ended.code, 1);
    let report = ended.report();
    assert_eq!(report.reason.as_deref(), Some("unit_failed"));
    assert_eq!(
        report.detail.as_deref(),
        Some(
            "/usr/bin/systemctl restart -- mdm-scep.service mdm-nanohub.service mdm-depserver.service mdm-cert.timer exited with status 1"
        )
    );
}

/// Catches a `cleanup` that keeps the files kept for a rollback, or is not safe to run twice:
/// after `verify` succeeded it forgets them and succeeds, and run again it succeeds.
#[test]
fn cleanup_after_verify_forgets_the_kept_files_and_runs_twice() {
    let run = Run::new("cleanup");
    let host = prepared(&run, true);
    assert_eq!(run.step(&host, &["cutover"], true).code, 0);
    active(&host, &RUN);
    listening(&host, false);
    assert_eq!(run.step(&host, &["verify"], true).code, 0);
    assert!(run.record().join("saved").exists());
    for _ in 0..2 {
        let ended = run.step(&host, &["cleanup"], true);
        assert_eq!(ended.code, 0, "{}", ended.err);
        assert!(!run.record().join("saved").exists());
    }
}

/// Catches an `uninstall` that leaves a unit running or enabled, deletes the rendered files
/// instead of keeping them, or removes the state: it stops every unit, removes the links of the
/// units the hook and `dep define-profile` enabled, moves the Caddyfile and the drop-ins to
/// `/var/lib/mdm/removed/<now>/`, reloads systemd, and keeps the state and the secrets.
#[test]
fn uninstall_stops_the_units_and_moves_the_rendered_files_aside() {
    let run = Run::new("uninstall");
    let host = prepared(&run, true);
    // A snapshot of this second is there already: the next free name is taken.
    host.put_dir("/var/lib/mdm/removed", 0o700, (SVC, SVC));
    host.put_dir("/var/lib/mdm/removed/1700000000", 0o700, (SVC, SVC));
    let wanted = "/etc/systemd/system/multi-user.target.wants/mdm-depsyncer.service";
    host.put_symlink(wanted, "/etc/systemd/system/mdm-depsyncer.service");
    let caddyfile = host.read(CADDYFILE).unwrap().unwrap();
    let ended = run.step(&host, &["uninstall"], true);
    assert_eq!(ended.code, 0, "{}", ended.err);
    let all: Vec<&str> = package::UNITS.iter().map(|u| u.name).collect();
    assert_eq!(
        systemctl(&host),
        [
            format!("stop -- {}", all.join(" ")),
            "daemon-reload".to_owned()
        ]
    );
    let paths = host.paths();
    for gone in rendered_files().iter().map(String::as_str).chain([wanted]) {
        assert!(!paths.iter().any(|p| p == gone), "{gone} is still there");
    }
    let kept = "/var/lib/mdm/removed/1700000000-1/Caddyfile";
    assert_eq!(host.read(kept).unwrap().unwrap(), caddyfile);
    assert!(
        host.read("/var/lib/mdm/removed/1700000000-1/mdm-proxy.service.d_settings.conf")
            .unwrap()
            .is_some()
    );
    assert!(host.read(CA).unwrap().is_some());
    assert!(ended.err.contains("kept: /var/lib/mdm"), "{}", ended.err);
}

/// Catches a step that runs while another holds the record: it fails with `record_locked` and
/// changes nothing.
#[test]
fn a_step_while_another_holds_the_record_is_refused() {
    let run = Run::new("locked");
    let host = package_host();
    std::fs::create_dir_all(run.record()).unwrap();
    let held = std::fs::File::open(run.record()).unwrap();
    held.try_lock().unwrap();
    let before = host.paths();
    let ended = run.prepare(&host, false);
    assert_eq!(ended.code, 1);
    assert_eq!(ended.report().reason.as_deref(), Some("record_locked"));
    assert_eq!(host.paths(), before);
    assert!(host.calls().is_empty());
}

/// Catches a hook that acts on a command line or params file it cannot read: a missing or
/// unknown step and `prepare` without its release exit 2 (`usage`); a step without
/// `--params`, a params file that is not there, one without `settings`, with a relative
/// `record_dir`, with a settings file other than the manifest's, or with a previous version
/// and no previous release, exit 1 (`params`). Every one writes a well-formed last line and
/// runs nothing.
#[test]
fn a_command_line_or_params_file_the_hook_cannot_read_is_refused() {
    let run = Run::new("usage");
    let host = package_host();
    let hook = |words: &[&str]| {
        let mut args: Vec<OsString> = ["mdm", "update-hook"].map(OsString::from).to_vec();
        args.extend(words.iter().map(OsString::from));
        run_mdm(&host, &args)
    };
    for words in [&[][..], &["deploy"], &["prepare"]] {
        let ended = hook(words);
        assert_eq!(ended.code, 2, "{words:?}");
        assert_eq!(ended.report().reason.as_deref(), Some("usage"), "{words:?}");
    }
    let missing = run.scratch.path().join("absent.toml");
    let no_settings = run.scratch.path().join("no-settings.toml");
    std::fs::write(&no_settings, "record_dir = \"/r\"\n").unwrap();
    let relative = run.scratch.path().join("relative.toml");
    std::fs::write(
        &relative,
        "settings = \"/etc/mdm/settings\"\nrecord_dir = \"r\"\n",
    )
    .unwrap();
    let other = run.scratch.path().join("other-settings.toml");
    std::fs::write(
        &other,
        "settings = \"/etc/elsewhere\"\nrecord_dir = \"/r\"\n",
    )
    .unwrap();
    let half = run.scratch.path().join("half.toml");
    std::fs::write(
        &half,
        "settings = \"/etc/mdm/settings\"\nrecord_dir = \"/r\"\n\"updater.previous_version\" = \"1.0.0\"\n",
    )
    .unwrap();
    for words in [
        vec!["cutover".to_owned()],
        vec![
            "cutover".to_owned(),
            "--params".to_owned(),
            missing.display().to_string(),
        ],
        vec![
            "cutover".to_owned(),
            "--params".to_owned(),
            no_settings.display().to_string(),
        ],
        vec![
            "cutover".to_owned(),
            "--params".to_owned(),
            relative.display().to_string(),
        ],
    ] {
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let ended = hook(&words);
        assert_eq!(ended.code, 1, "{words:?}");
        assert_eq!(
            ended.report().reason.as_deref(),
            Some("params"),
            "{words:?}"
        );
        assert_eq!(ended.report().class, Class::Environment, "{words:?}");
    }
    assert!(host.calls().is_empty());
}

/// Catches an `uninstall` of a host the package never prepared failing or making a snapshot of
/// nothing: it stops the units, makes no directory and succeeds.
#[test]
fn uninstall_of_a_host_never_prepared_stops_the_units_and_keeps_nothing() {
    let run = Run::new("uninstall-empty");
    let host = package_host();
    let before = host.paths();
    let ended = run.step(&host, &["uninstall"], false);
    assert_eq!(ended.code, 0, "{}", ended.err);
    assert_eq!(host.paths(), before);
    assert_eq!(systemctl(&host).len(), 2);
}

/// Catches an `uninstall` that moves aside what it could not read: a rendered file it may not
/// read fails the step before anything is moved or stopped.
#[test]
fn uninstall_that_cannot_read_a_rendered_file_fails_before_it_changes_anything() {
    let run = Run::new("uninstall-unreadable");
    let host = prepared(&run, true);
    host.deny_read(CADDYFILE);
    let before = host.paths();
    let ended = run.step(&host, &["uninstall"], true);
    assert_eq!(
        (ended.code, ended.report().reason.as_deref()),
        (1, Some("step_failed"))
    );
    assert_eq!(host.paths(), before);
    assert_eq!(systemctl(&host), Vec::<String>::new());
}

/// Catches a `verify` that passes what an update broke for good, or reads the sockets from a
/// tool that failed: something on port 80, or a Caddyfile another user may read, fails it
/// (`check_failed`); an `ss` that exits non-zero fails it as the host's (`step_failed`).
#[test]
fn verify_fails_a_listener_on_port_80_a_wrong_mode_and_an_ss_that_fails() {
    let run = Run::new("verify-80");
    let host = prepared(&run, true);
    assert_eq!(run.step(&host, &["cutover"], true).code, 0);
    active(&host, &RUN);
    host.reply(
        &["/usr/bin/ss", "-Hltn"],
        0,
        "LISTEN 0 4096 127.0.0.1:9004 0.0.0.0:*\nLISTEN 0 4096 127.0.0.1:9005 0.0.0.0:*\nLISTEN 0 4096 127.0.0.1:9001 0.0.0.0:*\nLISTEN 0 4096 0.0.0.0:80 0.0.0.0:*\n",
    );
    let ended = run.step(&host, &["verify"], true);
    assert_eq!(
        (ended.code, ended.report().reason.as_deref()),
        (1, Some("check_failed"))
    );
    assert_eq!(
        ended.report().detail.as_deref(),
        Some("nothing listens on port 80: listening on 0.0.0.0:80")
    );

    listening(&host, false);
    let caddyfile = host.read(CADDYFILE).unwrap().unwrap();
    host.put_file(CADDYFILE, 0o644, (0, SVC), &caddyfile);
    let ended = run.step(&host, &["verify"], true);
    assert_eq!(ended.code, 1);
    assert_eq!(
        ended.report().detail.as_deref(),
        Some(
            "/etc/mdm/Caddyfile is as the settings render it: is File 0644 owner 0:900, not File 0640 owner root:mdm"
        )
    );

    host.put_file(CADDYFILE, 0o640, (0, SVC), &caddyfile);
    host.deny_stat(&drop_in(DEPSYNCER));
    let ended = run.step(&host, &["verify"], true);
    assert_eq!(ended.code, 1);
    let detail = ended.report().detail.unwrap_or_default();
    assert!(
        detail.starts_with("/etc/systemd/system/mdm-depsyncer.service.d/settings.conf is as the settings render it: "),
        "{detail}"
    );

    host.reply(&["/usr/bin/ss", "-Hltn"], 1, "");
    let ended = run.step(&host, &["verify"], true);
    assert_eq!(
        (ended.code, ended.report().reason.as_deref()),
        (1, Some("step_failed"))
    );
    assert_eq!(ended.report().class, Class::Environment);
}
