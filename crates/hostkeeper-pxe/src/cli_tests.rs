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
    let status = run(argv, fetch, &mut out, &mut err);
    (
        status,
        String::from_utf8(out).expect("UTF-8"),
        String::from_utf8(err).expect("UTF-8"),
    )
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

/// The program itself runs the fetch with root as the owner it trusts: a daemon config that is
/// not there is refused before anything is made.
///
/// Catches a `main` that never reaches the verb.
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

/// The flags reach the fetch as given, with both platforms when none is named and each named
/// one otherwise, and a sealed directory is one line on standard output naming it, the release,
/// the Ubuntu release and the platforms.
///
/// Catches a `--platform` that is dropped, and a success that prints nothing to say where the
/// boot files are.
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
