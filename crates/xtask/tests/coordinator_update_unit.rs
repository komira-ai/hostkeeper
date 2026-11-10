//! The coordinator's self-update unit, as committed
//! (`docs/deploy/coordinator/hostkeeper-coordinator-update.{timer,service}`): the timer starts
//! the service, and the service's command is one the verb's own flag parser accepts, with the
//! download directory the unit gives it write access to; and that command, run as committed
//! (the installed `hostkeeper-coordinator-update` is this crate's binary of that name, the release
//! asset's program; the two paths under `/var/lib` and `/etc` are scratch ones), updates a coordinator, leaves an up-to-date one alone, and rolls a new one
//! that is not ready back. Its `gh` and `curl` are the test program `tests/bin/gh_fixture.rs`,
//! which answers the release, the Docker Engine API and the coordinator's version route from
//! scratch files: no network, no docker, no gh.

#[path = "../src/testdir.rs"]
mod testdir;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use serde_json::{Value, json};
use sha2::Digest;
use testdir::TestDir;

const UPDATER: &str = env!("CARGO_BIN_EXE_hostkeeper-coordinator-update");

/// Where the runbook installs the release asset's program, the path the committed unit starts.
const INSTALLED: &str = "/usr/local/libexec/hostkeeper/hostkeeper-coordinator-update";
const GH: &str = env!("CARGO_BIN_EXE_xtask-gh-fixture");
const COMMIT: &str = "0771c30f87f375ffd8b39e8b4630cd6ea377b764";
const OLD_ID: &str = "0ld0000000000000";
const NEW_ID: &str = "new0000000000000";

fn deploy_file(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/deploy/coordinator")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The value of `key` in the section `[section]` of a unit file, the last when it repeats.
fn unit_value<'a>(unit: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut current = "";
    let mut found = None;
    for line in unit.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name;
        } else if let Some(v) = line
            .strip_prefix(key)
            .and_then(|l| l.strip_prefix('='))
            .filter(|_| current == section)
        {
            found = Some(v);
        }
    }
    found
}

/// What the program's parser makes of `args` (the words after the program).
fn parse(args: &[&str]) -> Result<clap::ArgMatches, clap::Error> {
    let mut argv = vec!["hostkeeper-coordinator-update"];
    argv.extend(args);
    xtask::test_support::coordinator_update_cli().try_get_matches_from(argv)
}

#[test]
fn the_timer_starts_the_committed_unit_whose_command_the_programs_parser_accepts() {
    // Catches: a timer that starts another unit, fires on a calendar instead of after each run
    // ends (so two runs could overlap), a unit whose command the verb does not parse (a flag
    // renamed or dropped, a URL that is not https), a download directory the sandbox does not let
    // the unit write, or a docker group the unit does not join.
    let timer = deploy_file("hostkeeper-coordinator-update.timer");
    let name = "hostkeeper-coordinator-update.service";
    assert_eq!(
        unit_value(&timer, "Timer", "Unit").unwrap_or(name),
        name,
        "the timer starts the committed service"
    );
    assert_eq!(unit_value(&timer, "Timer", "OnBootSec"), Some("2min"));
    assert_eq!(
        unit_value(&timer, "Timer", "OnUnitInactiveSec"),
        Some("5min")
    );
    assert_eq!(unit_value(&timer, "Timer", "OnCalendar"), None);
    assert_eq!(
        unit_value(&timer, "Install", "WantedBy"),
        Some("timers.target")
    );

    let unit = deploy_file(name);
    assert_eq!(unit_value(&unit, "Service", "Type"), Some("oneshot"));
    assert_eq!(
        unit_value(&unit, "Service", "User"),
        Some("hostkeeper-coordinator-update")
    );
    assert_eq!(
        unit_value(&unit, "Service", "SupplementaryGroups"),
        Some("docker")
    );
    assert_eq!(
        unit_value(&unit, "Service", "EnvironmentFile"),
        Some("/etc/hostkeeper/coordinator-update.env")
    );
    let state = unit_value(&unit, "Service", "StateDirectory").expect("a state directory");
    let exec = unit_value(&unit, "Service", "ExecStart").expect("the unit's ExecStart");
    let mut words = exec.split_whitespace();
    assert_eq!(
        words.next(),
        Some(INSTALLED),
        "the installed release asset's program"
    );
    let args: Vec<&str> = words.collect();
    let flags = parse(&args).expect("the program parses the unit's command");
    let dir = flags
        .get_one::<PathBuf>("download_dir")
        .expect("--download-dir");
    assert_eq!(dir, &Path::new("/var/lib").join(state));
    assert_eq!(
        unit_value(&unit, "Service", "ReadWritePaths"),
        dir.to_str(),
        "the sandbox (ProtectSystem=strict) must let the unit write its download directory"
    );
    let url = flags
        .get_one::<String>("coordinator_url")
        .expect("--coordinator-url");
    assert!(url.starts_with("https://"), "{url}");
    assert_eq!(
        flags.get_one::<PathBuf>("ca_file"),
        Some(&PathBuf::from("/etc/hostkeeper/coordinator-ca.pem"))
    );

    // The parser is the check: the same command with a plain-http URL, or without its download
    // directory, is refused.
    let http: Vec<&str> = args
        .iter()
        .map(|a| {
            if a.starts_with("https://") {
                "http://coordinator.example.com:8444"
            } else {
                a
            }
        })
        .collect();
    assert!(parse(&http).is_err());
    let at = args
        .iter()
        .position(|a| *a == "--download-dir")
        .expect("--download-dir");
    let mut without = args.clone();
    without.drain(at..at + 2);
    assert!(parse(&without).is_err());
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// An OCI image layout tarball as `docker buildx` writes one: `index.json` naming one manifest,
/// which names its config; and that config's digest.
fn image_tar() -> (Vec<u8>, String) {
    let config = br#"{"architecture":"amd64"}"#.to_vec();
    let layer = b"the layer".to_vec();
    let digest = |b: &[u8]| format!("sha256:{}", sha256(b));
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "config": {"digest": digest(&config), "size": config.len()},
        "layers": [{"digest": digest(&layer), "size": layer.len()}],
    }))
    .expect("JSON");
    let index = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "manifests": [{"digest": digest(&manifest), "size": manifest.len()}],
    }))
    .expect("JSON");
    let mut builder = tar::Builder::new(Vec::new());
    let blob = |b: &[u8]| format!("blobs/sha256/{}", sha256(b));
    for (path, data) in [
        (
            "oci-layout".to_owned(),
            br#"{"imageLayoutVersion":"1.0.0"}"#.as_slice(),
        ),
        ("index.json".to_owned(), index.as_slice()),
        (blob(&config), config.as_slice()),
        (blob(&layer), layer.as_slice()),
        (blob(&manifest), manifest.as_slice()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        builder
            .append_data(&mut header, path, data)
            .expect("append");
    }
    (builder.into_inner().expect("tar"), digest(&config))
}

/// A scratch machine: `bin/gh` and `bin/curl` (the fixture), `bin/releases/` (the published
/// release v0.1.219), `bin/answers/` (docker and the coordinator), the download directory and
/// the CA file; and the committed command with its two machine paths moved into it.
struct Machine {
    _root: TestDir,
    answers: PathBuf,
    downloads: PathBuf,
    bin: PathBuf,
    args: Vec<String>,
    config: String,
}

fn platform() -> &'static str {
    if std::env::consts::ARCH == "aarch64" {
        "linux-arm64"
    } else {
        "linux-x86_64"
    }
}

fn machine(name: &str) -> Machine {
    let root = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name);
    let bin = root.join("bin");
    let answers = bin.join("answers");
    let downloads = root.join("downloads");
    let release = bin.join("releases/komira-ai/hostkeeper/v0.1.219");
    for dir in [&answers, &downloads, &release] {
        std::fs::create_dir_all(dir).expect("make a scratch directory");
    }
    // Hard links, not copies: a copy held open for writing can make a forked exec fail with
    // "text file busy". The fixture answers by the name it was started by.
    for as_name in ["gh", "curl"] {
        std::fs::hard_link(GH, bin.join(as_name)).expect("install the fixture");
    }
    let (tar, config) = image_tar();
    let image = format!("hostkeeper-coordinator-0.1.219-{}.image.tar", platform());
    let statement = json!({"subject": [{"digest": {"sha256": sha256(&tar)}}]});
    let payload = base64::engine::general_purpose::STANDARD.encode(statement.to_string());
    let assets: BTreeMap<String, Vec<u8>> = [
        (
            format!("{image}.sigstore.json"),
            json!({"dsseEnvelope": {"payload": payload}})
                .to_string()
                .into_bytes(),
        ),
        (
            "SHA256SUMS".to_owned(),
            format!("{}  {image}\n", sha256(&tar)).into_bytes(),
        ),
        (image, tar),
    ]
    .into_iter()
    .collect();
    for (name, bytes) in &assets {
        std::fs::write(release.join(name), bytes).expect("write an asset");
    }
    let names: Vec<Value> = assets.keys().map(|n| json!({"name": n})).collect();
    let list = json!([{
        "tag_name": "v0.1.219", "target_commitish": COMMIT, "draft": false, "assets": names,
    }]);
    std::fs::write(
        bin.join("releases/komira-ai/hostkeeper/list.json"),
        list.to_string(),
    )
    .expect("write the listing");

    let unit = deploy_file("hostkeeper-coordinator-update.service");
    let exec = unit_value(&unit, "Service", "ExecStart").expect("the unit's ExecStart");
    let mut words = exec.split_whitespace().map(str::to_owned);
    assert_eq!(words.next().as_deref(), Some(INSTALLED));
    let mut args: Vec<String> = words.collect();
    let ca = root.join("coordinator-ca.pem");
    std::fs::write(&ca, "the CA").expect("write the CA");
    for (flag, with) in [("--download-dir", &downloads), ("--ca-file", &ca)] {
        let at = args
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("the committed command has no {flag}"));
        args[at + 1] = with.to_str().expect("UTF-8").to_owned();
    }
    Machine {
        _root: root,
        answers,
        downloads,
        bin,
        args,
        config,
    }
}

impl Machine {
    /// Docker's or the coordinator's answer to `method path`: `status`, then `body`.
    fn answer(&self, method: &str, path: &str, status: u16, body: &Value) {
        let key: String = format!("{method} {path}")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        std::fs::write(self.answers.join(key), format!("{status}\n{body}")).expect("an answer");
    }

    /// A machine whose coordinator container runs v0.1.218, from before the version route.
    fn running_old(&self) {
        self.answer(
            "GET",
            "/v1.43/containers/hostkeeper-coordinator/json",
            200,
            &json!({
                "Id": OLD_ID, "Image": "sha256:oldimage", "RestartCount": 0,
                "State": {"Running": true, "Restarting": false},
                "Config": {"Image": "hostkeeper-coordinator:0.1.218",
                           "Entrypoint": ["/usr/bin/hostkeeper-coordinator"],
                           "Cmd": ["serve", "--database-dsn-file", "/run/secrets/db-dsn"]},
                "HostConfig": {"Binds": ["state:/var/lib/hostkeeper-coordinator"],
                               "NetworkMode": "host",
                               "RestartPolicy": {"Name": "unless-stopped"}},
                "Mounts": [{"Type": "volume", "Name": "state",
                            "Destination": "/var/lib/hostkeeper-coordinator"}],
                "NetworkSettings": {"Networks": {"host": {}}},
            }),
        );
        self.answer(
            "GET",
            "/v1.43/images/sha256:oldimage/json",
            200,
            &json!({"Id": "sha256:oldimage", "RepoTags": ["hostkeeper-coordinator:0.1.218"],
                    "Config": {"Entrypoint": ["/usr/bin/hostkeeper-coordinator"]}}),
        );
    }

    /// Docker loads the release's image and swaps the containers.
    fn docker_swaps(&self, new_running: bool) {
        self.answer(
            "POST",
            "/v1.43/images/load",
            200,
            &json!({"stream": "Loaded image"}),
        );
        self.answer(
            "GET",
            "/v1.43/images/hostkeeper-coordinator:0.1.219/json",
            200,
            &json!({"Id": self.config}),
        );
        self.answer(
            "POST",
            "/v1.43/containers/create?name=hostkeeper-coordinator-next",
            201,
            &json!({"Id": NEW_ID}),
        );
        for (method, path) in [
            ("POST", format!("/v1.43/containers/{OLD_ID}/stop?t=30")),
            ("POST", format!("/v1.43/containers/{NEW_ID}/start")),
            ("POST", format!("/v1.43/containers/{NEW_ID}/stop?t=30")),
            ("POST", format!("/v1.43/containers/{OLD_ID}/start")),
            ("DELETE", format!("/v1.43/containers/{OLD_ID}")),
            ("DELETE", format!("/v1.43/containers/{NEW_ID}")),
            (
                "POST",
                format!("/v1.43/containers/{NEW_ID}/rename?name=hostkeeper-coordinator"),
            ),
        ] {
            self.answer(method, &path, 204, &json!(""));
        }
        self.answer(
            "GET",
            &format!("/v1.43/containers/{NEW_ID}/json"),
            200,
            &json!({"Id": NEW_ID, "RestartCount": 0,
                    "State": {"Running": new_running, "Restarting": false}}),
        );
        self.answer(
            "GET",
            &format!("/v1.43/containers/{OLD_ID}/json"),
            200,
            &json!({"Id": OLD_ID, "RestartCount": 0, "State": {"Running": true}}),
        );
    }

    fn run(&self) -> std::process::Output {
        Command::new(UPDATER)
            .args(&self.args)
            .env("PATH", &self.bin)
            .output()
            .expect("run the unit's command")
    }

    fn docker_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.answers.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn downloaded(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.downloads)
            .expect("the download directory")
            .map(|e| {
                e.expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

#[test]
fn the_committed_command_replaces_an_older_coordinator_with_the_release() {
    // Catches: a committed command that, run as the unit runs it, does not reach docker through
    // its socket, does not take the newest release, does not verify and load it, or does not
    // swap the containers and exit 0.
    let m = machine("cu-unit-update");
    m.running_old();
    m.docker_swaps(true);
    m.answer(
        "GET",
        "/v1/version?at_least=0.1.219",
        200,
        &json!({"version": "0.1.219"}),
    );
    let out = m.run();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(
            "coordinator-update: hostkeeper-coordinator now runs v0.1.219 (was v0.1.218); its command line, mounts and volumes are the old container's\n"
        ),
        "{stdout}"
    );
    assert_eq!(
        m.docker_calls(),
        [
            "GET /v1.43/containers/hostkeeper-coordinator/json".to_owned(),
            "GET /v1.43/containers/hostkeeper-coordinator-next/json".to_owned(),
            "GET /v1.43/images/sha256:oldimage/json".to_owned(),
            "GET /v1/version".to_owned(),
            "POST /v1.43/images/load".to_owned(),
            "GET /v1.43/images/hostkeeper-coordinator:0.1.219/json".to_owned(),
            "POST /v1.43/containers/create?name=hostkeeper-coordinator-next".to_owned(),
            format!("POST /v1.43/containers/{OLD_ID}/stop?t=30"),
            format!("POST /v1.43/containers/{NEW_ID}/start"),
            format!("GET /v1.43/containers/{NEW_ID}/json"),
            "GET /v1/version?at_least=0.1.219".to_owned(),
            format!("DELETE /v1.43/containers/{OLD_ID}"),
            format!("POST /v1.43/containers/{NEW_ID}/rename?name=hostkeeper-coordinator"),
        ]
    );
    assert_eq!(m.downloaded(), ["v0.1.219"]);
}

#[test]
fn the_committed_command_leaves_an_up_to_date_coordinator_alone() {
    // Catches: a committed command that changes or downloads anything when the coordinator
    // already runs the newest release.
    let m = machine("cu-unit-current");
    m.running_old();
    m.answer("GET", "/v1/version", 200, &json!({"version": "0.1.219"}));
    let out = m.run();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains(
        "coordinator-update: up to date: hostkeeper-coordinator runs v0.1.219, which is at least v0.1.219; nothing changed\n"
    ));
    assert_eq!(
        m.docker_calls(),
        [
            "GET /v1.43/containers/hostkeeper-coordinator/json",
            "GET /v1.43/containers/hostkeeper-coordinator-next/json",
            "GET /v1.43/images/sha256:oldimage/json",
            "GET /v1/version",
        ]
    );
    assert!(m.downloaded().is_empty());
}

#[test]
fn the_committed_command_rolls_back_a_coordinator_that_is_not_ready_and_marks_the_version() {
    // Catches: a committed command that, when the new container never runs, does not start the
    // old one again, exits 0, or leaves no marker so the timer would try the same release again.
    let mut m = machine("cu-unit-rollback");
    m.running_old();
    m.docker_swaps(false);
    let at = m
        .args
        .iter()
        .position(|a| a == "--download-dir")
        .expect("--download-dir");
    // One check instead of the default sixty, so the run never pauses; the rest is committed.
    m.args
        .splice(at..at, ["--ready-checks".to_owned(), "1".to_owned()]);
    let out = m.run();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "v0.1.219 did not become ready within 1 checks: the old container runs again"
        ),
        "{stderr}"
    );
    let calls = m.docker_calls();
    assert_eq!(
        calls[calls.len() - 5..],
        [
            format!("GET /v1.43/containers/{NEW_ID}/logs?stdout=1&stderr=1&tail=50"),
            format!("POST /v1.43/containers/{NEW_ID}/stop?t=30"),
            format!("DELETE /v1.43/containers/{NEW_ID}"),
            format!("POST /v1.43/containers/{OLD_ID}/start"),
            format!("GET /v1.43/containers/{OLD_ID}/json"),
        ]
    );
    assert!(!calls.iter().any(|c| c.contains("rename")), "{calls:?}");
    assert_eq!(m.downloaded(), ["failed-v0.1.219", "v0.1.219"]);
    assert_eq!(
        std::fs::read_to_string(m.downloads.join("failed-v0.1.219")).expect("the marker"),
        "v0.1.219 did not become ready within 1 checks: the old container runs again\n"
    );
}

/// The program's own output for `args`, run in `dir`: exit status, stdout, stderr.
fn program(dir: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(UPDATER)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run hostkeeper-coordinator-update");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_missing_download_directory_exits_1_and_a_url_that_is_not_https_exits_2() {
    // Catches: a main that does not pass the outcome through as the exit status, a missing
    // download directory made or run against (the run is refused before docker or GitHub is
    // asked anything), a refusal without the program's name, or a URL that is not https read as
    // a failure, not a usage error.
    let d = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "cu-unit-exit");
    let url = ["--coordinator-url", "https://coordinator.example.com:8444"];
    let (code, out, err) = program(
        &d,
        &[
            "--download-dir",
            "missing",
            url[0],
            url[1],
            "--ca-file",
            "ca.pem",
        ],
    );
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (
            Some(1),
            "",
            "hostkeeper-coordinator-update: --download-dir missing: not a directory\n"
        )
    );
    assert!(!d.join("missing").exists());
    let (code, out, err) = program(
        &d,
        &[
            "--download-dir",
            ".",
            "--coordinator-url",
            "http://coordinator.example.com:8444",
            "--ca-file",
            "ca.pem",
        ],
    );
    assert_eq!((code, out.as_str()), (Some(2), ""));
    assert!(err.contains("not an https://<host>[:<port>] URL"), "{err}");
}

#[test]
fn the_program_prints_its_stamp_for_version() {
    // Catches: `--version` not reaching the parser, going to stderr, or not carrying the stamp
    // `cargo xtask release check-binary` holds the release asset to: `hostkeeper-coordinator-update
    // <crate version>`, followed for a release build (one given RELEASE_COMMIT) by `+` and the
    // first 12 digits of that commit.
    let d = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "cu-unit-version");
    let (code, out, err) = program(&d, &["--version"]);
    let version = env!("CARGO_PKG_VERSION");
    let want = match option_env!("RELEASE_COMMIT") {
        Some(commit) => format!(
            "hostkeeper-coordinator-update {version}+{}\n",
            &commit[..12]
        ),
        None => format!("hostkeeper-coordinator-update {version}\n"),
    };
    assert_eq!((code, out, err), (Some(0), want, String::new()));
}
