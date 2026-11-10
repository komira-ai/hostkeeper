//! The check of a built deb or rpm against the staged files, on packages laid out here as nfpm 2.48.0 lays them out (an
//! `ar` with `data.tar.gz`; a lead, a signature header, a header and a gzip newc `cpio`), each
//! changed in the one way a case names.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::rpm::*;
use super::*;
use crate::testdir::TestDir;

mod coordinator;
mod elf_and_depends;

const GH: &[u8] = b"a staged gh";
const ROOT: &[u8] = b"{\"mediaType\":\"application/vnd.dev.sigstore.trustedroot+json\"}\n";

fn scratch(name: &str) -> TestDir {
    let exe = std::env::current_exe().expect("test binary");
    TestDir::under(
        &exe.parent().expect("deps").join("xtask-linux-check-unit"),
        name,
    )
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(bytes).expect("gzip");
    e.finish().expect("gzip")
}

/// One file of a package as a case lays it out.
#[derive(Clone)]
struct File {
    path: &'static str,
    mode: u32,
    owner: &'static str,
    uid: u64,
    group: &'static str,
    directory: bool,
    data: Vec<u8>,
    /// What the entry points at, for a link.
    link: Option<&'static str>,
}

/// The path of the console's issue link in a package, and what it points at.
const LINK: &str = "./etc/issue.d/hostkeeper.issue";
const TARGET: &str = "/run/hostkeeper/enrollment.issue";

/// The two files and the issue link as nfpm.yaml installs them, and the daemon's binary beside
/// them.
fn files() -> Vec<File> {
    let file = |path, mode, data: &[u8]| File {
        path,
        mode,
        owner: "root",
        uid: 0,
        group: "root",
        directory: false,
        data: data.to_vec(),
        link: None,
    };
    vec![
        file("./usr/bin/hostkeeper", 0o755, b"the daemon"),
        file("./usr/lib/hostkeeper/gh", 0o755, GH),
        file("./usr/lib/hostkeeper/trusted_root.json", 0o644, ROOT),
        File {
            link: Some(TARGET),
            ..file(LINK, 0o777, b"")
        },
        file("./usr/bin/hostkeeper-pxe", 0o755, b"the boot server"),
    ]
}

/// `files` with the one at `path` changed by `change`.
fn with(path: &str, change: impl FnOnce(&mut File)) -> Vec<File> {
    let mut all = files();
    change(all.iter_mut().find(|f| f.path == path).expect("a file"));
    all
}

fn tar(files: &[File]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for f in files {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(if f.directory {
            tar::EntryType::Directory
        } else if f.link.is_some() {
            tar::EntryType::Symlink
        } else {
            tar::EntryType::Regular
        });
        if let Some(target) = f.link {
            h.set_link_name(target).expect("the link's target");
        }
        h.set_mode(f.mode);
        h.set_uid(f.uid);
        h.set_gid(0);
        h.set_username(f.owner).expect("owner");
        h.set_groupname(f.group).expect("group");
        h.set_size(f.data.len() as u64);
        b.append_data(&mut h, f.path, f.data.as_slice())
            .expect("entry");
    }
    b.into_inner().expect("tar")
}

/// An `ar` of `members`, each padded to an even length.
fn ar(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in members {
        out.extend(
            format!(
                "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                0,
                0,
                0,
                100_644,
                data.len()
            )
            .as_bytes(),
        );
        out.extend(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

/// The `control` of a deb that declares nothing.
const CONTROL: &str = "Package: hostkeeper\nVersion: 1.0.0\nArchitecture: amd64\n";

fn control_tar(control: &str) -> Vec<u8> {
    tar(&[File {
        path: "./control",
        mode: 0o644,
        owner: "root",
        uid: 0,
        group: "root",
        directory: false,
        data: control.as_bytes().to_vec(),
        link: None,
    }])
}

fn deb_with_control(files: &[File], control: &str) -> Vec<u8> {
    ar(&[
        ("debian-binary", b"2.0\n".to_vec()),
        ("control.tar.gz", gzip(&control_tar(control))),
        ("data.tar.gz", gzip(&tar(files))),
    ])
}

fn deb_of(files: &[File]) -> Vec<u8> {
    deb_with_control(files, CONTROL)
}

/// A newc `cpio` of the regular files and links of `files` (a link's data is its target, as
/// rpm keeps it), with its trailer.
fn cpio_of(files: &[File]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut entry = |name: &str, data: &[u8]| {
        let name_size = name.len() + 1;
        out.extend(b"070701");
        for field in [
            0,
            0o100_644,
            0,
            0,
            1,
            0,
            data.len(),
            0,
            0,
            0,
            0,
            name_size,
            0,
        ] {
            out.extend(format!("{field:08x}").as_bytes());
        }
        out.extend(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    };
    for f in files.iter().filter(|f| !f.directory) {
        match f.link {
            Some(target) => entry(f.path, target.as_bytes()),
            None => entry(f.path, &f.data),
        }
    }
    entry("TRAILER!!!", b"");
    out
}

/// One tag of an rpm header: its number, type, count and data.
type Tag = (u32, u32, u32, Vec<u8>);

fn strings(tag: u32, items: &[&str]) -> Tag {
    let mut data = Vec::new();
    for item in items {
        data.extend(item.as_bytes());
        data.push(0);
    }
    (tag, STRING_ARRAY, items.len() as u32, data)
}

/// An rpm header of `tags`, their data stored in order.
fn rpm_header(tags: &[Tag]) -> Vec<u8> {
    let mut index = Vec::new();
    let mut store: Vec<u8> = Vec::new();
    for (tag, kind, count, data) in tags {
        for n in [*tag, *kind, store.len() as u32, *count] {
            index.extend(n.to_be_bytes());
        }
        store.extend(data);
    }
    let mut out = HEADER_MAGIC.to_vec();
    out.extend([0; 4]);
    out.extend((tags.len() as u32).to_be_bytes());
    out.extend((store.len() as u32).to_be_bytes());
    out.extend(index);
    out.extend(store);
    out
}

/// The tags nfpm writes for `files`, each directory once.
fn rpm_tags(files: &[File], compressor: &str) -> Vec<Tag> {
    let mut dirs: Vec<String> = Vec::new();
    let (mut bases, mut indexes, mut modes) = (Vec::new(), Vec::new(), Vec::new());
    let (mut owners, mut groups, mut targets) = (Vec::new(), Vec::new(), Vec::new());
    for f in files {
        let path = f.path.trim_start_matches('.');
        let (dir, base) = path.split_at(path.rfind('/').expect("a slash") + 1);
        let at = dirs.iter().position(|d| d == dir).unwrap_or_else(|| {
            dirs.push(dir.to_owned());
            dirs.len() - 1
        });
        bases.push(base);
        indexes.extend((at as u32).to_be_bytes());
        let kind = match (f.directory, f.link) {
            (true, _) => 0o040_000,
            (false, Some(_)) => 0o120_000,
            (false, None) => 0o100_000,
        };
        modes.extend(((kind | f.mode) as u16).to_be_bytes());
        owners.push(f.owner);
        groups.push(f.group);
        targets.push(f.link.unwrap_or(""));
    }
    let dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
    let mut compressor = compressor.as_bytes().to_vec();
    compressor.push(0);
    // The link targets first: a string tag's strings are read to the end of the store, so a
    // tag after `DIRNAMES` would lend it strings.
    vec![
        strings(FILELINKTOS, &targets),
        (FILEMODES, INT16, files.len() as u32, modes),
        strings(FILEUSERNAME, &owners),
        strings(FILEGROUPNAME, &groups),
        (DIRINDEXES, INT32, files.len() as u32, indexes),
        strings(BASENAMES, &bases),
        strings(DIRNAMES, &dirs),
        (PAYLOADCOMPRESSOR, STRING, 1, compressor),
    ]
}

/// An rpm: a lead, an empty signature header padded to 8 bytes, `tags` and `payload`.
fn rpm_with(tags: &[Tag], payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0xed, 0xab, 0xee, 0xdb];
    out.resize(96, 0);
    out.extend(rpm_header(&[(1000, 7, 4, b"sign".to_vec())]));
    while out.len() % 8 != 0 {
        out.push(0);
    }
    out.extend(rpm_header(tags));
    out.extend(payload);
    out
}

fn rpm_of(files: &[File]) -> Vec<u8> {
    rpm_with(&rpm_tags(files, "gzip"), &gzip(&cpio_of(files)))
}

/// A directory holding `staged/gh` and `staged/trusted_root.json`, and `package` as `pkg`.
struct Case {
    dir: TestDir,
}

impl Case {
    fn new(package: &[u8]) -> Case {
        let dir = scratch("case");
        std::fs::create_dir(dir.join("staged")).expect("staged");
        std::fs::write(dir.join("staged/gh"), GH).expect("gh");
        std::fs::write(dir.join("staged/trusted_root.json"), ROOT).expect("root");
        std::fs::write(dir.join("staged/hostkeeper"), b"the daemon").expect("hostkeeper");
        std::fs::write(dir.join("staged/hostkeeper-pxe"), b"the boot server").expect("pxe");
        std::fs::write(dir.join("pkg"), package).expect("pkg");
        Case { dir }
    }

    fn package(&self) -> PathBuf {
        self.dir.join("pkg")
    }

    fn check(&self) -> Result<Vec<String>, Error> {
        check(
            &self.package(),
            Some(Staged::Daemon(&self.dir.join("staged"))),
        )
    }

    /// The problems a refusal lists, without the package's name in front.
    fn problems(&self) -> Vec<String> {
        let prefix = format!("{}: ", self.package().display());
        match self.check() {
            Err(Error::Refused(lines)) => lines
                .iter()
                .map(|l| l.strip_prefix(&prefix).expect("named").to_owned())
                .collect(),
            other => panic!("not refused with problems: {other:?}"),
        }
    }

    /// The message of a package this check cannot read.
    fn unreadable(&self) -> String {
        match self.check() {
            Err(Error::Failed(message)) => message,
            other => panic!("not refused as unreadable: {other:?}"),
        }
    }
}

fn both(files: &[File]) -> [Vec<u8>; 2] {
    [deb_of(files), rpm_of(files)]
}

#[test]
fn a_deb_and_an_rpm_that_install_the_staged_files_pass() {
    for package in both(&files()) {
        let case = Case::new(&package);
        let name = case.package().display().to_string();
        assert_eq!(
            case.check(),
            Ok(vec![
                format!("{name}: /usr/lib/hostkeeper/gh is 755 root:root and the staged bytes"),
                format!(
                    "{name}: /usr/lib/hostkeeper/trusted_root.json is 644 root:root and the staged bytes"
                ),
                format!("{name}: /usr/bin/hostkeeper is 755 root:root and the staged bytes"),
                format!("{name}: /usr/bin/hostkeeper-pxe is 755 root:root and the staged bytes"),
                format!(
                    "{name}: /etc/issue.d/hostkeeper.issue is a link to /run/hostkeeper/enrollment.issue"
                ),
                format!("{name}: holds no ELF program"),
                format!("{name}: declares no dependency"),
            ])
        );
    }
}

/// Catches a package that leaves out a file, installs it with another mode, owner or group, as
/// a directory, or with bytes other than the staged ones.
#[test]
fn each_way_a_package_differs_is_one_line() {
    let gh = "./usr/lib/hostkeeper/gh";
    let root = "./usr/lib/hostkeeper/trusted_root.json";
    let cases: Vec<(Vec<File>, Vec<&str>)> = vec![
        (
            files().into_iter().filter(|f| f.path != gh).collect(),
            vec!["/usr/lib/hostkeeper/gh: not in the package"],
        ),
        (
            with(gh, |f| f.mode = 0o775),
            vec!["/usr/lib/hostkeeper/gh: mode 775, not 755"],
        ),
        (
            with(root, |f| f.mode = 0o600),
            vec!["/usr/lib/hostkeeper/trusted_root.json: mode 600, not 644"],
        ),
        (
            with(gh, |f| f.owner = "nobody"),
            vec!["/usr/lib/hostkeeper/gh: owned by nobody:root, not root:root"],
        ),
        (
            with(root, |f| f.group = "adm"),
            vec!["/usr/lib/hostkeeper/trusted_root.json: owned by root:adm, not root:root"],
        ),
        (
            with(gh, |f| f.data = b"another gh".to_vec()),
            vec!["/usr/lib/hostkeeper/gh: its bytes are not those of STAGED/gh"],
        ),
        (
            with(root, |f| {
                f.directory = true;
                f.data = Vec::new();
            }),
            vec![
                "/usr/lib/hostkeeper/trusted_root.json: not a regular file",
                "/usr/lib/hostkeeper/trusted_root.json: its bytes are not those of STAGED/trusted_root.json",
            ],
        ),
    ];
    for (files, want) in cases {
        for package in both(&files) {
            let case = Case::new(&package);
            let staged = case.dir.join("staged").display().to_string();
            let want: Vec<String> = want.iter().map(|w| w.replace("STAGED", &staged)).collect();
            assert_eq!(case.problems(), want);
        }
    }
}

/// Catches a package without the console's issue link, with a file in its place, or with a link
/// to another file than the one the daemon writes; and an rpm whose header gives no link target:
/// each is one line, in the deb and in the rpm. Only the target's text is read, so the check
/// holds where the target does not exist, as where a package is built or checked.
#[test]
fn a_package_whose_issue_link_is_missing_or_points_elsewhere_is_one_line() {
    let link = "/etc/issue.d/hostkeeper.issue";
    let cases: Vec<(Vec<File>, String)> = vec![
        (
            files().into_iter().filter(|f| f.path != LINK).collect(),
            format!("{link}: not in the package"),
        ),
        (
            with(LINK, |f| {
                f.link = None;
                f.mode = 0o644;
                f.data = b"hostkeeper enrollment".to_vec();
            }),
            format!("{link}: not a link"),
        ),
        (
            with(LINK, |f| f.link = Some("/run/hostkeeper/admin.sock")),
            format!("{link}: points at /run/hostkeeper/admin.sock, not {TARGET}"),
        ),
    ];
    for (files, want) in cases {
        for package in both(&files) {
            assert_eq!(Case::new(&package).problems(), std::slice::from_ref(&want));
        }
    }
    let all = files();
    let tags: Vec<Tag> = rpm_tags(&all, "gzip")
        .into_iter()
        .filter(|t| t.0 != FILELINKTOS)
        .collect();
    let rpm = rpm_with(&tags, &gzip(&cpio_of(&all)));
    assert_eq!(
        Case::new(&rpm).problems(),
        [format!("{link}: points at nothing, not {TARGET}")]
    );
}

/// A deb entry owned by name `root` but a uid other than 0 is not root's.
#[test]
fn a_deb_entry_with_a_uid_other_than_0_is_not_roots() {
    let case = Case::new(&deb_of(&with("./usr/lib/hostkeeper/gh", |f| f.uid = 5)));
    assert_eq!(
        case.problems(),
        ["/usr/lib/hostkeeper/gh: owned by id 5:root, not root:root"]
    );
}

#[test]
fn a_file_that_is_neither_a_deb_nor_an_rpm_is_refused() {
    let case = Case::new(b"PK\x03\x04 a zip");
    assert_eq!(
        case.unreadable(),
        format!(
            "not a package this check reads: {} is neither a deb, an rpm nor a tarball",
            case.package().display()
        )
    );
}

#[test]
fn a_deb_this_check_cannot_read_is_refused_by_name() {
    let data = gzip(&tar(&files()));
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            ar(&[("debian-binary", b"2.0\n".to_vec())]),
            "the deb has no data.tar.gz",
        ),
        (
            ar(&[("data.tar.zst", b"zstd".to_vec())]),
            "data.tar.zst: only data.tar.gz is read",
        ),
        (
            ar(&[("data.tar.gz", b"this is not a gzip stream at all".to_vec())]),
            "data.tar.gz: invalid gzip header",
        ),
        (
            ar(&[("data.tar.gz", gzip(&[1; 512]))]),
            "data.tar: numeric field was not a number",
        ),
        (
            b"!<arch>\ndebian-binary   ".to_vec(),
            "cut short: 60 bytes at offset 8",
        ),
        (
            {
                let mut deb = ar(&[("data.tar.gz", data.clone())]);
                deb.truncate(deb.len() - 10);
                deb
            },
            "cut short",
        ),
        (
            {
                let mut deb = ar(&[("data.tar.gz", data)]);
                deb[8 + 48..8 + 58].copy_from_slice(b"ten bytes!");
                deb
            },
            "the ar member \"data.tar.gz\" has no size",
        ),
    ];
    for (deb, words) in cases {
        let message = Case::new(&deb).unreadable();
        assert!(
            message.starts_with(&format!("not a package this check reads: {words}")),
            "{message:?} is not {words:?}"
        );
    }
}

#[test]
fn an_rpm_this_check_cannot_read_is_refused_by_name() {
    let files = files();
    let payload = gzip(&cpio_of(&files));
    let tags = rpm_tags(&files, "gzip");
    let without = |tag: u32| -> Vec<Tag> { tags.iter().filter(|t| t.0 != tag).cloned().collect() };
    let changed = |tag: u32, f: &dyn Fn(&mut Tag)| -> Vec<Tag> {
        let mut all = tags.clone();
        f(all.iter_mut().find(|t| t.0 == tag).expect("tag"));
        all
    };
    let mut no_header = rpm_of(&files);
    no_header[96] = 0;
    let mut bad_cpio = cpio_of(&files);
    bad_cpio[0] = b'1';
    let mut bad_field = cpio_of(&files);
    bad_field[6 + 6 * 8] = b'z';
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (no_header, "no rpm header at offset 96"),
        (
            vec![0xed, 0xab, 0xee, 0xdb],
            "cut short: 4 bytes at offset 96",
        ),
        (
            rpm_with(&rpm_tags(&files, "xz"), &payload),
            "the rpm payload is xz: only gzip is read",
        ),
        (
            rpm_with(&without(PAYLOADCOMPRESSOR), &payload),
            "the rpm header has no tag 1125 of type 6",
        ),
        (
            rpm_with(&changed(BASENAMES, &|t| t.1 = STRING), &payload),
            "the rpm header has no tag 1117 of type 8",
        ),
        (
            rpm_with(&changed(DIRNAMES, &|t| t.2 = 9), &payload),
            "rpm tag 1118 holds fewer than 9 strings",
        ),
        (
            rpm_with(
                &changed(DIRINDEXES, &|t| t.3 = [0, 0, 0, 7].repeat(4)),
                &payload,
            ),
            "rpm file \"hostkeeper\" has no directory",
        ),
        (
            rpm_with(&changed(DIRINDEXES, &|t| t.2 = 99), &payload),
            "cut short: 4 bytes at offset",
        ),
        (
            rpm_with(&changed(FILEMODES, &|t| t.2 = 1), &payload),
            "rpm file /usr/lib/hostkeeper/gh has no mode, owner or group",
        ),
        (
            {
                let mut rpm = rpm_with(&tags, &payload);
                // The offset of the signature header's one tag, past its store.
                rpm[96 + 16 + 8..96 + 16 + 12].copy_from_slice(&99u32.to_be_bytes());
                rpm
            },
            "rpm tag 1000 points past its header",
        ),
        (
            rpm_with(&tags, b"this is not a gzip stream at all"),
            "the rpm payload: invalid gzip header",
        ),
        (
            rpm_with(&tags, &gzip(&bad_cpio)),
            "no cpio newc header at payload offset 0",
        ),
        (
            rpm_with(&tags, &gzip(&bad_field)),
            "cpio field \"z000000a\"",
        ),
    ];
    for (rpm, words) in cases {
        let message = Case::new(&rpm).unreadable();
        assert!(
            message.starts_with(&format!("not a package this check reads: {words}")),
            "{message:?} is not {words:?}"
        );
    }
}

/// An rpm whose payload lacks a file its header lists installs no staged bytes for it.
#[test]
fn an_rpm_whose_payload_lacks_a_file_is_refused() {
    let all = files();
    let payload: Vec<File> = all
        .iter()
        .filter(|f| f.path != "./usr/lib/hostkeeper/gh")
        .cloned()
        .collect();
    let rpm = rpm_with(&rpm_tags(&all, "gzip"), &gzip(&cpio_of(&payload)));
    let case = Case::new(&rpm);
    let staged = case.dir.join("staged/gh").display().to_string();
    assert_eq!(
        case.problems(),
        [format!(
            "/usr/lib/hostkeeper/gh: its bytes are not those of {staged}"
        )]
    );
}

#[test]
fn a_missing_package_or_staged_file_is_an_error() {
    let case = Case::new(&deb_of(&files()));
    let absent = case.dir.join("absent.deb");
    let message = check(&absent, Some(Staged::Daemon(&case.dir.join("staged"))))
        .expect_err("absent")
        .to_string();
    assert!(
        message.starts_with(&format!("read {}: ", absent.display())),
        "{message}"
    );
    std::fs::remove_file(case.dir.join("staged/trusted_root.json")).expect("remove");
    let message = case.check().expect_err("no staged root").to_string();
    let want = format!(
        "read {}: ",
        case.dir.join("staged/trusted_root.json").display()
    );
    assert!(message.starts_with(&want), "{message}");
}

/// The paths are the daemon's: the packaging test compares them with `hostkeeper_daemon::cli`.
#[test]
fn installed_paths_are_under_usr_lib_hostkeeper() {
    assert_eq!(INSTALLED_GH, "/usr/lib/hostkeeper/gh");
    assert_eq!(
        INSTALLED_TRUSTED_ROOT,
        "/usr/lib/hostkeeper/trusted_root.json"
    );
    assert_eq!(installed_path("./a/b"), "/a/b");
    assert_eq!(installed_path("a/b"), "/a/b");
    assert_eq!(Path::new(INSTALLED_GH).file_name(), Some(GH_FILE.as_ref()));
    assert_eq!(INSTALLED_ISSUE_LINK, "/etc/issue.d/hostkeeper.issue");
    assert_eq!(ISSUE_TARGET, "/run/hostkeeper/enrollment.issue");
}

/// The argv of the `release` command, as clap reads it.
#[derive(clap::Parser)]
struct Args {
    #[command(subcommand)]
    verb: crate::release::Verb,
}

/// Catches the package or `--staged` not reaching the check, the verb building the real
/// adapters though it reads no port, or `run_with` failing it instead of handing it back.
#[test]
fn the_verb_gets_its_flags_and_asks_for_no_adapters() {
    use std::cell::Cell;

    use clap::Parser as _;

    use crate::release::fakes::{FakeHistory, Fakes};
    use crate::release::ports::Adapters as _;
    let case = Case::new(&rpm_of(&files()));
    let (package, staged) = (case.package(), case.dir.join("staged"));
    let verb = || {
        let argv = [
            "release",
            "check-linux-pkg",
            package.to_str().expect("UTF-8"),
            "--staged",
            staged.to_str().expect("UTF-8"),
        ];
        Args::try_parse_from(argv).expect("parse the argv").verb
    };
    let connected = Cell::new(false);
    let out = crate::release::run(verb(), || {
        connected.set(true);
        Fakes::over(FakeHistory::default())
    });
    assert!(!connected.get());
    assert_eq!(out, case.check());
    assert!(out.is_ok(), "{out:?}");
    let fakes = Fakes::over(FakeHistory::default());
    assert_eq!(
        crate::release::run_with(verb(), &fakes.ports()),
        case.check()
    );
}
