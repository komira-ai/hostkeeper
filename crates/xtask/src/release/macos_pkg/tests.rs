use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;

use super::*;
use crate::release::fakes::{FakeExec, FakePkgbuild, PkgCall, ProductCall, scratch, tree_of};
use crate::testdir::TestDir;

/// The binary the pkg holds; nothing runs it.
pub(super) const BINARY: &[u8] = b"the macos-arm64 hostkeeper binary";

pub(super) const PLIST: &str = "Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist";
pub(super) const BIN: &str = "usr/local/bin/hostkeeper";
pub(super) const NEWSYSLOG: &str = "private/etc/newsyslog.d/hostkeeper.conf";
pub(super) const GH: &str = "usr/local/lib/hostkeeper/gh";
pub(super) const ROOT: &str = "usr/local/lib/hostkeeper/trusted_root.json";
/// The `gh` the pkg holds; nothing runs it.
pub(super) const GH_BYTES: &[u8] = b"the pinned darwin-arm64 gh";
/// The trusted root the pkg holds, one JSON object per line as `gh attestation trusted-root`
/// prints it.
pub(super) const ROOT_BYTES: &[u8] = b"{\"mediaType\":\"trusted-root\"}\n";
pub(super) const LOG_DIR: &str = "private/var/log/hostkeeper";

/// The index of the binary's entry in [`good_entries`], and of `gh` and the trusted root.
pub(super) const BIN_AT: usize = 14;
pub(super) const GH_AT: usize = 17;
pub(super) const ROOT_AT: usize = 18;

/// The paths of the payload, from the install location, with `/` after a directory's.
const TREE: [&str; 18] = [
    "Library/",
    "Library/LaunchDaemons/",
    "Library/LaunchDaemons/dev.hostkeeper.hostkeeper.plist",
    "private/",
    "private/etc/",
    "private/etc/newsyslog.d/",
    "private/etc/newsyslog.d/hostkeeper.conf",
    "private/var/",
    "private/var/log/",
    "private/var/log/hostkeeper/",
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
         numberOfFiles=\"8\" installKBytes=\"1\"/>\n    <scripts>\n        <postinstall \
         file=\"./postinstall\"/>\n    </scripts>\n    <bundle-version/>\n    \
         <upgrade-bundle/>\n    <update-bundle/>\n    <atomic-update-bundle/>\n    \
         <strict-identifier/>\n    <relocate/>\n</pkg-info>\n"
    )
}

pub(super) fn package_info() -> String {
    package_info_with(
        "identifier=\"dev.hostkeeper.hostkeeper\" version=\"0.1.5\" install-location=\"/\"",
    )
}

/// The `Distribution` `productbuild --package` writes for the component pkg of 0.1.5 (the layout
/// assumed in [`distribution`], to be confirmed on the macOS runner), with `refs` as the
/// `<pkg-ref>` that refers to the component and `extra` among the root element's children.
pub(super) fn distribution_with(refs: &str, extra: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<installer-gui-script \
         minSpecVersion=\"1\">\n    <pkg-ref id=\"dev.hostkeeper.hostkeeper\"/>\n    <options \
         customize=\"never\" require-scripts=\"false\"/>\n    <choices-outline>\n        <line \
         choice=\"default\">\n            <line choice=\"dev.hostkeeper.hostkeeper\"/>\n        \
         </line>\n    </choices-outline>\n    <choice id=\"default\"/>\n    <choice \
         id=\"dev.hostkeeper.hostkeeper\" visible=\"false\">\n        <pkg-ref \
         id=\"dev.hostkeeper.hostkeeper\"/>\n    </choice>\n    {refs}\n    <product \
         id=\"dev.hostkeeper.hostkeeper\" version=\"0.1.5\"/>\n    {extra}\n\
         </installer-gui-script>\n"
    )
}

/// The reference to the component of the good `Distribution`.
pub(super) const COMPONENT_REF: &str = "<pkg-ref id=\"dev.hostkeeper.hostkeeper\" \
    version=\"0.1.5\" onConclusion=\"none\">#hostkeeper.pkg</pkg-ref>";

pub(super) fn distribution_text() -> String {
    distribution_with(COMPONENT_REF, "")
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

/// The payload `pkgbuild` writes for the binary `binary`: every entry, in its order.
pub(super) fn good_entries(binary: &[u8]) -> Vec<Vec<u8>> {
    let plist = plist_text().expect("the plist");
    let newsyslog = newsyslog_text();
    vec![
        dir_entry("."),
        dir_entry("./Library"),
        dir_entry("./Library/LaunchDaemons"),
        odc_entry(&format!("./{PLIST}"), 0o100_644, 0, 0, plist.as_bytes()),
        dir_entry("./private"),
        dir_entry("./private/etc"),
        dir_entry("./private/etc/newsyslog.d"),
        odc_entry(
            &format!("./{NEWSYSLOG}"),
            0o100_644,
            0,
            0,
            newsyslog.as_bytes(),
        ),
        dir_entry("./private/var"),
        dir_entry("./private/var/log"),
        dir_entry(&format!("./{LOG_DIR}")),
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

/// The bytes of the payload's given files: `binary`, `plist`, and the test's `gh` and trusted
/// root.
pub(super) fn contents(binary: &[u8], plist: &[u8]) -> Contents {
    Contents {
        binary: binary.to_vec(),
        plist: plist.to_vec(),
        gh: GH_BYTES.to_vec(),
        trusted_root: ROOT_BYTES.to_vec(),
    }
}

/// The sources of the pkg as [`write_sources`] writes them under `dir`.
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

/// A product archive expanded as `pkgutil --expand` leaves it (`pkg` holds the `Distribution` and
/// the component's directory), with `entries` as the component's payload, and the binary, `gh`
/// and trusted root it is checked against, in a scratch directory of its own.
pub(super) struct Expanded {
    pub(super) dir: TestDir,
}

impl Expanded {
    pub(super) fn new(name: &str, entries: &[Vec<u8>]) -> Expanded {
        let dir = scratch(name);
        std::fs::create_dir(dir.join("pkg")).expect("the expanded product archive");
        std::fs::create_dir(dir.join("pkg").join(COMPONENT)).expect("the expanded component");
        write_sources(&dir);
        let expanded = Expanded { dir };
        std::fs::write(expanded.product().join(DISTRIBUTION), distribution_text())
            .expect("the distribution");
        expanded.write("Bom", b"BOMStore");
        expanded.write("PackageInfo", package_info().as_bytes());
        expanded.write("Payload", &gzip(&odc(entries)));
        std::fs::create_dir(expanded.pkg().join("Scripts")).expect("the scripts directory");
        expanded.write_script(POSTINSTALL, postinstall_text().as_bytes(), SCRIPT_MODE);
        expanded
    }

    pub(super) fn good(name: &str) -> Expanded {
        Expanded::new(name, &good_entries(BINARY))
    }

    /// The top directory of the product archive.
    pub(super) fn product(&self) -> PathBuf {
        self.dir.join("pkg")
    }

    /// The directory of the component in the product archive.
    pub(super) fn pkg(&self) -> PathBuf {
        self.product().join(COMPONENT)
    }

    pub(super) fn sources(&self) -> Sources {
        sources_in(&self.dir)
    }

    pub(super) fn write(&self, file: &str, bytes: &[u8]) {
        std::fs::write(self.pkg().join(file), bytes).expect("write a file of the pkg");
    }

    /// Writes `bytes` as the script `name` of the `Scripts` directory, with the mode `mode`.
    pub(super) fn write_script(&self, name: &str, bytes: &[u8], mode: u32) {
        let at = self.pkg().join("Scripts").join(name);
        std::fs::write(&at, bytes).expect("write a script");
        std::fs::set_permissions(&at, std::fs::Permissions::from_mode(mode)).expect("its mode");
    }

    pub(super) fn check(&self) -> Result<Vec<String>, Error> {
        check(&self.product(), &version(), &self.sources())
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
    // hostkeeper-launchd defines (the label, the LaunchDaemons directory, the installed binary).
    assert_eq!(binary_path(), BIN);
    assert_eq!(plist_path(), PLIST);
    assert_eq!(
        format!("/{}", binary_path()),
        hostkeeper_launchd::plist::INSTALLED_BINARY
    );
    assert_eq!(
        Path::new("/").join(plist_path()),
        Launchd::macos()
            .unit_dir
            .join(format!("{}.plist", Label::daemon()))
    );
    assert_eq!(identifier(), "dev.hostkeeper.hostkeeper");
    assert_eq!(identifier(), Label::daemon().as_str());
    let want = payload(contents(BINARY, b"plist"));
    let paths: Vec<&str> = want.keys().map(String::as_str).collect();
    assert_eq!(
        paths,
        [
            "",
            "Library",
            "Library/LaunchDaemons",
            PLIST,
            "private",
            "private/etc",
            "private/etc/newsyslog.d",
            NEWSYSLOG,
            "private/var",
            "private/var/log",
            LOG_DIR,
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
    // runs another program than the binary the pkg installs (`current` of the self-update binary
    // directory among them).
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
    let cases: [(String, Vec<String>); 6] = [
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
            text.replace(
                "<string>/usr/local/bin/hostkeeper</string>",
                "<string>/var/lib/updater/state/hostkeeper/bin/current</string>",
            ),
            vec![format!(
                "{at}: the job runs Some(\"/var/lib/updater/state/hostkeeper/bin/current\"), not \
                 \"/usr/local/bin/hostkeeper\", the binary the pkg installs, whose start seeds \
                 the binary directory of a self-update and runs current"
            )],
        ),
        (
            text.replace(
                "/var/log/hostkeeper/daemon.err.log",
                "/var/log/daemon.err.log",
            ),
            vec![format!(
                "{at}: StandardErrorPath is Some(\"/var/log/daemon.err.log\"), not \
                 \"/var/log/hostkeeper/daemon.err.log\", the file the pkg's newsyslog entry \
                 rotates in the directory the pkg makes"
            )],
        ),
        (
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict></dict></plist>".to_owned(),
            vec![
                format!("{at}: the label is None, not \"dev.hostkeeper.hostkeeper\""),
                format!("{at}: the job runs None, not \"/usr/local/bin/hostkeeper\""),
                format!(
                    "{at}: StandardOutPath is None, not \"/var/log/hostkeeper/daemon.out.log\", \
                     the file the pkg's newsyslog entry rotates in the directory the pkg makes"
                ),
                format!(
                    "{at}: StandardErrorPath is None, not \"/var/log/hostkeeper/daemon.err.log\", \
                     the file the pkg's newsyslog entry rotates in the directory the pkg makes"
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
        stage: dir.join("stage"),
        out: dir.join("dist"),
    }
}

fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).expect("stat").mode() & 0o7777
}

#[test]
fn build_lays_out_exactly_the_payload_and_the_postinstall_with_their_modes_and_wraps_the_component()
{
    // Catches: a pkg with anything but its payload and its postinstall (another script, a
    // config, a stray file) or without gh, the trusted root or the postinstall, a mode launchd or
    // the installer would refuse, a gh or postinstall that cannot run, or a mode that would
    // change a system directory, pkgbuild asked for another identifier, version or file name, a
    // component written outside the stage, or the component, not the product, left in the output
    // directory.
    let dir = scratch("macos-pkg-build");
    let r = request(&dir);
    let fake = FakePkgbuild::default();
    let lines = build(&fake, &r).expect("build");
    let pkg = r.out.join("hostkeeper-0.1.5-macos-arm64.pkg");
    let component = r.stage.join("hostkeeper.pkg");
    let root = r.stage.join("root");
    let scripts = r.stage.join("scripts");
    assert_eq!(
        *fake.calls.borrow(),
        [PkgCall {
            root: root.clone(),
            scripts: scripts.clone(),
            identifier: "dev.hostkeeper.hostkeeper".to_owned(),
            version: version(),
            pkg: component.clone(),
        }]
    );
    assert_eq!(
        *fake.product_calls.borrow(),
        [ProductCall {
            component: component.clone(),
            identifier: "dev.hostkeeper.hostkeeper".to_owned(),
            version: version(),
            product: pkg.clone(),
        }]
    );
    assert_eq!(
        std::fs::read(&component).expect("the component"),
        FakePkgbuild::BYTES
    );
    assert_eq!(
        tree_of(&r.out),
        ["hostkeeper-0.1.5-macos-arm64.pkg".to_owned()]
    );
    assert_eq!(*fake.trees.borrow(), [TREE.map(str::to_owned).to_vec()]);
    assert_eq!(
        lines,
        [format!(
            "hostkeeper-0.1.5-macos-arm64.pkg: sha256 {}",
            file_sha256(&pkg).expect("hash")
        )]
    );
    assert_eq!(
        std::fs::read(&pkg).expect("the pkg"),
        FakePkgbuild::PRODUCT_BYTES
    );
    let mut laid_out: Vec<String> = TREE.iter().map(|p| format!("root/{p}")).collect();
    laid_out
        .extend(["hostkeeper.pkg", "root/", "scripts/", "scripts/postinstall"].map(str::to_owned));
    laid_out.sort();
    assert_eq!(tree_of(&r.stage), laid_out);
    assert_eq!(mode_of(&r.stage), 0o755);
    assert_eq!(mode_of(&root), 0o755);
    assert_eq!(mode_of(&scripts), 0o755);
    assert_eq!(mode_of(&scripts.join("postinstall")), 0o755);
    assert_eq!(
        std::fs::read_to_string(scripts.join("postinstall")).expect("the postinstall"),
        postinstall_text()
    );
    for path in TREE {
        let at = root.join(path.trim_end_matches('/'));
        let want = match path {
            PLIST | NEWSYSLOG | ROOT => 0o644,
            _ => 0o755,
        };
        assert_eq!(mode_of(&at), want, "{path}");
    }
    assert_eq!(std::fs::read(root.join(BIN)).expect("the binary"), BINARY);
    assert_eq!(
        std::fs::read_to_string(root.join(PLIST)).expect("the plist"),
        plist_text().expect("the plist")
    );
    assert_eq!(
        std::fs::read_to_string(root.join(NEWSYSLOG)).expect("the newsyslog file"),
        newsyslog_text()
    );
    let log_dir = std::fs::read_dir(root.join(LOG_DIR)).expect("the log directory");
    assert_eq!(log_dir.count(), 0);
    assert_eq!(std::fs::read(root.join(GH)).expect("gh"), GH_BYTES);
    assert_eq!(
        std::fs::read(root.join(ROOT)).expect("the trusted root"),
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
    // The archive pkgbuild makes of the payload's tree, owned by root: each entry with its mode
    // on disk.
    let archive = |dir: &Path, uid: u32| {
        let mut entries = vec![odc_entry(".", 0o040_000 | mode_of(dir), uid, uid, b"")];
        for path in tree_of(dir) {
            let name = path.trim_end_matches('/');
            let at = dir.join(name);
            let (kind, data) = if path.ends_with('/') {
                (0o040_000, Vec::new())
            } else {
                (0o100_000, std::fs::read(&at).expect("read"))
            };
            entries.push(odc_entry(
                &format!("./{name}"),
                kind | mode_of(&at),
                uid,
                uid,
                &data,
            ));
        }
        entries
    };
    let entries = archive(&r.stage.join("root"), 0);
    let expanded = Expanded::new("macos-pkg-round-trip-expanded", &entries);
    // `pkgutil --expand` expands the scripts archive into `Scripts`, each script with its mode.
    let scripts = expanded.pkg().join("Scripts");
    std::fs::remove_dir_all(&scripts).expect("remove");
    std::fs::create_dir(&scripts).expect("the scripts directory");
    for path in tree_of(&r.stage.join("scripts")) {
        let from = r.stage.join("scripts").join(&path);
        std::fs::copy(&from, scripts.join(&path)).expect("copy a script");
    }
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
fn a_pkgbuild_or_productbuild_failure_is_passed_on() {
    // Catches: a failed pkgbuild or productbuild reported as a built pkg, or productbuild run over
    // a component that was not built.
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
    assert!(fake.product_calls.borrow().is_empty());
    let dir = scratch("macos-pkg-productbuild-fails");
    let r = request(&dir);
    let fake = FakePkgbuild {
        fail_product: Some(PortError::new("productbuild", "no package")),
        ..FakePkgbuild::default()
    };
    assert_eq!(
        build(&fake, &r),
        Err(Error::Port(PortError::new("productbuild", "no package")))
    );
    assert!(!r.out.join("hostkeeper-0.1.5-macos-arm64.pkg").exists());
}

/// The command line the adapter runs for `productbuild` over the component `component`, into the
/// product `product`.
fn productbuild_line(component: &str, product: &str) -> Vec<String> {
    [
        "productbuild",
        "--package",
        component,
        "--identifier",
        "dev.hostkeeper.hostkeeper",
        "--version",
        "0.1.5",
        product,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The command line the adapter runs for the tree `root`, the scripts directory `scripts` and
/// the pkg `pkg`.
fn pkgbuild_line(root: &str, scripts: &str, pkg: &str) -> Vec<String> {
    [
        "pkgbuild",
        "--root",
        root,
        "--scripts",
        scripts,
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
fn pkgbuild_runs_once_with_a_fixed_argument_list_and_the_scripts_directory() {
    // Catches: pkgbuild run without the scripts directory (a pkg whose job has nothing to start),
    // another install location, ownership taken from the build user, or any argument out of
    // place.
    let line = pkgbuild_line("/stage/root", "/stage/scripts", "/out/x.pkg");
    let words: Vec<&str> = line.iter().map(String::as_str).collect();
    let cli = PkgbuildCli::new(FakeExec::default().on(&words, true, "", ""));
    assert_eq!(
        cli.build_pkg(
            Path::new("/stage/root"),
            Path::new("/stage/scripts"),
            "dev.hostkeeper.hostkeeper",
            &version(),
            Path::new("/out/x.pkg")
        ),
        Ok(())
    );
    assert_eq!(*cli.exec.calls.borrow(), std::slice::from_ref(&line));
}

#[test]
fn productbuild_runs_once_over_the_component_with_a_fixed_argument_list_and_no_signature() {
    // Catches: productbuild run over another file than the component, without the product's
    // identifier or version (the Distribution would carry none, and the check refuses it), with
    // a signing identity, or any argument out of place.
    let line = productbuild_line("/stage/hostkeeper.pkg", "/out/x.pkg");
    let words: Vec<&str> = line.iter().map(String::as_str).collect();
    let cli = PkgbuildCli::new(FakeExec::default().on(&words, true, "", ""));
    assert_eq!(
        cli.build_product(
            Path::new("/stage/hostkeeper.pkg"),
            "dev.hostkeeper.hostkeeper",
            &version(),
            Path::new("/out/x.pkg")
        ),
        Ok(())
    );
    assert_eq!(*cli.exec.calls.borrow(), std::slice::from_ref(&line));
}

#[test]
fn a_productbuild_that_fails_or_cannot_start_is_a_port_error_with_its_words() {
    // Catches: a failed productbuild taken as success, or an error that does not say what ran.
    let line = productbuild_line("/c.pkg", "/p.pkg");
    let words: Vec<&str> = line.iter().map(String::as_str).collect();
    let what = format!("productbuild {}", line[1..].join(" "));
    let cli = PkgbuildCli::new(FakeExec::default().on(&words, false, "", "productbuild: no\n"));
    let build = |cli: &PkgbuildCli<FakeExec>, component: &str| {
        cli.build_product(
            Path::new(component),
            "dev.hostkeeper.hostkeeper",
            &version(),
            Path::new("/p.pkg"),
        )
    };
    assert_eq!(
        build(&cli, "/c.pkg"),
        Err(PortError::new(&what, "productbuild: no"))
    );
    let other = build(&cli, "/other.pkg").expect_err("not scripted");
    assert_eq!(
        other.what,
        format!(
            "productbuild {}",
            productbuild_line("/other.pkg", "/p.pkg")[1..].join(" ")
        )
    );
    assert!(
        other.why.starts_with("not scripted: productbuild"),
        "{other:?}"
    );
}

#[test]
fn a_pkgbuild_that_fails_or_cannot_start_is_a_port_error_with_its_words() {
    // Catches: a failed pkgbuild taken as success, or an error that does not say what ran.
    let line = pkgbuild_line("/r", "/s", "/p.pkg");
    let words: Vec<&str> = line.iter().map(String::as_str).collect();
    let what = format!("pkgbuild {}", line[1..].join(" "));
    let cli =
        PkgbuildCli::new(FakeExec::default().on(&words, false, "", "pkgbuild: no such root\n"));
    let build = |cli: &PkgbuildCli<FakeExec>, root: &str| {
        cli.build_pkg(
            Path::new(root),
            Path::new("/s"),
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
            pkgbuild_line("/other", "/s", "/p.pkg")[1..].join(" ")
        )
    );
    assert!(other.why.starts_with("not scripted: pkgbuild"), "{other:?}");
}

#[test]
fn a_path_that_is_not_utf8_is_a_port_error_and_runs_nothing() {
    // Catches: a path mangled into a command line that names another directory.
    let cli = PkgbuildCli::new(FakeExec::default());
    let bad = Path::new(OsStr::from_bytes(b"/stage/\xff"));
    let (good, scripts, pkg) = (Path::new("/r"), Path::new("/s"), Path::new("/p.pkg"));
    for (root, scripts, pkg) in [(bad, scripts, pkg), (good, bad, pkg), (good, scripts, bad)] {
        let e = cli
            .build_pkg(root, scripts, "dev.hostkeeper.hostkeeper", &version(), pkg)
            .expect_err("not UTF-8");
        assert_eq!(
            e,
            PortError::new(
                format!("pkgbuild {}", bad.display()),
                "the path is not UTF-8"
            )
        );
    }
    let e = cli
        .build_product(bad, "dev.hostkeeper.hostkeeper", &version(), pkg)
        .expect_err("not UTF-8");
    assert_eq!(
        e,
        PortError::new(
            format!("productbuild {}", bad.display()),
            "the path is not UTF-8"
        )
    );
    let e = cli
        .build_product(good, "dev.hostkeeper.hostkeeper", &version(), bad)
        .expect_err("not UTF-8");
    assert_eq!(e.why, "the path is not UTF-8");
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

mod distribution;
mod logs;
mod self_update;
