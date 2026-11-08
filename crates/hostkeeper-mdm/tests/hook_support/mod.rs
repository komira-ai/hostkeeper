//! What the tests of the `mdm` package's hook share: a fake host ready for the package, its
//! staged release, the params file and record directory of a run in a scratch directory, and
//! running `mdm update-hook` through the program's command line.

#![allow(
    dead_code,
    reason = "each hook test binary uses part of the shared helpers"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hostkeeper_hook::Report;
use hostkeeper_mdm_core::commands::scep_ca_init_args;
use hostkeeper_mdm_core::package::{self, FIXED};
use hostkeeper_mdm_step::fake::FakeHost;
use hostkeeper_mdm_step::{GETENT, SYSTEMCTL};
use hostkeeper_testkit::Scratch;

use crate::common::{FakeMachine, FakeNet, SVC, bind_line, example, with_system};

pub const STATE: &str = "/var/lib/mdm";
pub const ETC: &str = "/etc/mdm";
pub const SETTINGS: &str = "/etc/mdm/settings";
pub const UNITS: &str = "/etc/systemd/system";
pub const CA: &str = "/var/lib/mdm/scep/depot/ca.pem";
pub const CA_KEY: &str = "/var/lib/mdm/scep/depot/ca.key";
pub const CERTS: &str = "/var/lib/mdm/lego/certificates";
pub const CERT: &str = "/var/lib/mdm/lego/certificates/mdm.example.com.crt";
pub const CADDYFILE: &str = "/etc/mdm/Caddyfile";
pub const DNS_ENV: &str = "/etc/mdm/dns.env";
/// The staged release of the update under test, as the daemon unpacked it.
pub const STAGED: &str = "/var/lib/hostkeeper/packages/mdm/1.1.0";
/// The release before it.
pub const PREVIOUS: &str = "/var/lib/hostkeeper/packages/mdm/1.0.0";
pub const LEGO: &str = "/var/lib/hostkeeper/packages/mdm/1.1.0/bin/lego";
pub const SCEPSERVER: &str = "/var/lib/hostkeeper/packages/mdm/1.1.0/bin/scepserver";

/// The drop-in of `unit`.
pub fn drop_in(unit: &str) -> String {
    format!("{UNITS}/{unit}.d/settings.conf")
}

/// Every file `prepare` renders, in the order it writes them.
pub fn rendered_files() -> Vec<String> {
    let mut files = vec![CADDYFILE.to_owned()];
    files.extend(
        [
            package::SLICE,
            package::PROXY,
            package::DEPSYNCER,
            package::CERT_SERVICE,
            package::CERT_RENEW,
        ]
        .map(drop_in),
    );
    files
}

/// The example settings with every key the package fixes left out: what an operator of the
/// package writes.
pub fn package_example() -> String {
    example()
        .lines()
        .filter(|line| {
            !FIXED
                .iter()
                .any(|(key, _)| line.starts_with(&format!("{key}=")))
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

/// `lego run --help` as the pinned lego prints it: every flag the certificate unit passes.
pub const LEGO_HELP: &str = "   --accept-tos  --email value  --server value  --path value\n   --key-type value  --dns value  --domains value  --dns.resolvers value\n   --dns.propagation.wait value\n";

/// `lego renew --help` as the pinned lego prints it: every flag the renewal unit passes.
pub const LEGO_RENEW_HELP: &str = "   --email value  --server value  --path value  --key-type value\n   --dns value  --domains value  --dns.resolvers value  --dns.propagation.wait value\n   --days value\n";

/// A host ready for the package's first `prepare`: root, x86-64, the bind address carried, no
/// port held, the staged release unpacked with a lego that takes every flag, the settings
/// file in place, the user `mdm` not made yet, and every systemctl command answered.
pub fn package_host() -> FakeHost {
    let host = FakeHost::new(0);
    host.reply(&["/usr/bin/uname", "-m"], 0, "x86_64\n");
    host.reply(
        &["/usr/bin/ip", "-o", "-4", "addr", "show"],
        0,
        &bind_line(),
    );
    host.reply(&["/usr/bin/ss", "-Hltn"], 0, "");
    host.reply_prefix(&[SYSTEMCTL, "is-active"], 3, "inactive\n");
    for verb in [
        "daemon-reload",
        "enable",
        "start",
        "restart",
        "try-restart",
        "stop",
    ] {
        host.reply_prefix(&[SYSTEMCTL, verb], 0, "");
    }
    host.reply_prefix(&["/usr/sbin/runuser"], 0, "");
    host.reply(&[LEGO, "run", "--help"], 0, LEGO_HELP);
    host.reply(&[LEGO, "renew", "--help"], 0, LEGO_RENEW_HELP);
    for release in [STAGED, PREVIOUS] {
        for program in ["scepserver", "lego", "nanohub", "mdm"] {
            host.put_file(
                &format!("{release}/bin/{program}"),
                0o555,
                (0, 0),
                program.as_bytes(),
            );
        }
    }
    host.put_dir(ETC, 0o755, (0, 0));
    host.put_file(SETTINGS, 0o600, (0, 0), package_example().as_bytes());
    // No user `mdm` yet; `useradd` makes it, as the real one does.
    host.reply(&[GETENT, "passwd", "mdm"], 2, "");
    host.reply(&[GETENT, "group", "mdm"], 2, "");
    let useradd = [
        "/usr/sbin/useradd",
        "--system",
        "--user-group",
        "--home-dir",
        STATE,
        "--no-create-home",
        "--shell",
        "/usr/sbin/nologin",
        "mdm",
    ];
    host.reply(&useradd, 0, "");
    host.effect_user(
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
        "mdm",
        SVC,
    );
    ca_is_made_by_init(&host);
    host
}

/// The argv of the CA command `prepare` runs with the staged release's `scepserver`.
pub fn ca_argv() -> Vec<String> {
    let mut argv: Vec<String> = ["/usr/sbin/runuser", "-u", "mdm", "--", SCEPSERVER]
        .map(str::to_owned)
        .to_vec();
    let config = hostkeeper_mdm_core::config::MdmConfig::parse(
        &package::settings(&package_example()).expect("the package example fits"),
    )
    .expect("the package example parses");
    argv.extend(scep_ca_init_args(&config));
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

/// The CA command leaves a CA, and openssl finds its two files a pair.
pub fn ca_is_made_by_init(host: &FakeHost) {
    let argv = ca_argv();
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    host.effect(&words, CA_KEY, 0o400, (SVC, SVC), b"KEY");
    host.effect(&words, CA, 0o400, (SVC, SVC), b"CA");
    host.reply(&X509, 0, "PUBLIC KEY OF THE CA\n");
    host.reply(&PKEY, 0, "PUBLIC KEY OF THE CA\n");
}

/// The certificate lego writes, there.
pub fn with_certificate(host: &FakeHost) {
    host.put_dir(CERTS, 0o700, (SVC, SVC));
    host.put_file(CERT, 0o600, (SVC, SVC), b"CERT");
}

/// The DNS provider's credentials (the example's provider is cloudflare), there.
pub fn with_dns_env(host: &FakeHost) {
    host.put_file(DNS_ENV, 0o600, (0, 0), b"CLOUDFLARE_DNS_API_TOKEN=token\n");
}

/// One run of the hook: a scratch directory holding its params file and record directory.
pub struct Run {
    pub scratch: Scratch,
}

/// What a run of a step ended with.
#[derive(Debug)]
pub struct Ended {
    pub code: u8,
    pub out: String,
    pub err: String,
}

impl Ended {
    /// The report line, read by the contract's strict reader.
    pub fn report(&self) -> Report {
        Report::from_stdout(&self.out).expect("a well-formed last line")
    }
}

impl Run {
    pub fn new(name: &str) -> Run {
        let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
            .expect("a scratch directory");
        Run { scratch }
    }

    /// The record directory the params name.
    pub fn record(&self) -> PathBuf {
        self.scratch.path().join("record")
    }

    /// The params file of an update from [`PREVIOUS`] to [`STAGED`] (`previous: true`) or of a
    /// first install of [`STAGED`].
    pub fn params(&self, previous: bool) -> PathBuf {
        let mut text = format!(
            "settings = \"{SETTINGS}\"\nrecord_dir = \"{}\"\n\"updater.staged_release\" = \"{STAGED}\"\n\"updater.target_version\" = \"1.1.0\"\n\"updater.artifact_digest\" = \"sha256:{}\"\n",
            self.record().display(),
            "ab".repeat(32)
        );
        if previous {
            text.push_str(&format!(
                "\"updater.previous_release\" = \"{PREVIOUS}\"\n\"updater.previous_version\" = \"1.0.0\"\n"
            ));
        }
        let path = self.scratch.path().join(match previous {
            true => "update.toml",
            false => "install.toml",
        });
        std::fs::write(&path, text).expect("write the params file");
        path
    }

    /// Runs `mdm update-hook <words> --params <params of an update or a first install>`.
    pub fn step(&self, host: &FakeHost, words: &[&str], previous: bool) -> Ended {
        let params = self.params(previous);
        let mut args: Vec<OsString> = ["mdm", "update-hook"].map(OsString::from).to_vec();
        args.extend(words.iter().map(OsString::from));
        args.push(OsString::from("--params"));
        args.push(params.into_os_string());
        run_mdm(host, &args)
    }

    /// `prepare` of [`STAGED`].
    pub fn prepare(&self, host: &FakeHost, previous: bool) -> Ended {
        self.step(host, &["prepare", STAGED], previous)
    }
}

/// Runs `mdm` with `args` on `host`.
pub fn run_mdm(host: &FakeHost, args: &[OsString]) -> Ended {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let machine = FakeMachine::new(host);
    let net = FakeNet::new(Default::default());
    let code = with_system(&machine, &net, |system| {
        hostkeeper_mdm::program::run(args, system, &mut out, &mut err)
    });
    Ended {
        code,
        out: String::from_utf8(out).expect("UTF-8"),
        err: String::from_utf8(err).expect("UTF-8"),
    }
}

/// The systemctl commands `host` ran that change something, joined by spaces, without
/// `systemctl` itself.
pub fn systemctl(host: &FakeHost) -> Vec<String> {
    host.calls()
        .iter()
        .filter(|c| c[0] == SYSTEMCTL && !c[1].starts_with("is-"))
        .map(|c| c[1..].join(" "))
        .collect()
}
