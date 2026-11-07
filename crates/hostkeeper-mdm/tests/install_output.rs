//! The whole text `install` prints, for an apply and for a dry run, on a ready host.

mod common;
mod install_support;

use common::{binary_bytes, config, kit, random_hex, ready_host};
use hostkeeper_mdm_core::render::render;
use hostkeeper_mdm_step::Mode;
use install_support::{ca_is_made_by_init, install};

const SERVICES: [&str; 3] = [
    "hostkeeper-mdm-scep.service",
    "hostkeeper-mdm-nanohub.service",
    "hostkeeper-mdm-depserver.service",
];

/// The checks' lines, which come before the plan and are the same for both modes.
const CHECKS: &str = "archives: every one matches its pin
secret NANOHUB_API_KEY: makes /etc/hostkeeper-mdm/nanohub.env (0600 root; never shown)
secret NANODEP_API: makes /etc/hostkeeper-mdm/nanodep.env (0600 root; never shown)
secret SCEP_CHALLENGE_PASSWORD: makes /etc/hostkeeper-mdm/scep.env (0600 root; never shown)
proxy: not started; it waits for the TLS certificate /var/lib/hostkeeper-mdm/lego/certificates/mdm.example.com.crt
";

/// The steps of a first install on the ready host, as the plan words them, in order. The
/// sizes are those of the test archives' binaries, of a secret line (key, `=`, 64 hex digits,
/// newline) and of what `hostkeeper-mdm-core` renders.
fn steps() -> Vec<String> {
    let mut steps = vec!["mkdir /run/hostkeeper-mdm/restart mode 0700 owner root:root".to_owned()];
    for unit in SERVICES {
        steps.push(format!(
            "write /run/hostkeeper-mdm/restart/{unit} mode 0600 owner root:root (0 bytes)"
        ));
    }
    let mine = "owner hostkeeper-mdm:hostkeeper-mdm";
    steps.push(format!("mkdir /var/lib/hostkeeper-mdm mode 0700 {mine}"));
    for dir in [
        "nanohub",
        "scep",
        "scep/depot",
        "dep",
        "lego",
        "caddy",
        "www",
    ] {
        steps.push(format!(
            "mkdir /var/lib/hostkeeper-mdm/{dir} mode 0700 {mine}"
        ));
    }
    steps.push("mkdir /etc/hostkeeper-mdm mode 0750 owner root:hostkeeper-mdm".to_owned());
    steps.push("mkdir /opt/hostkeeper-mdm/bin mode 0755 owner root:root".to_owned());
    for name in [
        "nanohub",
        "depserver",
        "depsyncer",
        "scepserver",
        "caddy",
        "lego",
        "mdmctl",
    ] {
        let size = binary_bytes(name).len();
        steps.push(format!(
            "write /opt/hostkeeper-mdm/bin/{name} mode 0755 owner root:root ({size} bytes)"
        ));
    }
    for (file, key) in [
        ("nanohub.env", "NANOHUB_API_KEY"),
        ("nanodep.env", "NANODEP_API"),
        ("scep.env", "SCEP_CHALLENGE_PASSWORD"),
    ] {
        let size = format!("{key}={}\n", random_hex()).len();
        steps.push(format!(
            "write /etc/hostkeeper-mdm/{file} mode 0600 owner root:root ({size} bytes)"
        ));
    }
    steps.push(
        "run /usr/sbin/runuser -u hostkeeper-mdm -- /opt/hostkeeper-mdm/bin/scepserver ca -init -depot /var/lib/hostkeeper-mdm/scep/depot -years 10 -common_name Example MDM SCEP CA -organization Example -organizational_unit hostkeeper-mdm"
            .to_owned(),
    );
    let rendered = render(&config()).unwrap();
    for unit in &rendered.units {
        steps.push(format!(
            "write /etc/systemd/system/{} mode 0644 owner root:root ({} bytes)",
            unit.name,
            unit.text.len()
        ));
    }
    steps.push(format!(
        "write /etc/hostkeeper-mdm/Caddyfile mode 0640 owner root:hostkeeper-mdm ({} bytes)",
        rendered.caddyfile.len()
    ));
    steps.push("systemctl daemon-reload".to_owned());
    for unit in SERVICES {
        steps.push(format!("systemctl enable -- {unit}"));
        steps.push(format!("systemctl restart -- {unit}"));
        steps.push(format!("remove /run/hostkeeper-mdm/restart/{unit}"));
    }
    steps
}

fn golden(prefix: &str, tail: &str) -> String {
    let mut text = CHECKS.to_owned();
    for step in steps() {
        text.push_str(&format!("{prefix}{step}\n"));
    }
    text.push_str(tail);
    text
}

/// Catches a secret, or anything but names, sizes and modes, reaching the output of an apply:
/// the whole text of a first install is exactly the expected one (the secrets are 0xAB bytes,
/// so their hex form appears nowhere in it), and the marker steps are in it.
#[test]
fn the_whole_output_of_an_apply_is_the_plan_and_shows_no_secret() {
    let kit = kit();
    let host = ready_host(&kit);
    ca_is_made_by_init(&host);
    let (result, shown) = install(&host, &kit, Mode::Apply);
    result.unwrap();
    assert_eq!(shown, golden("+ ", ""));
    assert!(!shown.contains(&random_hex()));
}

/// Catches a secret, or anything but names, sizes and modes, reaching the output of a dry
/// run: the whole text is exactly the expected one, and nothing on the host changes.
#[test]
fn the_whole_output_of_a_dry_run_is_the_plan_and_shows_no_secret() {
    let kit = kit();
    let host = ready_host(&kit);
    let before = host.paths();
    let (result, shown) = install(&host, &kit, Mode::DryRun);
    result.unwrap();
    let note = "dry run: nothing changed; re-run with --apply to carry it out\n";
    assert_eq!(shown, golden("DRY-RUN: ", note));
    assert!(!shown.contains(&random_hex()));
    assert_eq!(host.paths(), before);
}
