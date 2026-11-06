use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;

use super::*;
use crate::release::fakes::{FakeExec, FakePkgbuild, PkgCall, scratch, tree_of};
use crate::testdir::TestDir;

/// The binary the pkg holds; nothing runs it.
pub(super) const BINARY: &[u8] = b"the macos-arm64 hostkeeper binary";
/// The `gh` the pkg holds; nothing runs it.
pub(super) const GH_BYTES: &[u8] = b"the pinned darwin-arm64 gh";
/// The trusted root the pkg holds, one JSON object per line as `gh attestation trusted-root`
/// prints it.
pub(super) const ROOT_BYTES: &[u8] = b"{\"mediaType\":\"trusted-root\"}\n";

pub(super) const PLIST: &str = "Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist";
pub(super) const BIN: &str = "usr/local/bin/hostkeeper";
pub(super) const GH: &str = "usr/local/lib/hostkeeper/gh";
pub(super) const ROOT: &str = "usr/local/lib/hostkeeper/trusted_root.json";

/// The paths of the payload, from the install location, with `/` after a directory's.
const TREE: [&str; 11] = [
    "Library/",
    "Library/LaunchDaemons/",
    "Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist",
    "usr/",
    "usr/local/",
    "usr/local/bin/",
    "usr/local/bin/hostkeeper",
    "usr/local/lib/",
    "usr/local/lib/hostkeeper/",
    "usr/local/lib/hostkeeper/gh",
    "usr/local/lib/hostkeeper/trusted_root.json",
];

pub(super) fn version() -> Version {
    "0.1.5".parse().expect("a version")
}

/// `PackageInfo` as `pkgbuild` writes it for the pkg of 0.1.5, with `attributes` in place of
/// the identifier, version and install location.
pub(super) fn package_info_with(attributes: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<pkg-info overwrite-permissions=\"true\" \
         relocatable=\"false\" {attributes} postinstall-action=\"none\" format-version=\"2\" \
         generator-version=\"InstallCmds-845 (24A335)\" auth=\"root\">\n    <payload \
         numberOfFiles=\"8\" installKBytes=\"1\"/>\n    <bundle-version/>\n    \
         <upgrade-bundle/>\n    <update-bundle/>\n    <atomic-update-bundle/>\n    \
         <strict-identifier/>\n    <relocate/>\n</pkg-info>\n"
    )
}

pub(super) fn package_info() -> String {
    package_info_with(
        "identifier=\"dev.hostkeeper.hostkeeper\" version=\"0.1.5\" install-location=\"/\"",
    )
}

/// One portable (`odc`) cpio entry: a header, the name and its NUL, the data.
pub(super) fn odc_entry(name: &str, mode: u32, uid: u32, gid: u32, data: &[u8]) -> Vec<u8> {
    let header = format!(
        "070707{:06o}{:06o}{mode:06o}{uid:06o}{gid:06o}{:06o}{:06o}{:011o}{:06o}{:011o}",
        1,
        7,
        1,
        0,
        1_800_000_000_u64,
        name.len() + 1,
        data.len()
    );
    assert_eq!(header.len(), ODC_HEADER);
    [header.as_bytes(), name.as_bytes(), b"\0", data].concat()
}

/// A cpio archive of `entries`, then the trailer, then a block's padding.
pub(super) fn odc(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = entries.concat();
    bytes.extend(odc_entry(TRAILER, 0, 0, 0, b""));
    bytes.extend([0_u8; 120]);
    bytes
}

pub(super) fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(bytes).expect("compress");
    gz.finish().expect("compress")
}

pub(super) fn dir_entry(name: &str) -> Vec<u8> {
    odc_entry(name, 0o040_755, 0, 0, b"")
}

/// The payload `pkgbuild` writes for the binary `binary`: every entry, in its order. The entry
/// of the binary is the 8th (index 7), of `gh` the 11th (index 10) and of the trusted root the
/// 12th (index 11).
pub(super) fn good_entries(binary: &[u8]) -> Vec<Vec<u8>> {
    let plist = plist_text().expect("the plist");
    vec![
        dir_entry("."),
        dir_entry("./Library"),
        dir_entry("./Library/LaunchDaemons"),
        odc_entry(&format!("./{PLIST}"), 0o100_644, 0, 0, plist.as_bytes()),
        dir_entry("./usr"),
        dir_entry("./usr/local"),
        dir_entry("./usr/local/bin"),
        odc_entry(&format!("./{BIN}"), 0o100_755, 0, 0, binary),
        dir_entry("./usr/local/lib"),
        dir_entry("./usr/local/lib/hostkeeper"),
        odc_entry(&format!("./{GH}"), 0o100_755, 0, 0, GH_BYTES),
        odc_entry(&format!("./{ROOT}"), 0o100_644, 0, 0, ROOT_BYTES),
    ]
}

/// The sources of the pkg as written under `dir` by [`write_sources`].
pub(super) fn sources_in(dir: &Path) -> Sources {
    Sources {
        binary: dir.join("hostkeeper"),
        gh: dir.join("gh"),
        trusted_root: dir.join("trusted_root.json"),
    }
}

/// Writes the binary, `gh` and the trusted root under `dir`; their paths.
pub(super) fn write_sources(dir: &Path) -> Sources {
    let sources = sources_in(dir);
    std::fs::write(&sources.binary, BINARY).expect("the binary");
    std::fs::write(&sources.gh, GH_BYTES).expect("gh");
    std::fs::write(&sources.trusted_root, ROOT_BYTES).expect("the trusted root");
    sources
}

/// A pkg expanded as `pkgutil --expand` leaves it, with `entries` as its payload, and the binary,
/// `gh` and trusted root it is checked against, in a scratch directory of its own.
pub(super) struct Expanded {
    pub(super) dir: TestDir,
}

impl Expanded {
    pub(super) fn new(name: &str, entries: &[Vec<u8>]) -> Expanded {
        let dir = scratch(name);
        std::fs::create_dir(dir.join("pkg")).expect("the expanded pkg");
        write_sources(&dir);
        let expanded = Expanded { dir };
        expanded.write("Bom", b"BOMStore");
        expanded.write("PackageInfo", package_info().as_bytes());
        expanded.write("Payload", &gzip(&odc(entries)));
        expanded
    }

    pub(super) fn good(name: &str) -> Expanded {
        Expanded::new(name, &good_entries(BINARY))
    }

    pub(super) fn pkg(&self) -> PathBuf {
        self.dir.join("pkg")
    }

    pub(super) fn sources(&self) -> Sources {
        sources_in(&self.dir)
    }

    pub(super) fn write(&self, file: &str, bytes: &[u8]) {
        std::fs::write(self.pkg().join(file), bytes).expect("write a file of the pkg");
    }

    pub(super) fn check(&self) -> Result<Vec<String>, Error> {
        check(&self.pkg(), &version(), &self.sources())
    }

    pub(super) fn problems(&self) -> Vec<String> {
        match self.check() {
            Err(Error::Refused(lines)) => lines,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}

#[test]
fn the_pkg_installs_the_daemon_binary_and_its_launchd_plist_as_hostkeeper_launchd_names_them() {
    // Catches: a binary or plist path, a plist file name or an identifier that is not the one
    // hostkeeper-launchd defines (the label, the LaunchDaemons directory, the program path).
    assert_eq!(binary_path(), BIN);
    assert_eq!(plist_path(), PLIST);
    assert_eq!(
        format!("/{}", binary_path()),
        hostkeeper_launchd::plist::DAEMON_PROGRAM
    );
    assert_eq!(
        Path::new("/").join(plist_path()),
        Launchd::macos()
            .unit_dir
            .join(format!("{}.plist", Label::daemon()))
    );
    assert_eq!(identifier(), "dev.hostkeeper.hostkeeper");
    assert_eq!(identifier(), Label::daemon().as_str());
    let want = payload(Contents {
        binary: BINARY.to_vec(),
        plist: b"plist".to_vec(),
        gh: GH_BYTES.to_vec(),
        trusted_root: ROOT_BYTES.to_vec(),
    });
    let paths: Vec<&str> = want.keys().map(String::as_str).collect();
    assert_eq!(
        paths,
        [
            "",
            "Library",
            "Library/LaunchDaemons",
            PLIST,
            "usr",
            "usr/local",
            "usr/local/bin",
            BIN,
            "usr/local/lib",
            "usr/local/lib/hostkeeper",
            GH,
            ROOT
        ]
    );
    assert_eq!(
        want[BIN],
        Want::File {
            mode: 0o755,
            bytes: BINARY.to_vec()
        }
    );
    assert_eq!(
        want[PLIST],
        Want::File {
            mode: 0o644,
            bytes: b"plist".to_vec()
        }
    );
    assert_eq!(
        want[GH],
        Want::File {
            mode: 0o755,
            bytes: GH_BYTES.to_vec()
        }
    );
    assert_eq!(
        want[ROOT],
        Want::File {
            mode: 0o644,
            bytes: ROOT_BYTES.to_vec()
        }
    );
}

#[test]
fn gh_and_the_trusted_root_are_where_the_daemon_reads_them_on_a_mac() {
    // Catches: the pkg installing `gh` or the trusted root at another path than the one the
    // daemon reads on a Mac, so the daemon's fetch or verify finds nothing; and the daemon
    // reading a CA bundle other than macOS's own, which the pkg does not install.
    let mac = hostkeeper_daemon::cli::HostFiles::for_platform(
        hostkeeper_launchd::PlatformKind::Macos,
        |_| false,
    );
    assert_eq!(Path::new("/").join(gh_path()), mac.gh);
    assert_eq!(Path::new("/").join(trusted_root_path()), mac.trusted_root);
    assert_eq!(
        (gh_path().as_str(), trusted_root_path().as_str()),
        (GH, ROOT)
    );
    assert_eq!(mac.ca_bundle, Path::new("/etc/ssl/cert.pem"));
}

#[test]
fn the_plist_parses_and_names_the_label_and_the_binary_hostkeeper_launchd_uses() {
    // Catches: a plist that does not parse, that names another label than the daemon's, or that
    // runs another program than the binary the pkg installs.
    let text = plist_text().expect("the plist");
    assert_eq!(
        text,
        hostkeeper_launchd::daemon_plist(&DaemonJob::standard()).expect("the plist")
    );
    let value = plist::Value::from_reader_xml(text.as_bytes()).expect("the plist parses");
    let dict = value.as_dictionary().expect("a dictionary");
    assert_eq!(
        dict.get("Label").and_then(plist::Value::as_string),
        Some("dev.hostkeeper.hostkeeper")
    );
    let args: Vec<&str> = dict
        .get("ProgramArguments")
        .and_then(plist::Value::as_array)
        .expect("arguments")
        .iter()
        .filter_map(plist::Value::as_string)
        .collect();
    assert_eq!(args, ["/usr/local/bin/hostkeeper", "daemon"]);
    let mut problems = Vec::new();
    plist_problems(text.as_bytes(), &mut problems);
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn a_plist_that_does_not_parse_or_names_another_label_or_program_is_listed() {
    // Catches: the read-back accepting a plist launchd would not load, or one for another job.
    let at = format!("/{PLIST}");
    let text = plist_text().expect("the plist");
    let cases: [(String, Vec<String>); 5] = [
        (
            "<plist><dict><key>Label</key>".to_owned(),
            vec![format!("{at} does not parse as a plist: ")],
        ),
        (
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><array/></plist>".to_owned(),
            vec![format!("{at} is not a plist dictionary")],
        ),
        (
            text.replace("dev.hostkeeper.hostkeeper", "dev.hostkeeper.other"),
            vec![format!(
                "{at}: the label is Some(\"dev.hostkeeper.other\"), not \"dev.hostkeeper.hostkeeper\""
            )],
        ),
        (
            text.replace("/usr/local/bin/hostkeeper", "/tmp/hostkeeper"),
            vec![format!(
                "{at}: the job runs Some(\"/tmp/hostkeeper\"), not the pkg's binary \
                 \"/usr/local/bin/hostkeeper\""
            )],
        ),
        (
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict></dict></plist>".to_owned(),
            vec![
                format!("{at}: the label is None, not \"dev.hostkeeper.hostkeeper\""),
                format!(
                    "{at}: the job runs None, not the pkg's binary \"/usr/local/bin/hostkeeper\""
                ),
            ],
        ),
    ];
    for (bytes, want) in cases {
        let mut problems = Vec::new();
        plist_problems(bytes.as_bytes(), &mut problems);
        assert_eq!(problems.len(), want.len(), "{problems:?}");
        for (got, want) in problems.iter().zip(&want) {
            assert!(got.starts_with(want.as_str()), "{got:?} is not {want:?}");
        }
    }
}

/// A request for the pkg of 0.1.5 over the binary, `gh` and trusted root written under `dir`.
fn request(dir: &Path) -> Request {
    Request {
        version: version(),
        sources: write_sources(dir),
        stage: dir.join("root"),
        out: dir.join("dist"),
    }
}

fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).expect("stat").mode() & 0o7777
}

#[test]
fn build_lays_out_exactly_the_binary_the_plist_gh_and_the_trusted_root_and_runs_pkgbuild_once() {
    // Catches: a pkg with anything but the binary, the plist, gh and the trusted root (a
    // script, a config, a stray file), or without one of them; a mode launchd or the installer
    // would refuse, a gh that cannot run, or a mode that would change a system directory; or
    // pkgbuild asked for another identifier, version or file name.
    let dir = scratch("macos-pkg-build");
    let r = request(&dir);
    let fake = FakePkgbuild::default();
    let lines = build(&fake, &r).expect("build");
    let pkg = r.out.join("hostkeeper-0.1.5-macos-arm64.pkg");
    assert_eq!(
        *fake.calls.borrow(),
        [PkgCall {
            root: r.stage.clone(),
            identifier: "dev.hostkeeper.hostkeeper".to_owned(),
            version: version(),
            pkg: pkg.clone(),
        }]
    );
    assert_eq!(*fake.trees.borrow(), [TREE.map(str::to_owned).to_vec()]);
    assert_eq!(
        lines,
        [format!(
            "hostkeeper-0.1.5-macos-arm64.pkg: sha256 {}",
            file_sha256(&pkg).expect("hash")
        )]
    );
    assert_eq!(std::fs::read(&pkg).expect("the pkg"), FakePkgbuild::BYTES);
    assert_eq!(mode_of(&r.stage), 0o755);
    for path in TREE {
        let at = r.stage.join(path.trim_end_matches('/'));
        let want = match path {
            PLIST | ROOT => 0o644,
            _ => 0o755,
        };
        assert_eq!(mode_of(&at), want, "{path}");
    }
    assert_eq!(
        std::fs::read(r.stage.join(BIN)).expect("the binary"),
        BINARY
    );
    assert_eq!(
        std::fs::read_to_string(r.stage.join(PLIST)).expect("the plist"),
        plist_text().expect("the plist")
    );
    assert_eq!(std::fs::read(r.stage.join(GH)).expect("gh"), GH_BYTES);
    assert_eq!(
        std::fs::read(r.stage.join(ROOT)).expect("the trusted root"),
        ROOT_BYTES
    );
}

#[test]
fn what_build_lays_out_is_what_the_check_accepts() {
    // Catches: the builder and the read-back disagreeing on a path, a mode or the bytes, so that
    // the pkg built on the macOS runner fails its own check, or a bad one passes.
    let dir = scratch("macos-pkg-round-trip");
    let r = request(&dir);
    build(&FakePkgbuild::default(), &r).expect("build");
    // The payload pkgbuild makes of the tree: each entry with its mode on disk, owned by root.
    let mut entries = vec![odc_entry(".", 0o040_000 | mode_of(&r.stage), 0, 0, b"")];
    for path in tree_of(&r.stage) {
        let name = path.trim_end_matches('/');
        let at = r.stage.join(name);
        let (kind, data) = if path.ends_with('/') {
            (0o040_000, Vec::new())
        } else {
            (0o100_000, std::fs::read(&at).expect("read"))
        };
        entries.push(odc_entry(
            &format!("./{name}"),
            kind | mode_of(&at),
            0,
            0,
            &data,
        ));
    }
    let expanded = Expanded::new("macos-pkg-round-trip-expanded", &entries);
    assert!(expanded.check().is_ok(), "{:?}", expanded.check());
}

#[test]
fn build_refuses_a_stage_that_exists_and_runs_nothing() {
    // Catches: a pkg built from a directory that already holds other files, which pkgbuild
    // would put into the payload.
    let dir = scratch("macos-pkg-stage-exists");
    let r = request(&dir);
    std::fs::create_dir(&r.stage).expect("a stage that exists");
    let fake = FakePkgbuild::default();
    let refusal = Err(Error::Failed(format!(
        "{} exists: the pkg's tree is laid out in a new directory, so it holds nothing else",
        r.stage.display()
    )));
    assert_eq!(build(&fake, &r), refusal);
    std::fs::remove_dir(&r.stage).expect("remove");
    std::os::unix::fs::symlink(dir.join("nowhere"), &r.stage).expect("a dangling link");
    assert_eq!(build(&fake, &r), refusal);
    assert!(fake.calls.borrow().is_empty());
    assert!(!r.out.exists());
}

#[test]
fn a_missing_binary_gh_or_trusted_root_or_an_unwritable_place_is_an_io_error_naming_the_path() {
    // Catches: a pkg built without its binary, gh or trusted root, or a failure to lay out or
    // write reported as success; pkgbuild is never asked, and nothing is laid out.
    let dir = scratch("macos-pkg-io");
    let fake = FakePkgbuild::default();
    let absent = dir.join("absent");
    for missing in 0..3 {
        let mut r = request(&dir);
        let source = [
            &mut r.sources.binary,
            &mut r.sources.gh,
            &mut r.sources.trusted_root,
        ];
        *source.into_iter().nth(missing).expect("a source") = absent.clone();
        assert!(matches!(
            build(&fake, &r),
            Err(Error::Io { what, .. }) if what == format!("read {}", absent.display())
        ));
        assert!(!r.stage.exists());
    }
    let mut r = request(&dir);
    r.stage = dir.join("no-parent").join("root");
    assert!(matches!(
        build(&fake, &r),
        Err(Error::Io { what, .. }) if what == format!("create {}", r.stage.display())
    ));
    let r = request(&dir);
    std::fs::write(&r.out, b"a file in the way").expect("write");
    assert!(matches!(
        build(&fake, &r),
        Err(Error::Io { what, .. }) if what == format!("create {}", r.out.display())
    ));
    assert!(fake.calls.borrow().is_empty());
}

#[test]
fn a_pkgbuild_failure_is_passed_on() {
    // Catches: a failed pkgbuild reported as a built pkg.
    let dir = scratch("macos-pkg-pkgbuild-fails");
    let r = request(&dir);
    let fake = FakePkgbuild {
        fail: Some(PortError::new("pkgbuild", "no root")),
        ..FakePkgbuild::default()
    };
    assert_eq!(
        build(&fake, &r),
        Err(Error::Port(PortError::new("pkgbuild", "no root")))
    );
    assert!(!r.out.join("hostkeeper-0.1.5-macos-arm64.pkg").exists());
}

/// The command line the adapter runs for the tree `root` and the pkg `pkg`.
fn pkgbuild_line(root: &str, pkg: &str) -> Vec<String> {
    [
        "pkgbuild",
        "--root",
        root,
        "--identifier",
        "dev.hostkeeper.hostkeeper",
        "--version",
        "0.1.5",
        "--install-location",
        "/",
        "--ownership",
        "recommended",
        pkg,
    ]
    .map(str::to_owned)
    .to_vec()
}

#[test]
fn pkgbuild_runs_once_with_a_fixed_argument_list_and_no_scripts() {
    // Catches: pkgbuild run with scripts, another install location, ownership taken from the
    // build user, or any argument out of place.
    let line = pkgbuild_line("/stage/root", "/out/x.pkg");
    let words: Vec<&str> = line.iter().map(String::as_str).collect();
    let cli = PkgbuildCli::new(FakeExec::default().on(&words, true, "", ""));
    assert_eq!(
        cli.build_pkg(
            Path::new("/stage/root"),
            "dev.hostkeeper.hostkeeper",
            &version(),
            Path::new("/out/x.pkg")
        ),
        Ok(())
    );
    assert_eq!(*cli.exec.calls.borrow(), std::slice::from_ref(&line));
    assert!(!line.iter().any(|w| w.contains("script")));
}

#[test]
fn a_pkgbuild_that_fails_or_cannot_start_is_a_port_error_with_its_words() {
    // Catches: a failed pkgbuild taken as success, or an error that does not say what ran.
    let line = pkgbuild_line("/r", "/p.pkg");
    let words: Vec<&str> = line.iter().map(String::as_str).collect();
    let what = format!("pkgbuild {}", line[1..].join(" "));
    let cli =
        PkgbuildCli::new(FakeExec::default().on(&words, false, "", "pkgbuild: no such root\n"));
    let build = |cli: &PkgbuildCli<FakeExec>, root: &str| {
        cli.build_pkg(
            Path::new(root),
            "dev.hostkeeper.hostkeeper",
            &version(),
            Path::new("/p.pkg"),
        )
    };
    assert_eq!(
        build(&cli, "/r"),
        Err(PortError::new(&what, "pkgbuild: no such root"))
    );
    let other = build(&cli, "/other").expect_err("not scripted");
    assert_eq!(
        other.what,
        format!(
            "pkgbuild {}",
            pkgbuild_line("/other", "/p.pkg")[1..].join(" ")
        )
    );
    assert!(other.why.starts_with("not scripted: pkgbuild"), "{other:?}");
}

#[test]
fn a_path_that_is_not_utf8_is_a_port_error_and_runs_nothing() {
    // Catches: a path mangled into a command line that names another directory.
    let cli = PkgbuildCli::new(FakeExec::default());
    let bad = Path::new(OsStr::from_bytes(b"/stage/\xff"));
    for (root, pkg) in [(bad, Path::new("/p.pkg")), (Path::new("/r"), bad)] {
        let e = cli
            .build_pkg(root, "dev.hostkeeper.hostkeeper", &version(), pkg)
            .expect_err("not UTF-8");
        assert_eq!(
            e,
            PortError::new(
                format!("pkgbuild {}", bad.display()),
                "the path is not UTF-8"
            )
        );
    }
    assert!(cli.exec.calls.borrow().is_empty());
}

#[test]
fn an_odc_archive_reads_back_entry_by_entry_up_to_its_trailer() {
    // Catches: a reader that drops an entry, reads a field at the wrong offset, or reads the
    // padding after the trailer as entries.
    let bytes = odc(&[
        odc_entry(".", 0o040_755, 0, 0, b""),
        odc_entry("./a", 0o100_644, 501, 20, b"data"),
    ]);
    assert_eq!(
        read_odc(&bytes),
        Ok(vec![
            CpioEntry {
                name: ".".to_owned(),
                mode: 0o040_755,
                uid: 0,
                gid: 0,
                data: Vec::new(),
            },
            CpioEntry {
                name: "./a".to_owned(),
                mode: 0o100_644,
                uid: 501,
                gid: 20,
                data: b"data".to_vec(),
            },
        ])
    );
    assert_eq!(read_odc(&odc(&[])), Ok(Vec::new()));
}

#[test]
fn an_odc_archive_that_is_cut_short_or_malformed_is_refused_with_where() {
    // Catches: a payload read past its end, a newc or binary cpio read as odc, or a field or
    // name taken loosely.
    let entry = odc_entry("./a", 0o100_644, 0, 0, b"data");
    let header_at = entry.len();
    let set = |at: usize, bytes: &[u8]| {
        let mut e = entry.clone();
        e[at..at + bytes.len()].copy_from_slice(bytes);
        e
    };
    let cases: [(Vec<u8>, String); 8] = [
        (
            entry.clone(),
            format!("the archive ends at byte {header_at} without its trailer"),
        ),
        (
            Vec::new(),
            "the archive ends at byte 0 without its trailer".to_owned(),
        ),
        (
            odc(&[set(0, b"070701")]),
            "the header at byte 0 starts \"070701\", not the odc magic 070707".to_owned(),
        ),
        (
            odc(&[set(18, b"10064x")]),
            "the header field at byte 18 is not 6 octal digits".to_owned(),
        ),
        (
            odc(&[set(76 + 3, b"!")]),
            "the name at byte 76 does not end in NUL".to_owned(),
        ),
        (
            odc(&[set(76 + 2, b"\xff")]),
            "the name at byte 76 is not UTF-8".to_owned(),
        ),
        (
            entry[..76 + 2].to_vec(),
            "the archive ends inside the name that starts at byte 76".to_owned(),
        ),
        (
            entry[..entry.len() - 1].to_vec(),
            "the archive ends inside the data that starts at byte 80".to_owned(),
        ),
    ];
    for (bytes, want) in cases {
        assert_eq!(read_odc(&bytes), Err(want.clone()), "{want}");
    }
    // A name size of zero has no NUL to end it.
    let empty = odc(&[set(59, b"000000")]);
    assert_eq!(
        read_odc(&empty),
        Err("the name at byte 76 does not end in NUL".to_owned())
    );
}
