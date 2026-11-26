//! The parts of `hk xcode verify` that are functions of their input: how `df`'s report is read,
//! the margin of free space, the kind of an archive, what the unexpanded archive is said to name,
//! the files a check makes and fails to make, and how a refusal of `xip` is told from its
//! acceptance. The whole run is in tests/cli/xcode_verify.rs.

use std::path::Path;

use hostkeeper_testkit::Scratch;

use super::free::{available, enough};
use super::tamper::{damage, refused, tools_package};
use super::toc::{metadata, names_version};
use super::{first_bytes, kind_of, make_dir, write_file};
use crate::xcode_push::Mac;
use crate::xcode_record::Output;

/// A new scratch directory of the test `name`, in the target directory's `tmp`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// Proves: the free space is the number before the capacity of the last line of `df -Pk`, in KiB
/// made bytes, whatever the file system is called (spaces in its name included), and a report
/// with no such line or no number is none.
#[test]
fn the_available_column_is_the_number_before_the_capacity() {
    let header = "Filesystem 1024-blocks Used Available Capacity Mounted on\n";
    for (report, bytes) in [
        (
            format!("{header}/dev/disk3s5 971350180 400000000 12345 30% /"),
            Some(12345 * 1024),
        ),
        (
            format!("{header}map auto_home 100 90 10 90% /System/Volumes/Data/home\n\n"),
            Some(10 * 1024),
        ),
        (
            format!("{header}/dev/x 1 1 7 1% /Volumes/My Disk 100%"),
            Some(7 * 1024),
        ),
        (
            format!("{header}/dev/disk3s5 971350180 400000000 lots 30% /"),
            None,
        ),
        (
            format!("{header}/dev/disk3s5 971350180 400000000 18446744073709551615 30% /"),
            None,
        ),
        (header.to_owned(), None),
        (String::new(), None),
        ("\n  \n".to_owned(), None),
        ("/dev/disk3s5 1 1 1 1".to_owned(), None),
    ] {
        assert_eq!(available(&report), bytes, "{report}");
    }
}

/// Proves: three times the archive's size is enough and one byte less is not, and the failure
/// names the directory, the bytes free, the bytes needed and the size.
#[test]
fn three_times_the_archive_is_the_margin_to_the_byte() {
    let dir = Path::new("/scratch/expansion");
    assert_eq!(enough(300, 100, dir), Ok(()));
    assert_eq!(enough(301, 100, dir), Ok(()));
    assert_eq!(
        enough(299, 100, dir),
        Err(
            "free_space_short: the volume of /scratch/expansion has 299 bytes free and the \
              expansion needs 300 (3 times the archive's 100)"
                .to_owned()
        )
    );
    assert!(enough(u64::MAX - 1, u64::MAX, dir).is_err());
}

/// Proves: the kind is `xar!`, `AA01` or `AEA1` by the first four bytes exactly, and anything else
/// (a prefix of them, another case) is `unknown`.
#[test]
fn the_kind_is_read_from_the_first_four_bytes_only() {
    for (bytes, kind) in [
        (b"xar!".as_slice(), "xar!"),
        (b"AA01", "AA01"),
        (b"AEA1", "AEA1"),
        (b"XAR!", "unknown"),
        (b"AA0", "unknown"),
        (b"", "unknown"),
    ] {
        assert_eq!(kind_of(bytes), kind, "{bytes:?}");
    }
}

/// Proves: a version or a build is named by any member name or text that holds `version` or
/// `build` in any case, and by nothing else.
#[test]
fn a_version_is_named_by_the_word() {
    for text in [
        "ProductBuildVersion",
        "Info.plist\nVERSION.plist",
        "build",
        "Build 27B52",
    ] {
        assert!(names_version(text), "{text}");
    }
    for text in ["", "Metadata\nContent\n", "27.2", "27B52"] {
        assert!(!names_version(text), "{text}");
    }
}

/// Proves: the extracted `Metadata` is a file with its text, a directory, a link (not followed),
/// or missing; and a file that is not text is read lossily.
#[test]
fn the_metadata_member_is_told_by_what_it_is() {
    let scratch = scratch("hk-verify-metadata");
    let dir = scratch.path();
    assert_eq!(metadata(dir), ("missing".to_owned(), None));
    std::fs::write(dir.join("Metadata"), b"text \xff").expect("a file");
    assert_eq!(
        metadata(dir),
        ("file".to_owned(), Some("text \u{fffd}".to_owned()))
    );
    std::fs::remove_file(dir.join("Metadata")).expect("removed");
    std::fs::create_dir(dir.join("Metadata")).expect("a directory");
    assert_eq!(metadata(dir), ("directory".to_owned(), None));
    std::fs::remove_dir(dir.join("Metadata")).expect("removed");
    std::os::unix::fs::symlink(dir.join("elsewhere"), dir.join("Metadata")).expect("a link");
    assert_eq!(metadata(dir), ("link".to_owned(), None));
}

/// Proves: the package of the Command Line Tools is the first `.pkg` by name; a directory that is
/// not there, or holds none, has none.
#[test]
fn the_first_package_of_the_command_line_tools_is_found() {
    let scratch = scratch("hk-verify-clt");
    let dir = scratch.path();
    assert_eq!(tools_package(&dir.join("missing")), None);
    assert_eq!(tools_package(dir), None);
    std::fs::write(dir.join("b.pkg"), b"").expect("a file");
    std::fs::write(dir.join("a.pkg"), b"").expect("a file");
    std::fs::write(dir.join("a.txt"), b"").expect("a file");
    assert_eq!(tools_package(dir), Some(dir.join("a.pkg")));
}

/// Proves: the files a check makes say which check could not make them and where; and the
/// damage to a file that is not there is a failure, and to an empty one too.
#[test]
fn a_file_that_cannot_be_made_or_damaged_is_said() {
    let scratch = scratch("hk-verify-files");
    let dir = scratch.path();
    let nowhere = dir.join("nowhere").join("child");
    let shown = nowhere.display();
    assert_eq!(
        make_dir(&nowhere, "toc"),
        Err(format!(
            "toc: cannot make {shown} (No such file or directory (os error 2))"
        ))
    );
    assert_eq!(
        write_file(&nowhere, b"x", "tamper"),
        Err(format!(
            "tamper: cannot write {shown} (No such file or directory (os error 2))"
        ))
    );
    assert_eq!(
        damage(&nowhere),
        Err(format!(
            "tamper: cannot change a byte of {shown} (No such file or directory (os error 2))"
        ))
    );
    let empty = dir.join("empty");
    std::fs::write(&empty, b"").expect("an empty file");
    assert!(damage(&empty).is_err());
    let one = dir.join("one");
    std::fs::write(&one, b"\x00").expect("a file");
    assert_eq!(damage(&one), Ok(()));
    assert_eq!(std::fs::read(&one).expect("read"), [0xff]);
    assert_eq!(
        first_bytes(&nowhere),
        Err(format!(
            "cannot read {shown} (No such file or directory (os error 2))"
        ))
    );
}

/// Proves: `xip --expand` exiting non-zero is a refusal; exiting 0 is an acceptance, which fails
/// saying what it accepted; and not exiting with a code (a signal) fails saying so, with its
/// stderr, since it is no answer.
#[test]
fn only_a_non_zero_exit_of_xip_is_a_refusal() {
    let mac = Mac::system();
    let said = |exit: Option<i32>, stderr: &str| Output {
        exit,
        stdout: String::new(),
        stderr: stderr.to_owned(),
    };
    assert_eq!(refused(&said(Some(1), ""), "x", &mac), Ok(()));
    assert_eq!(refused(&said(Some(255), ""), "x", &mac), Ok(()));
    assert_eq!(
        refused(&said(Some(0), ""), "the damaged clone", &mac),
        Err("tamper: /usr/bin/xip --expand accepted the damaged clone".to_owned())
    );
    assert_eq!(
        refused(&said(None, "killed\n"), "the damaged clone", &mac),
        Err(
            "tamper: /usr/bin/xip --expand on the damaged clone did not exit with a code: killed"
                .to_owned()
        )
    );
}
