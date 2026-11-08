#![cfg(feature = "privileged")]
//! The `mdm` package's certificate jobs, its timer and its proxy, as root against the host's
//! systemd: the units of `hostkeeper_mdm_core::package::UNITS` and the drop-ins
//! `package::drop_ins` renders for the example site, loaded from `/run/systemd/system` under
//! names of this test, with `lego` and `caddy` a fixture that records how it is started.
//!
//! Only names change on the way, never a directive: the package's paths become the test's tree
//! `/var/lib/hostkeeper-pkgtest-<pid>` (the units hide `/home` and `/tmp` from their services,
//! where the scratch directories of the other tests are), `mdm-` and `mdm.slice` become this
//! test's prefix, and the service user `mdm`, which this host does not have, becomes `nobody`.
//! The tree and the units are removed when the test ends, passed or failed.

mod support;

use std::fs;
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_mdm_core::acme::Acme;
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::package::{self, BIN, ETC, FIXED, STATE, UNITS};
use hostkeeper_steps::systemd::SYSTEMCTL;

/// The fixture program, built by cargo with these tests.
const FIXTURE: &str = env!("CARGO_BIN_EXE_mdm-units-fixture");

/// Where systemd reads runtime units from: gone at the next boot, and removed by the test.
const RUNTIME_UNITS: &str = "/run/systemd/system";

/// The DNS provider's variable of the example site's provider (cloudflare), and the test's value.
const TOKEN: (&str, &str) = ("CLOUDFLARE_DNS_API_TOKEN", "token-of-the-test");

/// The units the test loads: the two certificate jobs, their timer and the proxy.
const LOADED: [&str; 4] = [
    package::CERT_SERVICE,
    package::CERT_RENEW,
    package::CERT_TIMER,
    package::PROXY,
];

/// The example site's settings, read as the package reads them.
fn settings() -> (MdmConfig, Acme) {
    let example = include_str!("../../hostkeeper-mdm-core/config.example")
        .replace(
            "\nMDM_BIND=\n",
            &format!("\nMDM_BIND={}\n", Ipv4Addr::new(10, 0, 0, 10)),
        )
        .replace(
            "\nMDM_LAN_CIDR=\n",
            &format!("\nMDM_LAN_CIDR={}/24\n", Ipv4Addr::new(10, 0, 0, 0)),
        );
    let text: String = example
        .lines()
        .filter(|l| !FIXED.iter().any(|(k, _)| l.starts_with(&format!("{k}="))))
        .map(|l| format!("{l}\n"))
        .collect();
    let text = package::settings(&text).expect("the example fits the package");
    (
        MdmConfig::parse(&text).expect("parses"),
        Acme::parse(&text).expect("parses"),
    )
}

/// One run of the test: its tree, its prefix, and the units it loaded, all
/// stopped and removed when dropped.
struct Tree {
    root: PathBuf,
    prefix: String,
}

impl Tree {
    fn new() -> Tree {
        let root = rustix::process::geteuid().is_root();
        assert!(root, "the test runs as root, against the host's systemd");
        assert!(
            Path::new(hostkeeper_privileged::SYSTEMD_RUNTIME_DIR).is_dir(),
            "the test runs against systemd"
        );
        let pid = std::process::id();
        let tree = Tree {
            root: PathBuf::from(format!("/var/lib/hostkeeper-pkgtest-{pid}")),
            prefix: format!("hkmdm{pid}"),
        };
        tree.remove();
        tree
    }

    /// `text` with the package's names made this test's.
    fn renamed(&self, text: &str) -> String {
        let root = self.root.display();
        text.replace(BIN, &format!("{root}/bin"))
            .replace(STATE, &format!("{root}/state"))
            .replace(ETC, &format!("{root}/etc"))
            .replace("mdm-", &format!("{}-", self.prefix))
            .replace(package::SLICE, &format!("{}.slice", self.prefix))
            .replace("User=mdm\n", "User=nobody\n")
            .replace("Group=mdm\n", "Group=nogroup\n")
    }

    /// The test's name of the package's unit `unit`.
    fn unit(&self, unit: &str) -> String {
        self.renamed(unit)
    }

    fn path(&self, below: &str) -> PathBuf {
        self.root.join(below)
    }

    /// Stops the test's units, removes them and the tree, and reloads systemd.
    fn remove(&self) {
        let units: Vec<String> = LOADED.iter().map(|u| self.unit(u)).collect();
        let _ = support::run(SYSTEMCTL, &[&["stop", "--"][..], &as_str(&units)].concat());
        let _ = support::run(
            SYSTEMCTL,
            &[&["reset-failed", "--"][..], &as_str(&units)].concat(),
        );
        for unit in units.iter().chain([&format!("{}.slice", self.prefix)]) {
            let _ = fs::remove_file(Path::new(RUNTIME_UNITS).join(unit));
            let _ = fs::remove_dir_all(Path::new(RUNTIME_UNITS).join(format!("{unit}.d")));
        }
        let _ = fs::remove_dir_all(&self.root);
        let _ = support::run(SYSTEMCTL, &["daemon-reload"]);
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.remove();
    }
}

fn as_str(words: &[String]) -> Vec<&str> {
    words.iter().map(String::as_str).collect()
}

/// `nobody`'s user id.
fn nobody() -> u32 {
    support::run_ok("/usr/bin/id", &["-u", "nobody"])
        .trim()
        .parse()
        .expect("a uid")
}

/// Lays the tree out as the package's hook and the daemon leave it, and loads the units.
fn lay_out(tree: &Tree) {
    let (c, acme) = settings();
    let bin = tree.path("bin");
    fs::create_dir_all(&bin).expect("bin");
    for name in ["lego", "caddy"] {
        fs::copy(FIXTURE, bin.join(name)).expect("the fixture");
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    fs::create_dir_all(tree.path("etc")).expect("etc");
    for dir in ["state/lego", "state/caddy"] {
        let dir = tree.path(dir);
        fs::create_dir_all(&dir).expect("state");
        std::os::unix::fs::chown(&dir, Some(nobody()), None).expect("chown");
    }
    fs::set_permissions(&tree.root, fs::Permissions::from_mode(0o755)).expect("chmod");
    for unit in UNITS.iter().filter(|u| LOADED.contains(&u.name)) {
        let path = Path::new(RUNTIME_UNITS).join(tree.unit(unit.name));
        fs::write(path, tree.renamed(unit.text)).expect("write the unit");
    }
    let loaded = |name: &str| {
        LOADED
            .iter()
            .chain([&package::SLICE])
            .any(|unit| name.starts_with(&format!("{unit}.d/")))
    };
    for drop_in in package::drop_ins(&c, &acme)
        .into_iter()
        .filter(|d| loaded(&d.name))
    {
        let path = Path::new(RUNTIME_UNITS).join(tree.renamed(&drop_in.name));
        fs::create_dir_all(path.parent().expect("a directory")).expect("the drop-in directory");
        fs::write(path, tree.renamed(&drop_in.text)).expect("write the drop-in");
    }
    support::run_ok(SYSTEMCTL, &["daemon-reload"]);
}

/// The property `name` of the unit `unit`, named in full (`support::property` adds `.service`
/// to a name, which the timer and these names already have).
fn property(unit: &str, name: &str) -> String {
    let flag = format!("--property={name}");
    support::run_ok(
        SYSTEMCTL,
        &["show", "--system", "--value", flag.as_str(), "--", unit],
    )
    .trim_end()
    .to_owned()
}

/// What the journal holds of `units`, for a failure's message.
fn journal(units: &[&str]) -> String {
    let _ = support::run(support::JOURNALCTL, &["--sync"]);
    let mut args = vec!["--no-pager", "--output=short"];
    for unit in units {
        args.extend(["--unit", unit]);
    }
    String::from_utf8_lossy(&support::run(support::JOURNALCTL, &args).stdout).into_owned()
}

/// The lines lego was started with, so far.
fn calls(tree: &Tree) -> Vec<String> {
    fs::read_to_string(tree.path("state/lego/calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// How many times the proxy's server has started.
fn proxy_starts(tree: &Tree) -> usize {
    fs::read_to_string(tree.path("state/caddy/starts"))
        .unwrap_or_default()
        .lines()
        .count()
}

/// The arguments the example site's settings give lego, after the command.
fn lego_args(tree: &Tree, acme: &Acme, c: &MdmConfig) -> String {
    format!(
        "--email {} --server {} --path {} --key-type ec256 --dns {} --domains {} --dns.resolvers {} --dns.propagation.wait {}",
        acme.email,
        acme.server,
        tree.path("state/lego").display(),
        acme.provider,
        c.dns_name(),
        acme.resolvers_arg(),
        acme.propagation_wait_arg()
    )
}

/// Catches certificate units that order a certificate on every run (an ACME CA's limit on
/// duplicate certificates then stops the renewals), never order the first one, run before the
/// DNS credentials are there, start lego with arguments other than the settings', or leave the
/// proxy without the certificate lego wrote:
///
/// - without `dns.env`, the renewal job (what the timer runs) runs neither lego command and the
///   proxy stays stopped;
/// - with it, the renewal job first has `mdm-cert.service` order the first certificate (`lego run
///   --accept-tos` with the drop-in's settings expanded, and the credentials in its
///   environment), which starts the proxy, and then renews (`lego renew ... --days 30`) and
///   restarts the proxy;
/// - run again, the first job is skipped (lego has a certificate): only the renewal runs, and the
///   proxy is restarted;
/// - the timer runs the renewal job.
#[test]
fn the_certificate_units_order_once_renew_after_and_restart_the_proxy() {
    let tree = Tree::new();
    let (c, acme) = settings();
    lay_out(&tree);
    let renew = tree.unit(package::CERT_RENEW);
    let first = tree.unit(package::CERT_SERVICE);
    let proxy = tree.unit(package::PROXY);
    let timer = tree.unit(package::CERT_TIMER);
    // Every name below is one systemd loaded from the test's files: a property of a unit it
    // does not know reads as inactive, and would pass a check vacuously.
    for unit in [&renew, &first, &proxy, &timer] {
        assert_eq!(property(unit, "LoadState"), "loaded", "{unit}");
    }
    let units = [first.as_str(), renew.as_str(), proxy.as_str()];
    let state = |unit: &str, name: &str, want: &str| {
        let got = property(unit, name);
        assert_eq!(
            got,
            want,
            "{unit} {name}; the journal:\n{}",
            journal(&units)
        );
    };

    support::run_ok(SYSTEMCTL, &["start", renew.as_str()]);
    assert_eq!(calls(&tree), Vec::<String>::new());
    state(&first, "ConditionResult", "no");
    state(&renew, "ConditionResult", "no");
    state(&proxy, "ActiveState", "inactive");

    let dns_env = tree.path("etc/dns.env");
    fs::write(&dns_env, format!("{}={}\n", TOKEN.0, TOKEN.1)).expect("dns.env");
    fs::set_permissions(&dns_env, fs::Permissions::from_mode(0o600)).expect("chmod");
    support::run_ok(SYSTEMCTL, &["start", renew.as_str()]);
    let args = lego_args(&tree, &acme, &c);
    let token = format!("token {}", TOKEN.1);
    assert_eq!(
        calls(&tree),
        [
            format!("run --accept-tos {args}"),
            token.clone(),
            format!("renew {args} --days 30"),
            token.clone(),
        ]
    );
    let crt = tree.path(&format!("state/lego/certificates/{}.crt", c.dns_name()));
    assert!(crt.exists(), "the first job wrote the certificate");
    state(&first, "ConditionResult", "yes");
    state(&first, "Result", "success");
    state(&renew, "Result", "success");
    state(&proxy, "ActiveState", "active");
    support::wait_until(|| proxy_starts(&tree) == 2);

    support::run_ok(SYSTEMCTL, &["start", renew.as_str()]);
    assert_eq!(calls(&tree).len(), 6, "{:?}", calls(&tree));
    assert_eq!(calls(&tree)[4], format!("renew {args} --days 30"));
    state(&first, "ConditionResult", "no");
    state(&proxy, "ActiveState", "active");
    support::wait_until(|| proxy_starts(&tree) == 3);

    state(&timer, "Unit", &renew);
}
