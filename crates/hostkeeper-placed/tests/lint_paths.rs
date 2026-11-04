//! How the command lines of a package's unit files are read when they are judged against the
//! packages directory (`lint.rs`): systemd escapes and quotes, redundant separators and dot
//! components, a packages directory other than the Linux one, and launchd plists with comments
//! and character references.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::{Expected, Manifest};
use hostkeeper_placed::lint::{LintError, check_unit_text, lint, through_current};
use hostkeeper_placed::roots::PACKAGES_DIR;
use hostkeeper_placed::{BootView, PlacedError, Roots};
use support::{
    RUNNING, Spec, TARGET, WANTS, World, install, manifest_text, service, unit_text, version,
};

const GOOD: &str = "/var/lib/hostkeeper/packages/tierstore/current/bin/tierstore";
const BAD: &str = "/var/lib/hostkeeper/packages/tierstore/1.5.0/bin/tierstore";
const STATE_PACKAGES: &str = "/state/pkgs";
const STATE_BAD: &str = "/state/pkgs/tierstore/1.5.0/bin/tierstore";
const STATE_GOOD: &str = "/state/pkgs/tierstore/current/bin/tierstore";

fn refusal(packages: &str, file: &str, key: &str, path: &str) -> LintError {
    LintError::PackagePath {
        file: file.to_owned(),
        key: key.to_owned(),
        path: path.to_owned(),
        packages: packages.to_owned(),
        service: "tierstore".to_owned(),
    }
}

/// The result of judging a `tierstore.service` whose `[Service]` holds `line`, against the Linux
/// packages directory.
fn service_line(line: &str) -> Result<(), LintError> {
    service_line_in(PACKAGES_DIR, line)
}

fn service_line_in(packages: &str, line: &str) -> Result<(), LintError> {
    check_unit_text(
        Path::new(packages),
        "tierstore",
        "tierstore.service",
        &format!("[Service]\n{line}\n"),
    )
}

/// The refusal `service_line` gives for `path`.
fn refused(path: &str) -> Result<(), LintError> {
    Err(refusal(
        PACKAGES_DIR,
        "tierstore.service",
        "ExecStart=",
        path,
    ))
}

/// Catches an escaped spelling of a version path getting past the check: `\x2f` and `\057` for
/// the first `/`, `\x31` for a digit in the middle, an escaped `/` itself, and each of those
/// behind a `-` prefix are refused with the decoded path; the same escapes of a `current` path
/// are accepted, and a `\x` with no hex digits stands for `x`.
#[test]
fn escaped_command_paths_are_decoded_before_they_are_judged() {
    assert_eq!(
        service_line(r"ExecStart=\x2fvar/lib/hostkeeper/packages/tierstore/1.5.0/bin/tierstore"),
        refused(BAD)
    );
    assert_eq!(
        service_line(r"ExecStart=\057var/lib/hostkeeper/packages/tierstore/1.5.0/bin/tierstore"),
        refused(BAD)
    );
    assert_eq!(
        service_line(r"ExecStart=/var/lib/hostkeeper/packages/tierstore/\x31.5.0/bin/tierstore"),
        refused(BAD)
    );
    assert_eq!(
        service_line(r"ExecStart=\/var/lib/hostkeeper/packages/tierstore/1.5.0/bin/tierstore"),
        refused(BAD)
    );
    assert_eq!(
        service_line(r"ExecStart=-\x2fvar/lib/hostkeeper/packages/tierstore/1.5.0/bin/tierstore"),
        refused(BAD)
    );
    assert_eq!(
        service_line(
            r#"ExecStart="\x2fvar/lib/hostkeeper/packages/tierstore/1.5.0/bin/tierstore""#
        ),
        refused(BAD)
    );
    assert_eq!(
        service_line(r"ExecStart=\x2fvar/lib/hostkeeper/packages/tierstore/current/bin/tierstore"),
        Ok(())
    );
    assert_eq!(
        service_line(r"ExecStart=\057var/lib/hostkeeper/packages/tierstore/current/bin/tierstore"),
        Ok(())
    );
    assert_eq!(service_line(r"ExecStart=/usr/bin/\xZZ"), Ok(()));
    assert_eq!(service_line(r"ExecStart=/usr/bin/a\8"), Ok(()));
}

/// Catches quotes and spacing escapes split or joined wrongly: a quoted path with a space is one
/// word and refused with the space in it; `\s`, `\n` and `\t` do not split a word, while a plain
/// space does; an escaped quote opens no quote; an unterminated quote still yields its word; a
/// continuation joins the next line; a quoted `--flag=<path>` is judged by its value.
#[test]
fn quotes_and_spacing_escapes_make_the_words_systemd_makes() {
    assert_eq!(
        service_line(&format!("ExecStart=\"{BAD} two\" --x")),
        refused(&format!("{BAD} two"))
    );
    assert_eq!(
        service_line(&format!("ExecStart='{BAD}' --x")),
        refused(BAD)
    );
    for escape in ["s", "n", "t"] {
        assert_eq!(
            service_line(&format!("ExecStart=/usr/bin/x\\{escape}{BAD}")),
            Ok(()),
            "{escape}"
        );
    }
    assert_eq!(
        service_line(&format!("ExecStart=/usr/bin/x {BAD}")),
        refused(BAD)
    );
    assert_eq!(
        service_line(&format!("ExecStart=/usr/bin/x \\\"{BAD}")),
        Ok(())
    );
    assert_eq!(service_line(&format!("ExecStart=\"{BAD}")), refused(BAD));
    assert_eq!(
        service_line(&format!("ExecStart=/usr/bin/true \\\n{BAD}")),
        refused(BAD)
    );
    assert_eq!(
        service_line(&format!("ExecStart={GOOD} --config=\"{BAD}\"")),
        refused(BAD)
    );
    assert_eq!(
        service_line(&format!("ExecStart={GOOD} \"a b\" 'c d'")),
        Ok(())
    );
}

/// Catches a spelling of a path that the text comparison misses: doubled slashes, `.` and `..`
/// components that lead into a version directory or another package are refused with the path as
/// written, and ones that stay in `current`, or leave the packages directory, are accepted; a
/// `--flag=` value is resolved the same way.
#[test]
fn separators_and_dot_components_are_resolved_before_the_check() {
    let pinned = [
        "//var//lib/hostkeeper/packages/tierstore/1.5.0/x",
        "/var/lib/./hostkeeper/packages/tierstore/1.5.0/x",
        "/var/lib/hostkeeper/packages/tierstore/current/../1.5.0/x",
        "/var/lib/hostkeeper/packages/tierstore/current/../../other/current/x",
        "/var/lib/hostkeeper/packages/other/../tierstore/1.5.0/x",
        "/var/lib/hostkeeper/state/../packages/tierstore/1.5.0/x",
        "/usr/../var/lib/hostkeeper/packages/tierstore/1.5.0/x",
        "/../var/lib/hostkeeper/packages/tierstore/1.5.0/x",
    ];
    for path in pinned {
        assert_eq!(
            service_line(&format!("ExecStart={path}")),
            refused(path),
            "{path}"
        );
    }
    let kept = [
        "/var/lib/hostkeeper/packages/tierstore/current/./bin/x",
        "//var/lib/hostkeeper/packages//tierstore//current//bin/x",
        "/var/lib/hostkeeper/packages/tierstore/1.5.0/../current/bin/x",
        "/var/lib/hostkeeper/packages/../state/x",
        "/..",
    ];
    for path in kept {
        assert_eq!(service_line(&format!("ExecStart={path}")), Ok(()), "{path}");
    }
    let flag = "//var/lib/./hostkeeper/packages/tierstore/1.5.0/c";
    assert_eq!(
        service_line(&format!("ExecStart={GOOD} --config={flag}")),
        refused(flag)
    );
}

/// Catches `through_current` judging the Linux directory whatever the root is: it takes the
/// packages directory given (with or without a trailing slash), the packages directory itself and
/// a relative path are outside, and only `<packages>/<service>/current/<something>` is inside.
#[test]
fn through_current_follows_the_packages_directory_it_is_given() {
    for packages in ["/state/pkgs", "/state/pkgs/"] {
        let judge = |path: &str| through_current(Path::new(packages), "tierstore", path);
        assert!(judge(STATE_GOOD), "{packages}");
        assert!(judge("/state/pkgs"), "{packages}");
        assert!(judge("/state/pkgs/"), "{packages}");
        assert!(judge("/state/pkgsx/tierstore/1.5.0/x"), "{packages}");
        assert!(
            judge(BAD),
            "{packages}: the Linux directory is another place"
        );
        assert!(judge("relative/path"), "{packages}");
        assert!(!judge(STATE_BAD), "{packages}");
        assert!(!judge("/state/pkgs/tierstore"), "{packages}");
        assert!(!judge("/state/pkgs/tierstore/current"), "{packages}");
        assert!(!judge("/state/pkgs/other/current/x"), "{packages}");
        assert!(
            !judge("/state/pkgs/tierstore/current/../1.5.0/x"),
            "{packages}"
        );
    }
}

/// Catches a unit judged against the Linux packages directory when the host keeps its packages
/// elsewhere: with `/state/pkgs`, a command pinned to a version there is refused naming
/// `/state/pkgs`, a `current` path there is accepted, and the Linux version path is outside.
#[test]
fn a_nondefault_packages_directory_is_the_one_judged() {
    let line = |path: &str| service_line_in(STATE_PACKAGES, &format!("ExecStart={path}"));
    assert_eq!(
        line(STATE_BAD),
        Err(refusal(
            STATE_PACKAGES,
            "tierstore.service",
            "ExecStart=",
            STATE_BAD
        ))
    );
    assert_eq!(line(STATE_GOOD), Ok(()));
    assert_eq!(line(BAD), Ok(()));
    assert_eq!(
        line(STATE_BAD).expect_err("a pinned path").to_string(),
        format!(
            "units/tierstore.service ExecStart= names {STATE_BAD:?}, which is under /state/pkgs/ but not /state/pkgs/tierstore/current/"
        )
    );
    assert_eq!(
        check_unit_text(
            Path::new(STATE_PACKAGES),
            "tierstore",
            "tierstore.plist",
            &format!("<key>ProgramArguments</key><array><string>{STATE_BAD}</string></array>")
        ),
        Err(refusal(
            STATE_PACKAGES,
            "tierstore.plist",
            "ProgramArguments",
            STATE_BAD
        ))
    );
}

/// Writes the version directory of tierstore 1.5.0 under `/state/pkgs` in `world`, its template
/// running `exec`, and loads its manifest.
fn package_under_state(world: &World, exec: &str) -> Manifest {
    let dir = world.host("/state/pkgs/tierstore/1.5.0");
    let spec = Spec::new()
        .unit("tierstore@.service", &unit_text(exec))
        .run("tierstore@9301.service");
    fs::create_dir_all(dir.join("bin")).expect("create bin/");
    fs::create_dir_all(dir.join("units")).expect("create units/");
    fs::write(dir.join("bin/tierstore"), "#!/bin/false\n").expect("write the binary");
    fs::write(dir.join("units/tierstore@.service"), unit_text(exec)).expect("write the unit");
    fs::write(
        dir.join("manifest.toml"),
        manifest_text("tierstore", "1.5.0", &spec),
    )
    .expect("write the manifest");
    let expected = Expected {
        name: service("tierstore"),
        version: version("1.5.0"),
    };
    Manifest::load(&dir, &expected, &RUNNING).expect("the manifest loads")
}

/// Catches `lint` reading the Linux directory instead of the roots it is given: for a package
/// written under `/state/pkgs`, a template pinned to its version directory there is
/// `PackagePath` naming `/state/pkgs`, one through `current` there passes, and one naming the
/// Linux version directory passes (it is outside this host's packages).
#[test]
fn lint_judges_the_packages_directory_of_its_roots() {
    let world = World::new("statepkgs");
    let roots = Roots::new(
        Path::new("/var/lib/hostkeeper"),
        Path::new("/etc/systemd/system"),
        Path::new(STATE_PACKAGES),
    );
    let manifest = package_under_state(&world, STATE_BAD);
    assert_eq!(
        lint(&world.fs, &roots, &manifest),
        Err(refusal(
            STATE_PACKAGES,
            "tierstore@.service",
            "ExecStart=",
            STATE_BAD
        ))
    );
    let manifest = package_under_state(&world, STATE_GOOD);
    assert_eq!(lint(&world.fs, &roots, &manifest), Ok(()));
    let manifest = package_under_state(&world, BAD);
    assert_eq!(lint(&world.fs, &roots, &manifest), Ok(()));
}

/// A launchd plist whose `ProgramArguments` key is followed by `between`, then an array whose
/// first string is `first`.
fn plist(between: &str, first: &str) -> String {
    format!(
        "<plist><dict>\n<key>ProgramArguments</key>{between}\n<array>\n<string>{first}</string>\n</array>\n</dict></plist>\n"
    )
}

fn judge_plist(text: &str) -> Result<(), LintError> {
    check_unit_text(
        Path::new(PACKAGES_DIR),
        "tierstore",
        "tierstore.plist",
        text,
    )
}

/// Catches a comment hiding or faking a `ProgramArguments`: a comment between the key and the
/// array, or between the array and its first string, does not stop the first string being judged;
/// a commented-out key with a good path does not stand in for the real one; a commented-out key
/// with a bad path is not judged; an unclosed comment hides the rest, so nothing is judged.
#[test]
fn xml_comments_are_skipped_in_a_plist() {
    let comment = "<!-- the program -->";
    assert_eq!(
        judge_plist(&plist(comment, BAD)),
        Err(refusal(
            PACKAGES_DIR,
            "tierstore.plist",
            "ProgramArguments",
            BAD
        ))
    );
    assert_eq!(judge_plist(&plist(comment, GOOD)), Ok(()));
    let inside =
        format!("<key>ProgramArguments</key><array><!-- c --><string>{BAD}</string></array>");
    assert_eq!(
        judge_plist(&inside),
        Err(refusal(
            PACKAGES_DIR,
            "tierstore.plist",
            "ProgramArguments",
            BAD
        ))
    );
    let decoy_good = format!(
        "<!-- <key>ProgramArguments</key><array><string>{GOOD}</string></array> -->\n{}",
        plist("", BAD)
    );
    assert_eq!(
        judge_plist(&decoy_good),
        Err(refusal(
            PACKAGES_DIR,
            "tierstore.plist",
            "ProgramArguments",
            BAD
        ))
    );
    let decoy_bad = format!(
        "<!-- <key>ProgramArguments</key><array><string>{BAD}</string></array> -->\n{}",
        plist("", GOOD)
    );
    assert_eq!(judge_plist(&decoy_bad), Ok(()));
    let unclosed = format!("<!-- never closed\n{}", plist("", BAD));
    assert_eq!(judge_plist(&unclosed), Ok(()));
}

/// Catches an encoded path getting past the plist check: `&#47;`, `&#x2F;` and `&#X2f;` for `/`
/// are decoded and the version path is refused; `&amp;`, `&lt;`, `&gt;`, `&quot;` and `&apos;`
/// are decoded in the refused path; a reference decoded once is not decoded again; an unknown
/// reference, a lone `&` and a number that is no character are kept as written.
#[test]
fn xml_references_are_decoded_in_a_plist() {
    let refused = |written: &str, decoded: &str| {
        assert_eq!(
            judge_plist(&plist("", written)),
            Err(refusal(
                PACKAGES_DIR,
                "tierstore.plist",
                "ProgramArguments",
                decoded
            )),
            "{written}"
        );
    };
    refused(
        "&#47;var&#x2F;lib&#X2f;hostkeeper/packages/tierstore/1.5.0/bin/tierstore",
        BAD,
    );
    refused(
        "/var/lib/hostkeeper/packages/tierstore/1.5.0/a&amp;b",
        "/var/lib/hostkeeper/packages/tierstore/1.5.0/a&b",
    );
    refused(
        "/var/lib/hostkeeper/packages/tierstore/1.5.0/&lt;&gt;&quot;&apos;",
        "/var/lib/hostkeeper/packages/tierstore/1.5.0/<>\"'",
    );
    refused(
        "/var/lib/hostkeeper/packages/tierstore/1.5.0/&bogus;x",
        "/var/lib/hostkeeper/packages/tierstore/1.5.0/&bogus;x",
    );
    refused(
        "/var/lib/hostkeeper/packages/tierstore/1.5.0/a & b",
        "/var/lib/hostkeeper/packages/tierstore/1.5.0/a & b",
    );
    for written in [
        "&#47;var/lib/hostkeeper/packages/tierstore/current/bin/tierstore",
        "&amp;#47;var/lib/hostkeeper/packages/tierstore/1.5.0/bin/tierstore",
        "&#xD800;/var/lib/hostkeeper/packages/tierstore/1.5.0/bin/tierstore",
    ] {
        assert_eq!(judge_plist(&plist("", written)), Ok(()), "{written}");
    }
}

/// Catches an empty command line read as a path, or a crash on it: the reset `ExecStart=` and a
/// command of prefixes only name no file, and are accepted.
#[test]
fn an_empty_command_line_is_accepted() {
    for text in [
        "[Service]\nExecStart=\n",
        "[Service]\nExecStart=-\n",
        "[Service]\nExecStartPre=@-+\n",
    ] {
        assert_eq!(
            check_unit_text(
                Path::new(PACKAGES_DIR),
                "tierstore",
                "tierstore.service",
                text
            ),
            Ok(()),
            "{text:?}"
        );
    }
}

/// Catches a `\u` or `\U` escape getting past the check undecoded: `\u002f` and `\U0000002f` for
/// the first `/` are refused with the decoded path, a short `\u2f` is read to its last hex digit,
/// the same escape of a `current` path is accepted; `\u` with no digit and `\uZZ` stand for `u`
/// with the letters after it kept, and an escape of four or eight digits that is no character
/// (`\uD800`, `\UFFFFFFFF`) stands for `u` or `U` with its digits consumed.
#[test]
fn unicode_escapes_are_decoded_before_the_check() {
    let tail = "var/lib/hostkeeper/packages/tierstore/1.5.0/bin/tierstore";
    for head in [r"\u002f", r"\U0000002f", r"\u2f"] {
        assert_eq!(
            service_line(&format!("ExecStart={head}{tail}")),
            refused(BAD),
            "{head}"
        );
    }
    assert_eq!(
        service_line(
            r"ExecStart=\u002fvar/lib/hostkeeper/packages/tierstore/current/bin/tierstore"
        ),
        Ok(())
    );
    assert_eq!(
        service_line(
            r"ExecStart=\U0000002fvar/lib/hostkeeper/packages/tierstore/current/bin/tierstore"
        ),
        Ok(())
    );
    let dir = "/var/lib/hostkeeper/packages/tierstore";
    for (escape, kept) in [(r"\uZZ", "uZZ"), (r"\UFFFFFFFF", "U"), (r"\uD800", "u")] {
        assert_eq!(
            service_line(&format!("ExecStart={dir}/{escape}1.5.0/bin/tierstore")),
            refused(&format!("{dir}/{kept}1.5.0/bin/tierstore")),
            "{escape}"
        );
    }
    assert_eq!(
        service_line(r"ExecStart=\u/var/lib/x/1.5.0/tierstore"),
        Ok(())
    );
    assert_eq!(
        service_line(r"ExecStart=\U/var/lib/x/1.5.0/tierstore"),
        Ok(())
    );
}

/// Catches a `WantedBy=` name with a `:` being enabled: the unit `tierstore.service` wanted by
/// `x:y.<target>` is `BadTarget` from `lint` and from `enable_run_units`, nothing is written and
/// `placed.json` still loads; with a plain target the same flow enables, records and loads the
/// link, `disable` removes it and `reconcile` reports nothing wrong.
#[test]
fn a_colon_in_a_wantedby_name_is_refused_before_anything_is_written() {
    let world = World::new("colon-wantedby");
    let colon = format!("x:y.{}", "target");
    let spec = |wanted: &str| {
        let text = format!("[Service]\nExecStart={GOOD}\n[Install]\nWantedBy={wanted}\n");
        Spec::new()
            .unit("tierstore.service", &text)
            .run("tierstore.service")
    };
    let manifest = world.package("tierstore", "1.5.0", &spec(&colon));
    world.log.clear();
    let refusal = LintError::BadTarget {
        file: "tierstore.service".to_owned(),
        target: colon,
    };
    assert_eq!(
        lint(&world.fs, &world.roots, &manifest),
        Err(refusal.clone())
    );
    match world.placer().enable_run_units(&manifest) {
        Err(PlacedError::Lint(seen)) => assert_eq!(seen, refusal),
        other => panic!("expected BadTarget, got {other:?}"),
    }
    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), None);
    assert!(world.record().paths.is_empty());

    let manifest = install(&world, "tierstore", "1.5.0", &spec(TARGET));
    assert_eq!(lint(&world.fs, &world.roots, &manifest), Ok(()));
    let wants = format!("{WANTS}/tierstore.service");
    assert_eq!(
        world.read_link(&wants),
        Some(PathBuf::from("/etc/systemd/system/tierstore.service"))
    );
    assert!(world.record().paths.contains_key(Path::new(&wants)));

    let removal = world
        .placer()
        .disable(&service("tierstore"))
        .expect("disable");
    assert_eq!(removal.removed, [PathBuf::from(&wants)]);
    assert_eq!(world.read_link(&wants), None);

    let view = BootView {
        intents: Vec::new(),
        running: RUNNING,
    };
    let report = world.placer().reconcile(&view).expect("reconcile");
    assert!(report.conflicts.is_empty() && report.unresolved.is_empty());
    assert!(!world.record().reload_pending);
}
