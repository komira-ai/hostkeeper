//! `hostkeeper-coordinator serve`: its flags, its start-up, its refusals and its exit codes,
//! driven in-process through `config::{Cli, build, run, start, main}`. The binary's one verb is
//! `serve`; bad flags are exit 2, a refused start is exit 1. Nothing binds a socket: `run` and
//! `start` take the function that binds, and these tests give them one that binds nothing.

mod support;

use std::ffi::OsString;
use std::future::Pending;
use std::io;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser;
use clap::error::ErrorKind;
use futures::FutureExt;
use hostkeeper_coordinator::config::{Bound, Cli, Command, Serve, StartError, build, run, start};
use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::service::Unauthorized;
use hostkeeper_coordinator::tokens::Reload;
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_testkit::Scratch;
use hostkeeper_wire::identity::{
    DEFAULT_HOST_ROLE_URI_PREFIX, DEFAULT_ROLE_URI_PREFIX, SanPrefixes,
};
use rustls::pki_types::CertificateDer;
use sha2::{Digest as _, Sha256};
use support::{Ca, OPERATOR, recorded, scratch};
use tokio::io::DuplexStream;

/// The files the flags name, in a directory of their own.
struct Files {
    dir: Scratch,
}

impl Files {
    fn new(name: &str) -> Self {
        let dir = scratch(name);
        let leaf = Ca::new("config CA").coordinator();
        let files = Self { dir };
        files.put("tls.pem", &leaf.cert_pem, 0o644);
        files.put("tls.key", &leaf.key_pem, 0o600);
        files.put("ca.pem", &Ca::new("clients CA").pem, 0o644);
        files.put("oidc.toml", &config_text(STATIC_ISSUER), 0o644);
        files.put("operator.token", OPERATOR, 0o600);
        files.put("rollout.toml", &recorded("services.toml"), 0o600);
        files
    }

    fn put(&self, file: &str, text: &str, mode: u32) {
        let path = self.dir.path().join(file);
        std::fs::write(&path, text).expect("written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
    }

    fn path(&self, file: &str) -> PathBuf {
        self.dir.path().join(file)
    }

    /// Writes `tokens.toml` (mode 0600) with a `[[token]]` for each `(name, token)`, holding
    /// the hash of the token; answers its path as text.
    fn tokens(&self, entries: &[(&str, &str)]) -> String {
        let text: String = entries
            .iter()
            .map(|(name, token)| {
                format!(
                    "[[token]]\nname = \"{name}\"\nsha256 = \"{}\"\n",
                    sha256_hex(token)
                )
            })
            .collect();
        self.put("tokens.toml", &text, 0o600);
        self.path("tokens.toml").display().to_string()
    }

    /// The arguments of a `serve` the recorded flags describe, kbf's namespaces included.
    fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["hostkeeper-coordinator".into(), "serve".into()];
        let pairs = [
            ("--heartbeat-listen", "192.0.2.1:1".to_owned()),
            ("--private-listen", "192.0.2.1:2".to_owned()),
            ("--tls-cert", self.path("tls.pem").display().to_string()),
            ("--tls-key", self.path("tls.key").display().to_string()),
            ("--client-ca", self.path("ca.pem").display().to_string()),
            (
                "--state-file",
                self.path("desired.json").display().to_string(),
            ),
            (
                "--oidc-config",
                self.path("oidc.toml").display().to_string(),
            ),
            (
                "--operator-token-file",
                self.path("operator.token").display().to_string(),
            ),
            (
                "--rollout-file",
                self.path("rollout.toml").display().to_string(),
            ),
            ("--legacy-service", "kbf".to_owned()),
            ("--role-uri-prefix", "urn:kbf:role:".to_owned()),
            ("--host-role-uri-prefix", "urn:kbf:host-role:".to_owned()),
        ];
        for (flag, value) in pairs {
            args.push(flag.into());
            args.push(value.into());
        }
        args
    }
}

/// `args` with the value of `flag` replaced.
fn with(args: &[OsString], flag: &str, value: &str) -> Vec<OsString> {
    let mut args = args.to_vec();
    let at = args.iter().position(|a| a == flag).expect("a flag");
    args[at + 1] = value.into();
    args
}

/// `args` with every element equal to `from` spelled `to`.
fn renamed(args: &[OsString], from: &str, to: &str) -> Vec<OsString> {
    args.iter()
        .map(|a| {
            if a == from {
                OsString::from(to)
            } else {
                a.clone()
            }
        })
        .collect()
}

/// The SHA-256 of `text` as lowercase hex.
fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `args` without `flag` and its value.
fn without(args: &[OsString], flag: &str) -> Vec<OsString> {
    let mut args = args.to_vec();
    let at = args.iter().position(|a| a == flag).expect("a flag");
    args.drain(at..=at + 1);
    args
}

/// The roots the issuer's keys are fetched trusting: a CA of the test, so no test reads the
/// host's certificate store.
fn roots() -> Roots {
    Roots::Given(vec![CertificateDer::from(Ca::new("issuer roots").der)])
}

/// [`build`] trusting [`roots`].
fn build_given(flags: &Serve) -> Result<hostkeeper_coordinator::config::Parts, StartError> {
    build(flags, roots())
}

fn flags_of(args: Vec<OsString>) -> Serve {
    let Command::Serve(flags) = Cli::try_parse_from(args).expect("flags").command;
    flags
}

/// Why `build` refuses `args`.
fn refusal(args: Vec<OsString>) -> StartError {
    match build_given(&flags_of(args)) {
        Err(why) => why,
        Ok(_) => panic!("the start was not refused"),
    }
}

fn exit_code(args: Vec<OsString>) -> ExitCode {
    hostkeeper_coordinator::config::main(args)
}

/// Catches: the flags not being the interface: `--version` and `--help` exit 0 with the usage
/// naming `serve`, no verb, a missing flag, an unknown flag and a reload of zero seconds exit 2.
#[test]
fn the_flags_are_parsed_and_refused_as_the_usage_says() {
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "--help"]).expect_err("help");
    assert_eq!(help.kind(), ErrorKind::DisplayHelp);
    assert!(help.to_string().contains("serve"), "{help}");
    let serve_help =
        Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"]).expect_err("help");
    let text = serve_help.to_string();
    for flag in [
        "--heartbeat-listen",
        "--private-listen",
        "--tokens-file",
        "--operator-token-file",
        "--oidc-config",
        "--rollout-file",
        "--legacy-service",
        "--role-uri-prefix",
        "--host-role-uri-prefix",
    ] {
        assert!(text.contains(flag), "{flag} is missing from {text}");
    }
    for hidden in ["--services-file", "--control-listen"] {
        assert!(!text.contains(hidden), "{hidden} is hidden");
    }
    assert_eq!(
        exit_code(vec!["hostkeeper-coordinator".into(), "--help".into()]),
        ExitCode::SUCCESS
    );
    assert_eq!(
        exit_code(vec!["hostkeeper-coordinator".into(), "--version".into()]),
        ExitCode::SUCCESS
    );
    assert_eq!(
        exit_code(vec!["hostkeeper-coordinator".into()]),
        ExitCode::from(2)
    );
    assert_eq!(
        exit_code(vec!["hostkeeper-coordinator".into(), "serve".into()]),
        ExitCode::from(2)
    );
    let files = Files::new("config-flags");
    let mut args = files.args();
    args.push("--no-such-flag".into());
    assert_eq!(exit_code(args), ExitCode::from(2));
    assert_eq!(
        exit_code(with(&files.args(), "--heartbeat-listen", "not-an-address")),
        ExitCode::from(2)
    );
    let mut args = files.args();
    args.extend([OsString::from("--tls-reload-secs"), OsString::from("0")]);
    assert_eq!(exit_code(args), ExitCode::from(2));
}

/// Catches: a second verb, or a `serve` that can start without the services it manages:
/// `serve` is the only verb, and `--rollout-file` is required.
#[test]
fn serve_is_the_only_verb_and_the_rollout_file_is_required() {
    let files = Files::new("config-verb");
    let mut other = files.args();
    other[1] = "run".into();
    assert_eq!(
        Cli::try_parse_from(other).expect_err("no such verb").kind(),
        ErrorKind::InvalidSubcommand
    );
    let args = without(&files.args(), "--rollout-file");
    let missing = Cli::try_parse_from(args).expect_err("--rollout-file is required");
    assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);
    assert!(missing.to_string().contains("--rollout-file"), "{missing}");
    let Command::Serve(_) = Cli::try_parse_from(files.args()).expect("flags").command;
}

/// Catches: the certificate namespaces and the legacy service not being flags with the
/// defaults the project ships: without them the prefixes are `urn:hostkeeper:` ones and there
/// is no legacy service; with them they are what was given.
#[test]
fn the_prefixes_default_to_the_hostkeeper_namespaces_and_the_legacy_service_to_none() {
    let files = Files::new("config-defaults");
    let args = without(
        &without(
            &without(&files.args(), "--legacy-service"),
            "--role-uri-prefix",
        ),
        "--host-role-uri-prefix",
    );
    let flags = flags_of(args);
    assert_eq!(flags.role_uri_prefix, DEFAULT_ROLE_URI_PREFIX);
    assert_eq!(flags.host_role_uri_prefix, DEFAULT_HOST_ROLE_URI_PREFIX);
    assert_eq!(flags.legacy_service, None);
    assert_eq!(flags.tls_reload_secs, 60);
    assert_eq!(flags.ops_file, None);
    let given = flags_of(files.args());
    assert_eq!(given.role_uri_prefix, "urn:kbf:role:");
    assert_eq!(given.host_role_uri_prefix, "urn:kbf:host-role:");
    assert_eq!(given.legacy_service.as_deref(), Some("kbf"));
}

/// Catches: `--version` not naming the commit the binary was built from, which is what an
/// image carrying the binary is checked against: the line is `hostkeeper-coordinator <package
/// version>+<12 hex digits of the commit>` (or `unknown` for a source archive).
#[test]
fn the_version_names_the_commit_it_was_built_from() {
    let version =
        Cli::try_parse_from(["hostkeeper-coordinator", "--version"]).expect_err("the version");
    assert_eq!(version.kind(), ErrorKind::DisplayVersion);
    let text = version.to_string();
    let line = text.trim_end();
    let rest = line
        .strip_prefix("hostkeeper-coordinator ")
        .unwrap_or_else(|| panic!("not `hostkeeper-coordinator <version>`: {line:?}"));
    let (package, stamp) = rest
        .split_once('+')
        .unwrap_or_else(|| panic!("no `+<commit>` after the package version: {line:?}"));
    assert_eq!(package, env!("CARGO_PKG_VERSION"), "{line:?}");
    let hex = stamp.len() == 12
        && stamp
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    assert!(hex || stamp == "unknown", "the stamp is {stamp:?}");
    assert_eq!(stamp, hostkeeper_coordinator::BUILD_COMMIT);
}

/// Catches: a start that builds from flags it should refuse: the recorded flags (kbf as the
/// legacy service, kbf's certificate namespaces) build a coordinator that has them, and the
/// state file named is the one it opens.
#[test]
fn the_recorded_flags_build_a_coordinator_that_has_them() {
    let files = Files::new("config-build");
    let parts = build_given(&flags_of(files.args())).expect("a start");
    assert_eq!(parts.coordinator.legacy_service(), Some("kbf"));
    assert_eq!(
        *parts.coordinator.prefixes(),
        SanPrefixes::new("urn:kbf:role:", "urn:kbf:host-role:").expect("prefixes")
    );
    assert_eq!(
        *parts.coordinator.prefixes(),
        support::kbf_prefixes(),
        "the recorded flags.json says the same"
    );
    assert_eq!(
        parts.heartbeat_listen,
        "192.0.2.1:1".parse::<SocketAddr>().expect("an address")
    );
    assert_eq!(
        parts.control_listen,
        "192.0.2.1:2".parse::<SocketAddr>().expect("an address")
    );
    assert_eq!(parts.reload, Duration::from_secs(60));
    assert!(
        !files.path("desired.json").exists(),
        "nothing is created before the first change"
    );
}

/// Catches: a start that goes on with a file it cannot use, or flags that do not fit: a missing
/// certificate, an operator token file others can read, a bad allow-list, a missing rollout
/// file, an allow-list entry naming a service that is not defined or no service with no legacy
/// service to stand for, a legacy service that is not defined, namespaces that overlap and a
/// state file that is not a state each stop it with a reason that names the file or the flag; and
/// the binary exits 1.
#[test]
fn a_start_with_a_bad_file_or_flag_is_refused_with_a_reason() {
    let files = Files::new("config-refused");
    let args = files.args();

    let missing = files.path("nope.pem").display().to_string();
    let why = refusal(with(&args, "--tls-cert", &missing));
    assert!(matches!(why, StartError::Tls(_)), "{why}");
    assert!(why.to_string().contains("nope.pem"), "{why}");
    let unread = files.path("nope.token").display().to_string();
    assert_eq!(
        exit_code(with(&args, "--operator-token-file", &unread)),
        ExitCode::from(1)
    );

    files.put("open.token", OPERATOR, 0o644);
    let open = files.path("open.token").display().to_string();
    let why = refusal(with(&args, "--operator-token-file", &open));
    assert!(matches!(why, StartError::OperatorToken(_)), "{why}");
    assert!(why.to_string().contains("--operator-token-file"), "{why}");

    files.put("bad.toml", "[remote_operator]\n", 0o644);
    let bad = files.path("bad.toml").display().to_string();
    let why = refusal(with(&args, "--oidc-config", &bad));
    assert!(matches!(why, StartError::Oidc(_)), "{why}");
    assert!(why.to_string().contains("remote_operator"), "{why}");

    let nope = files.path("nope.toml").display().to_string();
    let why = refusal(with(&args, "--rollout-file", &nope));
    assert!(matches!(why, StartError::Services(_)), "{why}");
    assert!(why.to_string().contains("--rollout-file"), "{why}");

    let unknown = format!(
        "{}services = [\"kbf\", \"nope\"]\n",
        config_text(STATIC_ISSUER)
    );
    files.put("unknown.toml", &unknown, 0o644);
    let path = files.path("unknown.toml").display().to_string();
    let why = refusal(with(&args, "--oidc-config", &path));
    assert!(
        matches!(&why, StartError::OidcEntryService { entry: 1, service } if service == "nope"),
        "{why}"
    );
    assert!(why.to_string().contains("--oidc-config"), "{why}");

    let why = refusal(without(&args, "--legacy-service"));
    assert!(matches!(why, StartError::OidcEntryUnnamed(1)), "{why}");
    assert!(why.to_string().contains("--legacy-service"), "{why}");

    let why = refusal(with(&args, "--legacy-service", "nope"));
    assert!(
        matches!(&why, StartError::LegacyService(name) if name == "nope"),
        "{why}"
    );
    assert!(why.to_string().contains("--legacy-service"), "{why}");

    let why = refusal(with(&args, "--host-role-uri-prefix", "urn:kbf:role:"));
    assert!(matches!(why, StartError::Prefixes(_)), "{why}");
    assert!(why.to_string().contains("--role-uri-prefix"), "{why}");

    files.put("state.json", "{}", 0o600);
    let state = files.path("state.json").display().to_string();
    let why = refusal(with(&args, "--state-file", &state));
    assert!(matches!(why, StartError::Desired(_)), "{why}");
    assert!(why.to_string().contains("state.json"), "{why}");
}

/// Catches: a coordinator that manages no service being unable to start: an empty rollout
/// file, no `--legacy-service` and an allow-list entry with `services = []` (which grants CI
/// nothing) build a coordinator whose status lists no service, through `build` and through
/// `start`; an entry that names a service the file does not define is still refused.
#[tokio::test]
async fn a_coordinator_with_no_services_starts_and_grants_nothing() {
    let files = Files::new("config-empty");
    files.put("rollout.toml", "", 0o600);
    files.put(
        "empty.toml",
        &format!("{}services = []\n", config_text(STATIC_ISSUER)),
        0o644,
    );
    let path = files.path("empty.toml").display().to_string();
    let args = without(
        &with(&files.args(), "--oidc-config", &path),
        "--legacy-service",
    );
    let parts = build_given(&flags_of(args.clone())).expect("a start with no service");
    assert_eq!(parts.coordinator.legacy_service(), None);
    let status = parts.coordinator.status(Some(OPERATOR)).expect("status");
    let shown = serde_json::to_value(&status).expect("json");
    assert_eq!(shown["services"], serde_json::json!({}));
    assert_eq!(shown["per_service"], serde_json::json!({}));

    let bind = |addr: SocketAddr| async move { Ok::<_, io::Error>((addr, never as Never)) };
    let stop = async {
        tokio::task::yield_now().await;
        Ok(())
    };
    start(flags_of(args.clone()), roots(), bind, stop)
        .await
        .expect("the start runs and stops cleanly");

    let named = format!("{}services = [\"kbf\"]\n", config_text(STATIC_ISSUER));
    files.put("named.toml", &named, 0o644);
    let path = files.path("named.toml").display().to_string();
    let why = refusal(with(&args, "--oidc-config", &path));
    assert!(matches!(why, StartError::OidcEntryService { .. }), "{why}");
}

/// Catches: a state file in the single-state form read with no legacy service to own it: a file
/// the last kbf coordinator wrote is refused at start when `--legacy-service` is not given (an
/// allow-list entry that names `kbf` keeps the entry check out of the way), and read when it is.
#[test]
fn a_single_state_file_needs_the_legacy_service_to_belong_to() {
    let files = Files::new("config-single");
    std::fs::write(files.path("desired.json"), recorded("desired-single.json")).expect("written");
    let named = format!("{}services = [\"kbf\"]\n", config_text(STATIC_ISSUER));
    files.put("named.toml", &named, 0o644);
    let path = files.path("named.toml").display().to_string();
    let args = with(&files.args(), "--oidc-config", &path);
    let parts = build_given(&flags_of(args.clone())).expect("read with the legacy service");
    assert_eq!(parts.coordinator.legacy_service(), Some("kbf"));
    let why = refusal(without(&args, "--legacy-service"));
    assert!(matches!(why, StartError::Desired(_)), "{why}");
    assert!(
        why.to_string().contains("no legacy service is set"),
        "{why}"
    );
    let on_disk = DesiredStore::open(&files.path("desired.json"), Some("kbf")).expect("store");
    assert_eq!(
        on_disk.get("kbf").expect("state").version.to_string(),
        "2.0"
    );
}

/// Catches: a start over an ops file this build cannot honour: a file with a key it does not
/// know, or with a newer schema, at the path `--ops-file` names or, with no flag, at the state
/// file's name with `.ops` added, stops it with a reason that names the flag and the file; and
/// the file is left as it was.
#[test]
fn an_ops_file_this_build_cannot_read_stops_the_start() {
    let files = Files::new("config-ops");
    let args = files.args();
    let given = files.path("given.ops");
    std::fs::write(&given, r#"{"schema":1,"future":true}"#).expect("written");
    let mut explicit = args.clone();
    explicit.extend(["--ops-file".into(), given.clone().into_os_string()]);
    let why = refusal(explicit);
    assert!(matches!(why, StartError::Ops(_)), "{why}");
    let text = why.to_string();
    assert!(
        text.contains("--ops-file") && text.contains("given.ops"),
        "{text}"
    );
    assert_eq!(
        std::fs::read_to_string(&given).expect("read"),
        r#"{"schema":1,"future":true}"#
    );

    let newer = recorded("ops-schema2.json");
    let default = files.path("desired.json.ops");
    std::fs::write(&default, &newer).expect("written");
    let text = refusal(args).to_string();
    assert!(
        text.contains("--ops-file") && text.contains("desired.json.ops"),
        "{text}"
    );
    assert!(text.contains("newer"), "{text}");
    assert_eq!(std::fs::read_to_string(&default).expect("read"), newer);
}

type Never = fn() -> Pending<io::Result<DuplexStream>>;

fn never() -> Pending<io::Result<DuplexStream>> {
    std::future::pending()
}

/// Catches: a start that does not bind both listeners where the flags say, or does not say
/// where they are: `run` asks the bind function for the heartbeat address and then the control
/// address, calls `ready` with what it answered, and serves; it never returns `Ok`.
#[tokio::test]
async fn run_binds_both_listeners_and_reports_them() {
    let files = Files::new("config-run");
    let parts = build_given(&flags_of(files.args())).expect("a start");
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&asked);
    let bind = move |addr: SocketAddr| {
        log.lock().expect("log").push(addr);
        let answered: SocketAddr = format!("192.0.2.9:{}", addr.port()).parse().expect("addr");
        async move { Ok::<_, io::Error>((answered, never as Never)) }
    };
    let ready = Arc::new(Mutex::new(None));
    let told = Arc::clone(&ready);
    let outcome = run(parts, bind, move |bound| {
        *told.lock().expect("told") = Some(bound);
    })
    .now_or_never();
    assert!(outcome.is_none(), "it serves until it is dropped");
    let asked = asked.lock().expect("log").clone();
    let want: Vec<SocketAddr> = vec![
        "192.0.2.1:1".parse().expect("an address"),
        "192.0.2.1:2".parse().expect("an address"),
    ];
    assert_eq!(
        asked, want,
        "the heartbeat address first, then the control one"
    );
    assert_eq!(
        *ready.lock().expect("told"),
        Some(Bound {
            heartbeat: "192.0.2.9:1".parse().expect("an address"),
            control: "192.0.2.9:2".parse().expect("an address"),
        })
    );
}

/// Catches: `start` swallowing why it stopped: it runs until `stop` resolves and then is
/// `Ok`; a failing `stop` is an `Io` refusal naming the failure; an address that cannot be
/// bound is an `Io` refusal and nothing is served; a refused file is refused before anything
/// is bound.
#[tokio::test]
async fn start_runs_until_stopped_and_says_why_it_ended() {
    let files = Files::new("config-start");
    let binds = Arc::new(Mutex::new(0_usize));
    let counted = Arc::clone(&binds);
    let bind = move |addr: SocketAddr| {
        *counted.lock().expect("count") += 1;
        async move { Ok::<_, io::Error>((addr, never as Never)) }
    };
    let stop = async {
        tokio::task::yield_now().await;
        Ok(())
    };
    start(flags_of(files.args()), roots(), bind.clone(), stop)
        .await
        .expect("a stop is a clean end");
    assert_eq!(*binds.lock().expect("count"), 2);

    let failing = async { Err(io::Error::other("the signal handler failed")) };
    let Err(why) = start(flags_of(files.args()), roots(), bind, failing).await else {
        panic!("a failing stop is an error");
    };
    assert!(matches!(why, StartError::Io(_)), "{why}");
    assert!(
        why.to_string().contains("the signal handler failed"),
        "{why}"
    );

    let in_use = |_: SocketAddr| async {
        Err::<(SocketAddr, Never), io::Error>(io::Error::from(io::ErrorKind::AddrInUse))
    };
    let Err(why) = start(
        flags_of(files.args()),
        roots(),
        in_use,
        std::future::pending(),
    )
    .await
    else {
        panic!("an address in use is an error");
    };
    assert!(matches!(why, StartError::Io(_)), "{why}");

    let missing = files.path("nope.pem").display().to_string();
    let args = with(&files.args(), "--tls-cert", &missing);
    let untouched = Arc::new(Mutex::new(0_usize));
    let counted = Arc::clone(&untouched);
    let bind = move |addr: SocketAddr| {
        *counted.lock().expect("count") += 1;
        async move { Ok::<_, io::Error>((addr, never as Never)) }
    };
    let Err(why) = start(flags_of(args), roots(), bind, std::future::pending()).await else {
        panic!("a missing file is an error");
    };
    assert!(matches!(why, StartError::Tls(_)), "{why}");
    assert_eq!(*untouched.lock().expect("count"), 0, "nothing was bound");
}

/// Proves that the names the flags had are still accepted and hidden: `--services-file` and
/// `--control-listen` parse as `--rollout-file` and `--private-listen` and build a start, and
/// giving a flag with its old name beside it is a usage error (exit 2) naming the new flag.
///
/// Catches: a rename that breaks the start line of a coordinator already deployed, or one that
/// lets the two names set the flag twice and the last one win.
#[test]
fn the_old_flag_names_are_hidden_aliases_and_both_together_are_refused() {
    let files = Files::new("config-aliases");
    let old = renamed(
        &renamed(&files.args(), "--rollout-file", "--services-file"),
        "--private-listen",
        "--control-listen",
    );
    let flags = flags_of(old.clone());
    assert_eq!(flags.rollout_file, files.path("rollout.toml"));
    assert_eq!(
        flags.private_listen,
        "192.0.2.1:2".parse::<SocketAddr>().expect("an address")
    );
    build_given(&flags).expect("a start through the old names");

    let mut both = files.args();
    both.extend([
        OsString::from("--services-file"),
        files.path("rollout.toml").into_os_string(),
    ]);
    let conflict = Cli::try_parse_from(both.clone()).expect_err("both names of one flag");
    assert_eq!(conflict.kind(), ErrorKind::ArgumentConflict);
    assert!(
        conflict.to_string().contains("--rollout-file"),
        "{conflict}"
    );
    assert_eq!(exit_code(both), ExitCode::from(2));

    let mut both = files.args();
    both.extend([
        OsString::from("--control-listen"),
        OsString::from("192.0.2.1:3"),
    ]);
    let conflict = Cli::try_parse_from(both.clone()).expect_err("both names of one flag");
    assert_eq!(conflict.kind(), ErrorKind::ArgumentConflict);
    assert!(
        conflict.to_string().contains("--private-listen"),
        "{conflict}"
    );
    assert_eq!(exit_code(both), ExitCode::from(2));
}

/// Proves that `--tokens-file` and `--operator-token-file` together are refused at start with
/// the text of the refusal and exit 1, and that giving neither is refused with its own text.
///
/// Catches: two sets of operator tokens in force at once (one of them unrevocable), or a start
/// with no operator token at all.
#[test]
fn both_token_flags_together_are_refused() {
    let files = Files::new("config-token-flags");
    let mut both = files.args();
    both.extend([
        OsString::from("--tokens-file"),
        OsString::from(files.tokens(&[("alice", "alice-token-0123456789abcdef0123456789")])),
    ]);
    let why = refusal(both.clone());
    assert!(matches!(why, StartError::TokenFlags), "{why}");
    assert_eq!(
        why.to_string(),
        "--tokens-file and --operator-token-file are both given: name the operator's tokens one way"
    );
    assert_eq!(exit_code(both), ExitCode::from(1));

    let neither = without(&files.args(), "--operator-token-file");
    let why = refusal(neither.clone());
    assert!(matches!(why, StartError::NoTokenFlag), "{why}");
    assert_eq!(
        why.to_string(),
        "one of --tokens-file and --operator-token-file is required"
    );
    assert_eq!(exit_code(neither), ExitCode::from(1));
}

/// Proves that `--operator-token-file` is one token named `operator`: the token is accepted
/// under that name, a near miss and no token are refused, and an operator action it makes is
/// recorded by `operator` in the entry and in the audit file beside the ops file.
///
/// Catches: the legacy flag giving the token another name, or an action by it recorded under no
/// name.
#[test]
fn the_legacy_single_token_flag_is_one_token_named_operator() {
    let files = Files::new("config-legacy-token");
    let parts = build_given(&flags_of(files.args())).expect("a start");
    let c = &parts.coordinator;
    assert_eq!(
        c.operator(Some(OPERATOR))
            .map(|name| name.as_str().to_owned()),
        Ok("operator".to_owned())
    );
    assert_eq!(
        c.operator(Some("operator"))
            .map(|name| name.as_str().to_owned()),
        Err(Unauthorized)
    );
    assert_eq!(
        c.operator(Some(&OPERATOR[1..]))
            .map(|name| name.as_str().to_owned()),
        Err(Unauthorized)
    );
    assert_eq!(
        c.operator(None).map(|name| name.as_str().to_owned()),
        Err(Unauthorized)
    );
    let entry = c
        .operate(
            Some(OPERATOR),
            br#"{"action":"exclude","service":"kbf","node_id":"d1","reason":"disk"}"#,
        )
        .expect("the action is done");
    assert_eq!(entry.by, "operator");
    let audit = std::fs::read_to_string(files.path("desired.json.ops.audit")).expect("audit file");
    assert!(audit.contains(r#""by":"operator""#), "{audit}");
}

/// Proves that `--tokens-file` names each token: a token is accepted under its own name, a
/// token taken out of the file is refused after the file is read again, and a file others can
/// read is refused at start (exit 1) with the flag named.
///
/// Catches: every token being the same operator, a revoked token staying valid until a restart,
/// and a tokens file that anyone may rewrite being trusted.
#[test]
fn a_tokens_file_gives_each_token_its_name_and_is_read_again() {
    let files = Files::new("config-tokens-file");
    let alice = "alice-token-0123456789abcdef0123456789";
    let bob = "bob-token-0123456789abcdef0123456789";
    let mut args = without(&files.args(), "--operator-token-file");
    args.extend([
        OsString::from("--tokens-file"),
        OsString::from(files.tokens(&[("alice", alice), ("bob", bob)])),
    ]);
    let parts = build_given(&flags_of(args.clone())).expect("a start");
    let c = &parts.coordinator;
    let name = |token: &str| c.operator(Some(token)).map(|name| name.as_str().to_owned());
    assert_eq!(name(alice), Ok("alice".to_owned()));
    assert_eq!(name(bob), Ok("bob".to_owned()));
    assert_eq!(name(OPERATOR), Err(Unauthorized));

    files.tokens(&[("alice", alice)]);
    assert_eq!(c.tokens().reload(), Reload::Applied);
    assert_eq!(name(alice), Ok("alice".to_owned()));
    assert_eq!(name(bob), Err(Unauthorized));

    files.put(
        "open.toml",
        &format!(
            "[[token]]\nname = \"alice\"\nsha256 = \"{}\"\n",
            sha256_hex(alice)
        ),
        0o644,
    );
    let mut open = without(&args, "--tokens-file");
    open.extend([
        OsString::from("--tokens-file"),
        files.path("open.toml").into_os_string(),
    ]);
    let why = refusal(open.clone());
    assert!(matches!(why, StartError::Tokens(_)), "{why}");
    assert!(why.to_string().contains("--tokens-file"), "{why}");
    assert_eq!(exit_code(open), ExitCode::from(1));
}

/// Proves that a wildcard `--private-listen` is refused at start, however it is spelled (IPv4,
/// IPv6 and IPv4-mapped IPv6) and through the old flag name too, with the text that names the
/// flag and the address and exit 1; an address of one interface is accepted, and so is a
/// wildcard `--heartbeat-listen`, whose flag is unchanged.
///
/// Catches: the listener that serves CI and the operator bound on every interface.
#[test]
fn a_wildcard_private_listen_is_refused_at_start() {
    let files = Files::new("config-wildcard");
    let args = files.args();
    for addr in ["0.0.0.0:2", "[::]:2", "[::ffff:0.0.0.0]:2"] {
        let given = with(&args, "--private-listen", addr);
        let why = refusal(given.clone());
        let want: SocketAddr = addr.parse().expect("an address");
        assert!(
            matches!(&why, StartError::PrivateListen(a) if *a == want),
            "{why}"
        );
        assert_eq!(
            why.to_string(),
            format!(
                "--private-listen: {addr} is a wildcard address; the private listener takes CI's calls and the operator's, so name the address of one interface"
            )
        );
        assert_eq!(exit_code(given), ExitCode::from(1));
    }
    let old = renamed(
        &with(&args, "--private-listen", "0.0.0.0:2"),
        "--private-listen",
        "--control-listen",
    );
    let why = refusal(old.clone());
    assert!(matches!(why, StartError::PrivateListen(_)), "{why}");
    assert_eq!(exit_code(old), ExitCode::from(1));
    for addr in ["127.0.0.1:2", "192.0.2.1:2"] {
        build_given(&flags_of(with(&args, "--private-listen", addr)))
            .expect("one interface is accepted");
    }
    build_given(&flags_of(with(&args, "--heartbeat-listen", "0.0.0.0:1")))
        .expect("a wildcard heartbeat listener is accepted");
}
