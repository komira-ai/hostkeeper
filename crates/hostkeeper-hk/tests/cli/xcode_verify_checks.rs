//! The verdict of `hk xcode verify`, check by check, over the stand-ins of [`crate::verify_rig`]:
//! the kind, the programs, the signature and Apple's root, the damaged files, the free space, the
//! expansion, the table of contents and the Command Line Tools, each failing or passing as
//! the design's section 5.1 says.

use serde_json::{Value, json};

use crate::rig::{Opts, Reply, Server, der_of};
use crate::verify_rig::{ARCHIVE, Proof, SIGNED_BY_APPLE, df, root_certificate};

/// Proves: the first bytes decide the kind: `AA01` and `AEA1` (the formats the farm Macs defer)
/// and other bytes (a file of fewer than four, an empty one) fail the verdict with the kind said,
/// before pkgutil runs; the record names the kind.
#[test]
fn only_an_xar_archive_goes_on() {
    let cases: [(&[u8], &str, &str); 5] = [
        (
            b"AA01 an Apple Archive",
            "AA01",
            "fail: kind: the archive is AA01, not xar!: the farm Macs do not take it, an Xcode \
             archive here is an xip",
        ),
        (
            b"AEA1 an encrypted one",
            "AEA1",
            "fail: kind: the archive is AEA1, not xar!: the farm Macs do not take it, an Xcode \
             archive here is an xip",
        ),
        (
            b"PK\x03\x04 a zip",
            "unknown",
            "fail: kind: the archive starts with the bytes 504b0304, not xar!: it is not an xip",
        ),
        (
            b"xa",
            "unknown",
            "fail: kind: the archive starts with the bytes 7861, not xar!: it is not an xip",
        ),
        (
            b"",
            "unknown",
            "fail: kind: the archive starts with the bytes , not xar!: it is not an xip",
        ),
    ];
    for (n, (bytes, kind, says)) in cases.into_iter().enumerate() {
        let proof = Proof::new(&format!("kind-{n}"));
        let ran = proof.verify(&proof.archive(bytes));
        assert_eq!(ran.code, 1, "{n}: {ran:?}");
        let record = Proof::printed(&ran);
        assert_eq!(record["verdict"], says, "{n}");
        assert_eq!(record["archive"]["kind"], kind, "{n}");
        assert_eq!(record["signature"], Value::Null, "{n}");
        assert_eq!(proof.recorded("pkgutil.args"), "", "{n}");
        assert!(proof.record_path(bytes).is_file(), "{n}");
    }
}

/// Proves: a required program whose path is not a file fails the verdict naming it and its path,
/// the record still holding the programs; a missing `xcodes` and a missing optional program do
/// not fail.
#[test]
fn a_missing_program_fails_and_a_missing_optional_one_does_not() {
    for name in [
        "pkgutil", "xip", "plutil", "codesign", "spctl", "xar", "sw_vers", "uname", "cp", "df",
    ] {
        let proof = Proof::new(&format!("missing-{name}"));
        proof.untool(name);
        let ran = proof.verify(&proof.archive(ARCHIVE));
        assert_eq!(ran.code, 1, "{name}: {ran:?}");
        let record = Proof::printed(&ran);
        assert_eq!(
            record["verdict"],
            format!(
                "fail: programs: {name} is not a file at {}/{name}",
                proof.bin.display()
            ),
            "{name}"
        );
        let missing: Vec<&Value> = record["programs"]["required"]
            .as_array()
            .expect("programs")
            .iter()
            .filter(|p| p["file"] == false)
            .collect();
        assert_eq!(missing.len(), 1, "{name}");
        assert_eq!(missing[0]["name"], name);
        assert_eq!(record["signature"], Value::Null, "{name}");
    }
    // xcodes present, with a version; aa absent.
    let proof = Proof::new("optional");
    std::fs::hard_link(
        env!("CARGO_BIN_EXE_hk-fake-mac-tool"),
        proof.bin.join("xcodes"),
    )
    .expect("xcodes");
    proof.tool("xcodes.out", b"1.6.0\n");
    proof.untool("aa");
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(ran.code, 0, "{ran:?}");
    let programs = &Proof::printed(&ran)["programs"];
    assert_eq!(
        programs["xcodes"],
        json!({"exit": 0, "stdout": "1.6.0\n", "stderr": ""})
    );
    assert_eq!(
        programs["optional"][1],
        json!({"name": "aa", "path": format!("{}/aa", proof.bin.display()), "file": false})
    );
    assert_eq!(proof.recorded("xcodes.args"), "version\n");
}

/// Proves: the signature fails the verdict, each in its own words, when pkgutil exits non-zero;
/// when the status is a developer's rather than Apple's own; when the chain ends at another root
/// or at Apple Root CA with another fingerprint; and when the report gives no status or no chain
/// (a changed layout fails closed); the damaged files are not tried after it.
#[test]
fn a_signature_that_is_not_apples_fails_the_verdict() {
    let developer = SIGNED_BY_APPLE.replace(
        "signed Apple Software",
        "signed by a developer certificate issued by Apple for distribution",
    );
    let wrong_root = SIGNED_BY_APPLE.replace("68 C5 BE 91", "68 C5 BE 92");
    let other_root = SIGNED_BY_APPLE.replace("3. Apple Root CA", "3. Some Root CA");
    let no_status = SIGNED_BY_APPLE.replace("Status: signed Apple Software", "");
    let no_chain: String = SIGNED_BY_APPLE
        .lines()
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    let apple = "B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE91B5A11001F024";
    let cases: [(&str, &str, String); 6] = [
        ("1", "", "pkgutil --check-signature exited 1".to_owned()),
        (
            "0",
            &developer,
            "pkgutil does not report Apple's own signature: its status is \"signed by a developer \
             certificate issued by Apple for distribution\", not \"signed Apple Software\""
                .to_owned(),
        ),
        (
            "0",
            &wrong_root,
            format!(
                "pkgutil does not report Apple's own signature: its chain ends at \"Apple Root \
                 CA\" with the SHA-256 fingerprint \
                 \"B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE92B5A11001F024\", not at \
                 Apple Root CA ({apple})"
            ),
        ),
        (
            "0",
            &other_root,
            format!(
                "pkgutil does not report Apple's own signature: its chain ends at \"Some Root \
                 CA\" with the SHA-256 fingerprint \"{apple}\", not at Apple Root CA ({apple})"
            ),
        ),
        (
            "0",
            &no_status,
            "pkgutil does not report Apple's own signature: it gives no status".to_owned(),
        ),
        (
            "0",
            &no_chain,
            "pkgutil does not report Apple's own signature: it gives no certificate chain"
                .to_owned(),
        ),
    ];
    for (n, (exit, report, says)) in cases.into_iter().enumerate() {
        let proof = Proof::new(&format!("signature-{n}"));
        proof.tool("pkgutil.exit", exit.as_bytes());
        proof.tool("pkgutil.out", report.as_bytes());
        let verdict = proof.verdict(ARCHIVE);
        let named = if n == 0 {
            format!("fail: signature: {}/{says}", proof.bin.display())
        } else {
            format!("fail: signature: {says}")
        };
        assert_eq!(verdict, named, "{n}");
        assert_eq!(
            proof.recorded("cp.args"),
            "",
            "{n}: the damaged files were tried"
        );
    }
}

/// Proves: damage is told apart from a refusal by xip alone. `pkgutil` accepting the damaged clone
/// (exit 0, signed by Apple) passes when `xip` refuses it; `xip` accepting the damaged clone, or
/// the file that is no archive, fails, each saying which; `xip` that exits with no code fails.
#[test]
fn only_xip_accepting_a_damaged_file_fails_the_verdict() {
    let proof = Proof::new("pkgutil-accepts");
    proof.tool("pkgutil.damaged.xip.exit", b"0");
    proof.tool("pkgutil.damaged.xip.out", SIGNED_BY_APPLE.as_bytes());
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(ran.code, 0, "{ran:?}");
    let record = Proof::printed(&ran);
    assert_eq!(record["verdict"], "pass");
    assert_eq!(record["tamper"]["damaged"]["pkgutil"]["exit"], 0);
    assert_eq!(record["tamper"]["damaged"]["xip"]["exit"], 1);

    let proof = Proof::new("xip-accepts-clone");
    proof.untool("xip.damaged.xip.exit");
    assert_eq!(
        proof.verdict(ARCHIVE),
        format!(
            "fail: tamper: {}/xip --expand accepted the damaged clone",
            proof.bin.display()
        )
    );
    // Both damaged files are tried and recorded before the verdict is drawn.
    assert_eq!(proof.recorded("xip.args").matches("--expand").count(), 2);

    let proof = Proof::new("xip-accepts-fake");
    proof.untool("xip.non-archive.xip.exit");
    assert_eq!(
        proof.verdict(ARCHIVE),
        format!(
            "fail: tamper: {}/xip --expand accepted the 16-byte file that is no archive",
            proof.bin.display()
        )
    );
}

/// Proves: a volume that cannot clone (cp exits non-zero) records `not run, the volume cannot
/// clone` with cp's own output, runs pkgutil and xip on the 16-byte file alone, and passes: it is
/// never a silent skip and never a failure.
#[test]
fn a_volume_that_cannot_clone_passes_saying_so() {
    let proof = Proof::new("no-clone");
    proof.tool("cp.exit", b"1");
    proof.tool(
        "cp.damaged.xip.err",
        b"cp: clonefile failed: Operation not supported\n",
    );
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(ran.code, 0, "{ran:?}");
    let record = Proof::printed(&ran);
    assert_eq!(record["verdict"], "pass");
    let tamper = &record["tamper"];
    assert_eq!(tamper["damaged_clone"], "not run, the volume cannot clone");
    assert_eq!(
        tamper["clone"],
        json!({"exit": 1, "stdout": "", "stderr": "cp: clonefile failed: Operation not supported\n"})
    );
    assert_eq!(tamper["damaged"], Value::Null);
    assert_eq!(tamper["non_archive"]["xip"]["exit"], 1);
    assert_eq!(proof.recorded("pkgutil.args").lines().count(), 2);
}

/// Proves: the free space is checked before each expansion with three times the archive's size:
/// a volume short of it before the first damaged file's expansion fails `free_space_short`, naming
/// both numbers, and no `xip --expand` runs; short before the archive's own expansion fails the
/// same way with the free space before recorded and `xip` run only on the two damaged files; a
/// `df` that fails, or prints no Available column, fails naming `df`; exactly three times the
/// size is enough.
#[test]
fn too_little_free_space_stops_before_the_expansion() {
    let size = u64::try_from(ARCHIVE.len()).expect("a size");
    let expansion = |proof: &Proof, name: &str| {
        proof
            .rig
            .path("downloads/.hk-xcode-verify-scratch")
            .join(name)
            .display()
            .to_string()
    };
    let proof = Proof::new("short-first");
    proof.untool("df.1.out");
    proof.tool("df.out", df(0).as_bytes());
    let dir = expansion(&proof, "damaged-expansion");
    assert_eq!(
        proof.verdict(ARCHIVE),
        format!(
            "fail: free_space_short: the volume of {dir} has 0 bytes free and the expansion \
             needs {} (3 times the archive's {size})",
            size * 3
        )
    );
    assert_eq!(proof.recorded("xip.args"), "--help\n");

    let proof = Proof::new("short-last");
    proof.tool("df.3.out", df(0).as_bytes());
    let dir = expansion(&proof, "expansion");
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(ran.code, 1, "{ran:?}");
    let record = Proof::printed(&ran);
    assert_eq!(
        record["verdict"],
        format!(
            "fail: free_space_short: the volume of {dir} has 0 bytes free and the expansion \
             needs {} (3 times the archive's {size})",
            size * 3
        )
    );
    assert_eq!(record["expansion"]["free_before"], 0);
    assert_eq!(record["expansion"]["exit"], Value::Null);
    assert_eq!(proof.recorded("xip.args").matches("--expand").count(), 2);

    let proof = Proof::new("df-fails");
    proof.tool("df.exit", b"1");
    assert_eq!(
        proof.verdict(ARCHIVE),
        format!(
            "fail: tamper: {}/df -Pk {} exited 1",
            proof.bin.display(),
            expansion(&proof, "damaged-expansion")
        )
    );

    let proof = Proof::new("df-says-nothing");
    for n in 1..=4 {
        proof.tool(&format!("df.{n}.out"), b"Filesystem 1024-blocks\n");
    }
    assert_eq!(
        proof.verdict(ARCHIVE),
        format!(
            "fail: tamper: {}/df -Pk {} does not print a line with an Available column",
            proof.bin.display(),
            expansion(&proof, "damaged-expansion")
        )
    );
}

/// Proves: the expansion fails the verdict, each in its words, when `xip --expand` exits non-zero
/// (its output recorded), when it makes two apps or none, when plutil cannot read the version,
/// when the version or the build is not an Xcode release's, and when codesign refuses the app;
/// spctl refusing the app is recorded and does not fail it.
#[test]
fn an_expansion_that_is_not_one_xcode_fails_the_verdict() {
    let bin = |proof: &Proof| proof.bin.display().to_string();

    let proof = Proof::new("xip-fails");
    proof.tool("xip.Xcode_27.2.xip.exit", b"2");
    proof.tool("xip.Xcode_27.2.xip.err", b"xip: signature check failed\n");
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(ran.code, 1, "{ran:?}");
    let record = Proof::printed(&ran);
    assert_eq!(
        record["verdict"],
        format!("fail: expansion: {}/xip --expand exited 2", bin(&proof))
    );
    assert_eq!(record["expansion"]["exit"], 2);
    assert_eq!(
        record["expansion"]["output"],
        "xip: signature check failed\n"
    );
    assert_eq!(record["expansion"]["seconds"], 1.5);
    assert_eq!(record["expansion"]["free_after"], 700_000 * 1024);

    let proof = Proof::new("two-apps");
    proof.tool("xip.second", b"");
    let ran = proof.verify(&proof.archive(ARCHIVE));
    let record = Proof::printed(&ran);
    assert_eq!(
        record["verdict"],
        format!(
            "fail: expansion: {}/xip --expand made 2 apps, not one Xcode",
            bin(&proof)
        )
    );
    assert_eq!(
        record["expansion"]["apps"],
        json!(["Xcode-beta.app", "Xcode.app"])
    );

    let proof = Proof::new("no-app");
    proof.untool("xip.version");
    assert_eq!(
        proof.verdict(ARCHIVE),
        format!(
            "fail: expansion: {}/xip --expand made 0 apps, not one Xcode",
            bin(&proof)
        )
    );

    let proof = Proof::new("plutil-refuses");
    proof.tool("plutil.refuse", b"ProductBuildVersion");
    let ran = proof.verify(&proof.archive(ARCHIVE));
    let record = Proof::printed(&ran);
    assert_eq!(
        record["verdict"],
        "fail: expansion: plutil could not read the version and the build"
    );
    assert_eq!(record["expansion"]["version"], "27.2");
    assert_eq!(record["expansion"]["build"], Value::Null);
    assert_eq!(record["expansion"]["plutil"]["build"]["exit"], 1);

    for (n, (version, build, says)) in [
        (
            "27.2-beta",
            "27B52",
            "version \"27.2-beta\" and build \"27B52\"",
        ),
        ("27.2", "27 B52", "version \"27.2\" and build \"27 B52\""),
    ]
    .into_iter()
    .enumerate()
    {
        let proof = Proof::new(&format!("bad-release-{n}"));
        proof.tool("xip.version", format!("{version}\n").as_bytes());
        proof.tool("xip.product-build", format!("{build}\n").as_bytes());
        assert_eq!(
            proof.verdict(ARCHIVE),
            format!("fail: expansion: the app says {says}, which are not an Xcode release's"),
            "{n}"
        );
    }

    let proof = Proof::new("spctl-refuses");
    proof.tool("spctl.exit", b"3");
    proof.tool("spctl.out", b"Xcode.app: rejected\n");
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        Proof::printed(&ran)["expansion"]["spctl"],
        json!({"exit": 3, "stdout": "Xcode.app: rejected\n", "stderr": ""})
    );
}

/// Proves: the `Metadata` member and the table of contents are recorded for what they are and
/// never fail the verdict: a version named in a member's name or in the `Metadata` text sets
/// `names_version`; `Metadata` that is not extracted is `missing` with no text and a table of
/// contents that is not written is `null`; `Metadata` that is a directory or a link is said so;
/// and failing `xip --list`, `xar -tf` and `xar` runs are recorded with their exits.
#[test]
fn the_table_of_contents_is_recorded_and_never_fails_the_verdict() {
    let proof = Proof::new("toc-version");
    proof.tool(
        "xar.metadata",
        b"<key>ProductBuildVersion</key><string>27B52</string>",
    );
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(Proof::printed(&ran)["toc"]["names_version"], true);

    let proof = Proof::new("toc-name");
    proof.tool("xar.list", b"Metadata\nContent\nVersion.plist\n");
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(Proof::printed(&ran)["toc"]["names_version"], true);

    let proof = Proof::new("toc-nothing");
    for name in ["xar.metadata", "xar.toc"] {
        proof.untool(name);
    }
    proof.tool("xar.exit", b"1");
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(ran.code, 0, "{ran:?}");
    let toc = &Proof::printed(&ran)["toc"];
    assert_eq!(toc["toc"], Value::Null);
    assert_eq!(toc["metadata"]["kind"], "missing");
    assert_eq!(toc["metadata"]["text"], Value::Null);
    assert_eq!(toc["metadata"]["extract"]["exit"], 1);
    assert_eq!(toc["dump_toc"]["exit"], 1);
    assert_eq!(toc["xar_list"]["exit"], 1);
    assert_eq!(toc["names_version"], false);
}

/// Proves: a package of the Command Line Tools is checked by pkgutil for the record, the first
/// `.pkg` by name, with the status it gave; the file that is no `.pkg` is not.
#[test]
fn a_command_line_tools_package_is_recorded_with_its_status() {
    let proof = Proof::new("clt");
    proof.rig.put("clt/README.txt", b"not a package", 0o644);
    proof.rig.put("clt/CLTools_macOS_SDK.pkg", b"pkg", 0o644);
    proof.rig.put("clt/CLTools_Executables.pkg", b"pkg", 0o644);
    proof.tool(
        "pkgutil.CLTools_Executables.pkg.out",
        b"Package \"CLTools_Executables.pkg\":\n   Status: signed by a developer certificate issued by Apple for distribution\n",
    );
    let ran = proof.verify(&proof.archive(ARCHIVE));
    assert_eq!(ran.code, 0, "{ran:?}");
    let clt = &Proof::printed(&ran)["tamper"]["command_line_tools"];
    assert_eq!(
        clt["package"],
        proof
            .rig
            .path("clt/CLTools_Executables.pkg")
            .display()
            .to_string()
    );
    assert_eq!(
        clt["status"],
        "signed by a developer certificate issued by Apple for distribution"
    );
    assert_eq!(clt["pkgutil"]["exit"], 0);
}

/// Proves: Apple's root is read from a PEM file as from a DER one; a file that cannot be read, one
/// that is no certificate, and a PEM that holds none each fail the verdict naming the file and
/// why, with the record holding the error and no fingerprint; and a published root whose
/// fingerprint is not the constant fails, the record holding both.
#[test]
fn apple_roots_that_are_not_the_constant_or_not_certificates_fail() {
    let proof = Proof::new("root-pem");
    let der = std::fs::read(proof.rig.path("apple-root.cer")).expect("der");
    let (_, other_pem) = root_certificate();
    // The PEM of the very certificate the constant was set to: the same fingerprint as the DER.
    let same_pem = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        base64_of(&der)
    );
    let path = proof.rig.put("apple-root.pem", same_pem.as_bytes(), 0o644);
    let ran = proof.run(&[
        "xcode",
        "verify",
        &proof.archive(ARCHIVE),
        "--apple-root",
        &path,
    ]);
    assert_eq!(ran.code, 0, "{ran:?}");
    assert_eq!(
        Proof::printed(&ran)["signature"]["apple_root"]["sha256"],
        proof.root
    );

    // Another certificate: its fingerprint is not the constant.
    let proof = Proof::new("root-other");
    let path = proof.rig.put("other.pem", other_pem.as_bytes(), 0o644);
    let ran = proof.run(&[
        "xcode",
        "verify",
        &proof.archive(ARCHIVE),
        "--apple-root",
        &path,
    ]);
    assert_eq!(ran.code, 1, "{ran:?}");
    let record = Proof::printed(&ran);
    let root = &record["signature"]["apple_root"];
    assert_eq!(root["equal"], false);
    assert_eq!(root["constant"], proof.root);
    let found = root["sha256"].as_str().expect("a fingerprint");
    assert_ne!(found, proof.root);
    assert_eq!(
        record["verdict"],
        format!(
            "fail: signature: Apple's published root has the SHA-256 fingerprint {found}, not the {} the code holds",
            proof.root
        )
    );
    assert_eq!(
        proof.recorded("cp.args"),
        "",
        "the damaged files were tried"
    );

    let missing = proof.rig.path("none.cer").display().to_string();
    let garbage = proof.rig.put("garbage.cer", b"not a certificate", 0o644);
    let empty_pem = proof.rig.put(
        "empty.pem",
        b"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        0o644,
    );
    let cases = [
        (
            missing.clone(),
            format!("cannot read {missing} (No such file or directory (os error 2))"),
        ),
        (
            garbage.clone(),
            format!("{garbage} is not a certificate, in DER or PEM"),
        ),
        (
            empty_pem.clone(),
            format!("{empty_pem} holds no certificate"),
        ),
    ];
    for (n, (file, error)) in cases.into_iter().enumerate() {
        let ran = proof.run(&[
            "xcode",
            "verify",
            &proof.archive(ARCHIVE),
            "--apple-root",
            &file,
        ]);
        assert_eq!(ran.code, 1, "{n}: {ran:?}");
        let record = Proof::printed(&ran);
        assert_eq!(
            record["signature"]["apple_root"],
            json!({
                "source": file,
                "sha256": null,
                "subject": null,
                "constant": proof.root,
                "equal": false,
                "error": error,
            }),
            "{n}"
        );
        assert_eq!(
            record["verdict"],
            format!("fail: signature: Apple's root certificate: {error}"),
            "{n}"
        );
    }
}

/// The standard base64 of `bytes`, in lines of 64.
fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    let text = base64::engine::general_purpose::STANDARD.encode(bytes);
    text.as_bytes()
        .chunks(64)
        .map(|line| String::from_utf8_lossy(line).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Proves: with no `--apple-root`, the root is fetched with one `GET` of the Mac's certificate
/// page over HTTPS, trusting the roots the world gives and no token, and a 200 answer with the
/// certificate passes; another status, and no answer, fail the verdict saying so, and
/// `--timeout-secs` is taken as a global flag.
#[test]
fn apples_root_is_fetched_over_https_from_the_page() {
    let mut proof = Proof::new("fetch");
    let der = std::fs::read(proof.rig.path("apple-root.cer")).expect("the certificate");
    let served = der.clone();
    let server = Server::serve(
        &proof.rig.pki,
        || (),
        move |(), request| {
            if request.target == "/appleca/AppleIncRootCertificate.cer" {
                Reply::new(200, served.clone())
            } else {
                Reply::new(404, "")
            }
        },
    );
    let roots = vec![der_of(&proof.rig.pki.ca_pem)];
    proof.mac.apple_root_url = format!(
        "https://127.0.0.1:{}/appleca/AppleIncRootCertificate.cer",
        server.port
    );
    let archive = proof.archive(ARCHIVE);
    let opts = || Opts {
        roots: roots.clone(),
        ..Opts::real()
    };
    let ran = proof.run_with(
        opts(),
        &["--timeout-secs", "20", "xcode", "verify", &archive],
    );
    assert_eq!(ran.code, 0, "{ran:?}");
    let record = Proof::printed(&ran);
    assert_eq!(
        record["signature"]["apple_root"]["source"],
        proof.mac.apple_root_url
    );
    assert_eq!(record["signature"]["apple_root"]["sha256"], proof.root);
    let seen = server.seen();
    assert_eq!(seen.len(), 1, "{seen:#?}");
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].header("authorization"), None);

    // Another page: 404.
    proof.mac.apple_root_url = format!("https://127.0.0.1:{}/elsewhere", server.port);
    let ran = proof.run_with(opts(), &["xcode", "verify", &archive]);
    assert_eq!(ran.code, 1, "{ran:?}");
    assert_eq!(
        Proof::printed(&ran)["verdict"],
        format!(
            "fail: signature: Apple's root certificate: {} was answered HTTP 404",
            proof.mac.apple_root_url
        )
    );

    // No roots to trust: no request is made.
    let ran = proof.run_with(Opts::real(), &["xcode", "verify", &archive]);
    assert_eq!(ran.code, 1, "{ran:?}");
    let record = Proof::printed(&ran);
    let error = record["signature"]["apple_root"]["error"]
        .as_str()
        .expect("an error");
    assert!(
        error.starts_with(&format!(
            "cannot fetch {} (no certificate to trust)",
            proof.mac.apple_root_url
        )),
        "{error}"
    );
}
