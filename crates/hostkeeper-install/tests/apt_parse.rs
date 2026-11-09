//! What the `apt` backend reads of apt's, dpkg's and dpkg-query's output (packages-tests.md, "The
//! apt installer"): each parser of `apt::parse` over the forms Debian 12 (apt 2.6) and Debian 13
//! (apt 3.0) print, and dpkg's grammars and order of versions (`apt::version`), whose orders
//! below are what `dpkg --compare-versions` answers.

use std::cmp::Ordering;

use hostkeeper_install::repository::apt::parse::{
    self, BUSY, DEPENDENTS, HASH, INTERRUPTED, SIGNATURE, UNKNOWN, UNMET,
};
use hostkeeper_install::repository::apt::version::{compare, is_package, is_version};
use hostkeeper_install::repository::backend::{Fields, Installed, Stanza};
use hostkeeper_install::repository::{Class, DRIFT_REPAIRED, Failure, Outcome, Reason, ToolOutput};

/// An output that exited `code`, with `stdout` and `stderr`.
fn run(code: i32, stdout: &str, stderr: &str) -> ToolOutput {
    ToolOutput {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
        cut: false,
    }
}

/// The failure of a step that must have failed, as its reason and detail.
fn failed<T: std::fmt::Debug>(result: Result<T, Failure>) -> (Reason, String) {
    let failure = result.expect_err("the step fails");
    (failure.reason(), failure.detail().to_owned())
}

const D12_OTHER_KEY: &str = "Err:1 file:/repo testing InRelease\n  The following signatures couldn't be verified because the public key is not available: NO_PUBKEY 83F81AA1B13893D8\n";
const D12_OTHER_KEY_ERR: &str = "W: GPG error: file:/repo testing InRelease: The following signatures couldn't be verified because the public key is not available: NO_PUBKEY 83F81AA1B13893D8\nE: The repository 'file:/repo testing InRelease' is not signed.\n";
const D12_TAMPERED: &str = "Err:1 file:/repo testing InRelease\n  The following signatures were invalid: BADSIG 83F81AA1B13893D8 fixture <f@example.com>\n";
const EXPIRED_ERR: &str = "E: Release file for file:/repo/dists/testing/InRelease is expired (invalid since 1d 0h 0min 1s). Updates for this repository will not be applied.\n";
const UNSIGNED_ERR: &str = "E: The repository 'file:/repo testing Release' is not signed.\n";
const D13_OTHER_KEY: &str = "Err:1 file:/repo testing InRelease\n  Sub-process /usr/bin/sqv returned an error code (1), error message is: Missing key 173A9693D96FD1B4ABD3905183F81AA1B13893D8, which is needed to verify signature.\n";
const D13_OTHER_KEY_ERR: &str = "W: OpenPGP signature verification failed: file:/repo testing InRelease: Sub-process /usr/bin/sqv returned an error code (1), error message is: Missing key 173A9693D96FD1B4ABD3905183F81AA1B13893D8, which is needed to verify signature.\nE: The repository 'file:/repo testing InRelease' is not signed.\n";
const D13_TAMPERED: &str = "Err:1 file:/repo testing InRelease\n  Sub-process /usr/bin/sqv returned an error code (1), error message is: Verifying signature:            Message has been manipulated\n";
const UNMET_OUT: &str = "The following packages have unmet dependencies:\n needsmissing : Depends: nosuchpkg but it is not installable\n";
const UNMET_ERR: &str = "E: Unable to correct problems, you have held broken packages.\n";
const LOCK_ERR: &str = "E: Could not get lock /var/lib/dpkg/lock-frontend. It is held by process 4242 (apt-get)\nE: Unable to acquire the dpkg frontend lock (/var/lib/dpkg/lock-frontend), is another process using it?\n";
const INTERRUPTED_ERR: &str = "E: dpkg was interrupted, you must manually run 'dpkg --configure -a' to correct the problem.\n";

/// Proves an update that exits 0 succeeds.
#[test]
fn update_that_exits_0_succeeds() {
    assert_eq!(
        parse::update(&run(
            0,
            "Hit:1 file:/repo testing InRelease\nReading package lists...\n",
            ""
        )),
        Ok(())
    );
}

/// Proves each form of a signature failure on apt 2.6 and apt 3.0 (another key, a tampered
/// index, an expired one, an unsigned one) is `signature`, quoting the closing `E:` line.
/// Catches: a signature failure reported as a fetch failure, which a later try would repeat.
#[test]
fn update_reads_each_signature_form_as_signature() {
    for (name, out, err, quoted) in [
        (
            "d12 other key",
            D12_OTHER_KEY,
            D12_OTHER_KEY_ERR,
            "E: The repository 'file:/repo testing InRelease' is not signed.",
        ),
        (
            "d12 tampered",
            D12_TAMPERED,
            "E: The repository 'file:/repo testing InRelease' is not signed.\n",
            "E: The repository 'file:/repo testing InRelease' is not signed.",
        ),
        (
            "expired",
            "",
            EXPIRED_ERR,
            "E: Release file for file:/repo/dists/testing/InRelease is expired (invalid since 1d 0h 0min 1s). Updates for this repository will not be applied.",
        ),
        (
            "unsigned",
            "",
            UNSIGNED_ERR,
            "E: The repository 'file:/repo testing Release' is not signed.",
        ),
        (
            "d13 other key",
            D13_OTHER_KEY,
            D13_OTHER_KEY_ERR,
            "E: The repository 'file:/repo testing InRelease' is not signed.",
        ),
        (
            "d13 tampered",
            D13_TAMPERED,
            "E: The repository 'file:/repo testing InRelease' is not signed.\n",
            "E: The repository 'file:/repo testing InRelease' is not signed.",
        ),
    ] {
        let (reason, detail) = failed(parse::update(&run(100, out, err)));
        assert_eq!(reason, Reason::Signature, "{name}");
        assert_eq!(detail, format!("apt-get update: {quoted}"), "{name}");
    }
}

/// Proves every phrase of `SIGNATURE`, alone in the output, makes an update a signature failure.
#[test]
fn update_reads_every_signature_phrase() {
    for needle in SIGNATURE {
        let out = run(100, &format!("Err:1 x y z\n  {needle}\n"), "");
        assert_eq!(failed(parse::update(&out)).0, Reason::Signature, "{needle}");
    }
}

/// Proves an update that failed for another cause (the network, a missing suite, a kill) is
/// `fetch_failed`.
#[test]
fn update_reads_an_unreachable_repository_as_fetch_failed() {
    let out = "Err:1 https://repo.example.com/debian stable InRelease\n  Could not resolve 'repo.example.com'\n";
    let err = "W: Failed to fetch https://repo.example.com/debian/dists/stable/InRelease  Could not resolve 'repo.example.com'\nE: Some index files failed to download. They have been ignored, or old ones used instead.\n";
    let (reason, detail) = failed(parse::update(&run(100, out, err)));
    assert_eq!(reason, Reason::FetchFailed);
    assert_eq!(
        detail,
        "apt-get update: E: Some index files failed to download. They have been ignored, or old ones used instead."
    );
    let killed = ToolOutput::default();
    let (reason, detail) = failed(parse::update(&killed));
    assert_eq!(
        (reason, detail.as_str()),
        (Reason::FetchFailed, "apt-get update: ")
    );
}

const HELLO: &str = "Package: hello\nVersion: 1.0.0-1\nArchitecture: all\nSize: 1234\n\
    SHA256: 5C1C1E3F2A7F28A6E7B0C0A8C2C3E4D5F6A7B8C9D0E1F2A3B4C5D6E7F8091A2B\nDescription: a greeting\n";

fn stanza_of(blocks: &[&str]) -> ToolOutput {
    run(0, &blocks.join("\n"), "")
}

/// Proves the stanza whose package, version and architecture match is the one read, among
/// stanzas of another version, another architecture and another package, an architecture of
/// `all` matches, and an upper-case SHA256 is lowered.
#[test]
fn stanza_reads_the_matching_stanza_among_several() {
    let other_version = HELLO.replace("1.0.0-1", "2.0.0-1").replace("1234", "999");
    let other_arch = HELLO.replace("all", "arm64").replace("1234", "888");
    let other_package = HELLO.replace("hello", "hello-extra").replace("1234", "777");
    let output = stanza_of(&[&other_version, &other_arch, &other_package, HELLO]);
    let want = Stanza {
        sha256: "5c1c1e3f2a7f28a6e7b0c0a8c2c3e4d5f6a7b8c9d0e1f2a3b4c5d6e7f8091a2b".to_owned(),
        size: 1234,
    };
    assert_eq!(
        parse::stanza(&output, "hello", "1.0.0-1", "amd64"),
        Ok(Some(want.clone()))
    );
    let amd64 = HELLO.replace("all", "amd64");
    let output = stanza_of(&[&other_arch, &amd64]);
    assert_eq!(
        parse::stanza(&output, "hello", "1.0.0-1", "amd64"),
        Ok(Some(want))
    );
}

/// Proves an output with no stanza of that package, version and architecture is `None`, and so
/// is an empty one.
#[test]
fn stanza_without_a_match_is_none() {
    let other_version = HELLO.replace("1.0.0-1", "2.0.0-1");
    let other_arch = HELLO.replace("all", "arm64");
    for output in [
        stanza_of(&[&other_version]),
        stanza_of(&[&other_arch]),
        run(0, "", ""),
    ] {
        assert_eq!(
            parse::stanza(&output, "hello", "1.0.0-1", "amd64"),
            Ok(None)
        );
    }
    assert_eq!(
        parse::stanza(&stanza_of(&[HELLO]), "other", "1.0.0-1", "amd64"),
        Ok(None)
    );
}

/// Proves apt-cache's answers for an unknown package or version (exit 100, each phrase of
/// `UNKNOWN`) are `None`, not an error.
#[test]
fn stanza_reads_apt_caches_unknown_package_answers_as_none() {
    for text in [
        "E: No packages found\n",
        "E: Unable to locate package hello\n",
        "E: Can't select versions from package 'hello' as it is purely virtual\n",
        "E: Can't select candidate version from package hello as it has no candidate\n",
    ] {
        assert_eq!(
            parse::stanza(&run(100, "", text), "hello", "1.0.0-1", "amd64"),
            Ok(None),
            "{text}"
        );
    }
    for needle in UNKNOWN {
        let output = run(100, needle, "");
        assert_eq!(
            parse::stanza(&output, "hello", "1.0.0-1", "amd64"),
            Ok(None),
            "{needle}"
        );
    }
}

/// Proves another exit, a kill and an output cut at its start are `io_error`, quoting the run.
#[test]
fn stanza_reads_another_failure_and_cut_output_as_io_error() {
    let output = run(
        100,
        "",
        "E: The package lists or status file could not be parsed or opened.\n",
    );
    let (reason, detail) = failed(parse::stanza(&output, "hello", "1.0.0-1", "amd64"));
    assert_eq!(reason, Reason::IoError);
    assert_eq!(
        detail,
        "apt-cache show: E: The package lists or status file could not be parsed or opened."
    );
    let killed = ToolOutput::default();
    assert_eq!(
        failed(parse::stanza(&killed, "hello", "1.0.0-1", "amd64")).0,
        Reason::IoError
    );
    let cut = ToolOutput {
        cut: true,
        ..run(0, HELLO, "")
    };
    let (reason, detail) = failed(parse::stanza(&cut, "hello", "1.0.0-1", "amd64"));
    assert_eq!(reason, Reason::IoError);
    assert_eq!(detail, "apt-cache show printed more than is kept");
}

/// Proves a matching stanza with no SHA256, a short one, no Size or a Size that is not a number
/// is `manifest_invalid`, naming the package and version.
#[test]
fn stanza_without_a_sha256_or_a_size_is_manifest_invalid() {
    let no_sha = HELLO.replace("SHA256:", "SHA512:");
    let short_sha = HELLO
        .replace("SHA256: 5C1C", "SHA256: 5c1c")
        .replace("091A2B", "");
    let no_size = HELLO.replace("Size: 1234\n", "");
    let bad_size = HELLO.replace("1234", "12x4");
    for (name, text) in [
        ("sha", no_sha),
        ("short", short_sha),
        ("size", no_size),
        ("number", bad_size),
    ] {
        let (reason, detail) = failed(parse::stanza(
            &stanza_of(&[&text]),
            "hello",
            "1.0.0-1",
            "amd64",
        ));
        assert_eq!(reason, Reason::ManifestInvalid, "{name}");
        assert_eq!(
            detail, "the index's stanza of hello 1.0.0-1 has no SHA256 and Size",
            "{name}"
        );
    }
}

/// Proves a simulation that touches only the package (an install, a configure, a downgrade, with
/// or without `:amd64`) is accepted.
#[test]
fn simulate_of_the_package_alone_is_ok() {
    for text in [
        "Reading package lists...\nInst hello (1.0.0-1 testing [all])\nConf hello (1.0.0-1 testing [all])\n",
        "Inst hello [2.0.0-1] (1.0.0-1 testing [all])\nConf hello (1.0.0-1 testing [all])\n",
        "Inst hello:amd64 (1.0.0-1 testing [amd64])\nConf hello:amd64 (1.0.0-1 testing [amd64])\n",
        "Inst\nInstalling something\nNote: a line\n",
    ] {
        assert_eq!(
            parse::simulate(&run(0, text, ""), "hello"),
            Ok(()),
            "{text}"
        );
    }
}

/// Proves a simulation that also installs, configures, removes or purges another package is
/// `dependency_changes` naming those packages, without their architecture, in order.
#[test]
fn simulate_with_another_package_is_dependency_changes() {
    let text = "Inst helper (1.0 testing [all])\nInst needsother (1.0 testing [all])\nConf helper (1.0 testing [all])\nConf needsother (1.0 testing [all])\n";
    let (reason, detail) = failed(parse::simulate(&run(0, text, ""), "needsother"));
    assert_eq!(reason, Reason::DependencyChanges);
    assert_eq!(detail, "the install of needsother would also change helper");
    let text = "Inst hello (1.0)\nRemv zed:amd64 [1.0]\nPurg zed:amd64 [1.0]\nConf lib:amd64 (2)\nInst helper (1)\n";
    let (reason, detail) = failed(parse::simulate(&run(0, text, ""), "hello"));
    assert_eq!(reason, Reason::DependencyChanges);
    assert_eq!(
        detail,
        "the install of hello would also change helper, lib, zed"
    );
}

/// Proves an unmet dependency (both releases print the same) is `dependency_missing`, each
/// phrase of `UNMET` alone is too, and the lock or an interrupted dpkg are read before it.
#[test]
fn simulate_reads_unmet_busy_and_interrupted() {
    let (reason, detail) = failed(parse::simulate(
        &run(100, UNMET_OUT, UNMET_ERR),
        "needsmissing",
    ));
    assert_eq!(reason, Reason::DependencyMissing);
    assert_eq!(
        detail,
        "the dependency check: E: Unable to correct problems, you have held broken packages."
    );
    for needle in UNMET {
        let out = run(100, needle, "");
        assert_eq!(
            failed(parse::simulate(&out, "hello")).0,
            Reason::DependencyMissing,
            "{needle}"
        );
    }
    let (reason, detail) = failed(parse::simulate(&run(100, "", LOCK_ERR), "hello"));
    assert_eq!(reason, Reason::PackageManagerBusy);
    assert_eq!(
        detail,
        "the dependency check: E: Unable to acquire the dpkg frontend lock (/var/lib/dpkg/lock-frontend), is another process using it?"
    );
    let (reason, _) = failed(parse::simulate(&run(100, "", INTERRUPTED_ERR), "hello"));
    assert_eq!(reason, Reason::DpkgInterrupted);
    let both = format!("{INTERRUPTED_ERR}{LOCK_ERR}{UNMET_ERR}");
    assert_eq!(
        failed(parse::simulate(&run(100, "", &both), "hello")).0,
        Reason::DpkgInterrupted
    );
    let both = format!("{LOCK_ERR}{UNMET_ERR}");
    assert_eq!(
        failed(parse::simulate(&run(100, "", &both), "hello")).0,
        Reason::PackageManagerBusy
    );
}

/// Proves a failure with no marker, a kill and a cut listing are `io_error`, not a pass.
#[test]
fn simulate_reads_another_failure_and_cut_output_as_io_error() {
    let (reason, detail) = failed(parse::simulate(
        &run(100, "", "E: Something else\n"),
        "hello",
    ));
    assert_eq!(
        (reason, detail.as_str()),
        (Reason::IoError, "the dependency check: E: Something else")
    );
    assert_eq!(
        failed(parse::simulate(&ToolOutput::default(), "hello")).0,
        Reason::IoError
    );
    let cut = ToolOutput {
        cut: true,
        ..run(0, "Inst hello (1)\n", "")
    };
    assert_eq!(failed(parse::simulate(&cut, "hello")).0, Reason::IoError);
}

/// Proves a download that exits 0 succeeds, each phrase of `HASH` (and the real form) is
/// `store_digest_mismatch`, and any other failure or a kill is `fetch_failed`.
#[test]
fn download_reads_hash_failures_apart_from_fetch_failures() {
    assert_eq!(
        parse::download(&run(0, "Get:1 file:/repo hello 1.0.0-1 [1234 B]\n", "")),
        Ok(())
    );
    let real = run(
        100,
        "",
        "E: Failed to fetch file:/repo/pool/hello_1.0.0-1_all.deb  Hash Sum mismatch\n   Hashes of expected file:\n    - SHA256:5c1c\n   Hashes of received file:\n    - SHA256:aaaa\nE: Failed to fetch some archives.\n",
    );
    let (reason, detail) = failed(parse::download(&real));
    assert_eq!(reason, Reason::StoreDigestMismatch);
    assert_eq!(
        detail,
        "apt-get download: E: Failed to fetch some archives."
    );
    for needle in HASH {
        let out = run(100, "", &format!("E: Failed to fetch x  {needle}\n"));
        assert_eq!(
            failed(parse::download(&out)).0,
            Reason::StoreDigestMismatch,
            "{needle}"
        );
    }
    let other = run(
        100,
        "",
        "E: Failed to fetch https://repo.example.com/x.deb  Could not resolve 'repo.example.com'\n",
    );
    assert_eq!(failed(parse::download(&other)).0, Reason::FetchFailed);
    assert_eq!(
        failed(parse::download(&ToolOutput::default())).0,
        Reason::FetchFailed
    );
}

/// Proves the control fields of a package are read when all three are there, and a missing
/// field, or a failed run, is `manifest_invalid`.
#[test]
fn fields_reads_the_three_control_fields_or_fails() {
    assert_eq!(
        parse::fields(&run(
            0,
            "Package: hello\nVersion: 1.0.0-1\nArchitecture: all\n",
            ""
        )),
        Ok(Fields {
            package: "hello".to_owned(),
            version: "1.0.0-1".to_owned(),
            architecture: "all".to_owned()
        })
    );
    for text in [
        "Package: hello\nVersion: 1.0.0-1\n",
        "Version: 1.0.0-1\nArchitecture: all\n",
        "Package: hello\nArchitecture: all\n",
        "",
    ] {
        let (reason, _) = failed(parse::fields(&run(0, text, "")));
        assert_eq!(reason, Reason::ManifestInvalid, "{text:?}");
    }
    let broken = run(
        2,
        "",
        "dpkg-deb: error: 'x.deb' is not a Debian format archive\n",
    );
    let (reason, detail) = failed(parse::fields(&broken));
    assert_eq!(reason, Reason::ManifestInvalid);
    assert_eq!(
        detail,
        "the package's control fields cannot be read: dpkg-deb: error: 'x.deb' is not a Debian format archive"
    );
    let all_but_failed = run(1, "Package: hello\nVersion: 1\nArchitecture: all\n", "");
    assert_eq!(
        failed(parse::fields(&all_but_failed)).0,
        Reason::ManifestInvalid
    );
}

/// Proves dpkg-query's status abbreviation decides what is installed: `ii` and `hi` complete,
/// `iF` and `iU` installed but not complete, `rc`, `un` and nothing at all not installed.
#[test]
fn installed_reads_the_status_abbreviation() {
    let found = |text: &str| parse::installed(&run(0, text, ""));
    let version = |complete| {
        Ok(Some(Installed {
            version: "1.0.0-1".to_owned(),
            complete,
        }))
    };
    assert_eq!(found("1.0.0-1\tii \n"), version(true));
    assert_eq!(found("1.0.0-1\thi \n"), version(true));
    assert_eq!(found("1.0.0-1\tiF \n"), version(false));
    assert_eq!(found("1.0.0-1\tiU \n"), version(false));
    assert_eq!(found("1.0.0-1\trc \n"), Ok(None));
    assert_eq!(found("\tun \n"), Ok(None));
    assert_eq!(found(""), Ok(None));
    assert_eq!(found("1.0.0-1\n"), Ok(None));
    assert_eq!(found(" 1.0.0-1 \tii \n"), version(true));
}

/// Proves dpkg-query's answer for a package it does not know is `None`, and any other failure is
/// `io_error`.
#[test]
fn installed_reads_an_unknown_package_as_none_and_another_failure_as_io_error() {
    let unknown = run(1, "", "dpkg-query: no packages found matching hello\n");
    assert_eq!(parse::installed(&unknown), Ok(None));
    for needle in UNKNOWN {
        assert_eq!(parse::installed(&run(1, needle, "")), Ok(None), "{needle}");
    }
    let (reason, detail) = failed(parse::installed(&run(
        2,
        "",
        "dpkg-query: error: cannot open the database\n",
    )));
    assert_eq!(reason, Reason::IoError);
    assert_eq!(
        detail,
        "dpkg-query: dpkg-query: error: cannot open the database"
    );
}

/// Proves an install that exits 0 succeeds; the lock, an interrupted dpkg, an unmet dependency
/// and a maintainer script that fails are each their reason, the last `dpkg_error` quoting the
/// `E:` line.
#[test]
fn install_maps_each_failure_to_its_reason() {
    assert_eq!(
        parse::install(&run(0, "Setting up hello (1.0.0-1) ...\n", "")),
        Ok(())
    );
    let postinst = run(
        100,
        " installed failing package post-installation script subprocess returned error exit status 1\nErrors were encountered while processing:\n failing\n",
        "E: Sub-process /usr/bin/dpkg returned an error code (1)\n",
    );
    let (reason, detail) = failed(parse::install(&postinst));
    assert_eq!(reason, Reason::DpkgError);
    assert_eq!(
        detail,
        "apt-get install: E: Sub-process /usr/bin/dpkg returned an error code (1)"
    );
    assert_eq!(
        failed(parse::install(&run(100, "", INTERRUPTED_ERR))).0,
        Reason::DpkgInterrupted
    );
    assert_eq!(
        failed(parse::install(&run(100, "", LOCK_ERR))).0,
        Reason::PackageManagerBusy
    );
    assert_eq!(
        failed(parse::install(&run(100, UNMET_OUT, UNMET_ERR))).0,
        Reason::DependencyMissing
    );
    assert_eq!(
        failed(parse::install(&ToolOutput::default())).0,
        Reason::DpkgError
    );
}

/// Proves a hold that exits 0 succeeds, the lock and an interrupted dpkg are their reasons, and
/// anything else is `io_error`.
#[test]
fn hold_maps_failures_to_busy_interrupted_or_io_error() {
    assert_eq!(parse::hold(&run(0, "hello set on hold.\n", "")), Ok(()));
    for needle in BUSY {
        let out = run(100, "", &format!("E: {needle}\n"));
        assert_eq!(
            failed(parse::hold(&out)).0,
            Reason::PackageManagerBusy,
            "{needle}"
        );
    }
    for needle in INTERRUPTED {
        assert_eq!(
            failed(parse::hold(&run(100, needle, ""))).0,
            Reason::DpkgInterrupted,
            "{needle}"
        );
    }
    let (reason, detail) = failed(parse::hold(&run(
        100,
        "",
        "E: Unable to locate package hello\n",
    )));
    assert_eq!(
        (reason, detail.as_str()),
        (
            Reason::IoError,
            "apt-mark: E: Unable to locate package hello"
        )
    );
}

/// Proves a purge that exits 0 is done, a held lock is `Ok(false)` (to be tried again), an
/// interrupted dpkg is `dpkg_interrupted`, a dependent package is `dependency_changes` and any
/// other failure is `dpkg_error`.
#[test]
fn purge_maps_each_outcome() {
    assert_eq!(
        parse::purge(&run(0, "Removing hello (1.0.0-1) ...\n", "")),
        Ok(true)
    );
    for needle in BUSY {
        let out = run(2, "", &format!("dpkg: error: {needle} with pid 1\n"));
        assert_eq!(parse::purge(&out), Ok(false), "{needle}");
    }
    let interrupted = run(
        2,
        "",
        "dpkg: error: dpkg was interrupted, you must manually run 'dpkg --configure -a'\n",
    );
    let (reason, detail) = failed(parse::purge(&interrupted));
    assert_eq!(reason, Reason::DpkgInterrupted);
    assert_eq!(
        detail,
        "dpkg --purge: dpkg: error: dpkg was interrupted, you must manually run 'dpkg --configure -a'"
    );
    for needle in DEPENDENTS {
        let out = run(2, "", &format!("dpkg: {needle}\n"));
        assert_eq!(
            failed(parse::purge(&out)).0,
            Reason::DependencyChanges,
            "{needle}"
        );
    }
    let other = run(
        1,
        "",
        "dpkg: error processing package hello (--purge):\n subprocess installed pre-removal script returned error exit status 1\n",
    );
    let (reason, detail) = failed(parse::purge(&other));
    assert_eq!(reason, Reason::DpkgError);
    assert_eq!(
        detail,
        "dpkg --purge: subprocess installed pre-removal script returned error exit status 1"
    );
}

/// Proves a package name needs 2 to 128 bytes of `[a-z0-9+.-]` with a lower-case letter or digit
/// first.
#[test]
fn a_package_name_follows_dpkgs_grammar() {
    for good in [
        "ab",
        "mdm",
        "libc6",
        "1a",
        "a+b.c-d",
        "g++",
        &"a".repeat(128),
    ] {
        assert!(is_package(good), "{good}");
    }
    for bad in [
        "",
        "a",
        "Ab",
        "aB",
        "-a",
        "+a",
        ".a",
        "a_b",
        "a b",
        "a:b",
        "é1",
        &"a".repeat(129),
    ] {
        assert!(!is_package(bad), "{bad:?}");
    }
}

/// Proves a version is `[epoch:]upstream[-revision]` with an upstream that starts with a digit,
/// and refuses an empty revision, a non-digit or empty epoch, a bad byte and more than 128 bytes.
#[test]
fn a_version_follows_dpkgs_grammar() {
    for good in [
        "1.0",
        "1",
        "1.0.0-1",
        "1:2.0-1",
        "0:1.0",
        "1.0~rc1-1~bpo12+1",
        "1.0-1-2",
        "2.0+dfsg-1.1",
        &format!("1{}", "0".repeat(127)),
    ] {
        assert!(is_version(good), "{good}");
    }
    for bad in [
        "",
        "x-",
        "1.0-",
        "x:1.0",
        ":1.0",
        "a1",
        "-1",
        "1.0_1",
        "1.0-1_2",
        "1.0-1:2",
        "99999999999999999999:1",
        "1 0",
        &format!("1{}", "0".repeat(128)),
    ] {
        assert!(!is_version(bad), "{bad:?}");
    }
}

/// Proves the order of versions is dpkg's, pair by pair as `dpkg --compare-versions` answers:
/// `~` before the end, the end before letters, letters before other bytes, numbers by value,
/// the epoch first, and `1.0` equal to `1.0-0`.
#[test]
fn versions_compare_as_dpkg_does() {
    use Ordering::{Equal, Greater, Less};
    for (a, b, order) in [
        ("1.0", "1.0-0", Equal),
        ("12", "1a", Greater),
        ("1.0", "1.0-1", Less),
        ("1.0~rc1", "1.0", Less),
        ("1.0~~", "1.0~", Less),
        ("1.0~", "1.0", Less),
        ("1.0~rc1", "1.0~rc2", Less),
        ("1.0a", "1.0+", Less),
        ("1.0a", "1.0", Greater),
        ("1.0", "1.0a", Less),
        ("1.a", "1.0", Greater),
        ("1.~", "1.0", Less),
        ("1~a", "1~", Greater),
        ("1.a", "1+", Greater),
        ("1a", "1.a", Less),
        ("1.0.1", "1.0", Greater),
        ("1.0.0", "1.0", Greater),
        ("1.001", "1.1", Equal),
        ("00", "0", Equal),
        ("1.0010", "1.9", Greater),
        ("1.2.3", "1.2.10", Less),
        ("19", "20", Less),
        ("1.29", "1.30", Less),
        ("1.199", "1.200", Less),
        ("1:0.1", "2.0", Greater),
        ("2:1.0", "1:9.9", Greater),
        ("0:1.0", "1.0", Equal),
        ("2.2", "2.10", Less),
        ("5", "5.0", Less),
        ("1.0+dfsg", "1.0", Greater),
        ("1.0~rc1-1", "1.0-1", Less),
        ("1.0-1", "1.0-2", Less),
        ("1.0-10", "1.0-9", Greater),
        ("1.0-1a", "1.0-1", Greater),
        ("1.0-1~a", "1.0-1", Less),
        ("1.0-1.1", "1.0-1", Greater),
        ("1.0-1+b1", "1.0-1", Greater),
        ("1.0.0-1~bpo12+1", "1.0.0-1", Less),
        ("1.0-1", "1.0-1", Equal),
    ] {
        assert_eq!(compare(a, b), Some(order), "{a} {b}");
        assert_eq!(compare(b, a), Some(order.reverse()), "{b} {a}");
    }
}

/// Proves a comparison with a string that is not a dpkg version is `None`, on either side.
#[test]
fn comparing_an_invalid_version_is_none() {
    for bad in ["", "1.0-", "x1", "x:1", "1.0_1"] {
        assert_eq!(compare(bad, "1.0"), None, "{bad:?}");
        assert_eq!(compare("1.0", bad), None, "{bad:?}");
    }
}

/// The reasons of packages-repositories.md section 4.5 (and of 2.3 and packages-native.md 10.2
/// for the rest), each with its token, class and outcome, and its place in `Reason::ALL`.
const TABLE: [(Reason, &str, Class, Outcome); 18] = [
    (
        Reason::CoordinatorTooOld,
        "coordinator_too_old",
        Class::Environment,
        Outcome::Deferred,
    ),
    (
        Reason::DependencyChanges,
        "dependency_changes",
        Class::Environment,
        Outcome::Failed,
    ),
    (
        Reason::DependencyMissing,
        "dependency_missing",
        Class::Environment,
        Outcome::Failed,
    ),
    (
        Reason::DpkgError,
        "dpkg_error",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::DpkgInterrupted,
        "dpkg_interrupted",
        Class::Environment,
        Outcome::Failed,
    ),
    (
        Reason::FetchFailed,
        "fetch_failed",
        Class::Environment,
        Outcome::Deferred,
    ),
    (
        Reason::IoError,
        "io_error",
        Class::Environment,
        Outcome::Failed,
    ),
    (
        Reason::ManifestInvalid,
        "manifest_invalid",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::NotInRepository,
        "not_in_repository",
        Class::Environment,
        Outcome::Deferred,
    ),
    (
        Reason::OrderInvalid,
        "order_invalid",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::PackageConflict,
        "package_conflict",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::PackageDigestMismatch,
        "package_digest_mismatch",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::PackageManagerBusy,
        "package_manager_busy",
        Class::Environment,
        Outcome::Deferred,
    ),
    (
        Reason::PackageScripts,
        "package_scripts",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::PackageTooLarge,
        "package_too_large",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::RepositoryUnknown,
        "repository_unknown",
        Class::Environment,
        Outcome::Failed,
    ),
    (
        Reason::Signature,
        "signature",
        Class::Environment,
        Outcome::Failed,
    ),
    (
        Reason::StoreDigestMismatch,
        "store_digest_mismatch",
        Class::Environment,
        Outcome::Deferred,
    ),
];

/// A place for each reason; a new reason without an arm does not compile.
fn place(reason: Reason) -> usize {
    match reason {
        Reason::CoordinatorTooOld => 0,
        Reason::DependencyChanges => 1,
        Reason::DependencyMissing => 2,
        Reason::DpkgError => 3,
        Reason::DpkgInterrupted => 4,
        Reason::FetchFailed => 5,
        Reason::IoError => 6,
        Reason::ManifestInvalid => 7,
        Reason::NotInRepository => 8,
        Reason::OrderInvalid => 9,
        Reason::PackageConflict => 10,
        Reason::PackageDigestMismatch => 11,
        Reason::PackageManagerBusy => 12,
        Reason::PackageScripts => 13,
        Reason::PackageTooLarge => 14,
        Reason::RepositoryUnknown => 15,
        Reason::Signature => 16,
        Reason::StoreDigestMismatch => 17,
    }
}

/// Proves `Reason::ALL` holds every reason once, in token order, each token is unique and of
/// `[a-z_]`, and each reason's class and outcome are those of section 4.5's table.
#[test]
fn every_reason_has_its_token_class_and_outcome() {
    assert_eq!(Reason::ALL.len(), TABLE.len());
    for (index, reason) in Reason::ALL.into_iter().enumerate() {
        let (want, token, class, outcome) = TABLE[index];
        assert_eq!(reason, want, "ALL is in the table's order at {index}");
        assert_eq!(place(reason), index);
        assert_eq!(reason.token(), token);
        assert_eq!(reason.class(), class, "{token}");
        assert_eq!(reason.outcome(), outcome, "{token}");
        assert!(
            token.bytes().all(|b| b.is_ascii_lowercase() | (b == b'_')),
            "{token}"
        );
    }
    let tokens: Vec<&str> = Reason::ALL.iter().map(|r| r.token()).collect();
    let mut sorted = tokens.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        tokens, sorted,
        "ALL is sorted by token, with no token twice"
    );
}

/// Proves the class tokens are the verdict's spellings.
#[test]
fn a_class_has_its_verdict_token() {
    assert_eq!(Class::Release.token(), "release");
    assert_eq!(Class::Environment.token(), "environment");
    assert_eq!(DRIFT_REPAIRED, "drift_repaired");
}

/// Proves a failure shows its reason's token and its detail, and answers both separately.
#[test]
fn a_failure_displays_its_token_and_detail() {
    let failure = Failure::new(Reason::DpkgError, "apt-get install: E: broke");
    assert_eq!(failure.reason(), Reason::DpkgError);
    assert_eq!(failure.detail(), "apt-get install: E: broke");
    assert_eq!(failure.to_string(), "dpkg_error: apt-get install: E: broke");
    assert_eq!(
        Failure::new(Reason::Signature, String::from("x")).to_string(),
        "signature: x"
    );
}
