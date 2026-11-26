//! `hk xcode verify` against stand-ins for the Mac's programs ([`crate::verify_rig`]): the record
//! it prints and keeps, the arguments each program gets, its usage errors and refusals, the
//! scratch directory it leaves no trace of, and its help. The verdicts of each check are in
//! `xcode_verify_checks`.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use serde_json::Value;

use crate::rig::{Opts, Rig, args};
use crate::verify_rig::{ARCHIVE, Proof, expected, sha256};

/// Proves: over stand-ins for an archive Apple signed, `hk xcode verify` exits 0 and prints the
/// record by exact equality: the macOS, every program and whether its path is a file (the
/// optional three apart, `xcodes` absent without failing), the archive's name, size, SHA-256 and
/// kind, the signature's exit, streams, status, chain and Apple's root with its fingerprint and
/// subject beside the constant, the two damaged files and what pkgutil and xip said of each,
/// the table of contents, and the expansion with the free space before and after, the seconds
/// read from the clock, the one app, its contents, the version and the build, and codesign and
/// spctl.
#[test]
fn a_good_archive_passes_with_the_whole_record_printed() {
    let proof = Proof::new("good");
    let archive = proof.archive(ARCHIVE);
    let ran = proof.verify(&archive);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(Proof::printed(&ran), expected(&proof));
    assert!(
        ran.stderr.contains("verdict pass; record kept in "),
        "{ran:?}"
    );
    // A global flag may follow the archive, as it may follow any word of a command line.
    let root = proof.rig.path("apple-root.cer").display().to_string();
    let again = proof.run(&[
        "xcode",
        "verify",
        &archive,
        "--timeout-secs=5",
        "--apple-root",
        &root,
    ]);
    assert_eq!(again.code, 0, "{again:?}");
    assert_eq!(Proof::printed(&again)["verdict"], "pass");
}

/// Proves: the programs are run as the runbook runs them, in the directories the design names:
/// `pkgutil --check-signature <archive>` on the archive, the clone made by `cp -c` into the
/// scratch directory, `xip --expand` on the two damaged files and the archive, each in a
/// directory of the scratch directory, `xip --list`, `xar -tf`, `xar --dump-toc=<file> -f`,
/// `xar -xf <archive> Metadata`, `plutil -extract` of the two keys, `codesign --verify --deep
/// --strict` and `spctl --assess --type execute -vv` of the app, `df -Pk` of the scratch
/// directories, `sw_vers` and `uname -sm`.
#[test]
fn each_program_gets_the_arguments_of_the_runbook() {
    let proof = Proof::new("argv");
    let archive = proof.archive(ARCHIVE);
    assert_eq!(proof.verify(&archive).code, 0);
    let archive = std::path::absolute(&archive).expect("absolute");
    let a = archive.display();
    let scratch = proof.rig.path("downloads/.hk-xcode-verify-scratch");
    let s = scratch.display();
    assert_eq!(
        proof.recorded("pkgutil.args"),
        format!(
            "--check-signature\t{a}\n--check-signature\t{s}/damaged.xip\n\
             --check-signature\t{s}/non-archive.xip\n"
        )
    );
    assert_eq!(
        proof.recorded("cp.args"),
        format!("-c\t{a}\t{s}/damaged.xip\n")
    );
    assert_eq!(
        proof.recorded("xip.args"),
        format!(
            "--help\n--expand\t{s}/damaged.xip\n--expand\t{s}/non-archive.xip\n\
             --list\t{a}\n--expand\t{a}\n"
        )
    );
    assert_eq!(
        proof.recorded("xar.args"),
        format!("-tf\t{a}\n--dump-toc={s}/toc.xml\t-f\t{a}\n-xf\t{a}\tMetadata\n")
    );
    assert_eq!(
        proof.recorded("df.args"),
        format!(
            "-Pk\t{s}/damaged-expansion\n-Pk\t{s}/non-archive-expansion\n\
             -Pk\t{s}/expansion\n-Pk\t{s}/expansion\n"
        )
    );
    assert_eq!(proof.recorded("sw_vers.args"), "\n");
    assert_eq!(proof.recorded("uname.args"), "-sm\n");
    let plutil = proof.recorded("plutil.args");
    let lines: Vec<&str> = plutil.lines().collect();
    assert_eq!(lines.len(), 2, "{plutil}");
    assert_eq!(
        lines[0],
        format!(
            "-extract\tCFBundleShortVersionString\traw\t-o\t-\t{s}/expansion/Xcode.app/Contents/Info.plist"
        )
    );
    assert_eq!(
        lines[1],
        format!(
            "-extract\tProductBuildVersion\traw\t-o\t-\t{s}/expansion/Xcode.app/Contents/version.plist"
        )
    );
    assert_eq!(
        proof.recorded("codesign.args"),
        format!("--verify\t--deep\t--strict\t{s}/expansion/Xcode.app\n")
    );
    assert_eq!(
        proof.recorded("spctl.args"),
        format!("--assess\t--type\texecute\t-vv\t{s}/expansion/Xcode.app\n")
    );
}

/// Proves: the clone is the archive with the byte at its middle changed and nothing else, and the
/// file that is no archive is 16 bytes, as pkgutil and xip got them.
#[test]
fn the_damaged_clone_differs_from_the_archive_in_its_middle_byte_alone() {
    let proof = Proof::new("damage");
    assert_eq!(proof.verify(&proof.archive(ARCHIVE)).code, 0);
    let mut damaged = ARCHIVE.to_vec();
    let middle = ARCHIVE.len() / 2;
    damaged[middle] = !damaged[middle];
    assert_eq!(
        std::fs::read(proof.bin.join("pkgutil.damaged.xip.bytes")).expect("seen"),
        damaged
    );
    assert_eq!(
        std::fs::read(proof.bin.join("xip.damaged.xip.bytes")).expect("seen"),
        damaged
    );
    let fake = std::fs::read(proof.bin.join("pkgutil.non-archive.xip.bytes")).expect("seen");
    assert_eq!(fake, b"this is no xip!\n");
    assert_eq!(fake.len(), 16);
    // The archive itself is as it was.
    assert_eq!(
        std::fs::read(proof.rig.path("downloads/Xcode_27.2.xip")).expect("the archive"),
        ARCHIVE
    );
}

/// Proves: the record is written to `~/.hostkeeper/xcode/<sha256>.json` with mode 0600, holding
/// what stdout printed; `~/.hostkeeper` and its `xcode` directory are made with mode 0700 under
/// a umask of 000-like modes; and a record that is there already, of any kind, is replaced, with
/// no file of the replacement left beside it.
#[test]
fn the_record_is_kept_privately_and_replaces_any_record() {
    let proof = Proof::new("keep");
    let archive = proof.archive(ARCHIVE);
    let ran = proof.verify(&archive);
    assert_eq!(ran.code, 0, "{ran:?}");
    let path = proof.record_path(ARCHIVE);
    assert_eq!(
        std::fs::read_to_string(&path).expect("the record"),
        ran.stdout
    );
    let mode = |p: &Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&proof.rig.path("home/.hostkeeper")), 0o700);
    assert_eq!(mode(&proof.rig.path("home/.hostkeeper/xcode")), 0o700);
    // Another kind of record, written wide open, is replaced by a private one.
    std::fs::write(&path, b"{\"record\": \"hk-xcode-add-v1\"}").expect("an older record");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode");
    let again = proof.verify(&archive);
    assert_eq!(again.code, 0, "{again:?}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("the record"),
        again.stdout
    );
    assert_eq!(mode(&path), 0o600);
    let kept: Vec<String> = std::fs::read_dir(proof.rig.path("home/.hostkeeper/xcode"))
        .expect("the directory")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(kept, [format!("{}.json", sha256(ARCHIVE))]);
}

/// Proves: a run whose record cannot be put in place says so and exits 1: the path of the record
/// is a directory.
#[test]
fn a_record_that_cannot_replace_the_old_one_is_an_error() {
    let proof = Proof::new("blocked");
    let path = proof.record_path(ARCHIVE);
    std::fs::create_dir_all(&path).expect("a directory in the record's place");
    std::fs::set_permissions(
        proof.rig.path("home/.hostkeeper"),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("mode");
    std::fs::set_permissions(
        proof.rig.path("home/.hostkeeper/xcode"),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("mode");
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(ran.code, 1, "{ran:?}");
    assert_eq!(ran.stdout, "");
    assert!(
        ran.stderr
            .contains(&format!("hk: cannot replace {}: ", path.display())),
        "{ran:?}"
    );
    assert!(!{
        let mut new = proof.record_path(ARCHIVE).into_os_string();
        new.push(".new");
        Path::new(&new).exists()
    });
}

/// Proves: a check that fails still prints the record and writes it, with the verdict naming the
/// check, exit 1; what the checks after it would have recorded is `null`, and the scratch
/// directory is gone.
#[test]
fn a_failing_check_still_writes_the_record_naming_it() {
    let proof = Proof::new("fails");
    proof.tool("codesign.exit", b"3");
    proof.tool("codesign.out", b"Xcode.app: a sealed resource is missing\n");
    let archive = proof.archive(ARCHIVE);
    let ran = proof.verify(&archive);
    let codesign = format!(
        "fail: expansion: {}/codesign --verify --deep --strict exited 3",
        proof.bin.display()
    );
    assert_eq!(ran.code, 1, "{ran:?}");
    let record = Proof::printed(&ran);
    assert_eq!(record["verdict"], codesign);
    assert_eq!(record["expansion"]["codesign"]["exit"], 3);
    assert_eq!(
        record["expansion"]["codesign"]["stdout"],
        "Xcode.app: a sealed resource is missing\n"
    );
    assert_eq!(
        std::fs::read_to_string(proof.record_path(ARCHIVE)).expect("the record"),
        ran.stdout
    );
    assert!(
        ran.stderr.ends_with(&format!(
            "hk: verdict {codesign}; record kept in {}\n",
            proof.record_path(ARCHIVE).display()
        )),
        "{ran:?}"
    );
    assert_eq!(proof.beside(), ["Xcode_27.2.xip"]);
    // A check that fails early leaves the later sections null.
    let proof = Proof::new("fails-early");
    proof.tool("pkgutil.exit", b"1");
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(ran.code, 1, "{ran:?}");
    let record = Proof::printed(&ran);
    for later in ["tamper", "toc", "expansion"] {
        assert_eq!(record[later], Value::Null, "{later}");
    }
    assert_eq!(record["signature"]["exit"], 1);
    assert_eq!(proof.recorded("cp.args"), "");
    assert_eq!(proof.recorded("xar.args"), "");
}

/// Proves: off macOS `hk xcode verify` with no `--mac` and no `xcode_mac` is a usage error, exit
/// 2, with the exact text that says the archive is checked on a farm Mac and how to name one: no
/// program runs, nothing is written, and its help is still printed.
#[test]
fn off_macos_without_a_mac_it_is_a_usage_error_saying_how_to_name_one() {
    let mut proof = Proof::new("linux");
    proof.mac.os = "linux";
    let archive = proof.archive(ARCHIVE);
    let ran = proof.verify(&archive);
    assert_eq!(ran.code, 2, "{ran:?}");
    assert_eq!(ran.stdout, "");
    assert_eq!(
        ran.stderr,
        "hk: hk xcode verify checks the archive on a farm Mac, because this system is linux, \
         which has no pkgutil, xip or codesign of Apple's: name the Mac with --mac <node-id>, or \
         with xcode_mac in ~/.hostkeeper/hk.toml\n"
    );
    assert!(!proof.rig.path("home/.hostkeeper").exists());
    assert_eq!(proof.recorded("pkgutil.args"), "");
    let help = proof.run(&["xcode", "verify", "--help"]);
    assert_eq!(help.code, 0, "{help:?}");
    assert!(
        help.stdout
            .starts_with("hk xcode verify [--apple-root FILE | --mac NODE-ID] <archive.xip>\n")
    );
}

/// Proves: the usage errors exit 2 before anything runs: no archive, two, a flag it has not,
/// `--apple-root` twice or without a value.
#[test]
fn its_usage_errors_exit_2_before_anything_runs() {
    let proof = Proof::new("usage");
    let one = "hk: hk xcode verify takes the path of one archive (~/Downloads/Xcode_27.2.xip)\n";
    let cases: [(&[&str], &str); 6] = [
        (
            &["xcode", "verify", "a.xip", "--timeout-secs", "soon"],
            "hk: --timeout-secs takes a whole number of seconds, 1 to 999999\n",
        ),
        (&["xcode", "verify"], one),
        (&["xcode", "verify", "a.xip", "b.xip"], one),
        (
            &["xcode", "verify", "--force", "a.xip"],
            "hk: hk xcode verify has no flag --force\n",
        ),
        (
            &[
                "xcode",
                "verify",
                "--apple-root",
                "a.cer",
                "--apple-root=b.cer",
                "a.xip",
            ],
            "hk: --apple-root is given twice\n",
        ),
        (
            &["xcode", "verify", "a.xip", "--apple-root"],
            "hk: --apple-root needs a value\n",
        ),
    ];
    for (words, says) in cases {
        let ran = proof.run(words);
        assert_eq!((ran.code, ran.stderr.as_str()), (2, says), "{words:?}");
        assert_eq!(ran.stdout, "", "{words:?}");
    }
    assert_eq!(proof.recorded("sw_vers.args"), "");
    assert!(!proof.rig.path("home/.hostkeeper").exists());
}

/// Proves: what cannot be verified at all exits 1 before any program runs, writing no record:
/// an archive that is not there or is a directory, and no `$HOME` to keep the record under; and
/// a `~/.hostkeeper` that others may write is refused (exit 2) naming it.
#[test]
fn an_archive_or_a_home_that_cannot_be_used_ends_the_run() {
    let proof = Proof::new("unusable");
    let nowhere = proof.rig.path("nowhere.xip").display().to_string();
    let directory = proof.rig.path("downloads").display().to_string();
    for (path, says) in [
        (
            nowhere.clone(),
            format!("hk: cannot read {nowhere} (No such file or directory (os error 2))\n"),
        ),
        (
            directory.clone(),
            format!("hk: {directory} is not a file\n"),
        ),
    ] {
        let ran = proof.verify(&path);
        assert_eq!(
            (ran.code, ran.stderr.as_str()),
            (1, says.as_str()),
            "{path}"
        );
    }
    assert_eq!(proof.recorded("sw_vers.args"), "");
    let archive = proof.archive(ARCHIVE);
    let ran = proof.run_with(Opts::real().without_home(), &["xcode", "verify", &archive]);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            1,
            "hk: hk xcode verify keeps its record under $HOME, which is not set\n"
        )
    );
    // The runs above made it.
    let hostkeeper = proof.rig.path("home/.hostkeeper");
    std::fs::set_permissions(&hostkeeper, std::fs::Permissions::from_mode(0o777)).expect("mode");
    let ran = proof.verify(&archive);
    assert_eq!(
        (ran.code, ran.stderr.as_str()),
        (
            2,
            format!(
                "hk: {} may be changed by its group or others; run chmod go-w on it\n",
                hostkeeper.display()
            )
            .as_str()
        )
    );
    assert_eq!(proof.recorded("sw_vers.args"), "");
}

/// Proves: the scratch directory is beside the archive and nothing else is: a scratch directory a
/// stopped run left is removed first, what a run makes is gone at its end whether it passed or
/// failed, and the archive's directory then holds the archive alone.
#[test]
fn the_scratch_directory_is_removed_after_every_run() {
    let proof = Proof::new("scratch");
    let archive = proof.archive(ARCHIVE);
    let stale = proof.rig.path("downloads/.hk-xcode-verify-scratch");
    std::fs::create_dir(&stale).expect("a stale scratch directory");
    std::fs::write(stale.join("expansion"), b"left").expect("a stale file");
    assert_eq!(proof.verify(&archive).code, 0);
    assert_eq!(proof.beside(), ["Xcode_27.2.xip"]);
    proof.tool("xip.Xcode_27.2.xip.exit", b"2");
    assert_eq!(proof.verify(&archive).code, 1);
    assert_eq!(proof.beside(), ["Xcode_27.2.xip"]);
}

/// Proves: `hk xcode --help` lists `add` and `verify` after `list` and `remove`, with its one-paragraph doc,
/// and `docs/cli.md`'s text lists it after them with its flag; a service that is not `xcode` has no `verify`; and an unknown method is still
/// refused.
#[test]
fn verify_is_listed_beside_list_and_remove() {
    let rig = Rig::new("xcode-verify-listing");
    let ran = rig.hk(&args(&["xcode", "--help"]));
    assert_eq!((ran.code, ran.stderr.as_str()), (0, ""));
    let methods: Vec<&str> = ran
        .stdout
        .lines()
        .skip_while(|l| *l != "Methods:")
        .skip(1)
        .filter(|l| !l.starts_with("      "))
        .map(str::trim)
        .collect();
    assert_eq!(methods, ["list", "remove", "add", "verify"]);
    assert!(
        ran.stdout
            .contains("\n  verify\n      Checks an Xcode archive on the operator's own Mac")
    );
    let doc = hostkeeper_hk::markdown(hostkeeper_proto::ctl::COMMANDS);
    let at = |heading: &str| doc.find(heading).unwrap_or_else(|| panic!("{heading}"));
    let (list, remove, add, verify) = (
        at("#### `hk xcode list`"),
        at("#### `hk xcode remove`"),
        at("#### `hk xcode add`"),
        at("#### `hk xcode verify`"),
    );
    assert!(list < remove && remove < add && add < verify);
    assert!(doc[verify..].starts_with("#### `hk xcode verify`\n\nChecks an Xcode"));
    assert!(
        doc[verify..].contains("| `--apple-root` | FILE | Apple's root certificate (DER or PEM)")
    );
    assert!(!doc.contains("#### `hk hosts verify`"));
    let ran = rig.hk(&args(&["xcode", "bogus"]));
    assert_eq!(ran.code, 2, "{ran:?}");
    assert!(
        ran.stderr.contains("the service xcode has no method bogus"),
        "{ran:?}"
    );
    let ran = rig.fx(&args(&["xcode", "verify", "--help"]));
    assert_eq!(ran.code, 2, "{ran:?}");
    assert!(ran.stderr.contains("no service xcode"), "{ran:?}");
}

/// Proves: what `hk xcode --help` `hk xcode add --help` and `hk xcode verify --help` print is exactly the text in the
/// golden files, so a change of the verbs' help is a change of a file the review sees.
#[test]
fn the_help_of_xcode_equals_its_golden_files() {
    let rig = Rig::new("xcode-verify-golden");
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut stale = Vec::new();
    let cases: [(&str, &[&str]); 3] = [
        ("service_xcode.txt", &["xcode", "--help"]),
        ("xcode_add.txt", &["xcode", "add", "--help"]),
        ("xcode_verify.txt", &["xcode", "verify", "--help"]),
    ];
    for (file, words) in cases {
        let ran = rig.hk(&args(words));
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{words:?}");
        let written = std::fs::read_to_string(golden.join(file)).unwrap_or_default();
        if written != ran.stdout {
            let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join(file);
            std::fs::write(&out, &ran.stdout).expect("write the current text");
            stale.push(format!("{file}: {}", out.display()));
        }
    }
    assert!(
        stale.is_empty(),
        "stale golden files, current text in: {stale:#?}"
    );
}
