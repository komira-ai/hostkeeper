//! The `macos-pkg` and `check-macos-pkg` verbs from their command lines: each flag reaches the
//! pkg builder or the check, and neither verb asks for adapters it does not read.

use super::tests::{connects, empty, parse, text, verb};
use super::*;
use crate::release::fakes::scratch;

#[test]
fn check_macos_pkg_reads_the_expanded_pkg_without_asking_for_adapters() {
    // Catches: the expanded directory, `--version` or `--binary` not reaching the check, or the
    // verb building the real adapters though it reads no port.
    let d = scratch("release-check-macos-pkg-verb");
    let absent = d.join("expanded");
    let (out, connected) = connects(verb(&[
        "check-macos-pkg",
        text(&absent),
        "--version",
        "0.1.9",
        "--binary",
        text(&d.join("hostkeeper")),
        "--gh",
        text(&d.join("gh")),
        "--trusted-root",
        text(&d.join("trusted_root.json")),
    ]));
    assert!(
        matches!(&out, Err(Error::Io { what, .. }) if *what == format!("read {}", absent.display())),
        "{out:?}"
    );
    assert!(!connected);
}

#[test]
fn check_macos_pkg_takes_the_gh_and_the_trusted_root_it_checks_against() {
    // Catches: `--gh` or `--trusted-root` not reaching the check, or either left optional, so that
    // the pkg is checked without them.
    let base = [
        "check-macos-pkg",
        "expanded",
        "--version",
        "0.1.9",
        "--binary",
        "b",
    ];
    let full: Vec<&str> = base
        .into_iter()
        .chain(["--gh", "g", "--trusted-root", "r"])
        .collect();
    assert!(matches!(
        verb(&full),
        Verb::CheckMacosPkg { gh, trusted_root, .. }
            if gh == Path::new("g") && trusted_root == Path::new("r")
    ));
    for without in [["--gh", "g"], ["--trusted-root", "r"]] {
        let args: Vec<&str> = base.into_iter().chain(without).collect();
        let parsed = parse(&args);
        assert!(parsed.is_err(), "{args:?} parsed without a required flag");
    }
}

#[test]
fn macos_pkg_builds_through_the_pkgbuild_port_with_its_flags() {
    // Catches: `--version`, `--binary`, `--gh`, `--trusted-root`, `--stage` or `--out` not
    // reaching the pkg builder, or the builder run without the pkgbuild port.
    let d = scratch("release-macos-pkg-verb");
    std::fs::write(d.join("hostkeeper"), b"bin").expect("write the binary");
    std::fs::write(d.join("gh"), b"gh").expect("write gh");
    std::fs::write(d.join("trusted_root.json"), b"{}\n").expect("write the trusted root");
    let fakes = empty();
    let stage = d.join("root");
    let out = d.join("dist");
    let build = verb(&[
        "macos-pkg",
        "--version",
        "0.1.9",
        "--binary",
        text(&d.join("hostkeeper")),
        "--gh",
        text(&d.join("gh")),
        "--trusted-root",
        text(&d.join("trusted_root.json")),
        "--stage",
        text(&stage),
        "--out",
        text(&out),
    ]);
    let lines = run_with(build, &fakes.ports()).expect("build");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("hostkeeper-0.1.9-macos-arm64.pkg: sha256 "));
    let calls = fakes.pkgbuild.calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        (
            calls[0].root.as_path(),
            calls[0].scripts.as_path(),
            calls[0].pkg.as_path(),
            calls[0].version.to_string()
        ),
        (
            stage.join("root").as_path(),
            stage.join("scripts").as_path(),
            out.join("hostkeeper-0.1.9-macos-arm64.pkg").as_path(),
            "0.1.9".to_owned()
        )
    );
    let staged = |path: &str| std::fs::read(stage.join("root").join(path)).expect("a staged file");
    assert_eq!(staged("usr/local/bin/hostkeeper"), b"bin");
    assert_eq!(staged("usr/local/lib/hostkeeper/gh"), b"gh");
    assert_eq!(
        staged("usr/local/lib/hostkeeper/trusted_root.json"),
        b"{}\n"
    );
}
