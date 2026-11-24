//! Unit tests of `mac-farm`: its flags, the Command and Action it sends, the inputs it reads
//! from the checkout and the Cargo home, the programs it runs, the toolchain's download, the text
//! of a farm's failure and the summary. The runs against a farm are `farm_tests.rs` and
//! `outputs_tests.rs`.

use std::collections::BTreeMap;

use prost::Message as _;

use super::*;
use crate::release::ports::PortError;
use crate::testdir::TestDir;

pub(super) fn scratch(name: &str) -> TestDir {
    let beside = std::env::current_exe().expect("test binary");
    TestDir::under(
        &beside.parent().expect("deps").join("xtask-mac-farm-unit"),
        name,
    )
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

fn args() -> Args {
    Args {
        address: "grpc://farm.example:8980".to_owned(),
        instance: "main".to_owned(),
        tls: "true".to_owned(),
        properties: "os=macos,arch=arm64".to_owned(),
        scratch: PathBuf::from("scratch"),
        timeout_secs: 600,
        outputs: Vec::new(),
        env: Vec::new(),
        summary: None,
        ca_bundle: PathBuf::from(public::CA_BUNDLE),
        git: PathBuf::from("git"),
        cargo_program: PathBuf::from("cargo"),
        cargo: strings(&["test", "-p", "hello"]),
    }
}

/// Catches the scheme not following `--tls`, the properties or the outputs left in the order
/// given (REAPI wants both sorted), and a flag value dropped on the way to the settings.
#[test]
fn the_flags_make_the_settings() {
    let a = Args {
        outputs: strings(&["target/release/hk", "target/release/hello"]),
        env: strings(&["RELEASE_COMMIT=abc", "EMPTY="]),
        ..args()
    };
    assert_eq!(
        Settings::of(&a).expect("settings"),
        Settings {
            endpoint: "https://farm.example:8980".to_owned(),
            tls: true,
            instance: "main".to_owned(),
            properties: vec![
                ("arch".to_owned(), "arm64".to_owned()),
                ("os".to_owned(), "macos".to_owned()),
            ],
            timeout_secs: 600,
            cargo: strings(&["test", "-p", "hello"]),
            outputs: strings(&["target/release/hello", "target/release/hk"]),
            env: BTreeMap::from([
                ("EMPTY".to_owned(), String::new()),
                ("RELEASE_COMMIT".to_owned(), "abc".to_owned()),
            ]),
        }
    );
    let plain = Args {
        tls: "false".to_owned(),
        ..args()
    };
    assert_eq!(
        Settings::of(&plain).expect("settings").endpoint,
        "http://farm.example:8980"
    );
}

/// Catches each flag that is wrong taken anyway: an address that is not `grpc://<host>:<port>` (no
/// port, or one that is not a number), a TLS value that is neither word, properties that are
/// missing, not `key=value` or name a key twice, a zero timeout, and a Cargo command that is
/// empty or starts with an option (which `cargo` would read as its own, not the subcommand's)
/// or a `+toolchain` (the Mac has no rustup to pick one).
#[test]
fn a_wrong_flag_is_named() {
    for address in [
        "farm.example:1",
        "grpc://",
        "grpc://farm.example:1/x",
        "grpc://farm\tx.example:1",
        "grpc://farm.example",
        "grpc://farm.example:",
        "grpc://farm.example:80a",
    ] {
        let a = Args {
            address: address.to_owned(),
            ..args()
        };
        assert_eq!(Settings::of(&a), Err(Problem::Address(address.to_owned())));
    }
    let a = Args {
        tls: "yes".to_owned(),
        ..args()
    };
    assert_eq!(Settings::of(&a), Err(Problem::Tls("yes".to_owned())));
    for properties in [
        "",
        "os",
        "os=",
        "=macos",
        "os=macos,",
        "os=macos,,arch=arm64",
    ] {
        let a = Args {
            properties: properties.to_owned(),
            ..args()
        };
        assert_eq!(
            Settings::of(&a),
            Err(Problem::Properties(properties.to_owned())),
            "{properties:?}"
        );
    }
    let a = Args {
        properties: "os=macos, os = darwin".to_owned(),
        ..args()
    };
    assert_eq!(
        Settings::of(&a),
        Err(Problem::PropertyTwice("os".to_owned()))
    );
    let a = Args {
        timeout_secs: 0,
        ..args()
    };
    assert_eq!(Settings::of(&a), Err(Problem::NoTimeout));
    for (cargo, first) in [
        (strings(&["--locked", "test"]), "--locked"),
        (strings(&["+nightly", "test"]), "+nightly"),
        (Vec::new(), ""),
    ] {
        let a = Args { cargo, ..args() };
        assert_eq!(Settings::of(&a), Err(Problem::Cargo(first.to_owned())));
    }
}

/// Catches an output that is not where the command's tree can name it (absolute, with an empty,
/// `.` or `..` component, or in the run's own directory, where the toolchain is), and one given
/// twice (REAPI refuses a repeated output path); a name that merely starts like the run's own
/// directory is a plain path.
#[test]
fn a_wrong_output_is_named() {
    for path in [
        "",
        "/abs",
        "target//hk",
        "target/./hk",
        "../hk",
        "target/",
        ".mac-farm",
        ".mac-farm/toolchain/bin/cargo",
    ] {
        let a = Args {
            outputs: strings(&[path]),
            ..args()
        };
        assert_eq!(
            Settings::of(&a),
            Err(Problem::Output(path.to_owned())),
            "{path:?}"
        );
    }
    let a = Args {
        outputs: strings(&["target/hk", "target/hk"]),
        ..args()
    };
    assert_eq!(
        Settings::of(&a),
        Err(Problem::OutputTwice("target/hk".to_owned()))
    );
    let a = Args {
        outputs: strings(&[".mac-farmer/hk", "a/.mac-farm"]),
        ..args()
    };
    assert_eq!(
        Settings::of(&a).expect("plain").outputs,
        strings(&[".mac-farmer/hk", "a/.mac-farm"])
    );
}

/// Catches an `--env` that is not `NAME=VALUE` with a name a shell can set, one that would
/// replace a variable the verb sets itself (the run would no longer be offline, or would use
/// another compiler or Cargo home), and a name given twice (the Command's environment is a set
/// of names).
#[test]
fn a_wrong_env_is_named() {
    for pair in ["NOVALUE", "=x", "1A=x", "A-B=x", "A B=x"] {
        let a = Args {
            env: strings(&[pair]),
            ..args()
        };
        assert_eq!(
            Settings::of(&a),
            Err(Problem::Env(pair.to_owned())),
            "{pair:?}"
        );
    }
    for own in OWN_ENV {
        let a = Args {
            env: vec![format!("{own}=x")],
            ..args()
        };
        assert_eq!(Settings::of(&a), Err(Problem::EnvOwn(own.to_owned())));
    }
    let a = Args {
        env: strings(&["A=1", "A=2"]),
        ..args()
    };
    assert_eq!(Settings::of(&a), Err(Problem::EnvTwice("A".to_owned())));
    let a = Args {
        env: strings(&["_A1=x=y"]),
        ..args()
    };
    assert_eq!(
        Settings::of(&a).expect("ok").env,
        BTreeMap::from([("_A1".to_owned(), "x=y".to_owned())])
    );
}

/// Catches a Command that a Mac with no Rust and no network could not run: not the toolchain's
/// cargo, the arguments changed, the Cargo home or the compiler not the input root's, the
/// network not off, no path to the Mac's C compiler, an `--env` dropped, the environment out of
/// name order, or the outputs not its `output_paths`.
#[test]
fn the_command_is_the_sent_cargo_offline_with_the_sent_compiler() {
    let a = Args {
        env: strings(&["RELEASE_COMMIT=abc", "A=1"]),
        outputs: strings(&["target/release/hk", "target/release/hello"]),
        ..args()
    };
    let c = command(&Settings::of(&a).expect("settings"));
    assert_eq!(
        c.arguments,
        [".mac-farm/toolchain/bin/cargo", "test", "-p", "hello"]
    );
    let env: Vec<(&str, &str)> = c
        .environment_variables
        .iter()
        .map(|v| (v.name.as_str(), v.value.as_str()))
        .collect();
    assert_eq!(
        env,
        [
            ("A", "1"),
            ("CARGO_HOME", ".mac-farm/cargo-home"),
            ("CARGO_INCREMENTAL", "0"),
            ("CARGO_NET_OFFLINE", "true"),
            ("CARGO_TERM_COLOR", "always"),
            ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
            ("RELEASE_COMMIT", "abc"),
            ("RUSTC", ".mac-farm/toolchain/bin/rustc"),
            ("RUSTDOC", ".mac-farm/toolchain/bin/rustdoc"),
        ]
    );
    assert_eq!(c.working_directory, "");
    assert_eq!(
        c.output_paths,
        ["target/release/hello", "target/release/hk"]
    );
    let bare = command(&Settings::of(&args()).expect("settings"));
    assert!(bare.output_paths.is_empty());
}

/// Catches an Action that points at another Command or root, has no timeout or the wrong one,
/// or lacks the platform that sends it to a Mac; and its blobs not added for the upload.
#[test]
fn the_action_names_its_command_root_timeout_and_platform() {
    let s = Settings::of(&args()).expect("settings");
    let mut t = tree::build(Vec::new()).expect("tree");
    let root = t.root.clone();
    let digest = action(&s, &root, &mut t);
    let action = Action::decode(t.blobs[&digest.hash].as_slice()).expect("an Action");
    assert_eq!(action.input_root_digest, Some(root));
    let command_digest = action.command_digest.expect("command");
    let sent = Command::decode(t.blobs[&command_digest.hash].as_slice()).expect("a Command");
    assert_eq!(sent, command(&s));
    assert_eq!(action.timeout.map(|d| d.seconds), Some(600));
    let platform: Vec<(String, String)> = action
        .platform
        .expect("platform")
        .properties
        .into_iter()
        .map(|p| (p.name, p.value))
        .collect();
    assert_eq!(platform, s.properties);
    assert!(!action.do_not_cache);
}

/// Catches a tracked file read without its executable bit, a tracked symlink followed (its
/// target's bytes sent in its place), and a path read relative to the wrong directory.
#[test]
fn the_checkout_is_its_tracked_files_with_their_modes_and_symlinks() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("checkout");
    let root: &Path = &dir;
    std::fs::create_dir_all(root.join("tools")).expect("mkdir");
    std::fs::write(root.join("README.md"), "hi").expect("write");
    std::fs::write(root.join("tools/run"), "#!").expect("write");
    std::fs::set_permissions(
        root.join("tools/run"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("chmod");
    std::os::unix::fs::symlink("../README.md", root.join("tools/readme")).expect("symlink");
    let got =
        checkout(root, &strings(&["README.md", "tools/run", "tools/readme"])).expect("checkout");
    assert_eq!(
        got,
        vec![
            (
                "README.md".to_owned(),
                Entry::File {
                    bytes: b"hi".to_vec(),
                    executable: false
                }
            ),
            (
                "tools/run".to_owned(),
                Entry::File {
                    bytes: b"#!".to_vec(),
                    executable: true
                }
            ),
            (
                "tools/readme".to_owned(),
                Entry::Symlink("../README.md".to_owned())
            ),
        ]
    );
}

/// Catches a checkout that tracks the directory the run's own inputs go in (its files would
/// be replaced on the Mac), a tracked path that is gone, and a directory listed as a file.
#[test]
fn a_checkout_the_run_cannot_send_is_refused() {
    let dir = scratch("refused");
    let root: &Path = &dir;
    std::fs::create_dir_all(root.join("sub")).expect("mkdir");
    for ours in [".mac-farm", ".mac-farm/toolchain/bin/cargo"] {
        let refused = checkout(root, &[ours.to_owned()]).expect_err("refused");
        assert_eq!(refused, Error::Ours(ours.to_owned()));
        assert_eq!(
            refused.to_string(),
            format!("the checkout tracks {ours}, where the run puts its toolchain and crates")
        );
    }
    let gone = checkout(root, &["gone".to_owned()]).expect_err("gone");
    assert!(gone.to_string().starts_with("cannot read "), "{gone}");
    let not_file = checkout(root, &["sub".to_owned()]).expect_err("a directory");
    assert_eq!(
        not_file.to_string(),
        format!(
            "{} is neither a file nor a symlink",
            root.join("sub").display()
        )
    );
    assert!(checkout(root, &[".mac-farmer".to_owned()]).is_err());
}

/// Catches Cargo's locks, its cache tracker or installed programs sent (they change every run,
/// so no two runs would share an input root), a nested directory of the same name skipped,
/// and a file put anywhere but below the Cargo home of the input root.
#[test]
fn the_cargo_home_is_sent_without_its_locks_and_programs() {
    let dir = scratch("home");
    let home: &Path = &dir;
    for path in [
        "bin/cargo-x",
        ".package-cache",
        ".package-cache-mutate",
        ".global-cache",
        "registry/cache/index/a-1.0.0.crate",
        "registry/index/index/.cache/a",
        "git/db/bin/HEAD",
    ] {
        let path = home.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, path.display().to_string()).expect("write");
    }
    let got: Vec<String> = cargo_home(home)
        .expect("home")
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(
        got,
        [
            ".mac-farm/cargo-home/git/db/bin/HEAD",
            ".mac-farm/cargo-home/registry/cache/index/a-1.0.0.crate",
            ".mac-farm/cargo-home/registry/index/index/.cache/a",
        ]
    );
    let missing = cargo_home(&home.join("gone")).expect_err("missing");
    assert!(missing.to_string().starts_with("cannot list "), "{missing}");
}

/// Catches a farm failure printed without the job's output, an output of another reason
/// printed as the job's, and a detail that does not decode stopping the others.
#[test]
fn the_output_tail_of_a_failed_job_is_printed() {
    let info = |reason: &str, pairs: &[(&str, &str)]| prost_types::Any {
        type_url: "type.googleapis.com/google.rpc.ErrorInfo".to_owned(),
        value: rpc::ErrorInfo {
            reason: reason.to_owned(),
            metadata: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..rpc::ErrorInfo::default()
        }
        .encode_to_vec(),
    };
    let status = rpc::Status {
        code: 14,
        message: "the job was lost".to_owned(),
        details: vec![
            prost_types::Any {
                type_url: "junk".to_owned(),
                value: vec![0xff],
            },
            info("OTHER", &[("stdout", "not this")]),
            info(
                "JOB_OUTPUT_TAIL",
                &[("stdout", "out end"), ("stderr", "err end")],
            ),
            info("JOB_OUTPUT_TAIL", &[("truncated", "true")]),
        ],
    };
    assert_eq!(
        tail(&status),
        "--- the job's stdout (its end) ---\nout end\n--- the job's stderr (its end) ---\nerr end\n"
    );
    assert_eq!(tail(&rpc::Status::default()), "");
}

/// Catches git asked for anything but the NUL-separated list of the checkout's tracked files, in
/// the checkout; Cargo's fetch not for the Mac's target, not locked, not into the run's own Cargo
/// home or not from the checkout; and the list split wrong (an empty entry kept, a path with a
/// space cut).
#[test]
fn git_and_cargo_are_asked_for_the_checkout_and_the_macs_crates() {
    let root = Path::new("/checkout");
    let git = ls_files(Path::new("my-git"), root);
    assert_eq!(git.get_program(), "my-git");
    assert_eq!(git.get_args().collect::<Vec<_>>(), ["ls-files", "-z"]);
    assert_eq!(git.get_current_dir(), Some(root));
    let cargo = fetch(Path::new("my-cargo"), root, Path::new("/scratch/home"));
    assert_eq!(cargo.get_program(), "my-cargo");
    assert_eq!(
        cargo.get_args().collect::<Vec<_>>(),
        ["fetch", "--locked", "--target", "aarch64-apple-darwin"]
    );
    assert_eq!(
        cargo.get_envs().collect::<Vec<_>>(),
        [(
            std::ffi::OsStr::new("CARGO_HOME"),
            Some(std::ffi::OsStr::new("/scratch/home"))
        )]
    );
    assert_eq!(cargo.get_current_dir(), Some(root));
    assert_eq!(split_nul("a\0b c\0\0"), ["a", "b c"]);
    assert!(split_nul("").is_empty());
}

/// Catches a program that failed read as an empty success, its stderr not in the error (trimmed
/// of its last newline), a success whose output is dropped, and one that cannot start taken as
/// an empty list or a fetch that worked. This test's own binary stands in for git and Cargo: it
/// refuses their arguments as options it does not know.
#[test]
fn the_programs_it_runs_are_read_or_named_with_their_stderr() {
    let me = std::env::current_exe().expect("this test binary");
    let dir = scratch("programs");
    let listed = program(
        "list",
        Process::new(&me).args(["--list", "--exact", "no_such_test"]),
    );
    assert_eq!(listed, Ok("0 tests, 0 benchmarks\n".to_owned()));
    assert_eq!(
        git_files(&me, &dir),
        Err(Error::Program {
            what: "git ls-files".to_owned(),
            stderr: "error: Unrecognized option: 'z'".to_owned(),
        })
    );
    assert_eq!(
        fetch_crates(&me, &dir, &dir.join("home")),
        Err(Error::Program {
            what: "cargo fetch".to_owned(),
            stderr: "error: Unrecognized option: 'locked'".to_owned(),
        })
    );
    let absent = git_files(&dir.join("absent"), &dir).expect_err("cannot start");
    assert!(
        absent.to_string().starts_with("cannot run git ls-files: "),
        "{absent}"
    );
}

/// Catches a stage printed as the other one: the log must say when the run waits for a Mac.
#[test]
fn each_stage_prints_its_own_line() {
    assert_eq!(
        progress_line(Progress::Queued),
        "mac-farm: queued: waiting for a free Mac"
    );
    assert_eq!(
        progress_line(Progress::Executing),
        "mac-farm: running on a farm Mac"
    );
}

/// Catches a run with no farm configured (a fork, a repository with no farm) that fails or
/// tries to connect instead of saying so and passing.
#[test]
fn no_address_runs_nothing_and_says_so() {
    let a = Args {
        address: "  ".to_owned(),
        ..args()
    };
    assert_eq!(
        run(&a).expect("a notice"),
        ["::notice::mac-farm runs nothing: the variable FARM_ADDRESS is not set"]
    );
}

/// Catches a run that goes on without the roots its downloads trust: an unreadable
/// `--ca-bundle` ends it before anything is read or sent.
#[test]
fn an_unreadable_ca_bundle_ends_the_run() {
    let dir = scratch("bundle");
    let a = Args {
        ca_bundle: dir.join("absent.pem"),
        ..args()
    };
    let failed = run(&a).expect_err("no roots");
    assert!(
        failed.to_string().starts_with(&format!(
            "cannot read the roots of --ca-bundle {}: ",
            a.ca_bundle.display()
        )),
        "{failed}"
    );
}

/// A [`Web`] that answers every GET with one status and body, and records what was asked.
struct OneAnswer {
    status: u16,
    body: Vec<u8>,
    fail: bool,
    asked: std::sync::Mutex<Vec<(String, String, Option<u64>)>>,
}

impl Web for OneAnswer {
    fn get(&self, get: &Get<'_>, body: &mut dyn Write) -> Result<public::Head, PortError> {
        self.asked.lock().expect("asked").push((
            get.url.to_owned(),
            get.accept.to_owned(),
            get.limit,
        ));
        if self.fail {
            return Err(PortError::new(format!("GET {}", get.url), "refused"));
        }
        if self.status == 200 {
            body.write_all(&self.body).expect("body");
        }
        Ok(public::Head {
            status: self.status,
            ..public::Head::default()
        })
    }
}

fn web(status: u16, fail: bool) -> OneAnswer {
    OneAnswer {
        status,
        body: b"tarball".to_vec(),
        fail,
        asked: std::sync::Mutex::new(Vec::new()),
    }
}

/// Catches a tarball read with no size limit, an answer that is not 200 (a 404 page) taken as the
/// tarball, and a request that got no answer taken as an empty tarball.
#[test]
fn a_toolchain_tarball_is_an_answer_of_200_under_its_limit() {
    let url = "https://static.example/rustc.tar.xz";
    let ok = web(200, false);
    assert_eq!(tarball(&ok, url), Ok(b"tarball".to_vec()));
    assert_eq!(
        ok.asked.lock().expect("asked")[..],
        [(
            url.to_owned(),
            "application/octet-stream".to_owned(),
            Some(MAX_TARBALL_BYTES)
        )]
    );
    assert_eq!(
        tarball(&web(404, false), url),
        Err(format!("GET {url}: HTTP 404"))
    );
    assert_eq!(
        tarball(&web(200, true), url),
        Err(format!("GET {url}: refused"))
    );
}

/// Catches a summary that leaves out the command, what was sent, a pass's lines or a failure's
/// reason: it is the job's record of the run.
#[test]
fn the_summary_says_the_command_what_was_sent_and_how_it_ended() {
    let s = Settings::of(&args()).expect("settings");
    let facts = strings(&["mac-farm: 3 blobs, 1 missing from the farm (9 bytes to upload)"]);
    let passed = Ok(strings(&[
        "ok: passed",
        "mac-farm: wrote the output a (1 bytes)",
    ]));
    assert_eq!(
        summary(&s, &facts, &passed),
        "### mac-farm: `cargo test -p hello`\n\n\
         - mac-farm: 3 blobs, 1 missing from the farm (9 bytes to upload)\n\
         - ok: passed\n\
         - mac-farm: wrote the output a (1 bytes)\n\n"
    );
    let failed = Err(Error::NoResult("test -p hello".to_owned()));
    assert_eq!(
        summary(&s, &[], &failed),
        "### mac-farm: `cargo test -p hello`\n\n\
         - failed: the farm answered `cargo test -p hello` with no result\n\n"
    );
}

/// Catches the blobs of the inputs and the action losing one another: every digest the action
/// names, through the root, is a blob of the run.
#[test]
fn the_inputs_hold_every_blob_the_action_names() {
    let dir = scratch("inputs");
    let root = dir.join("checkout");
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(root.join("Cargo.toml"), "[workspace]").expect("write");
    let xz = super::farm_tests::toolchain_xz();
    let mut tracked = || Ok(strings(&["Cargo.toml"]));
    let mut fetch =
        |home: &Path| std::fs::write(home.join("registry"), "r").map_err(io("write".to_owned()));
    let mut download = |_: &str| Ok(xz.clone());
    let mut host = Host {
        tracked: &mut tracked,
        fetch: &mut fetch,
        pins: super::farm_tests::pins(&xz),
        download: &mut download,
    };
    let s = Settings::of(&args()).expect("settings");
    let (t, action) = inputs(&s, &root, &dir.join("scratch"), &mut host).expect("inputs");
    let mut names = BTreeMap::new();
    let mut todo = vec![(String::new(), t.root.clone())];
    while let Some((at, digest)) = todo.pop() {
        let d =
            reapi::v2::Directory::decode(t.blobs[&digest.hash].as_slice()).expect("a Directory");
        for f in d.files {
            let hash = f.digest.expect("digest").hash;
            assert!(t.blobs.contains_key(&hash), "{at}/{}", f.name);
            names.insert(format!("{at}/{}", f.name), hash);
        }
        for sub in d.directories {
            todo.push((format!("{at}/{}", sub.name), sub.digest.expect("digest")));
        }
    }
    let paths: Vec<&str> = names.keys().map(String::as_str).collect();
    assert_eq!(
        paths,
        [
            "/.mac-farm/cargo-home/registry",
            "/.mac-farm/toolchain/bin/cargo",
            "/Cargo.toml"
        ]
    );
    assert!(t.blobs.contains_key(&action.hash));
}

/// Catches an input the run could not gather taken as an empty one: a git that fails, a fetch
/// that fails, a toolchain that is not its pin and a scratch whose Cargo home cannot be made
/// each end the run with their own error before the farm is called.
#[test]
fn an_input_that_cannot_be_gathered_ends_the_run() {
    let dir = scratch("gather");
    let root = dir.join("checkout");
    std::fs::create_dir_all(&root).expect("mkdir");
    std::fs::write(root.join("Cargo.toml"), "[workspace]").expect("write");
    let xz = super::farm_tests::toolchain_xz();
    let s = Settings::of(&args()).expect("settings");
    let failed = |what: &str| Error::Program {
        what: what.to_owned(),
        stderr: "refused".to_owned(),
    };
    let gather = |tracked_ok: bool, fetch_ok: bool, sha: &[u8], scratch: &Path| {
        let mut tracked = || {
            if tracked_ok {
                Ok(strings(&["Cargo.toml"]))
            } else {
                Err(failed("git ls-files"))
            }
        };
        let mut fetch = |_: &Path| {
            if fetch_ok {
                Ok(())
            } else {
                Err(failed("cargo fetch"))
            }
        };
        let mut download = |_: &str| Ok(xz.clone());
        let mut host = Host {
            tracked: &mut tracked,
            fetch: &mut fetch,
            pins: super::farm_tests::pins(sha),
            download: &mut download,
        };
        inputs(&s, &root, scratch, &mut host).map(|_| ())
    };
    let scratch_dir = dir.join("scratch");
    assert_eq!(gather(true, true, &xz, &scratch_dir), Ok(()));
    assert_eq!(
        gather(false, true, &xz, &scratch_dir),
        Err(failed("git ls-files"))
    );
    assert_eq!(
        gather(true, false, &xz, &scratch_dir),
        Err(failed("cargo fetch"))
    );
    assert!(matches!(
        gather(true, true, b"other", &scratch_dir),
        Err(Error::Toolchain(ToolchainError::Sha256 { .. }))
    ));
    let a_file = dir.join("a-file");
    std::fs::write(&a_file, "").expect("write");
    let made = gather(true, true, &xz, &a_file).expect_err("not a directory");
    assert!(made.to_string().starts_with("cannot create "), "{made}");
}
