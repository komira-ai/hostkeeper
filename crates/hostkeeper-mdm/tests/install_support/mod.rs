//! What the `install` test binaries share: the paths of the example settings, running install
//! and uninstall on a fake host, and the fakes of a depot and its openssl.

#![allow(
    dead_code,
    reason = "each install test binary uses part of the shared helpers"
)]

use crate::common::{self, FakeNet, Kit, SVC, config};
use hostkeeper_mdm::MdmError;
use hostkeeper_mdm::install::{Install, run, run_uninstall};
use hostkeeper_mdm_core::commands::scep_ca_init_args;
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{Mode, SYSTEMCTL};

pub const STATE: &str = "/var/lib/hostkeeper-mdm";
pub const ETC: &str = "/etc/hostkeeper-mdm";
pub const BIN: &str = "/opt/hostkeeper-mdm/bin";
pub const UNITS: &str = "/etc/systemd/system";
pub const DEPOT: &str = "/var/lib/hostkeeper-mdm/scep/depot";
pub const CA: &str = "/var/lib/hostkeeper-mdm/scep/depot/ca.pem";
pub const CA_KEY: &str = "/var/lib/hostkeeper-mdm/scep/depot/ca.key";
pub const CERT: &str = "/var/lib/hostkeeper-mdm/lego/certificates/mdm.example.com.crt";
/// Where a pending restart of a unit is recorded.
pub const MARKERS: &str = "/run/hostkeeper-mdm/restart";

pub const CORE: [&str; 3] = [
    "hostkeeper-mdm-scep.service",
    "hostkeeper-mdm-nanohub.service",
    "hostkeeper-mdm-depserver.service",
];

pub fn install(host: &FakeHost, kit: &Kit, mode: Mode) -> (Result<(), MdmError>, String) {
    let net = FakeNet::new(Default::default());
    let a = Install {
        config: config(),
        bin_dir: "/srv/fetch".to_owned(),
        max_archive: 1 << 30,
        replacing: None,
    };
    let mut out = Vec::new();
    let result = common::with_env(host, &net, |env| run(env, &a, &kit.pins, mode, &mut out));
    (result, String::from_utf8(out).unwrap())
}

/// Uninstall at the injected time `now`.
pub fn uninstall_at(host: &FakeHost, mode: Mode, now: u64) -> (Result<(), MdmError>, String) {
    let net = FakeNet::new(Default::default());
    let mut out = Vec::new();
    let env = hostkeeper_mdm::Env {
        host,
        net: &net,
        api: &common::FakeApi::new(),
        random: &common::fill,
        now,
    };
    let result = run_uninstall(&env, &config(), mode, &mut out);
    (result, String::from_utf8(out).unwrap())
}

pub fn uninstall(host: &FakeHost, mode: Mode) -> (Result<(), MdmError>, String) {
    uninstall_at(host, mode, 1_700_000_000)
}

/// The commands that make a unit enabled and active, for a rerun.
pub fn running(host: &FakeHost, units: &[&str]) {
    for unit in units {
        host.reply(&[SYSTEMCTL, "is-enabled", unit], 0, "enabled\n");
        host.reply(&[SYSTEMCTL, "is-active", unit], 0, "active\n");
    }
}

pub fn ca_argv() -> Vec<String> {
    let mut argv: Vec<String> = [
        "/usr/sbin/runuser",
        "-u",
        "hostkeeper-mdm",
        "--",
        "/opt/hostkeeper-mdm/bin/scepserver",
    ]
    .map(str::to_owned)
    .to_vec();
    argv.extend(scep_ca_init_args(&config()));
    argv
}

/// `openssl x509 -noout -pubkey` on the depot's certificate.
pub const X509: [&str; 6] = ["/usr/bin/openssl", "x509", "-noout", "-pubkey", "-in", CA];
/// `openssl pkey -pubout` on the depot's key.
pub const PKEY: [&str; 7] = [
    "/usr/bin/openssl",
    "pkey",
    "-passin",
    "pass:",
    "-pubout",
    "-in",
    CA_KEY,
];

/// openssl answering that the depot's certificate and key hold the same public key.
pub fn openssl_finds_a_pair(host: &FakeHost) {
    host.reply(&X509, 0, "PUBLIC KEY OF THE CA\n");
    host.reply(&PKEY, 0, "PUBLIC KEY OF THE CA\n");
}

/// A depot holding a whole CA: both files, and openssl finding them a pair.
pub fn whole_depot(host: &FakeHost) {
    host.put_file(CA, 0o400, (SVC, SVC), b"EXISTING CA");
    host.put_file(CA_KEY, 0o400, (SVC, SVC), b"EXISTING KEY");
    openssl_finds_a_pair(host);
}

/// Tells the fake that the CA command leaves a CA (a certificate and its key), as the real one
/// does, and that openssl finds them a pair.
pub fn ca_is_made_by_init(host: &FakeHost) {
    let argv = ca_argv();
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    host.effect(&words, CA_KEY, 0o400, (SVC, SVC), b"KEY");
    host.effect(&words, CA, 0o400, (SVC, SVC), b"CA");
    openssl_finds_a_pair(host);
}

/// The commands recorded that change something: [`common::changes`] without the read-only
/// questions install asks (`uname -m`, and openssl reading the depot).
pub fn changes(host: &FakeHost) -> Vec<String> {
    common::changes(host)
        .into_iter()
        .filter(|line| *line != "/usr/bin/uname -m" && !line.starts_with("/usr/bin/openssl "))
        .collect()
}

/// The marker file of a unit.
pub fn marker(unit: &str) -> String {
    format!("{MARKERS}/{unit}")
}
