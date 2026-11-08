//! The check of ELF programs and declared dependencies, on a deb, an rpm, an Arch package and a
//! tarball, and the staged check of the programs and of an Arch package's owners.

use std::io::Cursor;

use super::*;
use crate::release::package::elf::fixture::{AARCH64, Spec, X86_64};

fn static_pie() -> Vec<u8> {
    Spec::static_pie(X86_64).build()
}

/// `base` with the data of the file at `path` replaced.
fn with_data(base: Vec<File>, path: &str, data: Vec<u8>) -> Vec<File> {
    let mut all = base;
    all.iter_mut()
        .find(|f| f.path == path)
        .expect("a file")
        .data = data;
    all
}

const HOSTKEEPER: &str = "./usr/bin/hostkeeper";
const PXE: &str = "./usr/bin/hostkeeper-pxe";

/// The files, with the daemon and the boot server the given programs.
fn programs(hostkeeper: Vec<u8>, pxe: Vec<u8>) -> Vec<File> {
    with_data(with_data(files(), HOSTKEEPER, hostkeeper), PXE, pxe)
}

/// The files as nfpm's Arch package holds them: owners without names.
fn nameless(files: Vec<File>) -> Vec<File> {
    files
        .into_iter()
        .map(|f| File {
            owner: "",
            group: "",
            ..f
        })
        .collect()
}

fn zstd(bytes: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(bytes, ruzstd::encoding::CompressionLevel::Fastest)
}

fn xz(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    lzma_rs::xz_compress(&mut Cursor::new(bytes), &mut out).expect("xz");
    out
}

/// An Arch package of `files`: a zstd tar with `.PKGINFO` first.
fn arch_with(files: &[File], pkginfo: &str) -> Vec<u8> {
    let info = File {
        path: "./.PKGINFO",
        mode: 0o644,
        owner: "",
        uid: 0,
        group: "",
        directory: false,
        data: pkginfo.as_bytes().to_vec(),
        link: None,
    };
    let mut all = vec![info];
    all.extend(files.iter().cloned());
    zstd(&tar(&all))
}

const PKGINFO: &str = "pkgname = hostkeeper\npkgver = 1.0.0-1\narch = x86_64\n";

/// The lines of a package that passes, without its name in front.
fn passes(package: &[u8]) -> Vec<String> {
    let case = Case::new(package);
    let prefix = format!("{}: ", case.package().display());
    check(&case.package(), None)
        .expect("accepted")
        .iter()
        .map(|l| l.strip_prefix(&prefix).expect("named").to_owned())
        .collect()
}

/// The problems a refusal lists, without the package's name in front.
fn refusals(package: &[u8]) -> Vec<String> {
    let case = Case::new(package);
    let prefix = format!("{}: ", case.package().display());
    match check(&case.package(), None) {
        Err(Error::Refused(lines)) => lines
            .iter()
            .map(|l| l.strip_prefix(&prefix).expect("named").to_owned())
            .collect(),
        other => panic!("not refused with problems: {other:?}"),
    }
}

/// `files` as a tarball in gzip, xz and zstd.
fn tarballs(files: &[File]) -> [Vec<u8>; 3] {
    let tar = tar(files);
    [gzip(&tar), xz(&tar), zstd(&tar)]
}

#[test]
fn static_programs_pass_in_each_format_with_no_dependency() {
    let all = programs(static_pie(), Spec::static_pie(AARCH64).build());
    let static_lines = [
        "/usr/bin/hostkeeper is a static ELF program",
        "/usr/bin/hostkeeper-pxe is a static ELF program",
    ];
    for package in [
        deb_of(&all),
        rpm_of(&all),
        arch_with(&nameless(all.clone()), PKGINFO),
    ] {
        assert_eq!(
            passes(&package),
            [static_lines[0], static_lines[1], "declares no dependency"]
        );
    }
    for package in tarballs(&all) {
        assert_eq!(passes(&package), static_lines);
    }
}

#[test]
fn a_fully_static_program_and_other_classes_of_elf_pass_when_static() {
    let fully_static = Spec {
        dynamic: false,
        ..Spec::static_pie(X86_64)
    };
    let small = Spec {
        class: 1,
        big_endian: true,
        ..Spec::static_pie(X86_64)
    };
    let all = programs(fully_static.build(), small.build());
    assert_eq!(passes(&deb_of(&all)).len(), 3);
}

#[test]
fn a_package_with_no_elf_program_passes_with_a_line_that_says_so() {
    assert_eq!(
        passes(&deb_of(&files())),
        ["holds no ELF program", "declares no dependency"]
    );
    let [tarball, ..] = tarballs(&files());
    assert_eq!(passes(&tarball), ["holds no ELF program"]);
}

#[test]
fn a_program_linked_to_a_library_or_asking_for_an_interpreter_is_refused_in_each_format() {
    let ssl = Spec {
        needed: vec!["libssl.so.3", "libc.so.6"],
        ..Spec::static_pie(X86_64)
    };
    let all = programs(ssl.build(), Spec::glibc().build());
    let want = [
        "/usr/bin/hostkeeper: ELF program linked to libssl.so.3",
        "/usr/bin/hostkeeper: ELF program linked to libc.so.6",
        "/usr/bin/hostkeeper-pxe: ELF program with the interpreter /lib64/ld-linux-x86-64.so.2, so not static",
        "/usr/bin/hostkeeper-pxe: ELF program linked to libc.so.6",
    ];
    let [gz, xz, zst] = tarballs(&all);
    for package in [
        deb_of(&all),
        rpm_of(&all),
        arch_with(&all, PKGINFO),
        gz,
        xz,
        zst,
    ] {
        assert_eq!(refusals(&package), want);
    }
}

#[test]
fn an_elf_file_that_cannot_be_read_is_refused_by_name() {
    let good = static_pie();
    let mut files = programs(good.clone(), b"\x7fELF".to_vec());
    files.push(File {
        path: "./usr/lib/x/cut",
        data: good[..70].to_vec(),
        ..files[0].clone()
    });
    let lines = refusals(&deb_of(&files));
    assert_eq!(
        lines[0],
        "/usr/bin/hostkeeper-pxe: an ELF file this check cannot read: 4 bytes, too short for an ELF header"
    );
    assert!(
        lines[1].starts_with("/usr/lib/x/cut: an ELF file this check cannot read: "),
        "{lines:?}"
    );
    assert_eq!(lines.len(), 2);
}

#[test]
fn a_link_whose_target_text_starts_like_an_elf_file_is_not_read_as_a_program() {
    let mut all = files();
    all.push(File {
        path: "./usr/bin/alias",
        link: Some("\x7fELF a target that is text"),
        data: Vec::new(),
        ..files()[0].clone()
    });
    all.push(File {
        path: "./usr/bin/dir",
        directory: true,
        data: Vec::new(),
        ..files()[0].clone()
    });
    for package in [deb_of(&all), rpm_of(&all)] {
        assert_eq!(
            passes(&package),
            ["holds no ELF program", "declares no dependency"]
        );
    }
}

fn deb_depends(control: &str) -> Vec<String> {
    refusals(&deb_with_control(&files(), control))
}

#[test]
fn a_deb_that_declares_a_dependency_is_refused_one_line_per_relation() {
    assert_eq!(
        deb_depends(
            "Package: hostkeeper\nDepends: libc6 (>= 2.34), libssl3 | libssl1.1\nPre-Depends: dpkg\nRecommends: not-a-dependency\n"
        ),
        [
            "declares the dependency libc6 (>= 2.34)",
            "declares the dependency libssl3 | libssl1.1",
            "declares the dependency dpkg",
        ]
    );
    assert_eq!(
        deb_depends("DEPENDS: a,\n b,\n\tc\nPRE-depends: d\nbad line without colon\n"),
        [
            "declares the dependency a",
            "declares the dependency b",
            "declares the dependency c",
            "declares the dependency d",
        ]
    );
    assert_eq!(
        deb_depends("\tfirst a continuation of nothing\nDepends: x\nDepends:\n"),
        ["declares the dependency x"]
    );
}

#[test]
fn a_deb_with_empty_depends_or_only_other_relations_passes() {
    for control in ["Depends:\nSuggests: gh\n", "Package: x\nBreaks: y\n", ""] {
        let package = deb_with_control(&files(), control);
        assert_eq!(
            passes(&package),
            ["holds no ELF program", "declares no dependency"]
        );
    }
}

#[test]
fn a_deb_whose_control_cannot_be_read_is_refused_by_name() {
    let data = gzip(&tar(&files()));
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            ar(&[("data.tar.gz", data.clone())]),
            "the deb has no control.tar.gz",
        ),
        (
            ar(&[
                ("control.tar.xz", b"xz".to_vec()),
                ("data.tar.gz", data.clone()),
            ]),
            "control.tar.xz: only control.tar.gz is read",
        ),
        (
            ar(&[
                ("control.tar.gz", gzip(&tar(&[]))),
                ("data.tar.gz", data.clone()),
            ]),
            "control.tar.gz has no ./control",
        ),
        (
            ar(&[
                ("control.tar.gz", gzip(&[1; 512])),
                ("data.tar.gz", data.clone()),
            ]),
            "control.tar: numeric field was not a number",
        ),
        (
            ar(&[("control.tar.gz", b"not gzip at all, no".to_vec())]),
            "control.tar.gz: invalid gzip header",
        ),
        (
            ar(&[("control.tar.gz", gzip(&control_tar(CONTROL)))]),
            "the deb has no data.tar.gz",
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
fn an_rpm_that_requires_more_than_rpmlib_is_refused_one_line_per_name() {
    let all = files();
    let mut tags = rpm_tags(&all, "gzip");
    tags.push(strings(
        REQUIRENAME,
        &[
            "rpmlib(CompressedFileNames)",
            "libc.so.6()(64bit)",
            "rpmlib(FileDigests)",
            "/bin/sh",
        ],
    ));
    let rpm = rpm_with(&tags, &gzip(&cpio_of(&all)));
    assert_eq!(
        refusals(&rpm),
        [
            "declares the dependency libc.so.6()(64bit)",
            "declares the dependency /bin/sh"
        ]
    );
}

#[test]
fn an_rpm_that_requires_only_rpmlib_or_nothing_passes() {
    let all = files();
    let mut tags = rpm_tags(&all, "gzip");
    tags.push(strings(
        REQUIRENAME,
        &[
            "rpmlib(CompressedFileNames)",
            "rpmlib(PayloadFilesHavePrefix)",
        ],
    ));
    let rpm = rpm_with(&tags, &gzip(&cpio_of(&all)));
    assert_eq!(
        passes(&rpm),
        ["holds no ELF program", "declares no dependency"]
    );
    assert_eq!(passes(&rpm_of(&all)), passes(&rpm));
}

#[test]
fn an_rpm_whose_requires_are_not_strings_is_refused_by_name() {
    let all = files();
    let mut tags = rpm_tags(&all, "gzip");
    tags.push((REQUIRENAME, INT32, 1, vec![0, 0, 0, 1]));
    let rpm = rpm_with(&tags, &gzip(&cpio_of(&all)));
    assert_eq!(
        Case::new(&rpm).unreadable(),
        "not a package this check reads: the rpm header has no tag 1049 of type 8"
    );
}

#[test]
fn an_arch_package_that_declares_a_dependency_is_refused_one_line_per_depend_line() {
    let info = format!(
        "{PKGINFO}# depend = commented\ndepend = glibc\ndepend=openssl>=3\noptdepend = gh: attestations\nmakedepend = cargo\n"
    );
    assert_eq!(
        refusals(&arch_with(&files(), &info)),
        [
            "declares the dependency glibc",
            "declares the dependency openssl>=3"
        ]
    );
}

#[test]
fn a_tarball_declares_nothing_even_with_a_depend_line_in_a_file() {
    let mut all = files();
    all.push(File {
        path: "./notes",
        data: b"depend = glibc\n".to_vec(),
        ..files()[0].clone()
    });
    let [tarball, ..] = tarballs(&all);
    assert_eq!(passes(&tarball), ["holds no ELF program"]);
}

#[test]
fn an_arch_package_with_nameless_root_owners_passes_the_staged_check() {
    let package = arch_with(&nameless(files()), PKGINFO);
    let case = Case::new(&package);
    let name = case.package().display().to_string();
    let lines = case.check().expect("accepted");
    assert_eq!(lines.len(), 7, "{lines:?}");
    assert_eq!(
        lines[0],
        format!("{name}: /usr/lib/hostkeeper/gh is 755 root:root and the staged bytes")
    );
    assert_eq!(lines[6], format!("{name}: declares no dependency"));
}

#[test]
fn a_nameless_owner_with_an_id_other_than_0_is_not_roots() {
    let mut all = nameless(files());
    all.iter_mut()
        .find(|f| f.path == "./usr/lib/hostkeeper/gh")
        .expect("gh")
        .uid = 7;
    assert_eq!(
        Case::new(&arch_with(&all, PKGINFO)).problems(),
        ["/usr/lib/hostkeeper/gh: owned by id 7:root, not root:root"]
    );
}

#[test]
fn the_staged_check_holds_the_two_programs_to_their_path_mode_and_bytes() {
    let daemon = "/usr/bin/hostkeeper";
    let pxe = "/usr/bin/hostkeeper-pxe";
    let cases: Vec<(Vec<File>, Vec<String>)> = vec![
        (
            files().into_iter().filter(|f| f.path != PXE).collect(),
            vec![format!("{pxe}: not in the package")],
        ),
        (
            with(HOSTKEEPER, |f| f.mode = 0o775),
            vec![format!("{daemon}: mode 775, not 755")],
        ),
        (
            with(PXE, |f| f.group = "adm"),
            vec![format!("{pxe}: owned by root:adm, not root:root")],
        ),
        (
            with(HOSTKEEPER, |f| f.data = b"another daemon".to_vec()),
            vec![format!(
                "{daemon}: its bytes are not those of STAGED/hostkeeper"
            )],
        ),
    ];
    for (files, want) in cases {
        for package in [deb_of(&files), rpm_of(&files)] {
            let case = Case::new(&package);
            let staged = case.dir.join("staged").display().to_string();
            let want: Vec<String> = want.iter().map(|w| w.replace("STAGED", &staged)).collect();
            assert_eq!(case.problems(), want);
        }
    }
}

#[test]
fn staged_with_a_static_program_adds_its_line_after_the_staged_ones() {
    let program = static_pie();
    let all = programs(program.clone(), b"the boot server".to_vec());
    let case = Case::new(&deb_of(&all));
    std::fs::write(case.dir.join("staged/hostkeeper"), &program).expect("staged");
    let lines = case.check().expect("accepted");
    let name = case.package().display().to_string();
    assert_eq!(
        lines[5..],
        [
            format!("{name}: /usr/bin/hostkeeper is a static ELF program"),
            format!("{name}: declares no dependency"),
        ]
    );
}

#[test]
fn staged_with_a_tarball_is_a_failure_that_says_so() {
    let [tarball, ..] = tarballs(&files());
    let case = Case::new(&tarball);
    let name = case.package().display().to_string();
    assert_eq!(
        case.check(),
        Err(Error::Failed(format!(
            "{name}: --staged does not apply to a tarball, which installs nothing at the daemon's paths"
        )))
    );
}

#[test]
fn a_tar_entry_cut_short_is_refused_by_name() {
    let mut cut = tar(&files());
    // The first entry's header (512 bytes) and three bytes of its data.
    cut.truncate(512 + 3);
    let deb = ar(&[
        ("control.tar.gz", gzip(&control_tar(CONTROL))),
        ("data.tar.gz", gzip(&cut)),
    ]);
    let message = Case::new(&deb).unreadable();
    assert_eq!(
        message,
        "not a package this check reads: data.tar: unexpected EOF during skip"
    );
}

#[test]
fn an_archive_that_cannot_be_decompressed_or_untarred_is_refused_by_name() {
    let cut = zstd(&tar(&files()))[..30].to_vec();
    let cases: Vec<Vec<u8>> = vec![
        [&[0x1f, 0x8b][..], b"junk"].concat(),
        [&[0xfd, b'7', b'z', b'X', b'Z', 0][..], b"junk"].concat(),
        [&[0x28, 0xb5, 0x2f, 0xfd][..], b"junk"].concat(),
        gzip(&[1; 512]),
        cut,
    ];
    for bytes in cases {
        let message = Case::new(&bytes).unreadable();
        assert!(
            message.starts_with("not a package this check reads: the archive: "),
            "{message:?}"
        );
    }
}
