//! The `mdm` package hook's `prepare`, on the fake host: the checks of `hostkeeper-mdm install`
//! re-pointed at the package's program, and what it makes.

mod common;
mod hook_support;

use common::{SVC, random_hex};
use hook_support::*;
use hostkeeper_hook::Class;
use hostkeeper_mdm_core::package;
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{Host, Kind};

/// Catches a first `prepare` that does not make what the stack needs, makes it with other
/// modes or owners, starts anything, or shows a secret: the user, the directories, the three
/// secrets (made from the random source, never in the output), the CA by the staged release's
/// `scepserver` as the service user, the Caddyfile and the four drop-ins, each with its mode and
/// owner; no unit is started or restarted, and the step succeeds with `reversible: true`.
#[test]
fn a_first_prepare_makes_the_user_the_state_the_secrets_the_ca_and_the_rendered_files() {
    let run = Run::new("prepare-first");
    let host = package_host();
    let ended = run.prepare(&host, false);
    assert_eq!(ended.code, 0, "{}", ended.err);
    let report = ended.report();
    assert_eq!(report.class, Class::Release);
    assert!(report.reversible && report.reversible_stated);
    let meta = |path: &str| {
        let m = host.stat(path).unwrap().unwrap_or_else(|| panic!("{path}"));
        (m.kind, m.mode, m.uid, m.gid)
    };
    assert_eq!(meta(STATE), (Kind::Dir, 0o700, SVC, SVC));
    for dir in [
        "nanohub",
        "scep",
        "scep/depot",
        "dep",
        "lego",
        "caddy",
        "www",
    ] {
        assert_eq!(
            meta(&format!("{STATE}/{dir}")),
            (Kind::Dir, 0o700, SVC, SVC)
        );
    }
    assert_eq!(meta(ETC), (Kind::Dir, 0o750, 0, SVC));
    assert_eq!(meta("/var/lib/mdm-pkg"), (Kind::Dir, 0o755, 0, 0));
    for (file, key) in [
        ("nanohub.env", "NANOHUB_API_KEY"),
        ("nanodep.env", "NANODEP_API"),
        ("scep.env", "SCEP_CHALLENGE_PASSWORD"),
    ] {
        let path = format!("{ETC}/{file}");
        assert_eq!(meta(&path), (Kind::File, 0o600, 0, 0));
        let text = String::from_utf8(host.read(&path).unwrap().unwrap()).unwrap();
        assert_eq!(text, format!("{key}={}\n", random_hex()));
    }
    assert!(!ended.err.contains(&random_hex()) && !ended.out.contains(&random_hex()));
    assert_eq!(meta(CADDYFILE), (Kind::File, 0o640, 0, SVC));
    for unit in [
        package::SLICE,
        package::PROXY,
        package::DEPSYNCER,
        package::CERT_SERVICE,
    ] {
        assert_eq!(
            meta(&format!("{UNITS}/{unit}.d")),
            (Kind::Dir, 0o755, 0, 0),
            "{unit}"
        );
        assert_eq!(meta(&drop_in(unit)), (Kind::File, 0o644, 0, 0), "{unit}");
    }
    let calls: Vec<Vec<String>> = host.calls();
    assert!(calls.contains(&ca_argv()), "{calls:?}");
    assert_eq!(systemctl(&host), Vec::<String>::new());
    assert!(ended.err.contains(&format!(
        "{DNS_ENV} is not there: the certificate job waits for it"
    )));
}

/// Catches a second `prepare` that changes anything, makes another secret or CA, or keeps what
/// the first wrote as what was there before: the rerun changes no file, runs no command that
/// changes the host, and the files kept for a rollback are still the first run's (nothing).
#[test]
fn a_second_prepare_changes_nothing() {
    let run = Run::new("prepare-twice");
    let host = package_host();
    assert_eq!(run.prepare(&host, false).code, 0);
    let before: Vec<(String, Option<Vec<u8>>)> = host
        .paths()
        .into_iter()
        .map(|p| {
            let bytes = host.read(&p).ok().flatten();
            (p, bytes)
        })
        .collect();
    host.clear_calls();
    let again = run.prepare(&host, false);
    assert_eq!(again.code, 0, "{}", again.err);
    let after: Vec<(String, Option<Vec<u8>>)> = host
        .paths()
        .into_iter()
        .map(|p| {
            let bytes = host.read(&p).ok().flatten();
            (p, bytes)
        })
        .collect();
    assert_eq!(before, after);
    assert!(again.err.contains("nothing to do"), "{}", again.err);
    assert!(again.err.contains("SCEP CA: kept"), "{}", again.err);
    assert!(!host.calls().contains(&ca_argv()));
}

/// Catches `prepare` acting on a host the kit's install refuses, or with another refusal than
/// the one that applies: each case exits 1 with its reason token and class, the detail on
/// stderr, and nothing on the host changed.
#[test]
fn prepare_refuses_a_host_the_install_refuses_and_changes_nothing() {
    type Setup = fn(&FakeHost);
    let cases: [(&str, Setup, &str, Class); 9] = [
        ("not_root", |_| {}, "not_root", Class::Environment),
        (
            "arm",
            |h| h.reply(&["/usr/bin/uname", "-m"], 0, "aarch64\n"),
            "wrong_machine",
            Class::Environment,
        ),
        (
            "unbound",
            |h| h.reply(&["/usr/bin/ip", "-o", "-4", "addr", "show"], 0, ""),
            "bind_not_carried",
            Class::Environment,
        ),
        (
            "held",
            |h| {
                h.reply(
                    &["/usr/bin/ss", "-Hltn"],
                    0,
                    "LISTEN 0 4096 127.0.0.1:9004 0.0.0.0:*\n",
                )
            },
            "port_held",
            Class::Environment,
        ),
        (
            "half-a-ca",
            |h| h.put_file(CA, 0o400, (SVC, SVC), b"CA"),
            "depot_damaged",
            Class::Environment,
        ),
        ("link", |h| h.put_link(STATE), "refused", Class::Environment),
        (
            "old-lego",
            |h| h.reply(&[LEGO, "run", "--help"], 0, "   --accept-tos\n"),
            "bad_lego",
            Class::Release,
        ),
        (
            "no-scepserver",
            |h| {
                h.remove(SCEPSERVER).unwrap();
            },
            "bad_artifact",
            Class::Release,
        ),
        (
            "fixed-key",
            |h| {
                let text = format!("{}MDM_USER=hostkeeper-mdm\n", package_example());
                h.put_file(SETTINGS, 0o600, (0, 0), text.as_bytes());
            },
            "bad_settings",
            Class::Environment,
        ),
    ];
    for (name, setup, reason, class) in cases {
        let run = Run::new(&format!("refuse-{name}"));
        let host = match name {
            "not_root" => package_host_as(1000),
            _ => package_host(),
        };
        setup(&host);
        let before = host.paths();
        let ended = run.prepare(&host, false);
        assert_eq!(ended.code, 1, "{name}: {}", ended.err);
        let report = ended.report();
        assert_eq!(
            report.reason.as_deref(),
            Some(reason),
            "{name}: {}",
            ended.err
        );
        assert_eq!(report.class, class, "{name}");
        assert_eq!(host.paths(), before, "{name}");
        assert_eq!(systemctl(&host), Vec::<String>::new(), "{name}");
        assert!(
            ended.err.contains(&format!(
                "mdm update-hook: {}",
                report.detail.unwrap_or_default()
            )),
            "{name}: {}",
            ended.err
        );
    }
}

/// The package host, run as `euid`.
fn package_host_as(euid: u32) -> FakeHost {
    let host = FakeHost::new(euid);
    host.put_file(SETTINGS, 0o644, (0, 0), package_example().as_bytes());
    host
}

/// Catches `prepare` refusing an update while the old version serves, or ignoring the DNS
/// credentials it can check: the ports held by the package's own active units are a rerun,
/// not a conflict; a `dns.env` holding the provider's variable is accepted (and named, its
/// value never shown); one another user may read, or one without the variable, is refused
/// before anything changes.
#[test]
fn prepare_takes_the_ports_of_its_own_units_and_checks_the_dns_credentials() {
    let run = Run::new("prepare-update");
    let host = package_host();
    let mut ss = String::new();
    for port in [9004, 9005, 9001, 443] {
        ss.push_str(&format!("LISTEN 0 4096 127.0.0.1:{port} 0.0.0.0:*\n"));
    }
    host.reply(&["/usr/bin/ss", "-Hltn"], 0, &ss);
    for unit in [
        "mdm-nanohub.service",
        "mdm-scep.service",
        "mdm-depserver.service",
        "mdm-proxy.service",
    ] {
        host.reply(
            &[hostkeeper_mdm_step::SYSTEMCTL, "is-active", unit],
            0,
            "active\n",
        );
    }
    with_dns_env(&host);
    let ended = run.prepare(&host, true);
    assert_eq!(ended.code, 0, "{}", ended.err);
    assert!(
        ended
            .err
            .contains("port 9004: held by mdm-nanohub.service (a rerun)")
    );
    assert!(
        ended
            .err
            .contains("/etc/mdm/dns.env: holds CLOUDFLARE_DNS_API_TOKEN")
    );
    assert!(!ended.err.contains("=token"), "{}", ended.err);

    for (name, mode, text) in [
        ("readable", 0o644, &b"CLOUDFLARE_DNS_API_TOKEN=token\n"[..]),
        ("empty", 0o600, b"OTHER=1\n"),
    ] {
        let run = Run::new(&format!("prepare-dns-{name}"));
        let host = package_host();
        host.put_file(DNS_ENV, mode, (0, 0), text);
        let before = host.paths();
        let ended = run.prepare(&host, false);
        assert_eq!(ended.code, 1, "{name}");
        assert_eq!(ended.report().reason.as_deref(), Some("refused"), "{name}");
        assert_eq!(host.paths(), before, "{name}");
    }
}

/// Catches `prepare` taking a staged release whose path it cannot pass on, or a step that
/// fails being reported as a refusal: a path that is not UTF-8 is the release's
/// (`bad_artifact`), and a `useradd` that fails stops the plan at that step (`step_failed`,
/// class `environment`).
#[test]
fn a_staged_path_that_is_not_text_and_a_step_that_fails_end_prepare() {
    use std::os::unix::ffi::OsStringExt;
    let run = Run::new("prepare-bytes");
    let host = package_host();
    let params = run.params(false);
    let args = [
        std::ffi::OsString::from("mdm"),
        "update-hook".into(),
        "prepare".into(),
        std::ffi::OsString::from_vec(b"/var/lib/hostkeeper/packages/mdm/\xff".to_vec()),
        "--params".into(),
        params.into_os_string(),
    ];
    let ended = run_mdm(&host, &args);
    assert_eq!(ended.code, 1);
    assert_eq!(ended.report().reason.as_deref(), Some("bad_artifact"));
    assert!(host.calls().is_empty());

    let run = Run::new("prepare-useradd");
    let host = package_host();
    host.reply(
        &[
            "/usr/sbin/useradd",
            "--system",
            "--user-group",
            "--home-dir",
            STATE,
            "--no-create-home",
            "--shell",
            "/usr/sbin/nologin",
            "mdm",
        ],
        9,
        "",
    );
    let ended = run.prepare(&host, false);
    assert_eq!(ended.code, 1);
    let report = ended.report();
    assert_eq!(
        (report.class, report.reason.as_deref()),
        (Class::Environment, Some("step_failed"))
    );
    assert_eq!(
        report.detail.as_deref(),
        Some(
            "step 1 (useradd mdm (system user and group, home /var/lib/mdm, no login)) failed: exit status 9"
        )
    );
}

/// Catches a second `prepare` (after the operator changed a rendered file by hand) keeping its
/// own write as what was there before: the rollback then puts back what the first `prepare`
/// found.
#[test]
fn a_rollback_after_two_prepares_puts_back_what_the_first_found() {
    let run = Run::new("prepare-twice-rollback");
    let host = package_host();
    host.add_user("mdm", SVC);
    host.put_file(CADDYFILE, 0o640, (0, SVC), b"BEFORE\n");
    assert_eq!(run.prepare(&host, true).code, 0);
    host.put_file(CADDYFILE, 0o640, (0, SVC), b"BY HAND\n");
    assert_eq!(run.prepare(&host, true).code, 0);
    assert_ne!(host.read(CADDYFILE).unwrap().unwrap(), b"BY HAND\n");
    let ended = run.step(&host, &["rollback"], true);
    assert_eq!(ended.code, 0, "{}", ended.err);
    assert_eq!(host.read(CADDYFILE).unwrap().unwrap(), b"BEFORE\n");
}
