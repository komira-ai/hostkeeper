//! The UI directory a coordinator serves when `serve` is given no `--ui-dir`: the default the
//! binary applies ([`Serve::with_default_ui_dir`] over [`DEFAULT_UI_DIR`]), and the path the
//! container image copies the operator web app to. Each case parses the flags, applies a default
//! that is a path in the test's scratch directory (never the real default path), builds the
//! coordinator, and asks its private listener for `/ui/` over the in-memory network with real
//! TLS. Nothing binds a socket.

mod support;

use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::Parser;
use hostkeeper_coordinator::config::{
    Bound, Cli, Command, DEFAULT_UI_DIR, Parts, Serve, StartError, build, run,
};
use hostkeeper_coordinator::ui::UiDirError;
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_testkit::Scratch;
use rustls::pki_types::CertificateDer;
use support::{Ca, Net, OPERATOR, accepting, client_config, exchange, scratch};

/// The `index.html` of the default directory.
const DEFAULT_INDEX: &str = "<!doctype html><title>the default</title>";

/// The `index.html` of the directory `--ui-dir` names.
const FLAG_INDEX: &str = "<!doctype html><title>the flag</title>";

/// The body of every `404` of the private surface.
const NOT_FOUND: &str = r#"{"error":"not_found"}"#;

/// The files a `serve` reads, in a scratch directory of its own, and the CA that issued the
/// coordinator's certificate.
struct Files {
    dir: Scratch,
    ca: Ca,
}

impl Files {
    fn new(name: &str) -> Self {
        let files = Self {
            dir: scratch(name),
            ca: Ca::new("ui default CA"),
        };
        let leaf = files.ca.coordinator();
        files.put("tls.pem", &leaf.cert_pem);
        files.put("tls.key", &leaf.key_pem);
        files.put("ca.pem", &Ca::new("clients CA").pem);
        files.put("oidc.toml", &config_text(STATIC_ISSUER));
        files.put("operator.token", OPERATOR);
        files
    }

    fn path(&self, file: &str) -> PathBuf {
        self.dir.path().join(file)
    }

    /// Writes `text` at `file`, making the directories above it.
    fn put(&self, file: &str, text: &str) {
        let path = self.path(file);
        let parent = path.parent().expect("a parent");
        std::fs::create_dir_all(parent).expect("the directories");
        std::fs::write(&path, text).expect("written");
        let mode = std::os::unix::fs::PermissionsExt::from_mode(0o600);
        std::fs::set_permissions(&path, mode).expect("mode");
    }

    /// The arguments of a `serve` with no `--ui-dir`, then `extra`.
    fn args(&self, extra: &[OsString]) -> Vec<OsString> {
        let path = |file: &str| self.path(file).into_os_string();
        let mut args: Vec<OsString> = vec!["hostkeeper-coordinator".into(), "serve".into()];
        args.extend([
            "--heartbeat-listen".into(),
            support::HEARTBEAT_ADDR.into(),
            "--private-listen".into(),
            support::CONTROL_ADDR.into(),
            "--tls-cert".into(),
            path("tls.pem"),
            "--tls-key".into(),
            path("tls.key"),
            "--client-ca".into(),
            path("ca.pem"),
            "--state-file".into(),
            path("desired.json"),
            "--oidc-config".into(),
            path("oidc.toml"),
            "--operator-token-file".into(),
            path("operator.token"),
            "--legacy-service".into(),
            "kbf".into(),
        ]);
        args.extend(extra.iter().cloned());
        args
    }
}

/// The `serve` flags of `args`.
fn flags_of(args: Vec<OsString>) -> Serve {
    let Command::Serve(flags) = Cli::try_parse_from(args).expect("flags").command else {
        panic!("serve")
    };
    flags
}

/// `--ui-dir path`.
fn ui_dir_flag(path: &Path) -> Vec<OsString> {
    vec!["--ui-dir".into(), path.as_os_str().to_owned()]
}

/// [`build`], trusting a CA of the test for the issuer's keys, so no test reads the host's
/// certificate store.
fn build_given(flags: &Serve) -> Result<Parts, StartError> {
    let roots = Roots::Given(vec![CertificateDer::from(Ca::new("issuer roots").der)]);
    build(flags, roots)
}

/// Runs `parts` over a new in-memory network and answers `GET path` on its private listener:
/// the status and the body. The coordinator is stopped before this returns.
async fn get(files: &Files, parts: Parts, path: &str) -> (u16, String) {
    let net = Net::default();
    let registry = net.clone();
    let bind = move |addr: SocketAddr| {
        let receive = registry.listen(addr);
        async move { Ok::<_, io::Error>((addr, accepting(receive))) }
    };
    let (tx, rx) = tokio::sync::oneshot::channel::<Bound>();
    let serving = tokio::spawn(async move {
        let _ = run(parts, bind, move |bound| {
            let _ = tx.send(bound);
        })
        .await;
    });
    let bound = rx.await.expect("bound");
    let config = client_config(&files.ca, None);
    let answer = exchange(&net, bound.control, &config, "GET", path, None, "")
        .await
        .expect("answered");
    // The parts hold the lock of the state files: the task is gone, and the lock with it,
    // before the next start of the test.
    serving.abort();
    let _ = serving.await;
    answer
}

/// Catches: the default path not served when `serve` is given no `--ui-dir` (a deploy of the
/// image answering `404` at `/ui/`): with no flag and a default that is a directory, the flags
/// name the default and the private listener answers `/ui/` with its `index.html`, and a file
/// below it by its path.
#[tokio::test]
async fn with_no_flag_the_default_directory_is_served_under_ui() {
    let files = Files::new("ui-default-served");
    files.put("default/index.html", DEFAULT_INDEX);
    files.put("default/pages/fleet.js", "export const fleet = 1;");
    let default = files.path("default");
    let flags = flags_of(files.args(&[])).with_default_ui_dir(&default);
    assert_eq!(flags.ui_dir, Some(default.clone()));
    let parts = build_given(&flags).expect("the start");
    let root = parts.coordinator.ui().map(|dir| dir.root().to_owned());
    assert_eq!(root, Some(std::fs::canonicalize(&default).expect("real")));
    assert_eq!(
        get(&files, parts, "/ui/").await,
        (200, DEFAULT_INDEX.to_owned())
    );
    let parts = build_given(&flags).expect("the start");
    assert_eq!(
        get(&files, parts, "/ui/pages/fleet.js").await,
        (200, "export const fleet = 1;".to_owned())
    );
}

/// Catches: a start refused, or a directory served, when the default path holds no directory:
/// with no flag and nothing at the default, or a file there, the flags stay without a UI
/// directory, the coordinator starts, and `/ui/` is `404`.
#[tokio::test]
async fn with_no_flag_and_no_directory_at_the_default_nothing_is_served() {
    let files = Files::new("ui-default-absent");
    files.put("file-default", DEFAULT_INDEX);
    for default in [files.path("missing"), files.path("file-default")] {
        let flags = flags_of(files.args(&[])).with_default_ui_dir(&default);
        assert_eq!(flags.ui_dir, None, "{}", default.display());
        let parts = build_given(&flags).expect("the start");
        assert!(parts.coordinator.ui().is_none());
        assert_eq!(
            get(&files, parts, "/ui/").await,
            (404, NOT_FOUND.to_owned())
        );
    }
}

/// Catches: the default overriding the flag, or a refused flag falling back to the default:
/// with `--ui-dir` naming a directory and a default that is a directory too, the flag's
/// directory is served; with `--ui-dir` naming nothing, the start is still refused naming the
/// flag and its path.
#[tokio::test]
async fn a_given_flag_wins_over_the_default_and_is_still_refused_when_wrong() {
    let files = Files::new("ui-default-flag");
    files.put("default/index.html", DEFAULT_INDEX);
    files.put("flag/index.html", FLAG_INDEX);
    let default = files.path("default");
    let flag = files.path("flag");
    let flags = flags_of(files.args(&ui_dir_flag(&flag))).with_default_ui_dir(&default);
    assert_eq!(flags.ui_dir, Some(flag));
    let parts = build_given(&flags).expect("the start");
    assert_eq!(
        get(&files, parts, "/ui/").await,
        (200, FLAG_INDEX.to_owned())
    );
    let missing = files.path("missing");
    let flags = flags_of(files.args(&ui_dir_flag(&missing))).with_default_ui_dir(&default);
    assert_eq!(flags.ui_dir, Some(missing.clone()));
    let Err(why) = build_given(&flags) else {
        panic!("the start was not refused")
    };
    assert!(
        matches!(&why, StartError::UiDir(UiDirError::Unreadable { path, .. }) if *path == missing),
        "{why}"
    );
    let prefix = format!("--ui-dir: {}: ", missing.display());
    assert!(why.to_string().starts_with(&prefix), "{why}");
}

/// Catches: the coordinator and its image disagreeing on where the web app is (the image
/// copying `ui/` to one path while the binary looks at another), or the default left out of the
/// flag's help: the default is the destination of the Dockerfile's `COPY ui`, and
/// `serve --help` names it.
#[test]
fn the_default_is_where_the_image_copies_the_web_app_and_the_help_names_it() {
    let dockerfile =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/container/Dockerfile");
    let text = std::fs::read_to_string(dockerfile).expect("the Dockerfile");
    let copies: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("COPY ui "))
        .collect();
    assert_eq!(copies, [DEFAULT_UI_DIR]);
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(&format!("Without it, {DEFAULT_UI_DIR} is served")),
        "{help}"
    );
}
