//! `migrate`: the plan on a host shaped like the live stack of the old kit, what it copies, what
//! it leaves alone, and what a rerun does.

mod common;
mod install_support;
mod migrate_support;

use common::{SVC, kit};
use hostkeeper_mdm_core::commands::scep_ca_init_args;
use hostkeeper_mdm_core::migrate::new_settings;
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{Host, Kind, Mode, NOTHING_TO_DO, RUNUSER, SYSTEMCTL};
use migrate_support::*;

/// The `DRY-RUN:` lines of `out`, without the prefix.
fn steps(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.strip_prefix("DRY-RUN: "))
        .map(str::to_owned)
        .collect()
}

fn new_path(old: &str) -> String {
    old.replace(OLD_STATE, NEW_STATE)
}

fn own(path: &str, mode: u32, bytes: &[u8], host: &FakeHost, who: (u32, u32)) {
    let node = host.node(path).unwrap_or_else(|| panic!("{path} is there"));
    assert_eq!(node.kind, Kind::File, "{path}");
    assert_eq!(node.bytes, bytes, "{path}");
    assert_eq!(
        (node.mode, node.uid, node.gid),
        (mode, who.0, who.1),
        "{path}"
    );
}

/// Catches a dry run that changes the host, and an apply that touches the old kit's state: a dry
/// run leaves the fake tree identical and runs no command but questions; with `--apply` every
/// path under the old state, the old etc and the old unit files is byte-, mode- and
/// owner-identical, and the copies exist under the new state with the new owner (the push
/// certificate tree, the topic, the DEP files and `dns.env`), the old SCEP CA, the old secrets
/// other than `dns.env`, and the lego, Caddy and www state are not.
#[test]
fn migrate_dry_runs_and_keeps_the_old_state() {
    let kit = kit();
    let host = live_host(&kit);
    let a = ask(&old_text());
    let before = snapshot(&host);

    let (result, shown) = migrate(&host, &kit, &a, Mode::DryRun);
    result.unwrap();
    assert_eq!(snapshot(&host), before);
    assert_eq!(changes(&host), Vec::<String>::new());
    assert!(shown.contains("dry run: nothing changed"), "{shown}");

    let old_files = |h: &FakeHost| {
        let mut all = below(h, OLD_STATE);
        all.extend(below(h, OLD_ETC));
        all.extend(
            snapshot(h)
                .into_iter()
                .filter(|(p, _)| p.starts_with(&format!("{UNITS}/kbf-mdm"))),
        );
        all
    };
    let old_before = old_files(&host);
    assert!(old_before.len() > 40);
    host.clear_calls();
    let (result, _) = migrate(&host, &kit, &a, Mode::Apply);
    result.unwrap();
    assert_eq!(old_files(&host), old_before);

    let me = (SVC, SVC);
    for path in push_cert_files() {
        own(&new_path(&path), 0o600, &bytes_of(&path), &host, me);
    }
    for dir in [
        "nanohub/mdm",
        "nanohub/mdm/push_cert",
        "nanohub/mdm/push_cert/ab",
        "nanohub/mdm/push_cert/ab/cd",
        "dep/dep_names",
    ] {
        let node = host.node(&format!("{NEW_STATE}/{dir}")).expect("made");
        assert_eq!((node.kind, node.mode, node.uid), (Kind::Dir, 0o700, SVC));
    }
    for name in ["push-topic", "push-cert.sha256"] {
        let old = format!("{OLD_STATE}/{name}");
        own(&new_path(&old), 0o644, &bytes_of(&old), &host, (0, 0));
    }
    for name in DEP_FILES {
        let old = format!("{OLD_STATE}/dep/dep_names/{name}.js-mdm");
        own(&new_path(&old), 0o600, &bytes_of(&old), &host, me);
    }
    let dns = format!("{OLD_ETC}/dns.env");
    own(
        &format!("{NEW_ETC}/dns.env"),
        0o600,
        &bytes_of(&dns),
        &host,
        (0, 0),
    );
    own(SETTINGS, 0o644, new_text(&a).as_bytes(), &host, (0, 0));

    // Not read, so not copied.
    let new_paths = below(&host, NEW_STATE);
    for p in new_paths.keys() {
        for word in [
            "subsys-fvkey",
            "enroll.mobileconfig",
            "lego/certificates",
            "index.txt",
        ] {
            assert!(!p.contains(word), "{p}");
        }
    }
    assert!(
        host.node(&format!("{NEW_STATE}/scep/depot/ca.key"))
            .is_none()
    );
    let hub_env = host.node(&format!("{NEW_ETC}/nanohub.env")).unwrap();
    assert_ne!(hub_env.bytes, bytes_of(&format!("{OLD_ETC}/nanohub.env")));
}

/// Catches a plan out of order, which would leave the old stack stopped by a run that then
/// fails, or start the new services before the data they read is there: the steps are, line by
/// line, (a) install's up to its first systemctl step (no systemctl in it, the fresh CA in it),
/// (b) the new settings, (c) the push certificate tree (directories first), the topic, the
/// DEP files and `dns.env`, (d) the one disable of the old units, (e) the rest of install's.
/// Applied, the commands run in that order.
#[test]
fn the_plan_runs_in_the_order_of_the_design() {
    let kit = kit();
    let host = live_host(&kit);
    let a = ask(&old_text());
    let (result, shown) = migrate(&host, &kit, &a, Mode::DryRun);
    result.unwrap();
    let all: Vec<String> = steps(&shown)
        .into_iter()
        .map(|l| match l.rfind(" (") {
            Some(i) if l.ends_with(" bytes)") => l[..i].to_owned(),
            _ => l,
        })
        .collect();
    let at = |prefix: &str| {
        all.iter()
            .position(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no step {prefix}"))
    };
    let (settings, disable) = (
        at("write /etc/hostkeeper-mdm.conf"),
        at("systemctl disable"),
    );
    let head = &all[..settings];
    assert!(
        head[0].starts_with("mkdir /run/hostkeeper-mdm/restart"),
        "{head:?}"
    );
    assert!(head.iter().any(|l| l.contains("scepserver ca -init")));
    assert!(head.iter().all(|l| !l.starts_with("systemctl")), "{head:?}");
    assert_eq!(
        head.last().unwrap(),
        "write /etc/hostkeeper-mdm/Caddyfile mode 0640 owner root:hostkeeper-mdm"
    );

    let me = "owner hostkeeper-mdm:hostkeeper-mdm";
    let push = format!("{NEW_STATE}/nanohub/mdm/push_cert");
    let cert = format!("{push}/ab/cd/{TOPIC}");
    let mut want = vec![
        "write /etc/hostkeeper-mdm.conf mode 0644 owner root:root".to_owned(),
        format!("mkdir {NEW_STATE}/nanohub/mdm mode 0700 {me}"),
        format!("mkdir {push} mode 0700 {me}"),
        format!("mkdir {push}/ab mode 0700 {me}"),
        format!("mkdir {push}/ab/cd mode 0700 {me}"),
    ];
    for ext in ["key", "pem", "stale_token"] {
        want.push(format!("write {cert}.{ext} mode 0600 {me}"));
    }
    for name in ["push-topic", "push-cert.sha256"] {
        want.push(format!(
            "write {NEW_STATE}/{name} mode 0644 owner root:root"
        ));
    }
    want.push(format!("mkdir {NEW_STATE}/dep/dep_names mode 0700 {me}"));
    let mut dep: Vec<String> = DEP_FILES.iter().map(|n| format!("{n}.js-mdm")).collect();
    dep.sort();
    for name in dep {
        want.push(format!(
            "write {NEW_STATE}/dep/dep_names/{name} mode 0600 {me}"
        ));
    }
    want.push(format!("write {NEW_ETC}/dns.env mode 0600 owner root:root"));
    want.push(format!(
        "systemctl disable --now -- {}",
        stopped().join(" ")
    ));
    assert_eq!(all[settings..=disable], want[..]);

    let tail = [
        "systemctl daemon-reload",
        "systemctl enable -- hostkeeper-mdm-scep.service",
        "systemctl restart -- hostkeeper-mdm-scep.service",
        "remove /run/hostkeeper-mdm/restart/hostkeeper-mdm-scep.service",
        "systemctl enable -- hostkeeper-mdm-nanohub.service",
        "systemctl restart -- hostkeeper-mdm-nanohub.service",
        "remove /run/hostkeeper-mdm/restart/hostkeeper-mdm-nanohub.service",
        "systemctl enable -- hostkeeper-mdm-depserver.service",
        "systemctl restart -- hostkeeper-mdm-depserver.service",
        "remove /run/hostkeeper-mdm/restart/hostkeeper-mdm-depserver.service",
    ];
    assert_eq!(all[disable + 1..], tail[..]);

    host.clear_calls();
    let (result, _) = migrate(&host, &kit, &a, Mode::Apply);
    result.unwrap();
    let mut ca = vec![
        RUNUSER.to_owned(),
        "-u".into(),
        "hostkeeper-mdm".into(),
        "--".into(),
        "/opt/hostkeeper-mdm/bin/scepserver".into(),
    ];
    ca.extend(scep_ca_init_args(
        &new_settings(&a.old, &a.target).unwrap().config,
    ));
    let sys = |rest: &str| format!("{SYSTEMCTL} {rest}");
    let mut ran = vec![
        ca.join(" "),
        sys(&format!("disable --now -- {}", stopped().join(" "))),
    ];
    ran.push(sys("daemon-reload"));
    for unit in install_support::CORE {
        ran.push(sys(&format!("enable -- {unit}")));
        ran.push(sys(&format!("restart -- {unit}")));
    }
    assert_eq!(changes(&host), ran);
}

/// Catches a rerun that copies again, overwrites what the new services changed, or rewrites the
/// settings: after an apply (the new services running, the old ones off) a second apply runs no
/// command that changes anything, changes no path, and prints the settings and every copy as
/// kept.
#[test]
fn a_rerun_copies_nothing_and_keeps_the_settings() {
    let kit = kit();
    let host = live_host(&kit);
    let a = ask(&old_text());
    ca_is_made(&host, &a);
    let (result, _) = migrate(&host, &kit, &a, Mode::Apply);
    result.unwrap();
    install_support::running(&host, &install_support::CORE);
    for unit in old_unit_files() {
        host.reply(&[SYSTEMCTL, "is-enabled", &unit], 1, "disabled\n");
        host.reply(&[SYSTEMCTL, "is-active", &unit], 3, "inactive\n");
    }
    // The old proxy is off and the new one waits for its certificate: the other ports are held
    // by the new services.
    let held: String = [9004, 9005, 9001]
        .iter()
        .map(|port| format!("LISTEN 0 4096 {}:{port} 0.0.0.0:*\n", common::bind()))
        .collect();
    host.reply(&["/usr/bin/ss", "-Hltn"], 0, &held);
    // The new services have changed a copied file since.
    let key = format!("{NEW_STATE}/dep/dep_names/key.js-mdm");
    host.put_file(&key, 0o600, (SVC, SVC), b"changed by NanoDEP");
    let before = snapshot(&host);
    host.clear_calls();

    let (result, shown) = migrate(&host, &kit, &a, Mode::Apply);
    result.unwrap();
    assert_eq!(snapshot(&host), before);
    assert_eq!(changes(&host), Vec::<String>::new());
    assert!(shown.contains(NOTHING_TO_DO), "{shown}");
    assert!(
        shown.contains(&format!("new settings: kept ({SETTINGS})")),
        "{shown}"
    );
    assert!(
        shown.contains("push certificate and key: 0 file(s) copied, 3 kept"),
        "{shown}"
    );
    assert!(
        shown.contains("DEP token and keys: 0 file(s) copied, 9 kept"),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("kept: {key} (already there)")),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("kept: {NEW_ETC}/dns.env (already there)")),
        "{shown}"
    );
    assert!(
        shown.contains("old kit: no unit is enabled or active; the unit files stay"),
        "{shown}"
    );
}

/// Catches a copy that overwrites a file that is there: a target that exists (here a push
/// certificate file, the topic and `dns.env`, with other bytes) is kept as it is and named as
/// kept, and the files that are not there are still copied.
#[test]
fn a_target_that_exists_is_kept_and_not_overwritten() {
    let kit = kit();
    let host = live_host(&kit);
    let a = ask(&old_text());
    let pem = new_path(&push_cert_files()[0]);
    for dir in [
        "nanohub/mdm",
        "nanohub/mdm/push_cert",
        "nanohub/mdm/push_cert/ab",
        "nanohub/mdm/push_cert/ab/cd",
    ] {
        host.put_dir(&format!("{NEW_STATE}/{dir}"), 0o700, (SVC, SVC));
    }
    host.put_file(&pem, 0o600, (SVC, SVC), b"newer pem");
    host.put_file(
        &format!("{NEW_STATE}/push-topic"),
        0o644,
        (0, 0),
        b"newer topic",
    );
    host.put_file(&format!("{NEW_ETC}/dns.env"), 0o600, (0, 0), b"newer dns");
    let (result, shown) = migrate(&host, &kit, &a, Mode::Apply);
    result.unwrap();
    own(&pem, 0o600, b"newer pem", &host, (SVC, SVC));
    own(
        &format!("{NEW_STATE}/push-topic"),
        0o644,
        b"newer topic",
        &host,
        (0, 0),
    );
    own(
        &format!("{NEW_ETC}/dns.env"),
        0o600,
        b"newer dns",
        &host,
        (0, 0),
    );
    for kept in [
        pem.as_str(),
        &format!("{NEW_STATE}/push-topic"),
        &format!("{NEW_ETC}/dns.env"),
    ] {
        assert!(
            shown.contains(&format!("kept: {kept} (already there)")),
            "{shown}"
        );
    }
    assert!(
        shown.contains("push certificate and key: 2 file(s) copied, 1 kept"),
        "{shown}"
    );
    let sha = format!("{OLD_STATE}/push-cert.sha256");
    own(&new_path(&sha), 0o644, &bytes_of(&sha), &host, (0, 0));
}

/// Catches a source that is not there being a refusal, or passing without a word: with no push
/// certificate, topic, checksum, DEP token (the directory empty, then gone) and `dns.env`, the
/// run still succeeds, copies none of them, and prints for each what the operator does instead.
#[test]
fn missing_sources_print_their_instructions() {
    let kit = kit();
    let host = live_host(&kit);
    let a = ask(&old_text());
    for path in host.paths() {
        let gone = path.starts_with(&format!("{OLD_STATE}/nanohub/mdm/push_cert"))
            || path.starts_with(&format!("{OLD_STATE}/dep/dep_names/"))
            || path.ends_with("/push-topic")
            || path.ends_with("/push-cert.sha256")
            || path == format!("{OLD_ETC}/dns.env");
        if gone {
            host.remove(&path).unwrap();
        }
    }
    let lines = [
        format!(
            "no push certificate: {OLD_STATE}/nanohub/mdm/push_cert is not there; run push-cert --cert <pem> --key <pem> after the move"
        ),
        format!(
            "no push-topic: {OLD_STATE}/push-topic is not there; push-cert --cert <pem> --key <pem> writes it"
        ),
        format!(
            "no push-cert.sha256: {OLD_STATE}/push-cert.sha256 is not there; push-cert --cert <pem> --key <pem> writes it"
        ),
        format!(
            "no DEP token: {OLD_STATE}/dep/dep_names holds no file named *.js-mdm; run dep get-cert and dep upload-token after the move (the owner of the ABM account downloads the token again)"
        ),
        format!(
            "no dns.env: {OLD_ETC}/dns.env is not there; write {NEW_ETC}/dns.env (root, 0600) with the DNS provider's key before tls"
        ),
    ];
    for dir_there in [true, false] {
        if !dir_there {
            host.remove(&format!("{OLD_STATE}/dep/dep_names")).unwrap();
        }
        let (result, shown) = migrate(&host, &kit, &a, Mode::DryRun);
        result.unwrap();
        for line in &lines {
            assert!(shown.lines().any(|l| l == line), "{line}\n{shown}");
        }
        for step in steps(&shown) {
            assert!(
                !step.contains("push_cert") && !step.contains("dep_names"),
                "{step}"
            );
            assert!(
                !step.contains("dns.env") && !step.contains("push-topic"),
                "{step}"
            );
        }
    }
}

/// Catches a DEP copy that takes whatever is in the directory: only files named for the old DEP
/// name are copied, and the other names are listed in the output as not copied.
#[test]
fn dep_files_of_other_names_are_not_copied() {
    let kit = kit();
    let host = live_host(&kit);
    let a = ask(&old_text());
    for name in ["notes.txt", "token.other"] {
        host.put_file(
            &format!("{OLD_STATE}/dep/dep_names/{name}"),
            0o600,
            (OLD_UID, OLD_UID),
            b"x",
        );
    }
    let (result, shown) = migrate(&host, &kit, &a, Mode::Apply);
    result.unwrap();
    assert!(
        shown
            .lines()
            .any(|l| l == "not copied (not named *.js-mdm): notes.txt, token.other"),
        "{shown}"
    );
    assert!(
        host.node(&format!("{NEW_STATE}/dep/dep_names/notes.txt"))
            .is_none()
    );
    assert!(
        host.node(&format!("{NEW_STATE}/dep/dep_names/token.other"))
            .is_none()
    );
    assert!(
        host.node(&format!("{NEW_STATE}/dep/dep_names/cert.js-mdm"))
            .is_some()
    );
}

/// Catches a copy of a file whose mode is wider than the new kit keeps: a push certificate file
/// of mode 0644 is copied as 0600, and one of 0400 stays 0400.
#[test]
fn copied_modes_are_at_most_0600() {
    let kit = kit();
    let host = live_host(&kit);
    let a = ask(&old_text());
    let files = push_cert_files();
    host.put_file(&files[0], 0o644, (OLD_UID, OLD_UID), b"wide");
    host.put_file(&files[1], 0o400, (OLD_UID, OLD_UID), b"narrow");
    let (result, _) = migrate(&host, &kit, &a, Mode::Apply);
    result.unwrap();
    own(&new_path(&files[0]), 0o600, b"wide", &host, (SVC, SVC));
    own(&new_path(&files[1]), 0o400, b"narrow", &host, (SVC, SVC));
}

/// Catches a stop of units that are not running, or a missing stop: with the old kit's units
/// partly off, only those enabled or active are in the one disable step, and the back-out lines
/// name the same units and the new ones.
#[test]
fn only_the_old_units_that_are_enabled_or_active_are_stopped() {
    let kit = kit();
    let host = live_host(&kit);
    let a = ask(&old_text());
    let n = old_units();
    for unit in [&n.proxy, &n.cert_timer] {
        host.reply(&[SYSTEMCTL, "is-enabled", unit], 1, "disabled\n");
        host.reply(&[SYSTEMCTL, "is-active", unit], 0, "active\n");
    }
    host.reply(&[SYSTEMCTL, "is-enabled", &n.reload_path], 1, "disabled\n");
    host.reply(&[SYSTEMCTL, "is-active", &n.reload_path], 3, "inactive\n");
    let (result, shown) = migrate(&host, &kit, &a, Mode::DryRun);
    result.unwrap();
    let stop: Vec<String> = stopped()
        .into_iter()
        .filter(|u| *u != n.reload_path)
        .collect();
    let line = format!("systemctl disable --now -- {}", stop.join(" "));
    assert_eq!(
        steps(&shown).iter().filter(|l| **l == line).count(),
        1,
        "{shown}"
    );
    let new = new_units();
    let back = [
        format!(
            "back out: systemctl disable --now -- {} {} {} {}",
            new.proxy, new.depserver, new.nanohub, new.scep
        ),
        format!("back out: systemctl enable --now -- {}", stop.join(" ")),
    ];
    for line in back {
        assert!(shown.lines().any(|l| l == line), "{line}\n{shown}");
    }
}

/// Catches next steps that point at the wrong settings file or skip one: after the plan the four
/// verbs are printed in order with the new settings file.
#[test]
fn the_next_steps_follow_the_plan() {
    let kit = kit();
    let host = live_host(&kit);
    let a = ask(&old_text());
    let (result, shown) = migrate(&host, &kit, &a, Mode::DryRun);
    result.unwrap();
    let tail: Vec<&str> = shown
        .lines()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    assert_eq!(
        tail,
        [
            "next, in this order:",
            "  hostkeeper-mdm tls --settings /etc/hostkeeper-mdm.conf --apply",
            "  hostkeeper-mdm push-cert --settings /etc/hostkeeper-mdm.conf --profile-only --apply",
            "  hostkeeper-mdm verify --settings /etc/hostkeeper-mdm.conf",
            "  hostkeeper-mdm dep define-profile --settings /etc/hostkeeper-mdm.conf --apply",
        ]
    );
}
