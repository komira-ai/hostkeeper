//! What the `migrate` test binaries share: a fake host shaped like the live stack of the old
//! kit, the old kit's settings file, and running the verb on it.

#![allow(
    dead_code,
    reason = "each migrate test binary uses part of the shared helpers"
)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io;
use std::net::Ipv4Addr;

use crate::common::{self, FakeNet, Kit, SVC, bind, ready_host};
use hostkeeper_mdm::migrate::{Migrate, run};
use hostkeeper_mdm::{Grant, MdmError};
use hostkeeper_mdm_core::commands::scep_ca_init_args;
use hostkeeper_mdm_core::migrate::{Old, Target, new_settings};
use hostkeeper_mdm_core::render::UnitNames;
use hostkeeper_mdm_step::fake::{FakeHost, Node, Strict};
use hostkeeper_mdm_step::{
    Cmd, GETENT, Host, Meta, Mode, Output, SYSTEMCTL, Trustee, USERADD, Way,
};

/// The old service user's id.
pub const OLD_UID: u32 = 800;
pub const OLD_STATE: &str = "/var/lib/js-mdm";
pub const OLD_ETC: &str = "/etc/js-mdm";
pub const NEW_STATE: &str = "/var/lib/hostkeeper-mdm";
pub const NEW_ETC: &str = "/etc/hostkeeper-mdm";
pub const SETTINGS: &str = "/etc/hostkeeper-mdm.conf";
pub const UNITS: &str = "/etc/systemd/system";
pub const TOPIC: &str = "com.apple.mgmt.External.0b6c2a54-8f1e-4a6d-9e0b-3d5f7a9c1e22";
/// The DEP files of the live stack: the name NanoDEP keeps them under is `js-mdm`.
pub const DEP_FILES: [&str; 9] = [
    "consumer_key",
    "consumer_secret",
    "access_token",
    "access_secret",
    "access_token_expiry",
    "key",
    "cert",
    "key_staging",
    "cert_staging",
];

const OLD_FILE: &str = "MDM_DNS_NAME=mdm.example.com
MDM_BIND=@TEST_BIND@
MDM_LAN_CIDR=@TEST_LAN@
MDM_USER=js-mdm
MDM_STATE=/var/lib/js-mdm
MDM_ETC=/etc/js-mdm
MDM_BIN=/opt/js-mdm/bin
MDM_ERASE_STATE=/var/lib/js-mdm-erase
MDM_NANOHUB_PORT=9004
MDM_SCEP_PORT=9005
MDM_DEP_PORT=9001
MDM_MEMORY_MAX=1G
MDM_CPU_QUOTA=100%
MDM_TASKS_MAX=256
MDM_DEP_NAME=js-mdm
MDM_ORG=Example
MDM_PROFILE_ID=com.example.jobscheduler.mdm
MDM_ACME_EMAIL=admin@example.com
MDM_ACME_SERVER=https://acme-v02.api.letsencrypt.org/directory
MDM_ACME_DNS_PROVIDER=godaddy
MDM_ERASE_NAMESPACE=js-mdm-erase
MDM_ERASE_PRINCIPAL=admin@example.com
MDM_ERASE_MAX_AGE=900
";

/// The live stack's settings file, with the addresses of the example site.
pub fn old_text() -> String {
    let lan = format!("{}/24", Ipv4Addr::new(10, 0, 0, 0));
    OLD_FILE
        .replace("@TEST_BIND@", &bind())
        .replace("@TEST_LAN@", &lan)
}

pub fn target() -> Target {
    Target {
        profile_id: "com.komira.hostkeeper.mdm".to_owned(),
        scep_ca_cn: None,
        acme_resolvers: Some("192.0.2.53:53 198.51.100.53:53".to_owned()),
        acme_propagation_wait: Some("300s".to_owned()),
    }
}

/// What `migrate` is asked for, with the settings of `old_text`.
pub fn ask(old_text: &str) -> Migrate {
    Migrate {
        from: "/etc/js-mdm.conf".to_owned(),
        old: Old::parse(old_text).expect("the old settings"),
        settings: SETTINGS.to_owned(),
        bin_dir: "/srv/fetch".to_owned(),
        target: target(),
        max_archive: 1 << 30,
    }
}

pub fn old_units() -> UnitNames {
    UnitNames::with_prefix("kbf-mdm", "kbf-mdm.slice")
}

pub fn new_units() -> UnitNames {
    UnitNames::with_prefix("hostkeeper-mdm", "hostkeeper-mdm.slice")
}

/// The unit files of the old kit, as the live host has them.
pub fn old_unit_files() -> Vec<String> {
    let n = old_units();
    vec![
        n.slice,
        n.scep,
        n.nanohub,
        n.depserver,
        n.depsyncer,
        n.proxy,
        n.cert_service,
        n.cert_timer,
        n.reload_path,
        n.reload_service,
    ]
}

/// The live stack's old units `migrate` stops, in the order it stops them: those enabled or
/// active. The two oneshot jobs are
/// `static` and inactive, so they are not among them; the slice is `static` and active.
pub fn stopped() -> Vec<String> {
    let n = old_units();
    vec![
        n.reload_path,
        n.cert_timer,
        n.proxy,
        n.depserver,
        n.nanohub,
        n.scep,
        n.slice,
    ]
}

const JS: (u32, u32) = (OLD_UID, OLD_UID);
const ROOT: (u32, u32) = (0, 0);

/// The bytes of a file of the live stack, by its path: distinct for each.
pub fn bytes_of(path: &str) -> Vec<u8> {
    format!("contents of {path}").into_bytes()
}

/// The push certificate's tree under the old state.
pub fn push_cert_files() -> Vec<String> {
    ["pem", "key", "stale_token"]
        .iter()
        .map(|ext| format!("{OLD_STATE}/nanohub/mdm/push_cert/ab/cd/{TOPIC}.{ext}"))
        .collect()
}

/// A host shaped like the live stack of the old kit (see the module comment of `migrate`): the
/// old state, etc and unit files, the old units enabled and active and holding the ports, the
/// new kit's service user, and the archives in `/srv/fetch`.
pub fn live_host(kit: &Kit) -> FakeHost {
    let host = ready_host(kit);
    host.add_user("js-mdm", OLD_UID);
    for dir in [
        "",
        "/nanohub",
        "/nanohub/mdm",
        "/nanohub/mdm/push_cert",
        "/nanohub/mdm/push_cert/ab",
        "/nanohub/mdm/push_cert/ab/cd",
        "/nanohub/subsys-fvkey",
        "/dep",
        "/dep/dep_names",
        "/scep",
        "/scep/depot",
        "/www",
        "/lego",
        "/lego/certificates",
        "/caddy",
    ] {
        host.put_dir(&format!("{OLD_STATE}{dir}"), 0o700, JS);
    }
    for path in push_cert_files() {
        host.put_file(&path, 0o600, JS, &bytes_of(&path));
    }
    for name in ["push-topic", "push-cert.sha256"] {
        let path = format!("{OLD_STATE}/{name}");
        host.put_file(&path, 0o644, ROOT, &bytes_of(&path));
    }
    for name in ["key", "cert"] {
        let path = format!("{OLD_STATE}/nanohub/subsys-fvkey/{name}");
        host.put_file(&path, 0o600, JS, &bytes_of(&path));
    }
    for name in DEP_FILES {
        let path = format!("{OLD_STATE}/dep/dep_names/{name}.js-mdm");
        host.put_file(&path, 0o600, JS, &bytes_of(&path));
    }
    for (name, mode) in [("ca.key", 0o400), ("ca.pem", 0o400), ("index.txt", 0o600)] {
        let path = format!("{OLD_STATE}/scep/depot/{name}");
        host.put_file(&path, mode, JS, &bytes_of(&path));
    }
    let path = format!("{OLD_STATE}/www/enroll.mobileconfig");
    host.put_file(&path, 0o600, JS, &bytes_of(&path));
    let path = format!("{OLD_STATE}/lego/certificates/mdm.example.com.crt");
    host.put_file(&path, 0o600, JS, &bytes_of(&path));

    host.put_dir(OLD_ETC, 0o750, (0, OLD_UID));
    host.put_file(
        &format!("{OLD_ETC}/Caddyfile"),
        0o640,
        (0, OLD_UID),
        b"caddyfile",
    );
    for name in ["nanohub.env", "nanodep.env", "scep.env", "dns.env"] {
        let path = format!("{OLD_ETC}/{name}");
        host.put_file(&path, 0o600, ROOT, &bytes_of(&path));
    }

    for unit in old_unit_files() {
        host.put_file(&format!("{UNITS}/{unit}"), 0o644, ROOT, b"[Unit]\n");
    }
    host.put_dir(&format!("{UNITS}/kbf-mdm-cert.service.d"), 0o755, ROOT);
    host.put_file(
        &format!("{UNITS}/kbf-mdm-cert.service.d/override.conf"),
        0o644,
        ROOT,
        b"[Service]\n",
    );
    let n = old_units();
    for unit in [
        &n.scep,
        &n.nanohub,
        &n.depserver,
        &n.proxy,
        &n.cert_timer,
        &n.reload_path,
    ] {
        host.reply(&[SYSTEMCTL, "is-enabled", unit], 0, "enabled\n");
        host.reply(&[SYSTEMCTL, "is-active", unit], 0, "active\n");
    }
    host.reply(&[SYSTEMCTL, "is-enabled", &n.slice], 0, "static\n");
    host.reply(&[SYSTEMCTL, "is-active", &n.slice], 0, "active\n");
    for unit in [&n.cert_service, &n.reload_service] {
        host.reply(&[SYSTEMCTL, "is-enabled", unit], 0, "static\n");
        host.reply(&[SYSTEMCTL, "is-active", unit], 3, "inactive\n");
    }
    host.reply(&[SYSTEMCTL, "is-enabled", &n.depsyncer], 1, "disabled\n");
    host.reply(&[SYSTEMCTL, "is-active", &n.depsyncer], 3, "inactive\n");

    let held = [9004, 9005, 9001, 443]
        .iter()
        .map(|port| format!("LISTEN 0 4096 {}:{port} 0.0.0.0:*\n", bind()))
        .collect::<String>();
    host.reply(&["/usr/bin/ss", "-Hltn"], 0, &held);
    host
}

/// The grants the command line runs `migrate` with for [`ask`]: the old state to the old user
/// and the new state to the new user, by name, and the archives to their owner.
pub fn grants() -> Vec<Grant> {
    let user = |dir: &str, name: &str| Grant {
        dir: dir.to_owned(),
        who: Trustee::User(name.to_owned()),
    };
    vec![
        user(OLD_STATE, "js-mdm"),
        user(NEW_STATE, "hostkeeper-mdm"),
        Grant {
            dir: "/srv/fetch".to_owned(),
            who: Trustee::Uid(1000),
        },
    ]
}

/// Runs `migrate` on `host` at the injected time of the common fixtures, judged as the machine
/// judges it with [`grants`].
pub fn migrate(
    host: &FakeHost,
    kit: &Kit,
    a: &Migrate,
    mode: Mode,
) -> (Result<(), MdmError>, String) {
    migrate_on(&common::strict(host, &grants()), kit, a, mode)
}

/// Runs `migrate` on any host at the injected time of the common fixtures.
pub fn migrate_on(
    host: &dyn Host,
    kit: &Kit,
    a: &Migrate,
    mode: Mode,
) -> (Result<(), MdmError>, String) {
    let net = FakeNet::new(Default::default());
    let mut out = Vec::new();
    let result = common::with_env_on(host, &net, |env| run(env, a, &kit.pins, mode, &mut out));
    (result, String::from_utf8(out).unwrap())
}

/// Every node of the fake tree.
pub fn snapshot(host: &FakeHost) -> BTreeMap<String, Node> {
    host.paths()
        .into_iter()
        .map(|p| {
            let node = host.node(&p).expect("listed");
            (p, node)
        })
        .collect()
}

/// The nodes at or below `dir`.
pub fn below(host: &FakeHost, dir: &str) -> BTreeMap<String, Node> {
    let prefix = format!("{dir}/");
    snapshot(host)
        .into_iter()
        .filter(|(p, _)| p == dir || p.starts_with(&prefix))
        .collect()
}

/// The commands recorded that are not questions: account lookups, `systemctl is-*`, `uname`,
/// `ip`, `ss` and openssl reading a depot change nothing.
pub fn changes(host: &FakeHost) -> Vec<String> {
    host.calls()
        .iter()
        .filter(|c| {
            !(c[0] == GETENT
                || (c[0] == SYSTEMCTL && c[1].starts_with("is-"))
                || [
                    "/usr/bin/uname",
                    "/usr/bin/ip",
                    "/usr/bin/ss",
                    "/usr/bin/openssl",
                ]
                .contains(&c[0].as_str()))
        })
        .map(|c| c.join(" "))
        .collect()
}

/// The new settings this run writes.
pub fn new_text(a: &Migrate) -> String {
    new_settings(&a.old, &a.target).expect("accepted").text
}

/// Makes the fake leave the CA `scepserver ca -init` makes, and openssl find them a pair.
pub fn ca_is_made(host: &FakeHost, a: &Migrate) {
    ca_is_made_as(host, a, b"KEY", b"CA");
    let depot = format!("{NEW_STATE}/scep/depot");
    let pem = format!("{depot}/ca.pem");
    let key = format!("{depot}/ca.key");
    host.reply(
        &["/usr/bin/openssl", "x509", "-noout", "-pubkey", "-in", &pem],
        0,
        "PUBLIC KEY\n",
    );
    host.reply(
        &[
            "/usr/bin/openssl",
            "pkey",
            "-passin",
            "pass:",
            "-pubout",
            "-in",
            &key,
        ],
        0,
        "PUBLIC KEY\n",
    );
}

/// Makes the fake's `scepserver ca -init` (as the new user) leave `key` as `ca.key` and `pem`
/// as `ca.pem`, both the new user's, 0400, as the real one does; openssl is not scripted.
pub fn ca_is_made_as(host: &FakeHost, a: &Migrate, key: &[u8], pem: &[u8]) {
    let c = new_settings(&a.old, &a.target).expect("accepted").config;
    let mut argv: Vec<String> = [
        "/usr/sbin/runuser",
        "-u",
        "hostkeeper-mdm",
        "--",
        "/opt/hostkeeper-mdm/bin/scepserver",
    ]
    .map(str::to_owned)
    .to_vec();
    argv.extend(scep_ca_init_args(&c));
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    let depot = format!("{NEW_STATE}/scep/depot");
    host.effect(&words, &format!("{depot}/ca.key"), 0o400, (SVC, SVC), key);
    host.effect(&words, &format!("{depot}/ca.pem"), 0o400, (SVC, SVC), pem);
}

/// What `useradd` is run with for the new service user.
pub const USERADD_NEW: [&str; 9] = [
    USERADD,
    "--system",
    "--user-group",
    "--home-dir",
    NEW_STATE,
    "--no-create-home",
    "--shell",
    "/usr/sbin/nologin",
    "hostkeeper-mdm",
];

/// The live host before the move: the old kit's stack, and no user `hostkeeper-mdm` until
/// `useradd` makes it (with the uid the fixtures' files of the new user have). Its
/// `scepserver ca -init` leaves a CA as [`ca_is_made_as`] does; openssl is not scripted.
pub fn host_before_the_move_making(a: &Migrate, key: &[u8], pem: &[u8]) -> FakeHost {
    let host = live_host(&common::kit());
    host.reply(&[GETENT, "passwd", "hostkeeper-mdm"], 2, "");
    host.reply(&[GETENT, "group", "hostkeeper-mdm"], 2, "");
    host.reply(&USERADD_NEW, 0, "");
    host.effect_user(&USERADD_NEW, "hostkeeper-mdm", SVC);
    ca_is_made_as(&host, a, key, pem);
    host
}

/// [`host_before_the_move_making`] with the fake CA and openssl of [`ca_is_made`].
pub fn host_before_the_move(a: &Migrate) -> FakeHost {
    let host = host_before_the_move_making(a, b"KEY", b"CA");
    ca_is_made(&host, a);
    host
}

/// A host that fails its `fail_at`-th change (a write, a mkdir, a removal, or a command that is
/// not a lookup or a question), as an apply that stops at that step, and counts the changes.
pub struct FailsAt<'a> {
    host: Strict<'a>,
    fail_at: Option<usize>,
    changes: Cell<usize>,
}

impl<'a> FailsAt<'a> {
    pub fn new(host: Strict<'a>, fail_at: Option<usize>) -> FailsAt<'a> {
        FailsAt {
            host,
            fail_at,
            changes: Cell::new(0),
        }
    }

    /// How many changes were made or failed.
    pub fn changes(&self) -> usize {
        self.changes.get()
    }

    /// One more change; the error the test makes when it is the `fail_at`-th.
    fn change(&self) -> io::Result<()> {
        let n = self.changes.get() + 1;
        self.changes.set(n);
        match self.fail_at == Some(n) {
            true => Err(io::Error::other("the change the test fails")),
            false => Ok(()),
        }
    }
}

/// Whether `cmd` only asks: an account lookup, `systemctl is-*`, and the host checks.
fn asks(cmd: &Cmd) -> bool {
    let program = cmd.program.as_str();
    program == GETENT
        || (program == SYSTEMCTL && cmd.args.first().is_some_and(|a| a.starts_with("is-")))
        || [
            "/usr/bin/uname",
            "/usr/bin/ip",
            "/usr/bin/ss",
            "/usr/bin/openssl",
        ]
        .contains(&program)
}

impl Host for FailsAt<'_> {
    fn euid(&self) -> u32 {
        self.host.euid()
    }
    fn egid(&self) -> u32 {
        self.host.egid()
    }
    fn canonical(&self, path: &str) -> io::Result<String> {
        self.host.canonical(path)
    }
    fn stat(&self, path: &str) -> io::Result<Option<Meta>> {
        self.host.stat(path)
    }
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        self.host.read(path)
    }
    fn read_limited(&self, path: &str, limit: u64) -> io::Result<Option<Vec<u8>>> {
        self.host.read_limited(path, limit)
    }
    fn list(&self, path: &str) -> io::Result<Option<Vec<String>>> {
        self.host.list(path)
    }
    fn write(&self, path: &str, mode: u32, owner: (u32, u32), bytes: &[u8]) -> io::Result<()> {
        self.change()?;
        self.host.write(path, mode, owner, bytes)
    }
    fn mkdir(&self, path: &str, mode: u32, owner: (u32, u32)) -> io::Result<()> {
        self.change()?;
        self.host.mkdir(path, mode, owner)
    }
    fn remove(&self, path: &str) -> io::Result<()> {
        self.change()?;
        self.host.remove(path)
    }
    fn run(&self, cmd: &Cmd) -> io::Result<Output> {
        if !asks(cmd) {
            self.change()?;
        }
        self.host.run(cmd)
    }
    fn way(&self, path: &str) -> io::Result<Option<Way>> {
        self.host.way(path)
    }
}

/// How a convergence check runs `migrate` on a host: directly, or through a wrapper.
pub type Go<'g> = dyn Fn(&dyn Host, Mode) -> (Result<(), MdmError>, String) + 'g;

/// For a fresh host from `make`: one apply that does not fail makes `steps` changes, one per
/// step it prints, and a rerun of it is accepted and leaves every file as it is (the fake does
/// not model systemd's state, so the rerun may plan its systemctl steps again).
/// Then, for each step k: an apply that fails at step k stops there, naming it; the dry run
/// that follows (`mdm_migrate.sh plan`) is accepted and changes nothing; and the apply after
/// it leaves the host exactly as the apply that did not fail. Every run goes through `go`.
pub fn rerun_converges_after_each_step(make: &dyn Fn() -> FakeHost, go: &Go<'_>) {
    let host = make();
    let whole = FailsAt::new(common::strict(&host, &grants()), None);
    let (result, shown) = go(&whole, Mode::Apply);
    result.unwrap_or_else(|e| panic!("{e}\n{shown}"));
    let steps = whole.changes();
    let applied = shown.lines().filter(|l| l.starts_with("+ ")).count();
    assert_eq!(steps, applied, "one change per step\n{shown}");
    assert!(steps > 0, "{shown}");
    let done = snapshot(&host);
    let (result, shown) = go(&common::strict(&host, &grants()), Mode::Apply);
    result.unwrap_or_else(|e| panic!("rerun after a whole apply: {e}\n{shown}"));
    assert_eq!(snapshot(&host), done, "rerun after a whole apply");

    for k in 1..=steps {
        let host = make();
        let failing = FailsAt::new(common::strict(&host, &grants()), Some(k));
        let (first, _) = go(&failing, Mode::Apply);
        let first = first.unwrap_err().to_string();
        assert!(
            first.starts_with(&format!("step {k} ("))
                && first.ends_with("the change the test fails"),
            "{first}"
        );
        let stopped = snapshot(&host);
        let (plan, shown) = go(&common::strict(&host, &grants()), Mode::DryRun);
        plan.unwrap_or_else(|e| panic!("dry run after step {k}: {e}\n{shown}"));
        assert_eq!(snapshot(&host), stopped, "dry run after step {k}");
        let (again, shown) = go(&common::strict(&host, &grants()), Mode::Apply);
        again.unwrap_or_else(|e| panic!("rerun after step {k}: {e}\n{shown}"));
        assert_eq!(snapshot(&host), done, "rerun after a failure at step {k}");
    }
}
