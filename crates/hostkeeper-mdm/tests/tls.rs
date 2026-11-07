//! `tls` and the secret reader on the fake host.

mod common;

use common::{FakeNet, SVC, changes, config, kit, ready_host, with_env};
use hostkeeper_mdm::tls::{LEGO_FLAGS, run};
use hostkeeper_mdm::{MdmError, secret};
use hostkeeper_mdm_core::render::render_tls;
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{Host, Mode, SYSTEMCTL};

const DNS_ENV: &str = "/etc/hostkeeper-mdm/dns.env";
const LEGO: &str = "/opt/hostkeeper-mdm/bin/lego";
const CERT: &str = "/var/lib/hostkeeper-mdm/lego/certificates/mdm.example.com.crt";
const TOKEN: &str = "tok-SECRET-1234";
const KEY: &str = "/var/lib/hostkeeper-mdm/lego/certificates/mdm.example.com.key";
const UNITS: &str = "/etc/systemd/system";

fn openssl_show() -> [&'static str; 8] {
    [
        "/usr/bin/openssl",
        "x509",
        "-noout",
        "-subject",
        "-issuer",
        "-enddate",
        "-in",
        CERT,
    ]
}

const OPENSSL: &str = "/usr/bin/openssl";
/// The seconds `verify` and `tls` call "ending soon": `VALID_FOR_DAYS` days.
const SOON: &str = "1814400";

fn openssl_read() -> [&'static str; 5] {
    [OPENSSL, "x509", "-noout", "-in", CERT]
}

fn openssl_cert_key() -> [&'static str; 6] {
    [OPENSSL, "x509", "-noout", "-pubkey", "-in", CERT]
}

fn openssl_key_key() -> [&'static str; 5] {
    [OPENSSL, "pkey", "-pubout", "-in", KEY]
}

fn openssl_checkend() -> [&'static str; 7] {
    [OPENSSL, "x509", "-noout", "-checkend", SOON, "-in", CERT]
}

/// A host ready for `tls`: root, the DNS key file, lego with every flag, nothing issued.
fn tls_host() -> FakeHost {
    let host = ready_host(&kit());
    host.put_file(
        DNS_ENV,
        0o600,
        (0, 0),
        format!("CLOUDFLARE_DNS_API_TOKEN={TOKEN}\n").as_bytes(),
    );
    host.put_file(LEGO, 0o755, (0, 0), b"lego");
    host.reply(&[LEGO, "run", "--help"], 0, &LEGO_FLAGS.join("  "));
    host.reply(
        &openssl_show(),
        0,
        "subject=CN = mdm.example.com\nissuer=CN = Example CA\nnotAfter=Jan  1 00:00:00 2027 GMT\n",
    );
    // What openssl says of a certificate that is whole, matches its key and is valid for long.
    host.reply(&openssl_read(), 0, "");
    host.reply(&openssl_cert_key(), 0, "PUBLIC-KEY-A\n");
    host.reply(&openssl_key_key(), 0, "PUBLIC-KEY-A\n");
    host.reply(&openssl_checkend(), 0, "Certificate will not expire\n");
    host
}

/// A host with the certificate and its key already there.
fn issued_host() -> FakeHost {
    let host = tls_host();
    host.put_file(CERT, 0o644, (SVC, SVC), b"CERT");
    host.put_file(KEY, 0o600, (SVC, SVC), b"KEY");
    host
}

fn go(host: &FakeHost, mode: Mode) -> (Result<(), MdmError>, String) {
    let net = FakeNet::new(Default::default());
    let mut out = Vec::new();
    let result = with_env(host, &net, |env| {
        run(env, &config(), &common::acme(), mode, &mut out)
    });
    (result, String::from_utf8(out).unwrap())
}

fn issue_on_start(host: &FakeHost) {
    host.effect(
        &[SYSTEMCTL, "start", "--", "hostkeeper-mdm-cert.service"],
        CERT,
        0o644,
        (SVC, SVC),
        b"CERT",
    );
    host.effect(
        &[SYSTEMCTL, "start", "--", "hostkeeper-mdm-cert.service"],
        KEY,
        0o600,
        (SVC, SVC),
        b"KEY",
    );
}

/// Catches a dry run that acts or shows the DNS key: it lists the unit writes, the timer and
/// the first issuance, says the proxy waits for the certificate, runs only `lego run --help`,
/// and the key never appears in output or on a command line.
#[test]
fn a_dry_run_lists_the_units_and_the_first_issuance_and_shows_no_key() {
    let host = tls_host();
    let before = host.paths();
    let (result, shown) = go(&host, Mode::DryRun);
    result.unwrap();
    assert_eq!(host.paths(), before);
    assert_eq!(changes(&host), [format!("{LEGO} run --help")]);
    for line in [
        "dns.env: holds CLOUDFLARE_DNS_API_TOKEN (values not shown)",
        "DRY-RUN: write /etc/systemd/system/hostkeeper-mdm-cert.service mode 0644 owner root:root (",
        "DRY-RUN: write /etc/systemd/system/hostkeeper-mdm-cert.timer mode 0644 owner root:root (",
        "DRY-RUN: write /etc/systemd/system/hostkeeper-mdm-proxy-reload.path mode 0644 owner root:root (",
        "DRY-RUN: systemctl daemon-reload",
        "DRY-RUN: systemctl enable -- hostkeeper-mdm-proxy-reload.path",
        "DRY-RUN: systemctl restart -- hostkeeper-mdm-cert.timer",
        "DRY-RUN: systemctl start -- hostkeeper-mdm-cert.service",
        &format!("proxy: started once {CERT} is there"),
    ] {
        assert!(shown.contains(line), "missing {line:?} in\n{shown}");
    }
    assert!(!shown.contains(TOKEN));
    assert!(host.calls().iter().all(|c| !c.join(" ").contains(TOKEN)));
}

/// Catches an apply that writes units unlike the core's render, forgets the first issuance, or
/// starts the proxy before the certificate exists; the certificate it prints is public data.
#[test]
fn apply_writes_the_core_units_issues_once_then_starts_the_proxy() {
    let host = tls_host();
    issue_on_start(&host);
    let (result, shown) = go(&host, Mode::Apply);
    result.unwrap();
    for unit in render_tls(&config(), &common::acme()).unwrap() {
        let n = host.node(&format!("{UNITS}/{}", unit.name)).unwrap();
        assert_eq!((n.mode, n.uid, n.gid), (0o644, 0, 0), "{}", unit.name);
        assert_eq!(
            n.bytes,
            unit.text.as_bytes(),
            "{} differs from the render",
            unit.name
        );
    }
    let ran = changes(&host);
    let issue = ran
        .iter()
        .position(|c| c.ends_with("start -- hostkeeper-mdm-cert.service"))
        .unwrap();
    let proxy = ran
        .iter()
        .position(|c| c.ends_with("start -- hostkeeper-mdm-proxy.service"))
        .unwrap();
    assert!(issue < proxy, "{ran:?}");
    assert!(
        shown.contains("certificate: subject=CN = mdm.example.com\n"),
        "{shown}"
    );
    assert!(shown.contains("certificate: issuer=CN = Example CA\n"));
    assert!(!shown.contains(TOKEN));
    assert!(host.calls().iter().all(|c| !c.join(" ").contains(TOKEN)));
}

/// Catches a rerun that issues again or touches the units: with the certificate there and the
/// units, timer and proxy running, it finds nothing to do (twice, once per plan) and still
/// shows the certificate.
#[test]
fn a_rerun_with_a_certificate_finds_nothing_to_do() {
    let host = tls_host();
    issue_on_start(&host);
    go(&host, Mode::Apply).0.unwrap();
    for unit in [
        "hostkeeper-mdm-proxy-reload.path",
        "hostkeeper-mdm-cert.timer",
        "hostkeeper-mdm-proxy.service",
    ] {
        host.reply(&[SYSTEMCTL, "is-enabled", unit], 0, "enabled\n");
        host.reply(&[SYSTEMCTL, "is-active", unit], 0, "active\n");
    }
    host.clear_calls();
    let (result, shown) = go(&host, Mode::Apply);
    result.unwrap();
    assert_eq!(shown.matches("nothing to do\n").count(), 2, "{shown}");
    assert!(shown.contains("certificate: subject="));
    assert_eq!(
        changes(&host),
        [
            format!("{LEGO} run --help"),
            openssl_read().join(" "),
            openssl_cert_key().join(" "),
            openssl_key_key().join(" "),
            openssl_checkend().join(" "),
            openssl_show().join(" ")
        ]
    );
}

/// Catches a dry run with a certificate that skips the proxy plan: it shows the proxy start
/// and stops (nothing is shown of the certificate, since nothing was applied).
#[test]
fn a_dry_run_with_a_certificate_plans_the_proxy() {
    let host = issued_host();
    let (result, shown) = go(&host, Mode::DryRun);
    result.unwrap();
    assert!(
        shown.contains("DRY-RUN: systemctl start -- hostkeeper-mdm-proxy.service"),
        "{shown}"
    );
    assert!(!shown.contains("certificate:"));
    assert!(!shown.contains("started once"));
}

/// Catches an issuance that "succeeds" with no certificate (the run must fail before the
/// proxy), and a certificate openssl cannot read; both exit 1.
#[test]
fn a_missing_or_unreadable_certificate_fails_the_run() {
    let host = tls_host();
    let (result, _) = go(&host, Mode::Apply);
    let err = result.unwrap_err();
    assert!(matches!(err, MdmError::NoCertificate(_)), "{err}");
    assert_eq!(err.exit_code(), 1);
    assert!(
        !changes(&host)
            .iter()
            .any(|c| c.ends_with("hostkeeper-mdm-proxy.service"))
    );

    let host = tls_host();
    issue_on_start(&host);
    host.reply(&openssl_show(), 1, "");
    let err = go(&host, Mode::Apply).0.unwrap_err();
    assert!(matches!(err, MdmError::NotCertificate(_)), "{err}");
    assert_eq!(err.exit_code(), 1);
}

/// Catches each refusal that must precede any change: not root, a DNS key file that is missing,
/// too open, lacking a variable or holding a control character, lego missing, or a lego
/// without a flag the unit passes (a flag that is only a prefix of another is not found).
#[test]
fn the_checks_refuse_before_anything_changes() {
    let refused = |host: &FakeHost| {
        let before = host.paths();
        let err = go(host, Mode::Apply).0.unwrap_err();
        assert_eq!(host.paths(), before, "changed the host");
        err
    };

    let err = refused(&FakeHost::new(1000));
    assert!(
        matches!(err, MdmError::NeedsRoot { euid: 1000, .. }),
        "{err}"
    );
    assert_eq!(err.exit_code(), 2);

    let host = tls_host();
    Host::remove(&host, DNS_ENV).unwrap();
    let err = refused(&host);
    assert!(matches!(err, MdmError::NoSecret { .. }), "{err}");

    let host = tls_host();
    host.put_file(DNS_ENV, 0o644, (0, 0), b"CLOUDFLARE_DNS_API_TOKEN=x\n");
    assert!(matches!(
        refused(&host),
        MdmError::SecretMode { mode: 0o644, .. }
    ));

    let host = tls_host();
    host.put_file(
        DNS_ENV,
        0o600,
        (0, 0),
        b"OTHER=x\nCLOUDFLARE_DNS_API_TOKEN=\n",
    );
    assert!(matches!(refused(&host), MdmError::NoSecret { .. }));

    let host = tls_host();
    host.put_file(DNS_ENV, 0o600, (0, 0), b"CLOUDFLARE_DNS_API_TOKEN=a\x07b\n");
    let err = refused(&host);
    assert!(matches!(err, MdmError::SecretValue { .. }), "{err}");
    assert!(!err.to_string().contains('\u{7}'));

    let host = tls_host();
    Host::remove(&host, LEGO).unwrap();
    let err = refused(&host);
    assert_eq!(
        err.to_string(),
        format!("{LEGO} is missing: hostkeeper-mdm install --apply installs it")
    );

    let host = tls_host();
    let without_dns: Vec<&str> = LEGO_FLAGS
        .iter()
        .copied()
        .filter(|f| *f != "--dns")
        .collect();
    host.reply(&[LEGO, "run", "--help"], 0, &without_dns.join(", "));
    let err = refused(&host);
    assert!(
        matches!(err, MdmError::LegoFlag { flag: "--dns", .. }),
        "{err}"
    );
    assert_eq!(err.exit_code(), 2);
}

/// Catches a `lego run --help` that exits non-zero but still prints every flag (a wrapper script
/// that fails, an error page): it is refused before any change, exit 2, with the status.
#[test]
fn a_lego_whose_help_exits_non_zero_is_refused_even_when_it_lists_every_flag() {
    let host = tls_host();
    host.reply(&[LEGO, "run", "--help"], 3, &LEGO_FLAGS.join("  "));
    let before = host.paths();
    let err = go(&host, Mode::Apply).0.unwrap_err();
    assert!(
        matches!(&err, MdmError::LegoHelp { lego, code: 3 } if lego == LEGO),
        "{err}"
    );
    assert_eq!(err.exit_code(), 2);
    assert_eq!(
        err.to_string(),
        format!("{LEGO} run --help exited with status 3: not a lego that runs here")
    );
    assert_eq!(host.paths(), before);
    assert_eq!(changes(&host), [format!("{LEGO} run --help")]);
}

/// Catches a directory or a link at the certificate path counted as "certificate present"
/// (the proxy was started on it): both are refused, exit 2, before any change.
#[test]
fn a_directory_or_a_link_at_the_certificate_path_is_refused_before_any_change() {
    for put in [
        (|h: &FakeHost| h.put_dir(CERT, 0o755, (0, 0))) as fn(&FakeHost),
        |h| h.put_symlink(CERT, "/etc/passwd"),
    ] {
        for mode in [Mode::DryRun, Mode::Apply] {
            let host = tls_host();
            put(&host);
            let before = host.paths();
            let err = go(&host, mode).0.unwrap_err();
            assert!(
                matches!(&err, MdmError::CertNotFile(p) if p == CERT),
                "{err}"
            );
            assert_eq!(err.exit_code(), 2);
            assert_eq!(
                err.to_string(),
                format!(
                    "{CERT} is not a regular file (a directory or a link): remove it, and run tls again"
                )
            );
            assert_eq!(host.paths(), before);
            assert_eq!(changes(&host), [format!("{LEGO} run --help")]);
        }
    }
}

/// Catches a certificate file openssl cannot read being carried through the plan: it is
/// refused (exit 2, unlike a certificate that is unreadable after an issuance) before any
/// change, and only the reading `openssl` ran.
#[test]
fn a_certificate_file_openssl_cannot_read_is_refused_before_any_change() {
    let host = issued_host();
    host.reply(&openssl_read(), 1, "");
    let before = host.paths();
    let err = go(&host, Mode::Apply).0.unwrap_err();
    assert!(
        matches!(&err, MdmError::CertUnreadable(p) if p == CERT),
        "{err}"
    );
    assert_eq!(err.exit_code(), 2);
    assert_eq!(
        err.to_string(),
        format!(
            "{CERT} is there and openssl cannot read it as a PEM certificate: move it aside, and run tls again"
        )
    );
    assert_eq!(host.paths(), before);
    assert_eq!(
        changes(&host),
        [format!("{LEGO} run --help"), openssl_read().join(" ")]
    );
}

/// Catches a key that is missing, not a regular file, not the certificate's, or that openssl
/// cannot compare, left for the proxy to fail on: each plans a new issuance like a first one
/// (the certificate unit started before the proxy) and prints why, never the key.
#[test]
fn a_missing_or_mismatched_key_plans_a_new_issuance_and_says_why() {
    type Spoil = fn(&FakeHost);
    let cases: [(&str, Spoil); 6] = [
        ("is not there, a new certificate is issued", |h| {
            Host::remove(h, KEY).unwrap();
        }),
        ("is not a regular file, a new certificate is issued", |h| {
            Host::remove(h, KEY).unwrap();
            h.put_dir(KEY, 0o700, (0, 0));
        }),
        ("is not a regular file, a new certificate is issued", |h| {
            Host::remove(h, KEY).unwrap();
            h.put_symlink(KEY, "/etc/shadow");
        }),
        (
            "is not the key of the certificate, a new certificate is issued",
            |h| {
                h.reply(&openssl_key_key(), 0, "PUBLIC-KEY-B\n");
            },
        ),
        (
            "cannot be compared with the certificate, a new certificate is issued",
            |h| {
                h.reply(&openssl_key_key(), 1, "");
            },
        ),
        (
            "cannot be compared with the certificate, a new certificate is issued",
            |h| {
                h.reply(&openssl_cert_key(), 1, "");
            },
        ),
    ];
    for (why, spoil) in cases {
        let host = issued_host();
        issue_on_start(&host);
        spoil(&host);
        let (result, shown) = go(&host, Mode::Apply);
        result.unwrap();
        let line = format!("key: {KEY} {why}\n");
        assert!(shown.contains(&line), "missing {line:?} in\n{shown}");
        let ran = changes(&host);
        let issue = ran
            .iter()
            .position(|c| c.ends_with("start -- hostkeeper-mdm-cert.service"))
            .unwrap_or_else(|| panic!("no new issuance in {ran:?}"));
        let proxy = ran
            .iter()
            .position(|c| c.ends_with("start -- hostkeeper-mdm-proxy.service"))
            .unwrap();
        assert!(issue < proxy, "{ran:?}");
        assert!(!shown.contains("KEY\n") && !shown.contains("PUBLIC-KEY"));
    }

    let host = issued_host();
    host.reply(&openssl_key_key(), 0, "PUBLIC-KEY-B\n");
    let (result, shown) = go(&host, Mode::DryRun);
    result.unwrap();
    assert!(shown.contains("DRY-RUN: systemctl start -- hostkeeper-mdm-cert.service\n"));
    assert!(shown.contains(&format!("proxy: started once {CERT} is there\n")));
}

/// Catches a certificate with a matching key and a long life being issued again: the
/// certificate unit is not started.
#[test]
fn a_certificate_valid_for_longer_than_the_renewal_threshold_is_not_issued_again() {
    let host = issued_host();
    let (result, shown) = go(&host, Mode::Apply);
    result.unwrap();
    let ran = changes(&host);
    assert!(
        !ran.iter()
            .any(|c| c.contains("hostkeeper-mdm-cert.service")),
        "{ran:?}"
    );
    assert!(!shown.contains("key:"));
    assert!(!shown.contains("expires"));
}

/// Catches a certificate that ends within `VALID_FOR_DAYS` days left to the timer: the run starts
/// the certificate unit once, after the reload path unit is started (so it is watching when the
/// new certificate is written) and before the proxy, and the run ends with the new certificate.
#[test]
fn a_certificate_ending_soon_is_renewed_once_after_the_reload_path_is_watching() {
    assert_eq!(
        hostkeeper_mdm::verify::VALID_FOR_DAYS * 24 * 3600,
        SOON.parse::<u64>().unwrap()
    );
    let host = issued_host();
    host.reply(&openssl_checkend(), 1, "Certificate will expire\n");
    host.effect(
        &[SYSTEMCTL, "start", "--", "hostkeeper-mdm-cert.service"],
        CERT,
        0o644,
        (SVC, SVC),
        b"NEW-CERT",
    );
    let (result, shown) = go(&host, Mode::Apply);
    result.unwrap();
    let ran = changes(&host);
    let at = |end: &str| {
        let hits: Vec<usize> = (0..ran.len()).filter(|i| ran[*i].ends_with(end)).collect();
        assert_eq!(hits.len(), 1, "{end}: {ran:?}");
        hits[0]
    };
    let watching = at("restart -- hostkeeper-mdm-proxy-reload.path");
    let renew = at("start -- hostkeeper-mdm-cert.service");
    let proxy = at("start -- hostkeeper-mdm-proxy.service");
    assert!(watching < renew && renew < proxy, "{ran:?}");
    assert_eq!(host.node(CERT).unwrap().bytes, b"NEW-CERT");
    assert!(
        shown.contains(&format!(
            "certificate: {}\ncertificate: issuer=CN = Example CA\n",
            "subject=CN = mdm.example.com"
        )),
        "{shown}"
    );
    assert!(shown.contains("certificate: notAfter=Jan  1 00:00:00 2027 GMT\n"));
    assert!(!shown.contains("started once"));
    assert!(shown.contains("certificate: ends within 21 days, renewed\n"));

    let host = issued_host();
    host.reply(&openssl_checkend(), 1, "Certificate will expire\n");
    let (result, shown) = go(&host, Mode::DryRun);
    result.unwrap();
    assert!(shown.contains("DRY-RUN: systemctl start -- hostkeeper-mdm-cert.service\n"));
    assert!(shown.contains("DRY-RUN: systemctl start -- hostkeeper-mdm-proxy.service\n"));
}

/// Catches a reload path unit that watches another file, or a reload service that restarts
/// something other than the proxy: the units `tls` writes watch exactly the certificate lego
/// keeps and try-restart exactly the proxy unit.
#[test]
fn the_reload_path_watches_the_certificate_and_its_service_try_restarts_the_proxy() {
    let units = render_tls(&config(), &common::acme()).unwrap();
    let text = |name: &str| {
        units
            .iter()
            .find(|u| u.name == name)
            .unwrap_or_else(|| panic!("no {name}"))
            .text
            .clone()
    };
    let path = text("hostkeeper-mdm-proxy-reload.path");
    let watched: Vec<&str> = path.lines().filter(|l| l.contains("Changed=")).collect();
    assert_eq!(watched, [format!("PathChanged={CERT}")]);
    let started: Vec<&str> = path.lines().filter(|l| l.starts_with("Unit=")).collect();
    assert_eq!(started, ["Unit=hostkeeper-mdm-proxy-reload.service"]);
    let service = text("hostkeeper-mdm-proxy-reload.service");
    let exec: Vec<&str> = service.lines().filter(|l| l.starts_with("Exec")).collect();
    assert_eq!(
        exec,
        ["ExecStart=/usr/bin/systemctl try-restart hostkeeper-mdm-proxy.service"]
    );
}

/// The whole stdout of a dry run and of an apply, with the DNS provider's token distinctive: not a
/// line more than the plan, the check lines and the public certificate, and the token nowhere.
#[test]
fn the_whole_output_of_a_dry_run_and_an_apply_holds_no_dns_credential() {
    const DISTINCT: &str = "cf-9f3a7c1e5b2d8046-DISTINCT";
    let write = |name: &str| {
        let unit = render_tls(&config(), &common::acme())
            .unwrap()
            .into_iter()
            .find(|u| u.name == name)
            .unwrap();
        format!(
            "{{}} write {UNITS}/{name} mode 0644 owner root:root ({} bytes)\n",
            unit.text.len()
        )
    };
    let with = |tag: &str, tail: &str| {
        let mut want = String::from("dns.env: holds CLOUDFLARE_DNS_API_TOKEN (values not shown)\n");
        for name in [
            "hostkeeper-mdm-cert.service",
            "hostkeeper-mdm-cert.timer",
            "hostkeeper-mdm-proxy-reload.path",
            "hostkeeper-mdm-proxy-reload.service",
        ] {
            want.push_str(&write(name).replace("{}", tag));
        }
        want.push_str(tail);
        want
    };

    let host = tls_host();
    host.put_file(
        DNS_ENV,
        0o600,
        (0, 0),
        format!("CLOUDFLARE_DNS_API_TOKEN={DISTINCT}\n").as_bytes(),
    );
    let (result, dry) = go(&host, Mode::DryRun);
    result.unwrap();
    assert_eq!(
        dry,
        with(
            "DRY-RUN:",
            &format!(
                "DRY-RUN: systemctl daemon-reload\nDRY-RUN: systemctl enable -- hostkeeper-mdm-proxy-reload.path\nDRY-RUN: systemctl restart -- hostkeeper-mdm-proxy-reload.path\nDRY-RUN: systemctl enable -- hostkeeper-mdm-cert.timer\nDRY-RUN: systemctl restart -- hostkeeper-mdm-cert.timer\nDRY-RUN: systemctl start -- hostkeeper-mdm-cert.service\ndry run: nothing changed; re-run with --apply to carry it out\nproxy: started once {CERT} is there\n"
            )
        )
    );
    assert!(!dry.contains(DISTINCT));

    issue_on_start(&host);
    let (result, applied) = go(&host, Mode::Apply);
    result.unwrap();
    assert_eq!(
        applied,
        with(
            "+",
            "+ systemctl daemon-reload\n+ systemctl enable -- hostkeeper-mdm-proxy-reload.path\n+ systemctl restart -- hostkeeper-mdm-proxy-reload.path\n+ systemctl enable -- hostkeeper-mdm-cert.timer\n+ systemctl restart -- hostkeeper-mdm-cert.timer\n+ systemctl start -- hostkeeper-mdm-cert.service\n+ systemctl enable -- hostkeeper-mdm-proxy.service\n+ systemctl start -- hostkeeper-mdm-proxy.service\ncertificate: subject=CN = mdm.example.com\ncertificate: issuer=CN = Example CA\ncertificate: notAfter=Jan  1 00:00:00 2027 GMT\n"
        )
    );
    assert!(!applied.contains(DISTINCT));
    assert!(host.calls().iter().all(|c| !c.join(" ").contains(DISTINCT)));
}

/// Catches a secret reader that prints, or accepts, what it should not: the value is exposed
/// only by `expose`, `Debug` hides it, and a file that is not 0600 root's is refused.
#[test]
fn a_secret_is_read_from_a_root_only_file_and_never_shown() {
    let host = FakeHost::new(0);
    host.put_file("/s", 0o600, (0, 0), b"A=1\nKEY=value\n");
    let s = secret::read(&host, "/s", "KEY").unwrap();
    assert_eq!(s.expose(), "value");
    assert_eq!(format!("{s:?}"), "Secret(..)");
    assert!(matches!(
        secret::read(&host, "/s", "NOPE"),
        Err(MdmError::NoSecret { .. })
    ));
    host.put_file("/s", 0o600, (5, 0), b"KEY=value\n");
    assert!(matches!(
        secret::read(&host, "/s", "KEY"),
        Err(MdmError::SecretMode { uid: 5, .. })
    ));
    assert!(secret::has_value(b"KEY=v\n", "KEY"));
    assert!(!secret::has_value(b"KEY=\nOTHER=v\n", "KEY"));
}
