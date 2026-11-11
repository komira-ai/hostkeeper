//! The verbs of the native packages from their command lines: `deb`, and the `--service` of
//! `macos-pkg`, `check-macos-pkg` and `check-linux-pkg`.

use super::tests::{connects, empty, parse, text, verb};
use super::*;
use crate::release::fakes::scratch;
use crate::release::native::{
    HELLO_PKG_INPUTS, HOSTKEEPER_PKG_INPUTS, MDM_NO_PKG, PXE_NO_PKG, macos_inputs,
};

#[test]
fn deb_takes_its_flags_and_dates_the_package_by_the_commit_in_git() {
    // Catches: a flag that does not reach the builder, or a deb dated otherwise than by the
    // commit's time in git (the fake history does not know the commit, so the build stops
    // there, after the program is read).
    let d = scratch("release-deb-verb");
    std::fs::write(d.join("hello"), b"p").expect("write");
    let commit = "c".repeat(40);
    let v = verb(&[
        "deb",
        "--service",
        "hello",
        "--platform",
        "linux-arm64",
        "--version",
        "0.1.9",
        "--commit",
        &commit,
        "--binary",
        text(&d.join("hello")),
        "--fail-ready",
        "--out",
        text(&d.join("out")),
    ]);
    assert!(matches!(
        &v,
        Verb::Deb {
            service: Service::Hello,
            platform: Platform::LinuxArm64,
            fail_ready: true,
            archives: None,
            ..
        }
    ));
    let e = run_with(v, &empty().ports()).expect_err("unknown commit");
    assert_eq!(e.to_string(), format!("git: {commit}: unknown commit"));
    for missing in [
        "--service",
        "--platform",
        "--version",
        "--commit",
        "--binary",
        "--out",
    ] {
        let mut args = vec![
            "deb",
            "--service",
            "mdm",
            "--platform",
            "linux-x86_64",
            "--version",
            "0.1.9",
            "--commit",
            &commit,
            "--binary",
            "b",
            "--out",
            "o",
        ];
        let at = args.iter().position(|a| *a == missing).expect("the flag");
        args.drain(at..at + 2);
        assert!(parse(&args).is_err(), "{missing} is required");
    }
}

/// `release deb --service pxe` from its command line builds pxe's `.deb` for each Linux
/// platform out of a static program, dated by the commit's time in git, and
/// `release check-linux-pkg <deb> --service pxe` from its command line passes what it built.
///
/// Catches a `--service pxe` that does not reach the pxe builder or the service check, or a
/// deb named otherwise than the release's asset name.
#[test]
fn pxes_deb_and_its_check_run_from_their_command_lines() {
    use crate::release::fakes::{FakeHistory, Fakes};
    use crate::release::package::elf::fixture::{AARCH64, Spec, X86_64};
    let d = scratch("release-deb-verb-pxe");
    let mut history = FakeHistory::default();
    let commit = history.commit(None, &[]).to_string();
    let fakes = Fakes::over(history);
    for (platform, machine, arch) in [
        ("linux-x86_64", X86_64, "amd64"),
        ("linux-arm64", AARCH64, "arm64"),
    ] {
        let binary = d.join(format!("pxe-{arch}"));
        std::fs::write(&binary, Spec::static_pie(machine).build()).expect("write");
        let out = d.join(format!("out-{arch}"));
        let v = verb(&[
            "deb",
            "--service",
            "pxe",
            "--platform",
            platform,
            "--version",
            "0.1.9",
            "--commit",
            &commit,
            "--binary",
            text(&binary),
            "--out",
            text(&out),
        ]);
        assert!(matches!(
            &v,
            Verb::Deb {
                service: Service::Pxe,
                ..
            }
        ));
        run_with(v, &fakes.ports()).expect("pxe's deb");
        let deb = out.join(format!("pxe_0.1.9_{arch}.deb"));
        assert!(deb.is_file(), "{}", deb.display());
        let (checked, connected) =
            connects(verb(&["check-linux-pkg", text(&deb), "--service", "pxe"]));
        assert!(checked.is_ok(), "{arch}: {checked:?}");
        assert!(!connected);
    }
}

#[test]
fn check_linux_pkg_takes_service_or_staged_but_not_both_and_reads_no_port() {
    // Catches: --service ignored, or given with --staged and one of them silently dropped.
    let d = scratch("release-check-linux-pkg-service");
    let absent = d.join("absent.deb");
    let (out, connected) = connects(verb(&[
        "check-linux-pkg",
        text(&absent),
        "--service",
        "hello",
    ]));
    assert!(
        matches!(&out, Err(Error::Io { what, .. }) if what.contains("absent.deb")),
        "{out:?}"
    );
    assert!(!connected);
    let (out, _) = connects(verb(&[
        "check-linux-pkg",
        text(&absent),
        "--service",
        "mdm",
        "--staged",
        "s",
    ]));
    assert_eq!(
        out,
        Err(Error::Failed(
            package::linux_check::SERVICE_AND_STAGED.to_owned()
        ))
    );
}

#[test]
fn macos_pkg_and_its_check_take_hello_without_gh_and_refuse_every_other_mix() {
    // Catches: hello's pkg built or checked with hostkeeper's inputs, hostkeeper's without them
    // or as a failing build, and a pkg of mdm.
    let gh = Some(PathBuf::from("g"));
    let root = Some(PathBuf::from("r"));
    assert_eq!(macos_inputs(Service::Hello, true, None, None), Ok(None));
    assert_eq!(
        macos_inputs(Service::Hostkeeper, false, gh.clone(), root.clone()),
        Ok(Some((PathBuf::from("g"), PathBuf::from("r"))))
    );
    let refused = |why: &str| Err(Error::Failed(why.to_owned()));
    assert_eq!(
        macos_inputs(Service::Hello, false, gh.clone(), None),
        refused(HELLO_PKG_INPUTS)
    );
    assert_eq!(
        macos_inputs(Service::Hello, false, None, root.clone()),
        refused(HELLO_PKG_INPUTS)
    );
    assert_eq!(
        macos_inputs(Service::Mdm, false, None, None),
        refused(MDM_NO_PKG)
    );
    assert_eq!(
        macos_inputs(Service::Pxe, false, None, None),
        refused(PXE_NO_PKG)
    );
    assert_eq!(
        macos_inputs(Service::Hostkeeper, true, gh.clone(), root.clone()),
        refused(package::FAIL_READY_ONLY_HELLO)
    );
    assert_eq!(
        macos_inputs(Service::Hostkeeper, false, gh, None),
        refused(HOSTKEEPER_PKG_INPUTS)
    );

    let d = scratch("release-macos-pkg-hello-verb");
    std::fs::write(d.join("hello"), b"bin").expect("write");
    let fakes = empty();
    let lines = run_with(
        verb(&[
            "macos-pkg",
            "--service",
            "hello",
            "--version",
            "0.1.9",
            "--binary",
            text(&d.join("hello")),
            "--stage",
            text(&d.join("stage")),
            "--out",
            text(&d.join("out")),
        ]),
        &fakes.ports(),
    )
    .expect("built");
    assert!(
        lines[0].starts_with("hello-0.1.9-macos-arm64.pkg: sha256 "),
        "{lines:?}"
    );
    assert_eq!(fakes.pkgbuild.calls.borrow()[0].scripts, None);
    let (out, connected) = connects(verb(&[
        "check-macos-pkg",
        text(&d.join("none")),
        "--service",
        "hello",
        "--version",
        "0.1.9",
        "--binary",
        text(&d.join("hello")),
    ]));
    assert!(
        matches!(&out, Err(Error::Io { what, .. }) if what.contains("none")),
        "{out:?}"
    );
    assert!(!connected);
}
