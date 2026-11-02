use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::*;
use crate::testdir::TestDir;

const TOOL: &[u8] = b"\x7fELF a pinned tool's bytes";
const OLD: &[u8] = b"the tool installed before";

/// An empty directory of its own for one test, removed when it drops.
fn scratch(name: &str) -> TestDir {
    let exe = std::env::current_exe().expect("test binary");
    TestDir::under(
        &exe.parent().expect("deps").join("xtask-install-unit"),
        name,
    )
}

/// A tar of a directory `tool-1.0`, a decoy file, `tool-1.0/tool` holding `TOOL` and a
/// symlink `tool-1.0/link`.
fn tar() -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let mut dir = tar::Header::new_gnu();
    dir.set_entry_type(tar::EntryType::Directory);
    dir.set_mode(0o755);
    dir.set_size(0);
    b.append_data(&mut dir, "tool-1.0/", &[][..]).expect("dir");
    for (path, data) in [("tool-1.0/README", &b"decoy"[..]), ("tool-1.0/tool", TOOL)] {
        let mut h = tar::Header::new_gnu();
        h.set_mode(0o644);
        h.set_size(data.len() as u64);
        b.append_data(&mut h, path, data).expect("file");
    }
    let mut link = tar::Header::new_gnu();
    link.set_entry_type(tar::EntryType::Symlink);
    link.set_size(0);
    b.append_link(&mut link, "tool-1.0/link", "tool")
        .expect("link");
    b.into_inner().expect("tar")
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(bytes).expect("gzip");
    e.finish().expect("gzip")
}

fn member(path: &str) -> PathBuf {
    PathBuf::from(path)
}

fn tar_gz(path: &str) -> Unpack {
    Unpack::TarGz {
        member: member(path),
    }
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .expect("ls")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

/// A directory holding `tool` with the bytes `OLD`, and the path of `tool`.
fn with_old_tool(name: &str) -> (TestDir, PathBuf) {
    let dir = scratch(name);
    let out = dir.join("tool");
    std::fs::write(&out, OLD).expect("old tool");
    (dir, out)
}

/// `out` still holds `OLD`, and nothing else is in its directory.
fn assert_old_tool_alone(dir: &Path, out: &Path) {
    assert_eq!(
        std::fs::read(out).expect("read"),
        OLD,
        "the old tool is intact"
    );
    assert_eq!(names(dir), ["tool"], "no temporary file is left");
}

#[test]
fn a_sha256_is_64_lowercase_hex_digits() {
    // Catches: a parser that accepts a short, upper-case or non-hex pin (a typo then
    // pins nothing), or that refuses the real form.
    let hex = "9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f";
    let digest: Sha256 = hex.parse().expect("parse");
    assert_eq!(digest.to_string(), hex);
    assert_eq!(format!("{digest:?}"), hex);
    for bad in [
        &hex[..62],
        &hex.to_uppercase(),
        &format!("{}g", &hex[..63]),
        "",
    ] {
        assert_eq!(
            bad.parse::<Sha256>(),
            Err(NotASha256(bad.to_owned())),
            "{bad}"
        );
    }
    assert_eq!(
        NotASha256("x".to_owned()).to_string(),
        "not a SHA-256: \"x\" (expected 64 lowercase hex digits)"
    );
}

#[test]
fn a_wrong_hash_is_refused_before_anything_is_unpacked_or_written() {
    // Catches: skipping the compare (the archive below unpacks fine, so the tool would be
    // installed), or comparing after unpacking (the second case is no archive at all, so a late
    // compare reports an unpack error instead of the mismatch). The old tool stays as it was.
    let (dir, out) = with_old_tool("wrong-hash");
    let archive = gzip(&tar());
    let pinned = Sha256::of(b"the bytes the pin was made from");
    for (bytes, unpack) in [
        (&archive[..], tar_gz("tool-1.0/tool")),
        (b"not gzip", tar_gz("x")),
    ] {
        assert_eq!(
            install(bytes, &pinned, &unpack, &out),
            Err(Error::Mismatch {
                expected: pinned,
                actual: Sha256::of(bytes)
            })
        );
        assert_old_tool_alone(&dir, &out);
    }
}

#[test]
fn a_mismatch_names_both_digests() {
    // Catches: a message that leaves out the digest a maintainer needs to update the pin.
    let e = Error::Mismatch {
        expected: Sha256([0; 32]),
        actual: Sha256([0xff; 32]),
    };
    assert_eq!(
        e.to_string(),
        format!(
            "the download's SHA-256 is {}, not the pinned {}; nothing was installed",
            "ff".repeat(32),
            "00".repeat(32)
        )
    );
}

#[test]
fn a_matching_raw_download_replaces_the_old_tool_and_is_executable() {
    // Catches: a raw download altered on the way to disk, a file left without the execute bits
    // (the job's next step runs it), an existing tool not replaced, or a temporary file left
    // beside it.
    let (dir, out) = with_old_tool("raw");
    install(TOOL, &Sha256::of(TOOL), &Unpack::Raw, &out).expect("install");
    assert_eq!(std::fs::read(&out).expect("read"), TOOL);
    let mode = std::fs::metadata(&out).expect("stat").permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(names(&dir), ["tool"]);
}

#[test]
fn a_gzip_tar_yields_the_tool_and_a_missing_directory_is_made() {
    // Catches: a tar decoded with the wrong codec, a member picked by the wrong name (the decoy
    // README comes first), or `--out` in a directory that does not exist yet refused.
    let dir = scratch("tar-gz");
    let out = dir.join("bin").join("tool");
    let archive = gzip(&tar());
    install(
        &archive,
        &Sha256::of(&archive),
        &tar_gz("tool-1.0/tool"),
        &out,
    )
    .expect("install");
    assert_eq!(std::fs::read(&out).expect("read"), TOOL);
}

#[test]
fn an_archive_that_does_not_decode_is_an_unpack_error_and_the_old_tool_stays() {
    // Catches: a decode failure that installs the raw bytes, an empty file or a short file
    // instead of failing: bytes that are not gzip, a gzip stream cut short before its trailer,
    // a tar whose header is not a tar's, and a tar cut short inside the member asked for.
    let not_tar = [1u8; 1024];
    let whole = gzip(&tar());
    let cut_gzip = whole[..whole.len() - 4].to_vec();
    let mut cut_tar = tar();
    let at = cut_tar
        .windows(TOOL.len())
        .position(|w| w == TOOL)
        .expect("the tool's bytes are in the tar");
    cut_tar.truncate(at + TOOL.len() / 2);
    let cases: [(&str, Vec<u8>, &str); 4] = [
        ("not gzip", b"not gzip".to_vec(), "x"),
        ("gzip cut short", cut_gzip, "tool-1.0/tool"),
        ("not a tar", gzip(&not_tar), "x"),
        ("tar cut short", gzip(&cut_tar), "tool-1.0/tool"),
    ];
    for (name, bytes, path) in cases {
        let (dir, out) = with_old_tool(name);
        let result = install(&bytes, &Sha256::of(&bytes), &tar_gz(path), &out);
        assert!(
            matches!(result, Err(Error::Unpack(_))),
            "{name}: {result:?}"
        );
        assert_old_tool_alone(&dir, &out);
    }
}

#[test]
fn a_missing_member_or_one_that_is_not_a_regular_file_is_refused() {
    // Catches: an installer that writes an empty file for a member the archive lacks (a renamed
    // directory upstream), or for a symlink or directory entry.
    let archive = gzip(&tar());
    let cases = [
        (
            "tool-1.0/cargo-deny",
            Error::NoMember(member("tool-1.0/cargo-deny")),
        ),
        ("tool-1.0/link", Error::NotAFile(member("tool-1.0/link"))),
        ("tool-1.0", Error::NotAFile(member("tool-1.0"))),
    ];
    for (path, expected) in cases {
        let (dir, out) = with_old_tool("member");
        let result = install(&archive, &Sha256::of(&archive), &tar_gz(path), &out);
        assert_eq!(result, Err(expected));
        assert_old_tool_alone(&dir, &out);
    }
    assert_eq!(
        Error::NoMember(member("a/b")).to_string(),
        "the archive has no \"a/b\""
    );
    assert_eq!(
        Error::NotAFile(member("a/b")).to_string(),
        "\"a/b\" in the archive is not a regular file"
    );
}

#[test]
fn an_install_that_cannot_make_the_directory_of_out_changes_nothing() {
    // Catches: a failure to create the directory reported as something else, or a temporary
    // file left behind: the parent of `--out` is an ordinary file.
    let dir = scratch("no-dir");
    std::fs::write(dir.join("file"), OLD).expect("file");
    let out = dir.join("file").join("tool");
    let result = install(TOOL, &Sha256::of(TOOL), &Unpack::Raw, &out);
    assert!(
        matches!(&result, Err(Error::Install { path, .. }) if *path == out),
        "{result:?}"
    );
    assert_eq!(names(&dir), ["file"]);
    assert_eq!(std::fs::read(dir.join("file")).expect("read"), OLD);
}

#[test]
fn an_install_that_cannot_rename_leaves_no_temporary_file() {
    // Catches: a temporary file left behind when the last step fails (the next reader of the
    // directory then sees a half-installed tool): `--out` is a directory, so the rename over it
    // fails after the temporary file was written.
    let dir = scratch("rename-fails");
    let out = dir.join("tool");
    std::fs::create_dir_all(out.join("inside")).expect("dir");
    let result = install(TOOL, &Sha256::of(TOOL), &Unpack::Raw, &out);
    assert!(
        matches!(&result, Err(Error::Install { path, why }) if *path == out && !why.is_empty()),
        "{result:?}"
    );
    assert_eq!(names(&dir), ["tool"]);
    assert!(out.join("inside").is_dir(), "the old --out is untouched");
}

#[test]
fn an_existing_file_or_symlink_at_the_temporary_name_is_never_opened() {
    // Catches: a temporary file opened with plain create (which truncates an existing file, and
    // follows a symbolic link to whatever it names), so a link left at that name could truncate
    // another file or put other bytes at `--out`. The install fails and touches neither.
    let (dir, out) = with_old_tool("symlink");
    let victim = dir.join("victim");
    std::fs::write(&victim, b"not to be touched").expect("victim");
    let part = part_path(&out, 7);
    std::os::unix::fs::symlink(&victim, &part).expect("symlink");
    let result = install_as(TOOL, &Sha256::of(TOOL), &Unpack::Raw, &out, 7);
    assert!(matches!(&result, Err(Error::Install { .. })), "{result:?}");
    assert_eq!(
        std::fs::read(&victim).expect("victim"),
        b"not to be touched"
    );
    assert_eq!(std::fs::read(&out).expect("read"), OLD);
    assert!(part.is_symlink(), "the link is left as it was found");
}

#[test]
fn concurrent_installs_to_one_out_never_mix_their_bytes() {
    // Catches: installs that share a temporary file, so one renames the other's half-written
    // or overwritten bytes into place. Each thread installs whole bytes of its own; `--out`
    // ends as one of them, whole, and no temporary file is left.
    let dir = scratch("concurrent");
    let out = dir.join("tool");
    let tools: Vec<Vec<u8>> = (0..8u8).map(|i| vec![i; 64 * 1024]).collect();
    std::thread::scope(|s| {
        for tool in &tools {
            let out = &out;
            s.spawn(move || install(tool, &Sha256::of(tool), &Unpack::Raw, out).expect("install"));
        }
    });
    let installed = std::fs::read(&out).expect("read");
    assert!(tools.contains(&installed), "--out is one whole install");
    assert_eq!(names(&dir), ["tool"]);
}

#[test]
fn every_install_gets_a_temporary_name_of_its_own_beside_out() {
    // Catches: a temporary file in another directory (the rename then crosses file systems, or
    // fails), or one name for every install.
    let out = Path::new("/some/dir/tool");
    let a = part_path(out, 1);
    let b = part_path(out, 2);
    assert_ne!(a, b);
    assert_eq!(a.parent(), out.parent());
    assert_eq!(
        a,
        PathBuf::from(format!("/some/dir/.tool.{}.1.part", std::process::id()))
    );
}

#[test]
fn extract_and_member_must_agree() {
    // Catches: a tar format accepted without the member to take (what would it install?), or a
    // member silently ignored for a format that has none.
    let m = || Some(member("d/tool"));
    assert_eq!(Unpack::new(None, None), Ok(Unpack::Raw));
    assert_eq!(Unpack::new(Some(Format::TarGz), m()), Ok(tar_gz("d/tool")));
    assert_eq!(
        Unpack::new(Some(Format::TarGz), None),
        Err(UnpackFlags::ExtractWithoutMember)
    );
    assert_eq!(
        Unpack::new(None, m()),
        Err(UnpackFlags::MemberWithoutExtract)
    );
    assert_eq!(
        UnpackFlags::ExtractWithoutMember.to_string(),
        "--extract tar.gz needs --member, the file to take"
    );
    assert_eq!(
        UnpackFlags::MemberWithoutExtract.to_string(),
        "--member is a file in a tar: it needs --extract tar.gz"
    );
}

#[test]
fn install_tool_reads_the_download_installs_it_and_prints_what_it_did() {
    // Catches: a verb that installs without the check, prints nothing a log can show, or reads
    // a missing download as an empty one.
    let dir = scratch("install-tool");
    let download = dir.join("tool.download");
    std::fs::write(&download, TOOL).expect("download");
    let out = dir.join("bin").join("tool");
    let line = install_tool(&download, &Sha256::of(TOOL), &Unpack::Raw, &out).expect("install");
    assert_eq!(
        line,
        format!(
            "ok: {} has SHA-256 {}; installed {}",
            download.display(),
            Sha256::of(TOOL),
            out.display()
        )
    );
    assert_eq!(std::fs::read(&out).expect("tool"), TOOL);

    let absent = dir.join("absent");
    let e = install_tool(&absent, &Sha256::of(TOOL), &Unpack::Raw, &dir.join("never"))
        .expect_err("absent");
    assert!(
        matches!(&e, Error::Read { path, why } if *path == absent && !why.is_empty()),
        "{e}"
    );
    assert!(!dir.join("never").exists());
}
