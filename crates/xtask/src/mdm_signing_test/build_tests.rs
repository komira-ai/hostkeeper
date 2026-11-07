//! The tests of `mdm-signing-test build`, against a fake process runner that writes what each
//! program would write and records every command line.

use std::cell::RefCell;
use std::path::Path;

use plist::Value;

use super::build::{OPENSSL, build, leaf_config, root_config, search_list};
use super::commands::ProfileIdentifier;
use super::{Error, KIT_FILE, Kit, PKG_DIR, PROFILE_FILE};
use crate::release::fakes::scratch;
use crate::release::ports::{Exec, Ran};

/// What `security list-keychains` prints in the tests: the user's one keychain.
const LOGIN: &str = "/k/login.keychain-db";

/// A process runner that records each command line, writes each command's output file with
/// the line as its bytes, prints what `security list-keychains` and `pkgutil` print, and fails the first
/// command whose line holds `fail_on`.
#[derive(Default)]
pub(super) struct FakeMac {
    pub(super) calls: RefCell<Vec<String>>,
    /// Each payload `pkgbuild` was given: its files' names, modes and texts.
    payloads: RefCell<Vec<Vec<(String, u32, String)>>>,
    fail_on: Option<&'static str>,
    /// What `pkgutil --check-signature` prints.
    check: Option<&'static str>,
    /// Fails to start the first program whose line holds this.
    no_start: Option<&'static str>,
    /// What `openssl rand` prints, when not a good password.
    rand: Option<&'static str>,
    /// The mode of `work/` when the first key was written.
    work_mode: RefCell<Option<u32>>,
}

impl FakeMac {
    fn failing(on: &'static str) -> FakeMac {
        FakeMac {
            fail_on: Some(on),
            ..FakeMac::default()
        }
    }
}

/// The file a command line writes, if it writes one.
fn output_of(program: &str, args: &[String]) -> Option<String> {
    let after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    match program {
        OPENSSL => after("-out"),
        "pkgbuild" | "productbuild" | "productsign" => args.last().cloned(),
        _ => None,
    }
}

impl Exec for FakeMac {
    fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Ran> {
        // An argument holding a space is quoted, so the lines show where each one ends.
        let quoted: Vec<String> = args
            .iter()
            .map(|a| {
                if a.contains(' ') {
                    format!("'{a}'")
                } else {
                    a.clone()
                }
            })
            .collect();
        let line = format!("{program} {}", quoted.join(" "));
        self.calls.borrow_mut().push(line.clone());
        if self.no_start.is_some_and(|s| line.contains(s)) {
            return Err(std::io::Error::other("no such program"));
        }
        if self.fail_on.is_some_and(|s| line.contains(s)) {
            return Ok(Ran {
                success: false,
                stdout: Vec::new(),
                stderr: b"it failed\n".to_vec(),
            });
        }
        if program == "pkgbuild" {
            use std::os::unix::fs::PermissionsExt;
            let mut files = Vec::new();
            for entry in std::fs::read_dir(&args[1])? {
                let path = entry?.path();
                let mode = std::fs::metadata(&path)?.permissions().mode() & 0o7777;
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                files.push((name, mode, std::fs::read_to_string(&path)?));
            }
            self.payloads.borrow_mut().push(files);
        }
        if let Some(out) = output_of(program, args) {
            std::fs::write(&out, line.as_bytes())?;
        }
        if program == OPENSSL
            && let Some(key) = args.iter().position(|a| a == "-keyout")
        {
            use std::os::unix::fs::PermissionsExt;
            let path = Path::new(&args[key + 1]);
            if self.work_mode.borrow().is_none() {
                let work = path.parent().unwrap_or(path);
                let mode = std::fs::metadata(work)?.permissions().mode() & 0o7777;
                *self.work_mode.borrow_mut() = Some(mode);
            }
            std::fs::write(path, b"a test key")?;
        }
        let stdout = match (program, args.first().map(String::as_str)) {
            (OPENSSL, Some("rand")) => self
                .rand
                .unwrap_or("0123456789abcdef0123456789abcdef\n")
                .to_owned(),
            ("security", Some("list-keychains")) if args.len() == 3 => {
                format!("    \"{LOGIN}\"\n")
            }
            ("pkgutil", _) => self
                .check
                .unwrap_or("Package \"p\":\n   Status: signed by untrusted certificate\n")
                .to_owned(),
            _ => String::new(),
        };
        Ok(Ran {
            success: true,
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
        })
    }
}

fn farm() -> ProfileIdentifier {
    "com.example.hostkeeper.mdm".parse().expect("an identifier")
}

/// The command lines of `calls` with the directory `dir` written `<kit>`.
fn lines(calls: &[String], dir: &Path) -> Vec<String> {
    let dir = dir.to_str().expect("utf-8");
    calls.iter().map(|c| c.replace(dir, "<kit>")).collect()
}

#[test]
fn the_build_runs_the_ca_pkg_and_signing_commands_in_order() {
    // Catches: a command dropped, reordered or given other flags: the identity signing pkg 4
    // being A's, a pkg left unsigned, the keychain not granted to productsign, the search list
    // not put back, or the keychain not deleted.
    let dir = scratch("signing-build");
    let out = dir.join("kit");
    let mac = FakeMac::default();
    build(&mac, &out, &farm()).expect("the kit builds");
    let calls = lines(&mac.calls.borrow(), &out);
    let pw = calls
        .iter()
        .find_map(|c| c.strip_prefix("security create-keychain -p "))
        .and_then(|rest| rest.split(' ').next())
        .expect("a keychain is made")
        .to_owned();
    assert_eq!(pw.len(), 32);
    let want = include_str!("build_golden.txt")
        .lines()
        .map(|l| l.replace("<pw>", &pw))
        .collect::<Vec<_>>();
    assert_eq!(calls, want);
}

#[test]
fn the_kit_holds_four_pkgs_the_root_profile_and_no_key() {
    // Catches: a key, a keychain, root B or a work file left in the kit; a pkg not under its
    // digest; kit.toml not naming each pkg's identifier, digest and signature check.
    let dir = scratch("signing-kit");
    let out = dir.join("kit");
    let mac = FakeMac::default();
    let printed = build(&mac, &out, &farm()).expect("the kit builds");
    // The keys were written into a directory only the build's user reads.
    assert_eq!(*mac.work_mode.borrow(), Some(0o700));
    // Each pkg's payload is its one marker file, which names it, readable by all.
    let payloads = mac.payloads.borrow();
    let want: Vec<Vec<(String, u32, String)>> = super::PKGS
        .iter()
        .map(|p| vec![(format!("pkg-{}", p.number), 0o644, super::build::marker(p))])
        .collect();
    assert_eq!(*payloads, want);
    assert_eq!(
        want[2][0].2,
        "hostkeeper pkg signing test: pkg 3, distribution pkg signed under the root the profile \
         installs\n"
    );
    let mut top: Vec<String> = std::fs::read_dir(&out)
        .expect("the kit")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    top.sort();
    assert_eq!(top, [KIT_FILE, PKG_DIR, PROFILE_FILE]);
    let kit = Kit::read(&out).expect("kit.toml reads back");
    assert_eq!(kit.profile_identifier, "com.example.hostkeeper.mdm");
    assert_eq!(
        kit.root_profile.identifier,
        "com.example.hostkeeper.mdm.signing-test.root"
    );
    let mut served: Vec<String> = std::fs::read_dir(out.join(PKG_DIR))
        .expect("the pkg directory")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    served.sort();
    let mut digests: Vec<String> = kit
        .pkgs
        .iter()
        .map(|p| format!("{}.pkg", p.sha256))
        .collect();
    digests.sort();
    assert_eq!(served, digests);
    for (pkg, n) in kit.pkgs.iter().zip(1..) {
        assert_eq!(
            pkg.identifier,
            format!("dev.hostkeeper.signing-test.pkg{n}")
        );
        assert_eq!(pkg.version, "1");
        let bytes =
            std::fs::read(out.join(PKG_DIR).join(format!("{}.pkg", pkg.sha256))).expect("the pkg");
        let text = String::from_utf8(bytes).expect("the fake's bytes");
        let want_tool = if n == 1 {
            "pkgbuild"
        } else if n == 2 {
            "productbuild"
        } else {
            "productsign"
        };
        assert!(
            text.starts_with(want_tool),
            "pkg {n} was last written by {text}"
        );
        let signed = n >= 3;
        assert_eq!(
            pkg.build_check_signature,
            if signed {
                "Package \"p\":\n   Status: signed by untrusted certificate"
            } else {
                ""
            }
        );
    }
    // The root profile holds the DER root openssl wrote: the fake's command line.
    let profile = Value::from_file(out.join(PROFILE_FILE)).expect("the profile is a plist");
    let cert = profile
        .as_dictionary()
        .and_then(|d| d.get("PayloadContent"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_dictionary)
        .expect("one payload");
    let der = cert
        .get("PayloadContent")
        .and_then(Value::as_data)
        .expect("the root");
    let der = String::from_utf8_lossy(der);
    assert!(der.starts_with(&format!("{OPENSSL} x509 -in ")), "{der}");
    assert!(der.ends_with("root-a.cer"), "{der}");
    assert!(der.contains("root-a.pem"), "{der}");
    assert_eq!(printed.len(), 6);
    assert_eq!(
        printed[0],
        format!(
            "{PROFILE_FILE}: the profile com.example.hostkeeper.mdm.signing-test.root installs \
             root A (sha256 {})",
            kit.root_profile.root_sha256
        )
    );
    assert_eq!(
        printed[3],
        format!(
            "pkg-3: distribution pkg signed under the root the profile installs: pkg/{}.pkg",
            kit.pkgs[2].sha256
        )
    );
    assert_eq!(
        printed[5],
        "kit.toml: written; every key and root B are deleted with work/"
    );
}

#[test]
fn a_kit_directory_that_exists_is_refused_before_any_command() {
    // Catches: a build writing into a directory that holds something else, or a kit of an
    // earlier build mixed with this one's.
    let dir = scratch("signing-exists");
    let mac = FakeMac::default();
    let got = build(&mac, &dir, &farm());
    assert_eq!(
        got,
        Err(Error::Refused(format!(
            "{} exists: the kit is built into a new directory",
            dir.display()
        )))
    );
    assert!(mac.calls.borrow().is_empty());
}

#[test]
fn a_failed_command_deletes_the_work_directory_and_its_keys() {
    // Catches: a failure part-way leaving the test CAs' private keys in the kit.
    for (fail, what) in [
        ("rand -hex", "rand -hex 16"),
        ("installer-b.csr -CA", "x509 -req"),
        ("pkgbuild", "pkgbuild --root"),
        ("productbuild", "productbuild --package"),
        ("pkcs12", "pkcs12 -export"),
        ("root-a.cer", "-outform DER"),
    ] {
        let dir = scratch("signing-fail");
        let out = dir.join("kit");
        let mac = FakeMac::failing(fail);
        let got = build(&mac, &out, &farm());
        let Err(Error::Port(e)) = got else {
            panic!("{fail}: {got:?}")
        };
        assert!(e.what.contains(what), "{fail}: {}", e.what);
        assert_eq!(e.why, "it failed");
        assert!(!out.join("work").exists(), "{fail}: work/ is left");
        assert!(
            !e.what.contains("pass:") || e.what.contains("pass:<password>"),
            "{}",
            e.what
        );
    }
}

#[test]
fn a_failed_signing_still_restores_the_search_list_and_deletes_the_keychain() {
    // Catches: a productsign or import failure leaving the test keychain on the user's search
    // list, or on disk.
    for fail in [
        "productsign",
        "security import",
        "set-key-partition-list",
        "unlock-keychain",
    ] {
        let dir = scratch("signing-sign-fail");
        let out = dir.join("kit");
        let mac = FakeMac::failing(fail);
        let got = build(&mac, &out, &farm());
        assert!(matches!(got, Err(Error::Port(_))), "{fail}: {got:?}");
        let calls = lines(&mac.calls.borrow(), &out);
        let n = calls.len();
        assert_eq!(
            calls[n - 2],
            format!("security list-keychains -d user -s {LOGIN}"),
            "{fail}"
        );
        assert_eq!(
            calls[n - 1],
            "security delete-keychain <kit>/work/signing-test.keychain-db",
            "{fail}"
        );
    }
}

#[test]
fn a_password_never_appears_in_an_error() {
    // Catches: the keychain's password, which unlocks the test identities, printed in a
    // failure's message.
    let dir = scratch("signing-pw");
    let out = dir.join("kit");
    let mac = FakeMac::failing("unlock-keychain");
    let got = build(&mac, &out, &farm());
    let Err(Error::Port(e)) = got else {
        panic!("{got:?}")
    };
    assert_eq!(
        e.what.replace(out.to_str().expect("utf-8"), "<kit>"),
        "security unlock-keychain -p <password> <kit>/work/signing-test.keychain-db"
    );
}

#[test]
fn a_restore_or_delete_failure_after_a_good_signing_fails_the_build() {
    // Catches: the keychain left on the search list, or on disk, with the build reported done.
    for fail in ["-s /k/login", "delete-keychain"] {
        let dir = scratch("signing-restore");
        let out = dir.join("kit");
        let got = build(&FakeMac::failing(fail), &out, &farm());
        let Err(Error::Port(e)) = got else {
            panic!("{fail}: {got:?}")
        };
        assert!(e.what.contains(fail), "{}", e.what);
        assert!(!out.join(KIT_FILE).exists());
    }
}

#[test]
fn a_signed_pkg_pkgutil_does_not_read_as_signed_is_refused() {
    // Catches: productsign exiting 0 and writing an unsigned pkg, which would be sent as pkg 3.
    let dir = scratch("signing-unsigned");
    let out = dir.join("kit");
    let mac = FakeMac {
        check: Some("Package \"p\":\n   Status: no signature\n"),
        ..FakeMac::default()
    };
    let got = build(&mac, &out, &farm());
    let Err(Error::Refused(why)) = got else {
        panic!("{got:?}")
    };
    assert_eq!(
        why.replace(out.to_str().expect("utf-8"), "<kit>"),
        "pkgutil --check-signature <kit>/work/signed-3.pkg does not read pkg 3 as signed: \
         Package \"p\":\n   Status: no signature"
    );
    assert!(!out.join("work").exists());
}

#[test]
fn a_program_that_does_not_start_is_named() {
    // Catches: a missing tool (a Linux host has no pkgbuild or pkgutil) reported without its
    // command line.
    for (missing, what) in [
        ("pkgbuild", "pkgbuild --root"),
        ("pkgutil", "pkgutil --check-signature"),
    ] {
        let dir = scratch("signing-nostart");
        let out = dir.join("kit");
        let mac = FakeMac {
            no_start: Some(missing),
            ..FakeMac::default()
        };
        let Err(Error::Port(e)) = build(&mac, &out, &farm()) else {
            panic!("{missing}")
        };
        assert!(e.what.starts_with(what), "{}", e.what);
        assert_eq!(e.why, "no such program");
    }
}

#[test]
fn the_certificates_are_a_ca_and_a_code_signing_identity_under_it() {
    // Catches: a root without CA:TRUE (no chain to trust), or an identity without the
    // digitalSignature key usage and the codeSigning extended key usage an installer
    // signature is checked for.
    assert_eq!(
        root_config("R"),
        "[ req ]\ndistinguished_name = dn\nprompt = no\n\n[ dn ]\nCN = R\n\n[ root_ext ]\n\
         basicConstraints = critical, CA:TRUE\nkeyUsage = critical, keyCertSign, cRLSign\n\
         subjectKeyIdentifier = hash\n"
    );
    assert_eq!(
        leaf_config("L"),
        "[ req ]\ndistinguished_name = dn\nprompt = no\n\n[ dn ]\nCN = L\n\n[ leaf_ext ]\n\
         basicConstraints = critical, CA:FALSE\nkeyUsage = critical, digitalSignature\n\
         extendedKeyUsage = codeSigning\nsubjectKeyIdentifier = hash\n\
         authorityKeyIdentifier = keyid\n"
    );
}

#[test]
fn the_search_list_is_read_as_security_prints_it() {
    // Catches: the quotes or the indent kept in a path, or a blank line read as a keychain,
    // so the list put back would name keychains that do not exist.
    assert_eq!(
        search_list(
            b"    \"/a/login.keychain-db\"\n\n    \"/Library/Keychains/System.keychain\"\n"
        ),
        ["/a/login.keychain-db", "/Library/Keychains/System.keychain"]
    );
    assert_eq!(search_list(b""), Vec::<String>::new());
}

#[test]
fn a_kit_path_that_is_not_utf8_is_refused_before_any_file_is_made() {
    // Catches: a path the command lines cannot carry passed to openssl in a lossy form, so a
    // program would write its key or pkg elsewhere.
    use std::os::unix::ffi::OsStrExt;
    let dir = scratch("signing-utf8");
    let out = dir.join(std::ffi::OsStr::from_bytes(b"kit-\xff"));
    let mac = FakeMac::default();
    let got = build(&mac, &out, &farm());
    assert_eq!(
        got,
        Err(Error::Refused(format!(
            "{} is not UTF-8",
            out.join("work").join("root-a.key").display()
        )))
    );
    // Only the password is made before the first path is needed.
    assert_eq!(*mac.calls.borrow(), [format!("{OPENSSL} rand -hex 16")]);
    assert!(!out.join("work").exists());
}

#[test]
fn a_password_that_is_not_32_hex_digits_is_refused() {
    // Catches: an empty or short output of `openssl rand` taken as the keychain's password.
    for printed in ["", "abc\n", "0123456789ABCDEF0123456789ABCDEF\n"] {
        let dir = scratch("signing-rand");
        let out = dir.join("kit");
        let mac = FakeMac {
            rand: Some(printed),
            ..FakeMac::default()
        };
        let got = build(&mac, &out, &farm());
        assert_eq!(
            got,
            Err(Error::Refused(format!(
                "{OPENSSL} rand -hex 16 printed {} bytes that are not 32 hex digits",
                printed.trim().len()
            ))),
            "{printed:?}"
        );
        assert!(!out.join("work").exists());
    }
}
