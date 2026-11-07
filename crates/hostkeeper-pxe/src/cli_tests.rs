//! The command line: what parses, the exit status and the one line of each outcome, and the
//! host's ports made from the flags and reaching the release. The release is a TLS-less
//! listener on a free loopback port that closes each connection, so the real HTTPS client is
//! refused at once; nothing leaves the machine.

use std::collections::BTreeMap;
use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::thread;

use hostkeeper_testkit::Scratch;

use super::*;
use crate::seal::{SealedNetboot, SealedSet};
use crate::testing::scratch;

/// Runs the command line `args` (after the program name) through `fetch`; the exit status,
/// standard output and standard error.
fn run_with(args: &[&str], fetch: Fetch<'_>) -> (u8, String, String) {
    let mut argv: Vec<OsString> = vec!["hostkeeper-pxe".into()];
    argv.extend(args.iter().map(OsString::from));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status = run(argv, fetch, &unreached_serve, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("UTF-8"),
        String::from_utf8(err).expect("UTF-8"),
    )
}

/// A serve the fetch tests do not expect to be reached.
fn unreached_serve(_: &ServeFlags) -> Result<(), String> {
    panic!("serve is not reached")
}

/// A fetch the tests do not expect to be reached.
fn unreached(_: &FetchFlags) -> Result<Seal, String> {
    panic!("the fetch is not reached")
}

/// The files of a command run: a daemon config owned by the test's own user, a CA bundle, and
/// the path of the new directory.
struct Files {
    scratch: Scratch,
    owner: u32,
}

impl Files {
    fn new(name: &str) -> Files {
        let scratch = scratch(name);
        let config = scratch.path().join("hostkeeper.toml");
        fs::write(
            &config,
            "coordinator = \"https://coordinator.example:8443\"\n\
             ca_sha256 = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\
             release_channel = \"https://releases.example\"\n\
             [trusted_signers]\n\
             organisation = \"example-org\"\n\
             workflow = \".github/workflows/artifacts.yml\"\n\
             refs = [\"refs/heads/main\"]\n\
             hosted_runner = true\n",
        )
        .expect("write the config");
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).expect("mode 0600");
        let owner = fs::metadata(&config).expect("the config").uid();
        let ca = rcgen::generate_simple_self_signed(vec!["ca.example".to_owned()])
            .expect("a CA certificate");
        fs::write(scratch.path().join("ca.pem"), ca.cert.pem()).expect("write the CA bundle");
        Files { scratch, owner }
    }

    fn path(&self, name: &str) -> String {
        self.scratch.path().join(name).display().to_string()
    }
}

/// A command line that does not parse exits 2 and says why on standard error: no verb, no
/// `--release`, a release that is not `https://` or has a query, a platform that has no boot
/// set, an unknown flag. The fetch is never reached.
///
/// Catches a fetch started from a plain-HTTP release or for a Mac.
#[test]
fn a_command_line_that_does_not_parse_exits_2() {
    let fetch = ["fetch", "--dir", "d", "--daemon-config", "c"];
    let cases: [(Vec<&str>, &str); 6] = [
        (vec![], "Usage: hostkeeper-pxe <COMMAND>"),
        (fetch.to_vec(), "--release <RELEASE>"),
        (
            [&fetch[..], &["--release", "http://releases.example/v1"]].concat(),
            "not an https:// URL without a query or fragment",
        ),
        (
            [
                &fetch[..],
                &["--release", "https://releases.example/v1?x=1"],
            ]
            .concat(),
            "not an https:// URL without a query or fragment",
        ),
        (
            [
                &fetch[..],
                &[
                    "--release",
                    "https://r.example",
                    "--platform",
                    "macos-arm64",
                ],
            ]
            .concat(),
            "\"macos-arm64\" is not a platform with a boot set: linux-x86_64 or linux-arm64",
        ),
        (
            [
                &fetch[..],
                &["--release", "https://r.example", "--mode", "proxy"],
            ]
            .concat(),
            "unexpected argument '--mode'",
        ),
    ];
    for (args, text) in cases {
        let (status, out, err) = run_with(&args, &unreached);
        assert_eq!(status, 2, "{args:?}");
        assert!(out.is_empty(), "{args:?}");
        assert!(err.contains(text), "{args:?}: {err}");
    }
}

/// `--help` and `--version` go to standard output and exit 0; the help names the verb.
///
/// Catches help printed as an error.
#[test]
fn help_goes_to_standard_output() {
    let (status, out, err) = run_with(&["--help"], &unreached);
    assert_eq!((status, err.as_str()), (0, ""));
    assert!(out.contains("fetch"), "{out}");
    let (status, out, _) = run_with(&["fetch", "--help"], &unreached);
    assert_eq!(status, 0);
    for flag in [
        "--release",
        "--dir",
        "--daemon-config",
        "--platform",
        "--ca-bundle",
        "--gh",
        "--trusted-root",
        "--download-timeout-secs",
    ] {
        assert!(out.contains(flag), "{flag}: {out}");
    }
    let _ = main(vec!["hostkeeper-pxe".into(), "--version".into()]);
}

/// The program itself runs the fetch with root as the owner it trusts; a daemon config that is
/// not there is refused before anything is made. (Its exit status and line are asserted on the
/// binary, in `tests/binary.rs`.)
#[test]
fn the_program_runs_the_verb() {
    let files = Files::new("main");
    let dir = files.path("boot");
    let missing = files.path("missing.toml");
    let args = [
        "hostkeeper-pxe",
        "fetch",
        "--release",
        "https://r.example",
        "--dir",
        &dir,
    ];
    let mut argv: Vec<OsString> = args.iter().map(OsString::from).collect();
    argv.extend(["--daemon-config".into(), missing.into()]);
    let _ = main(argv);
    assert!(!std::path::Path::new(&dir).exists());
}

/// A sealed directory is one line on standard output naming it, the release, the Ubuntu release
/// and the platforms sealed.
///
/// Catches a success that prints nothing to say where the boot files are.
#[test]
fn a_sealed_fetch_prints_one_line_naming_the_directory() {
    let seal = |flags: &FetchFlags| {
        let platforms: BTreeMap<String, SealedSet> = BTreeMap::new();
        let names: Vec<String> = flags.platforms.iter().map(|p| p.to_string()).collect();
        let mut seal = Seal {
            netboot: SealedNetboot {
                commit: String::new(),
                sha256: String::new(),
                source_ref: String::new(),
            },
            platforms,
            seal: 1,
            ubuntu: "24.04.5".to_owned(),
            version: "1.2.3".to_owned(),
        };
        let listed = match names.is_empty() {
            true => vec!["linux-arm64".to_owned(), "linux-x86_64".to_owned()],
            false => names,
        };
        for name in listed {
            seal.platforms.insert(name, fake_set());
        }
        Ok(seal)
    };
    let base = [
        "fetch",
        "--release",
        "https://r.example/v1.2.3",
        "--dir",
        "/srv/boot",
        "--daemon-config",
        "c",
    ];
    let (status, out, err) = run_with(&base, &seal);
    assert_eq!((status, err.as_str()), (0, ""));
    assert_eq!(
        out,
        "sealed /srv/boot: hostkeeper 1.2.3, Ubuntu 24.04.5, linux-arm64 and linux-x86_64\n"
    );
    let (status, out, _) = run_with(
        &[&base[..], &["--platform", "linux-x86_64"]].concat(),
        &seal,
    );
    assert_eq!(status, 0);
    assert_eq!(
        out,
        "sealed /srv/boot: hostkeeper 1.2.3, Ubuntu 24.04.5, linux-x86_64\n"
    );
}

fn fake_set() -> SealedSet {
    let file = crate::seal::SealedFile {
        bytes: 1,
        name: String::new(),
        sha256: String::new(),
    };
    SealedSet {
        deb: crate::seal::SealedDeb {
            bytes: 1,
            commit: String::new(),
            name: String::new(),
            sha256: String::new(),
            source_ref: String::new(),
        },
        grub: file.clone(),
        initrd: file.clone(),
        iso: file.clone(),
        kernel: file.clone(),
        shim: file,
    }
}

/// A daemon config the daemon's loader refuses (here: none at the path) is refused with the
/// loader's error, exit 1; so is a CA bundle with no certificate. Nothing is made.
///
/// Catches a fetch run without the operator's trusted signer, or with no root to trust.
#[test]
fn a_config_or_ca_bundle_that_does_not_load_is_refused() {
    let files = Files::new("config");
    let dir = files.path("boot");
    let missing = files.path("missing.toml");
    let (status, _, err) = run_with(
        &[
            "fetch",
            "--release",
            "https://r.example",
            "--dir",
            &dir,
            "--daemon-config",
            &missing,
        ],
        &|flags| fetch_with(flags, files.owner),
    );
    assert_eq!(status, 1);
    assert!(
        err.starts_with(&format!("hostkeeper-pxe fetch: {missing}: ")),
        "{err}"
    );
    let config = files.path("hostkeeper.toml");
    let empty = files.path("empty.pem");
    fs::write(&empty, "").expect("an empty bundle");
    let (status, _, err) = run_with(
        &[
            "fetch",
            "--release",
            "https://r.example",
            "--dir",
            &dir,
            "--daemon-config",
            &config,
            "--ca-bundle",
            &empty,
        ],
        &|flags| fetch_with(flags, files.owner),
    );
    assert_eq!(status, 1);
    assert_eq!(
        err,
        format!("hostkeeper-pxe fetch: --ca-bundle {empty}: no certificate could be read\n")
    );
    assert!(!std::path::Path::new(&dir).exists());
}

/// With a loadable config and CA bundle, the host's ports are made and the fetch asks the
/// release for `netboot.json` over HTTPS; a release that closes the connection is refused
/// naming the URL, exit 1, and the new directory is gone; with and without `--platform`.
///
/// Catches flags that never reach the real HTTPS client, and a refused run that leaves a
/// half-made directory.
#[test]
fn the_host_ports_ask_the_release_and_a_refusal_keeps_nothing() {
    let files = Files::new("ports");
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener.local_addr().expect("its address").port();
    let closer = thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            drop(stream);
        }
    });
    let release = format!("https://127.0.0.1:{port}/v1.2.3");
    let dir = files.path("boot");
    let config = files.path("hostkeeper.toml");
    let ca = files.path("ca.pem");
    for extra in [&[][..], &["--platform", "linux-arm64"][..]] {
        let mut args = vec![
            "fetch",
            "--release",
            &release,
            "--dir",
            &dir,
            "--daemon-config",
            &config,
            "--ca-bundle",
            &ca,
        ];
        args.extend_from_slice(extra);
        let (status, out, err) = run_with(&args, &|flags| fetch_with(flags, files.owner));
        assert_eq!((status, out.as_str()), (1, ""));
        assert_eq!(
            err,
            format!(
                "hostkeeper-pxe fetch: {release}/netboot.json: could not connect, or the request \
                 was not answered\n"
            )
        );
        assert!(!std::path::Path::new(&dir).exists());
    }
    closer.join().expect("the listener");
}

/// No `--platform` fetches both platforms; each `--platform` named is fetched, and only those;
/// a platform named twice is fetched once.
///
/// Catches a farm of one architecture made to download both, and a default of one platform.
#[test]
fn both_platforms_unless_platform_names_some() {
    let (arm, x86) = (BootPlatform::LinuxArm64, BootPlatform::LinuxX86_64);
    assert_eq!(platforms_of(&[]), BTreeSet::from([arm, x86]));
    assert_eq!(platforms_of(&[x86]), BTreeSet::from([x86]));
    assert_eq!(platforms_of(&[arm, arm]), BTreeSet::from([arm]));
    let parsed = Cli::try_parse_from([
        "hostkeeper-pxe",
        "fetch",
        "--release",
        "https://r.example",
        "--dir",
        "d",
        "--daemon-config",
        "c",
        "--platform",
        "linux-arm64",
        "--platform",
        "linux-x86_64",
    ])
    .expect("parses");
    let Verb::Fetch(flags) = parsed.verb else {
        panic!("a fetch")
    };
    assert_eq!(flags.platforms, [arm, x86]);
}

/// A `gh` that records the one run it is asked for and answers yes.
struct Recording(std::rc::Rc<std::cell::RefCell<Vec<hostkeeper_package::gh::Spawn>>>);

impl Runner for Recording {
    fn run(
        &self,
        spawn: &hostkeeper_package::gh::Spawn,
    ) -> io::Result<hostkeeper_package::gh::Ran> {
        self.0.borrow_mut().push(spawn.clone());
        Ok(hostkeeper_package::gh::Ran {
            code: Some(0),
            ..Default::default()
        })
    }
}

/// The flags of a `fetch` with `extra` after the required ones.
fn fetch_flags(extra: &[&str]) -> FetchFlags {
    let mut args = vec![
        "hostkeeper-pxe",
        "fetch",
        "--release",
        "https://r.example/v1.2.3",
        "--dir",
        "/srv/boot",
        "--daemon-config",
        "c",
    ];
    args.extend_from_slice(extra);
    let Verb::Fetch(flags) = Cli::try_parse_from(args).expect("parses").verb else {
        panic!("a fetch")
    };
    flags
}

/// The verifier the flags make runs the `gh` of `--gh`, passes the root of `--trusted-root` as
/// `--custom-trusted-root`, and runs in `gh`'s home under `--dir`; without the flags, the paths
/// the hostkeeper deb installs.
///
/// Catches a `--gh` or `--trusted-root` that is parsed and then ignored, so the attestations
/// would be checked by another `gh` or against another root than the operator named.
#[test]
fn the_verifier_runs_the_gh_and_trusted_root_the_flags_name() {
    use hostkeeper_package::ports::{Attest, CommitId, SourceRef, Verifier};
    for (extra, gh, root) in [
        (
            vec![
                "--gh",
                "/opt/pinned/gh",
                "--trusted-root",
                "/opt/pinned/root.json",
            ],
            "/opt/pinned/gh",
            "/opt/pinned/root.json",
        ),
        (vec![], GH, TRUSTED_ROOT),
    ] {
        let flags = fetch_flags(&extra);
        let runs = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let verifier = verifier_of(
            &flags,
            Box::new(Recording(std::rc::Rc::clone(&runs))),
            Box::new(crate::testing::bundle::TrustAll),
        );
        let clock = crate::testing::FakeClock::at(5);
        let signers = crate::testing::bundle::signers();
        let sha = hostkeeper_config::daemon::Sha256Hex::parse(&"a".repeat(64)).expect("a digest");
        let commit = CommitId::new(crate::testing::bundle::COMMIT).expect("a commit");
        let source_ref = SourceRef::new("refs/heads/main").expect("a ref");
        let deadline = hostkeeper_steps::Stamp::now(&clock);
        let request = Attest {
            archive: Path::new("/srv/boot/netboot.json"),
            bundle: Path::new("/srv/boot/.verify/netboot.json.sigstore.json"),
            sha256: &sha,
            commit: &commit,
            source_ref: &source_ref,
            signers: &signers,
            deadline: &deadline,
        };
        verifier
            .verify(&request)
            .expect("the recording gh says yes");
        let runs = runs.borrow();
        let spawn = runs.first().expect("gh ran once");
        assert_eq!(runs.len(), 1);
        assert_eq!(spawn.program, Path::new(gh));
        assert_eq!(spawn.cwd, Path::new("/srv/boot/.verify/gh"));
        let args: Vec<String> = spawn
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = args
            .iter()
            .position(|a| a == "--custom-trusted-root")
            .expect("the root flag");
        assert_eq!(args[at + 1], root);
    }
}

/// The request the flags and the daemon config make: `--download-timeout-secs` is the total
/// bound of each download (four hours without it) beside the 30 s connect bound, and the
/// config's trusted signer, attestation bound and package bound are passed as they are.
///
/// Catches a `--download-timeout-secs` that is parsed and then ignored, and a config bound
/// passed to the wrong field.
#[test]
fn the_request_carries_the_flags_and_the_config_bounds() {
    let config = DaemonConfig::parse(
        "coordinator = \"https://coordinator.example:8443\"\n\
         ca_sha256 = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\
         release_channel = \"https://releases.example\"\n\
         [trusted_signers]\n\
         organisation = \"example-org\"\n\
         workflow = \".github/workflows/artifacts.yml\"\n\
         refs = [\"refs/heads/main\"]\n\
         hosted_runner = true\n\
         [bounds]\n\
         max_package_bytes = 1048576\n\
         fetch_timeout_secs = 2100\n\
         attestation_timeout_secs = 45\n",
    )
    .expect("a config");
    for (extra, total) in [
        (vec!["--download-timeout-secs", "123"], 123),
        (vec![], DOWNLOAD_TIMEOUT_SECS),
    ] {
        let request = request_of(&fetch_flags(&extra), config.clone());
        assert_eq!(
            request,
            Request {
                release: "https://r.example/v1.2.3".to_owned(),
                dir: PathBuf::from("/srv/boot"),
                platforms: BTreeSet::from([BootPlatform::LinuxArm64, BootPlatform::LinuxX86_64]),
                signers: crate::testing::bundle::signers(),
                attestation_timeout_secs: 45,
                max_deb_bytes: 1_048_576,
                timeouts: Timeouts {
                    connect: Duration::from_secs(30),
                    total: Duration::from_secs(total),
                },
            }
        );
    }
}
