//! The paths a package claims and the load rules of linking: `claim.rs` (the rules on a
//! `[[files]] path`, a unit file's name, duplicate claims, the exact claims of the tierstore
//! example) and `lint.rs` (`[units] run` entries, command lines that must go through `current`,
//! launchd plists, `WantedBy=`, unreadable unit files).

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::{Class, FileEntry, Manifest, REFUSED_PREFIXES};
use hostkeeper_config::names::PathError;
use hostkeeper_placed::claim::{check_files_path, check_source};
use hostkeeper_placed::lint::{
    COMMAND_PREFIXES, EXEC_KEYS, LintError, MAX_UNIT_NAME, check_unit_text, is_unit_name, lint,
    wanted_by,
};
use hostkeeper_placed::roots::PACKAGES_DIR;
use hostkeeper_placed::{ClaimError, Claims, PathKind, Roots};
use support::{Spec, TARGET, World, unit_text, unit_without_install};

const GOOD: &str = "/var/lib/hostkeeper/packages/tierstore/current/bin/tierstore";
const BAD: &str = "/var/lib/hostkeeper/packages/tierstore/1.5.0/bin/tierstore";

/// The tierstore example loaded in a fresh world.
fn tierstore(name: &str) -> (World, Manifest) {
    let world = World::new(name);
    let manifest = world.package("tierstore", "1.5.0", &Spec::tierstore());
    (world, manifest)
}

/// A manifest of the example whose only `[[files]]` entry is at `path`.
fn with_file_at(manifest: &Manifest, path: &str) -> Manifest {
    let mut edited = manifest.clone();
    edited.files = vec![FileEntry {
        source: "files/tierstore.conf".to_owned(),
        path: path.to_owned(),
    }];
    edited
}

/// Replaces the text of `units/<file>` of the example's version directory.
fn overwrite_unit(world: &World, file: &str, bytes: &[u8]) {
    let path = world
        .version_dir("tierstore", "1.5.0")
        .join("units")
        .join(file);
    fs::write(path, bytes).expect("overwrite a unit");
}

/// The packages directory of a Linux host.
fn packages() -> &'static Path {
    Path::new(PACKAGES_DIR)
}

/// The result of `check_unit_text` for a `tierstore.service` whose `[Service]` holds `line`.
fn service_line(line: &str) -> Result<(), LintError> {
    check_unit_text(
        packages(),
        "tierstore",
        "tierstore.service",
        &format!("[Service]\n{line}\n"),
    )
}

fn package_path(file: &str, key: &str, path: &str) -> LintError {
    LintError::PackagePath {
        file: file.to_owned(),
        key: key.to_owned(),
        path: path.to_owned(),
        packages: PACKAGES_DIR.to_owned(),
        service: "tierstore".to_owned(),
    }
}

/// Catches a refused prefix that is missing from the check, or matched on the wrong side: each
/// refused prefix, itself and a path below it, is refused by `check_files_path` and by
/// `Claims::of` on a manifest edited after loading; an empty path, a relative path and a path
/// with a `..` component each have their own refusal.
#[test]
fn forbidden_prefixes_refuse_each_files_path() {
    let (world, manifest) = tierstore("forbidden");
    for prefix in REFUSED_PREFIXES {
        for path in [prefix.to_owned(), format!("{prefix}/a.conf")] {
            let expected = ClaimError::Refused {
                path: path.clone(),
                prefix,
            };
            assert_eq!(check_files_path(&path), Err(expected.clone()));
            let edited = with_file_at(&manifest, &path);
            let err = Claims::of(&world.roots, &edited).expect_err("a refused prefix");
            assert_eq!(err, expected);
        }
    }
    assert_eq!(check_files_path(""), Err(ClaimError::EmptyPath));
    assert_eq!(
        check_files_path("etc/a.conf"),
        Err(ClaimError::BadPath {
            path: "etc/a.conf".to_owned(),
            why: PathError::NotAbsolute
        })
    );
    assert_eq!(
        check_files_path("../a.conf"),
        Err(ClaimError::BadPath {
            path: "../a.conf".to_owned(),
            why: PathError::NotAbsolute
        })
    );
    assert_eq!(
        check_files_path("/opt/../etc/a.conf"),
        Err(ClaimError::BadPath {
            path: "/opt/../etc/a.conf".to_owned(),
            why: PathError::DotComponent
        })
    );
    assert_eq!(
        Claims::of(&world.roots, &with_file_at(&manifest, "")).expect_err("empty"),
        ClaimError::EmptyPath
    );
    assert_eq!(
        Claims::of(&world.roots, &with_file_at(&manifest, "etc/a.conf")).expect_err("relative"),
        ClaimError::BadPath {
            path: "etc/a.conf".to_owned(),
            why: PathError::NotAbsolute
        }
    );
    assert_eq!(
        Claims::of(&world.roots, &with_file_at(&manifest, "/opt/./a.conf"))
            .expect_err("dot component"),
        ClaimError::BadPath {
            path: "/opt/./a.conf".to_owned(),
            why: PathError::DotComponent
        }
    );
}

/// Catches matching by string prefix: `/etc/hostkeeperx` and `/usrx` share the text of a refused
/// prefix but are other directories, and are accepted.
#[test]
fn etc_hostkeeperx_is_not_refused() {
    let (world, manifest) = tierstore("component");
    for path in [
        "/etc/hostkeeperx/a.conf",
        "/usrx/a.conf",
        "/var/libx/a.conf",
        "/etc/hostkeepe",
    ] {
        assert_eq!(check_files_path(path), Ok(()), "{path}");
        let claims = Claims::of(&world.roots, &with_file_at(&manifest, path)).expect("accepted");
        assert!(claims.paths().contains(Path::new(path)));
    }
}

/// Catches a unit file of another name being placed: `other.service` in the unit files of
/// tierstore is refused, and so is a file that is no unit at all.
#[test]
fn a_unit_file_name_must_start_with_the_service_name() {
    let (world, manifest) = tierstore("unitname");
    for file in ["other.service", "tierstore.conf", "tierstore@bad!.service"] {
        let mut edited = manifest.clone();
        edited.unit_files.insert(file.to_owned());
        assert_eq!(
            Claims::of(&world.roots, &edited).expect_err("a foreign unit file"),
            ClaimError::UnitName {
                service: "tierstore".to_owned(),
                file: file.to_owned()
            }
        );
    }
    let mut dashed = manifest.clone();
    dashed
        .unit_files
        .insert("tierstore-cache.service".to_owned());
    let claims = Claims::of(&world.roots, &dashed).expect("a dashed name is the service's");
    assert!(
        claims
            .paths()
            .contains(Path::new("/etc/systemd/system/tierstore-cache.service"))
    );
}

/// Catches two entries of one package claiming a path without a refusal: two `[[files]]` at one
/// path, and a `[[files]]` path equal to a unit's path, are `Duplicate`.
#[test]
fn two_entries_claiming_one_path_are_refused() {
    let (world, manifest) = tierstore("duplicate");
    let mut twice = manifest.clone();
    twice.files.push(FileEntry {
        source: "files/other.conf".to_owned(),
        path: "/etc/tierstore/tierstore.conf".to_owned(),
    });
    assert_eq!(
        Claims::of(&world.roots, &twice).expect_err("two files"),
        ClaimError::Duplicate {
            path: PathBuf::from("/etc/tierstore/tierstore.conf")
        }
    );

    let roots = Roots::new(
        Path::new("/var/lib/hostkeeper"),
        Path::new("/etc/units"),
        Path::new("/var/lib/hostkeeper/packages"),
    );
    let on_unit = with_file_at(&manifest, "/etc/units/tierstore@.service");
    assert_eq!(
        Claims::of(&roots, &on_unit).expect_err("a file on a unit"),
        ClaimError::Duplicate {
            path: PathBuf::from("/etc/units/tierstore@.service")
        }
    );
    let apart = Claims::of(&roots, &manifest).expect("units and files apart");
    assert!(
        apart
            .paths()
            .contains(Path::new("/etc/units/tierstore@.service"))
    );
}

/// Catches a wrong path, kind or link target: the tierstore example claims its template at the
/// unit directory and its file at `/etc/tierstore/tierstore.conf`, each linked into
/// `current/`.
#[test]
fn claims_of_the_tierstore_example_are_exact() {
    let (world, manifest) = tierstore("exact");
    let claims = Claims::of(&world.roots, &manifest).expect("claims");
    assert_eq!(claims.service().as_str(), "tierstore");
    assert_eq!(claims.version().as_str(), "1.5.0");
    let got: Vec<_> = claims.iter().cloned().collect();
    let current = "/var/lib/hostkeeper/packages/tierstore/current";
    assert_eq!(
        got,
        [
            hostkeeper_placed::Claim {
                path: PathBuf::from("/etc/systemd/system/tierstore@.service"),
                kind: PathKind::Unit,
                link_target: PathBuf::from(format!("{current}/units/tierstore@.service")),
            },
            hostkeeper_placed::Claim {
                path: PathBuf::from("/etc/tierstore/tierstore.conf"),
                kind: PathKind::File,
                link_target: PathBuf::from(format!("{current}/files/tierstore.conf")),
            },
        ]
    );
    assert!(
        claims
            .paths()
            .contains(Path::new("/etc/tierstore/tierstore.conf"))
    );
    assert!(!claims.paths().contains(Path::new("/etc/tierstore")));
    assert_eq!(
        claims.paths().into_iter().collect::<Vec<_>>(),
        [
            PathBuf::from("/etc/systemd/system/tierstore@.service"),
            PathBuf::from("/etc/tierstore/tierstore.conf"),
        ]
    );
}

/// Catches a `run` entry that is not started from a linked file: an instance of a linked
/// template and a plain linked unit are accepted; an instance with no template, a unit that is
/// no file, a foreign name and the template itself are `RunUnit`.
#[test]
fn run_entries_must_be_linked_units_or_instances_of_linked_templates() {
    let spec = Spec::tierstore().unit("tierstore.service", &unit_text(GOOD));
    let world = World::new("rununit");
    let manifest = world.package("tierstore", "1.5.0", &spec);
    assert_eq!(lint(&world.fs, &world.roots, &manifest), Ok(()));

    let mut plain = manifest.clone();
    plain.run = vec!["tierstore.service".to_owned()];
    assert_eq!(lint(&world.fs, &world.roots, &plain), Ok(()));

    let refused = |edit: &dyn Fn(&mut Manifest), unit: &str| {
        let mut edited = manifest.clone();
        edited.run = vec![unit.to_owned()];
        edit(&mut edited);
        assert_eq!(
            lint(&world.fs, &world.roots, &edited),
            Err(LintError::RunUnit {
                unit: unit.to_owned()
            }),
            "{unit}"
        );
    };
    refused(
        &|m| {
            m.unit_files.remove("tierstore@.service");
        },
        "tierstore@9301.service",
    );
    refused(&|_| {}, "ghost.service");
    refused(&|_| {}, "tierstore-ghost.service");
    refused(&|_| {}, "tierstore@.service");
    refused(&|_| {}, "tierstore@9301.timer");
}

/// Catches a command line that pins the unit to a version or to another package, and a prefix
/// strip that is missing: `current/` is accepted; a version directory, another service's
/// directory, and each of those behind a systemd prefix are `PackagePath` with the path
/// stripped of the prefix.
#[test]
fn execstart_must_use_the_current_path() {
    assert_eq!(service_line(&format!("ExecStart={GOOD}")), Ok(()));
    assert_eq!(
        service_line(&format!("ExecStart={GOOD} serve --port %i")),
        Ok(())
    );
    let other = "/var/lib/hostkeeper/packages/other/current/bin/x";
    for bad in [BAD, other] {
        assert_eq!(
            service_line(&format!("ExecStart={bad}")),
            Err(package_path("tierstore.service", "ExecStart=", bad))
        );
    }
    for prefix in COMMAND_PREFIXES {
        assert_eq!(
            service_line(&format!("ExecStart={prefix}{BAD}")),
            Err(package_path("tierstore.service", "ExecStart=", BAD)),
            "{prefix}"
        );
    }
    assert_eq!(
        service_line(&format!("ExecStart=-@{BAD}")),
        Err(package_path("tierstore.service", "ExecStart=", BAD))
    );
    assert_eq!(service_line(&format!("ExecStart=-{GOOD}")), Ok(()));
    assert_eq!(
        service_line("ExecStart=/usr/bin/env FOO=bar /usr/bin/tierstore serve"),
        Ok(())
    );
    assert_eq!(
        service_line("ExecStart=/usr/bin/tierstore --root=/var/lib/hostkeeper"),
        Ok(())
    );
}

/// Catches a path under the packages directory that goes around `current`: the directory
/// itself and its trailing-slash spelling, a similar directory name, a bare `current`, a service
/// whose name only starts with this one's, and `.` or `..` after `current` are judged on the
/// resolved path: `current/./bin/x` stays in `current`, `current/..` leaves it.
#[test]
fn through_current_judges_every_shape_of_package_path() {
    let accepted = [
        "/var/lib/hostkeeper/packagesx/tierstore/1.5.0/bin/x",
        "/var/lib/hostkeeper/packages",
        "/var/lib/hostkeeper/packages/",
        "/var/lib/hostkeeper/packages/tierstore/current/bin/x.y/z",
        "/var/lib/hostkeeper/packages/tierstore/current/./bin/x",
        "/var/lib/hostkeeper/packages/tierstore/current/bin/.",
        "/var/lib/hostkeeper/state",
        "/usr/bin/tierstore",
    ];
    for path in accepted {
        assert_eq!(service_line(&format!("ExecStart={path}")), Ok(()), "{path}");
    }
    let refused = [
        "/var/lib/hostkeeper/packages/tierstore",
        "/var/lib/hostkeeper/packages/tierstore/current",
        "/var/lib/hostkeeper/packages/tierstorex/current/bin/x",
        "/var/lib/hostkeeper/packages/tierstore/current/bin/../../1.5.0/bin/x",
        "/var/lib/hostkeeper/packages/tierstore/current/..",
    ];
    for path in refused {
        assert_eq!(
            service_line(&format!("ExecStart={path}")),
            Err(package_path("tierstore.service", "ExecStart=", path)),
            "{path}"
        );
    }
}

/// Catches an exec key that is not checked, or a key that is not an exec key being checked: every
/// key of `EXEC_KEYS` refuses a version path with its own name in the error, and keys such as
/// `WorkingDirectory=` and `Environment=` may name anything.
#[test]
fn every_exec_key_is_checked_and_other_keys_are_not() {
    assert_eq!(EXEC_KEYS.len(), 7);
    for key in EXEC_KEYS {
        assert_eq!(
            service_line(&format!("{key}={BAD}")),
            Err(package_path("tierstore.service", &format!("{key}="), BAD)),
            "{key}"
        );
        assert_eq!(service_line(&format!("{key}={GOOD}")), Ok(()), "{key}");
    }
    assert_eq!(
        service_line(&format!("ExecStartPre = {BAD}")),
        Err(package_path("tierstore.service", "ExecStartPre=", BAD))
    );
    for key in [
        "WorkingDirectory",
        "Environment",
        "Description",
        "ExecStarts",
        "execstart",
    ] {
        assert_eq!(service_line(&format!("{key}={BAD}")), Ok(()), "{key}");
    }
    let in_install = format!("[Install]\nExecStart={BAD}\n");
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.service", &in_install),
        Err(package_path("tierstore.service", "ExecStart=", BAD))
    );
}

/// Catches a bad path hidden in an argument, in quotes, or in a continuation line: a
/// `--config=<path>` word, a double- or single-quoted path and a path on a continued line are
/// refused, and a quoted good path is accepted.
#[test]
fn bad_paths_are_found_in_flags_quotes_and_continuations() {
    let flag_path = "/var/lib/hostkeeper/packages/tierstore/1.5.0/c.toml";
    assert_eq!(
        service_line(&format!("ExecStart={GOOD} --config={flag_path}")),
        Err(package_path("tierstore.service", "ExecStart=", flag_path))
    );
    assert_eq!(
        service_line(&format!("ExecStart={GOOD} --config={GOOD}.toml")),
        Ok(())
    );
    assert_eq!(
        service_line(&format!("ExecStart=\"{BAD}\" serve")),
        Err(package_path("tierstore.service", "ExecStart=", BAD))
    );
    assert_eq!(
        service_line(&format!("ExecStart='{BAD}' serve")),
        Err(package_path("tierstore.service", "ExecStart=", BAD))
    );
    assert_eq!(service_line(&format!("ExecStart=\"{GOOD}\" serve")), Ok(()));
    let continued = format!("[Service]\nExecStart={GOOD} \\\n  serve \\\n  --config={flag_path}\n");
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.service", &continued),
        Err(package_path("tierstore.service", "ExecStart=", flag_path))
    );
}

/// Catches comments, blank lines, lines without `=` and a dangling continuation being read
/// wrongly: commented-out bad lines are accepted; a line that is only a continuation at the end
/// of the file is still judged; an unterminated continuation of nothing is ignored.
#[test]
fn comments_blank_lines_and_trailing_continuations() {
    let commented = format!(
        "[Service]\n# ExecStart={BAD}\n; ExecStart={BAD}\n\n   \n  #ExecStart={BAD}\nExecStart={GOOD}\n"
    );
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.service", &commented),
        Ok(())
    );
    let no_assignment = "[Service]\nExecStart\nsome words\n[Unit]\n";
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.service", no_assignment),
        Ok(())
    );
    let at_eof = format!("[Service]\nExecStart={BAD} \\");
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.service", &at_eof),
        Err(package_path("tierstore.service", "ExecStart=", BAD))
    );
    let empty_tail = "[Service]\nExecStart=/usr/bin/true\n\\";
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.service", empty_tail),
        Ok(())
    );
    let comment_inside = format!("[Service]\nExecStart=/usr/bin/true \\\n# a comment\n {BAD}\n");
    assert_eq!(
        check_unit_text(
            packages(),
            "tierstore",
            "tierstore.service",
            &comment_inside
        ),
        Err(package_path("tierstore.service", "ExecStart=", BAD))
    );
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.service", ""),
        Ok(())
    );
}

fn plist(first: &str) -> String {
    format!(
        "<plist><dict>\n<key>Label</key>\n<string>x</string>\n<key>ProgramArguments</key>\n<array>\n  <string>{first}</string>\n  <string>{BAD}</string>\n</array>\n</dict></plist>\n"
    )
}

/// Catches a launchd plist judged like a systemd unit, or not judged: the first
/// `ProgramArguments` string is held to the `current` rule, later strings and `ExecStart=` text
/// are not, and a plist with no usable `ProgramArguments` is accepted.
#[test]
fn a_plist_is_judged_by_its_first_program_argument() {
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.plist", &plist(GOOD)),
        Ok(())
    );
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.plist", &plist(BAD)),
        Err(package_path("tierstore.plist", "ProgramArguments", BAD))
    );
    assert_eq!(
        check_unit_text(
            packages(),
            "tierstore",
            "tierstore.plist",
            &plist(&format!("  {BAD}  "))
        ),
        Err(package_path("tierstore.plist", "ProgramArguments", BAD))
    );
    let none = "<plist><dict><key>Label</key><string>x</string></dict></plist>";
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.plist", none),
        Ok(())
    );
    let text_exec = format!("ExecStart={BAD}\n");
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.plist", &text_exec),
        Ok(())
    );
    let no_array = format!("<key>ProgramArguments</key><string>{BAD}</string>");
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.plist", &no_array),
        Ok(())
    );
    let no_string = "<key>ProgramArguments</key><array></array>";
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.plist", no_string),
        Ok(())
    );
    let unclosed = format!("<key>ProgramArguments</key><array><string>{BAD}");
    assert_eq!(
        check_unit_text(packages(), "tierstore", "tierstore.plist", &unclosed),
        Ok(())
    );
}

/// Catches `WantedBy=` read from the wrong section or key, or a reset ignored: values are
/// collected in order across lines, an empty `WantedBy=` clears what came before, and other
/// sections and keys do not count.
#[test]
fn wanted_by_reads_the_install_section_as_systemd_does() {
    assert_eq!(wanted_by(""), Vec::<String>::new());
    assert_eq!(wanted_by(&unit_text(GOOD)), [TARGET]);
    assert_eq!(wanted_by(&unit_without_install(GOOD)), Vec::<String>::new());
    assert_eq!(
        wanted_by("[Install]\nWantedBy=a.service b.service\nWantedBy=c.service\n"),
        ["a.service", "b.service", "c.service"]
    );
    assert_eq!(
        wanted_by("[Install]\nWantedBy=a.service\nWantedBy=\nWantedBy=b.service\n"),
        ["b.service"]
    );
    assert_eq!(
        wanted_by("[Install]\nWantedBy=a.service\nWantedBy=\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        wanted_by(
            "[Service]\nWantedBy=a.service\n[Install]\nRequiredBy=b.service\nAlso=c.service\n"
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        wanted_by("[Install]\nWantedBy = a.service \\\n  b.service\n"),
        ["a.service", "b.service"]
    );
}

/// A unit name of exactly `len` bytes: `a`s and the `.service` suffix.
fn unit_name_of_len(len: usize) -> String {
    format!("{}.service", "a".repeat(len - ".service".len()))
}

/// Catches a unit-name check that is too loose or too strict: a stem of letters, digits and
/// `_ . @ -` with a unit type as suffix is a name; a name with no suffix or a suffix that is no
/// unit type, an empty or dotted stem, an `@` with no prefix or no instance or a second `@`, a
/// `:` (outside the grammar of the paths the record keeps), other characters and one byte over
/// `MAX_UNIT_NAME` are not.
#[test]
fn is_unit_name_accepts_only_unit_names() {
    let longest = unit_name_of_len(MAX_UNIT_NAME);
    assert_eq!(longest.len(), 249);
    for name in [
        "a.service",
        "a.b.service",
        "x@9301.service",
        "x-y-1_z.socket",
        "x@y-z_w-1.service",
        "A0.timer",
        longest.as_str(),
    ] {
        assert!(is_unit_name(name), "{name}");
    }
    let too_long = unit_name_of_len(MAX_UNIT_NAME + 1);
    assert_eq!(too_long.len(), 250);
    for name in [
        "",
        "a",
        "a.b",
        "a.invalid",
        "A0",
        "a@b@c.service",
        "@x.service",
        "x@.service",
        ".x.service",
        ".service",
        ".hidden",
        "..",
        "../x",
        "a/b.service",
        "a b.service",
        "a;b.service",
        "a\u{e9}.service",
        "a$.service",
        "a%i.service",
        "x:y.service",
        "x@y:z.service",
        ":x.service",
        too_long.as_str(),
    ] {
        assert!(!is_unit_name(name), "{name:?}");
    }
}

/// Catches a run unit with no way to be enabled: a `run` unit whose file has no `[Install]`
/// section, one whose only `WantedBy=` was cleared by an empty one, and one with a `WantedBy=`
/// in the wrong section are `NoWantedBy`; a unit that is not run needs none.
#[test]
fn a_run_unit_without_wanted_by_is_refused() {
    let (world, manifest) = tierstore("nowanted");
    assert_eq!(lint(&world.fs, &world.roots, &manifest), Ok(()));
    let expected = Err(LintError::NoWantedBy {
        unit: "tierstore@9301.service".to_owned(),
        file: "tierstore@.service".to_owned(),
    });
    for text in [
        unit_without_install(GOOD),
        format!("{}WantedBy=\n", unit_text(GOOD)),
        format!("[Service]\nExecStart={GOOD}\nWantedBy={TARGET}\n[Install]\nAlso=x.service\n"),
    ] {
        overwrite_unit(&world, "tierstore@.service", text.as_bytes());
        assert_eq!(lint(&world.fs, &world.roots, &manifest), expected, "{text}");
    }
    let mut idle = manifest.clone();
    idle.run = Vec::new();
    assert_eq!(lint(&world.fs, &world.roots, &idle), Ok(()));
}

/// Catches a `WantedBy=` that is not a unit name: `../x` is `BadTarget` naming the file and the
/// target, whether it comes alone or after a good one; an empty reset after a bad one clears
/// it.
#[test]
fn a_wanted_by_that_is_not_a_unit_name_is_refused() {
    let (world, manifest) = tierstore("badtarget");
    let text = |wanted: &str| format!("[Service]\nExecStart={GOOD}\n[Install]\n{wanted}\n");
    let bad = Err(LintError::BadTarget {
        file: "tierstore@.service".to_owned(),
        target: "../x".to_owned(),
    });
    for wanted in [
        "WantedBy=../x".to_owned(),
        format!("WantedBy={TARGET} ../x"),
        format!("WantedBy={TARGET}\nWantedBy=../x"),
        "WantedBy=../x other.service".to_owned(),
    ] {
        overwrite_unit(&world, "tierstore@.service", text(&wanted).as_bytes());
        assert_eq!(lint(&world.fs, &world.roots, &manifest), bad, "{wanted}");
    }
    let cleared = text(&format!("WantedBy=../x\nWantedBy=\nWantedBy={TARGET}"));
    overwrite_unit(&world, "tierstore@.service", cleared.as_bytes());
    assert_eq!(lint(&world.fs, &world.roots, &manifest), Ok(()));
}

/// Catches an unreadable unit file passing the lint: a unit file missing from the version
/// directory, one that is a directory and one that is not UTF-8 are `Unreadable` naming the
/// file, before any other rule is applied.
#[test]
fn unreadable_unit_files_are_refused() {
    let (world, manifest) = tierstore("unreadable");
    let mut missing = manifest.clone();
    missing
        .unit_files
        .insert("tierstore-extra.service".to_owned());
    assert_eq!(
        lint(&world.fs, &world.roots, &missing),
        Err(LintError::Unreadable {
            file: "tierstore-extra.service".to_owned(),
            why: "No such file or directory (os error 2)".to_owned()
        })
    );

    let units = world.version_dir("tierstore", "1.5.0").join("units");
    fs::create_dir(units.join("tierstore-extra.service")).expect("a directory");
    assert_eq!(
        lint(&world.fs, &world.roots, &missing),
        Err(LintError::Unreadable {
            file: "tierstore-extra.service".to_owned(),
            why: "Is a directory (os error 21)".to_owned()
        })
    );

    overwrite_unit(&world, "tierstore@.service", &[0xff, 0xfe, b'\n']);
    let mut run_bad = manifest.clone();
    run_bad.run = vec!["ghost.service".to_owned()];
    assert_eq!(
        lint(&world.fs, &world.roots, &run_bad),
        Err(LintError::Unreadable {
            file: "tierstore@.service".to_owned(),
            why: "not UTF-8".to_owned()
        })
    );
}

/// Catches the lint applying the rules in the wrong order or stopping early: a bad command line
/// is reported from `lint` with the file's name, before a `run` problem of the same manifest, and
/// a bad path in a file that is not run is still refused.
#[test]
fn lint_checks_every_unit_file_before_the_run_entries() {
    let spec = Spec::tierstore().unit("tierstore-aux.service", &unit_text(GOOD));
    let world = World::new("order");
    let manifest = world.package("tierstore", "1.5.0", &spec);
    assert_eq!(lint(&world.fs, &world.roots, &manifest), Ok(()));
    overwrite_unit(&world, "tierstore-aux.service", unit_text(BAD).as_bytes());
    let mut edited = manifest.clone();
    edited.run = vec!["ghost.service".to_owned()];
    assert_eq!(
        lint(&world.fs, &world.roots, &edited),
        Err(package_path("tierstore-aux.service", "ExecStart=", BAD))
    );
}

/// Catches an error that reports the wrong class or reason: every `LintError` is a release
/// refusal with reason `manifest_mismatch`, and the text of a version-path refusal is exact.
#[test]
fn lint_errors_are_release_manifest_mismatches() {
    let errors = [
        LintError::RunUnit {
            unit: "a.service".to_owned(),
        },
        package_path("tierstore.service", "ExecStart=", BAD),
        LintError::NoWantedBy {
            unit: "a.service".to_owned(),
            file: "a.service".to_owned(),
        },
        LintError::BadTarget {
            file: "a.service".to_owned(),
            target: "../x".to_owned(),
        },
        LintError::Unreadable {
            file: "a.service".to_owned(),
            why: "not UTF-8".to_owned(),
        },
    ];
    for error in errors {
        assert_eq!(error.class(), Class::Release, "{error}");
        assert_eq!(error.reason(), "manifest_mismatch", "{error}");
    }
    assert_eq!(
        package_path("tierstore.service", "ExecStart=", BAD).to_string(),
        format!(
            "units/tierstore.service ExecStart= names {BAD:?}, which is under /var/lib/hostkeeper/packages/ but not /var/lib/hostkeeper/packages/tierstore/current/"
        )
    );
}

/// Catches a `[[files]] source` that leaves `files/`: an absolute source, one with `..`
/// components, one below another directory of the package, an empty one and a bare `files/` are
/// each `BadSource` naming the entry, from `Claims::of` on a manifest edited after loading and
/// from `check_source`; a source below `files/` is accepted.
#[test]
fn a_files_source_must_be_relative_and_below_files() {
    let (world, manifest) = tierstore("sources");
    for source in ["/etc/passwd", "files/../../x", "bin/x", "", "files/"] {
        let mut edited = manifest.clone();
        edited.files = vec![FileEntry {
            source: source.to_owned(),
            path: "/etc/tierstore/tierstore.conf".to_owned(),
        }];
        let err = Claims::of(&world.roots, &edited).expect_err("a bad source");
        assert!(
            matches!(&err, ClaimError::BadSource { entry, .. } if entry == source),
            "{source:?}: {err:?}"
        );
        assert!(
            matches!(check_source(source), Err(ClaimError::BadSource { .. })),
            "{source:?}"
        );
    }
    assert_eq!(check_source("files/sub/a.conf"), Ok(()));
    let mut deep = manifest.clone();
    deep.files[0].source = "files/sub/a.conf".to_owned();
    let claims = Claims::of(&world.roots, &deep).expect("a source below files/");
    assert!(claims.iter().any(|claim| {
        claim.link_target
            == Path::new("/var/lib/hostkeeper/packages/tierstore/current/files/sub/a.conf")
    }));
}

/// A manifest of the example with the `[[files]]` entries at `paths`, each from its own source.
fn with_files_at(manifest: &Manifest, paths: &[&str]) -> Manifest {
    let mut edited = manifest.clone();
    edited.files = paths
        .iter()
        .enumerate()
        .map(|(i, path)| FileEntry {
            source: format!("files/f{i}.conf"),
            path: (*path).to_owned(),
        })
        .collect();
    edited
}

/// Catches two claims of one package where one path lies inside the other: `[[files]]` at
/// `/etc/shared` and `/etc/shared/child.conf`, in either order, are `Overlap` naming both paths;
/// a file below a unit's path is `Overlap` too; a sibling whose name only starts with the same
/// text is no overlap.
#[test]
fn a_claim_below_another_claim_of_the_package_is_an_overlap() {
    let (world, manifest) = tierstore("overlap");
    let overlap = |path: &str, other: &str| ClaimError::Overlap {
        path: PathBuf::from(path),
        other: PathBuf::from(other),
    };
    assert_eq!(
        Claims::of(
            &world.roots,
            &with_files_at(&manifest, &["/etc/shared", "/etc/shared/child.conf"])
        )
        .expect_err("a file below a file"),
        overlap("/etc/shared/child.conf", "/etc/shared")
    );
    assert_eq!(
        Claims::of(
            &world.roots,
            &with_files_at(&manifest, &["/etc/shared/child.conf", "/etc/shared"])
        )
        .expect_err("a file above a file"),
        overlap("/etc/shared", "/etc/shared/child.conf")
    );
    let roots = Roots::new(
        Path::new("/var/lib/hostkeeper"),
        Path::new("/etc/units"),
        Path::new("/var/lib/hostkeeper/packages"),
    );
    assert_eq!(
        Claims::of(
            &roots,
            &with_files_at(&manifest, &["/etc/units/tierstore@.service/x"])
        )
        .expect_err("a file below a unit"),
        overlap(
            "/etc/units/tierstore@.service/x",
            "/etc/units/tierstore@.service"
        )
    );
    let apart = with_files_at(
        &manifest,
        &["/etc/shared", "/etc/shared2", "/etc/shared.d/a"],
    );
    let claims = Claims::of(&world.roots, &apart).expect("siblings do not overlap");
    assert_eq!(claims.iter().count(), 4);
}

/// A manifest whose units are `files` (name and text) and whose `run` is `run`, written into
/// a fresh world and loaded.
fn world_with_units(name: &str, files: &[(&str, String)], run: &str) -> (World, Manifest) {
    let mut spec = Spec::new().run(run);
    for (file, text) in files {
        spec = spec.unit(file, text);
    }
    let world = World::new(name);
    let manifest = world.package("tierstore", "1.5.0", &spec);
    (world, manifest)
}

/// A unit file whose command is good and whose `WantedBy=` is `wanted`.
fn wanted_by_text(wanted: &str) -> String {
    format!("[Service]\nExecStart={GOOD}\n[Install]\nWantedBy={wanted}\n")
}

/// Catches a run instance read from its template when its own unit file is linked: with only
/// `tierstore@9301.service` among the unit files, that file's `WantedBy=` is the one judged
/// (it passes, and with none it is `NoWantedBy` naming that file); with the template present as
/// well, the instance file's `WantedBy=` wins in both directions.
#[test]
fn an_instance_with_its_own_unit_file_is_read_from_that_file() {
    let instance = "tierstore@9301.service";
    let (world, alone) = world_with_units("instalone", &[(instance, unit_text(GOOD))], instance);
    assert_eq!(lint(&world.fs, &world.roots, &alone), Ok(()));
    overwrite_unit(&world, instance, unit_without_install(GOOD).as_bytes());
    assert_eq!(
        lint(&world.fs, &world.roots, &alone),
        Err(LintError::NoWantedBy {
            unit: instance.to_owned(),
            file: instance.to_owned()
        })
    );

    let both = |name: &str, own: &str, template: &str| {
        world_with_units(
            name,
            &[
                (instance, wanted_by_text(own)),
                ("tierstore@.service", wanted_by_text(template)),
            ],
            instance,
        )
    };
    let (world, manifest) = both("instwins", TARGET, "broken.invalid");
    assert_eq!(lint(&world.fs, &world.roots, &manifest), Ok(()));
    let (world, manifest) = both("instloses", "broken.invalid", TARGET);
    assert_eq!(
        lint(&world.fs, &world.roots, &manifest),
        Err(LintError::BadTarget {
            file: instance.to_owned(),
            target: "broken.invalid".to_owned()
        })
    );
    let (world, manifest) = both("instnone", "", TARGET);
    assert_eq!(
        lint(&world.fs, &world.roots, &manifest),
        Err(LintError::NoWantedBy {
            unit: instance.to_owned(),
            file: instance.to_owned()
        })
    );
}

/// Catches a `WantedBy=` the daemon could not enable: a name of no known unit type and a
/// name with a specifier are `BadTarget`; a name of 249 bytes is accepted and one of 250 is
/// refused.
#[test]
fn wanted_by_targets_are_unit_names_within_the_length_limit() {
    let (world, manifest) = tierstore("targets");
    let specifier = format!("tierstore-group@%i.{TARGET}");
    let longest = unit_name_of_len(MAX_UNIT_NAME);
    let too_long = unit_name_of_len(MAX_UNIT_NAME + 1);
    for wanted in ["broken.invalid", specifier.as_str(), too_long.as_str()] {
        overwrite_unit(
            &world,
            "tierstore@.service",
            wanted_by_text(wanted).as_bytes(),
        );
        assert_eq!(
            lint(&world.fs, &world.roots, &manifest),
            Err(LintError::BadTarget {
                file: "tierstore@.service".to_owned(),
                target: wanted.to_owned()
            }),
            "{wanted}"
        );
    }
    overwrite_unit(
        &world,
        "tierstore@.service",
        wanted_by_text(&longest).as_bytes(),
    );
    assert_eq!(lint(&world.fs, &world.roots, &manifest), Ok(()));
}
