//! The real machine: who owns a path, and a host that can look inside a directory it trusts.

use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use hostkeeper_mdm::{Grant, Machine, RealMachine};
use hostkeeper_testkit::Scratch;

fn scratch(name: &str) -> Scratch {
    Scratch::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("hostkeeper-mdm-{name}"),
    )
    .unwrap()
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Catches an owner taken from the wrong file or a missing path reported as an error: the owner
/// of a scratch directory is the uid of the user running the test, and a path that is not there
/// has none.
#[test]
fn the_owner_of_a_directory_is_its_uid_and_a_missing_path_has_none() {
    let dir = scratch("owner");
    let own = std::fs::metadata(dir.path()).unwrap().uid();
    assert_eq!(RealMachine.owner(text(dir.path())).unwrap(), Some(own));
    let missing = dir.path().join("missing");
    assert_eq!(RealMachine.owner(text(&missing)).unwrap(), None);
}

/// Catches every failure to look at a path being read as "not there": a path below a file is
/// ENOTDIR, which is an error, not an absent owner.
#[test]
fn a_path_below_a_file_is_an_error_not_a_missing_owner() {
    let dir = scratch("notdir");
    let file = dir.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let below = file.join("below");
    let e = RealMachine.owner(text(&below)).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(20), "{e}");
    assert_ne!(e.kind(), ErrorKind::NotFound);
}

/// Catches a host made without its grants or unable to look at the disk: the host a grant for
/// the scratch directory makes sees a file in it, with its owner, and a file that is not there
/// as absent.
#[test]
fn the_real_host_stats_inside_a_directory_it_is_granted() {
    let dir = scratch("host");
    let own = std::fs::metadata(dir.path()).unwrap().uid();
    std::fs::write(dir.path().join("file"), b"x").unwrap();
    let grant = Grant {
        dir: text(dir.path()).to_owned(),
        uid: own,
    };
    let host = RealMachine.host(&[grant]);
    let file = host.stat(text(&dir.path().join("file"))).unwrap().unwrap();
    assert_eq!(file.uid, own);
    assert!(host.stat(text(&dir.path().join("none"))).unwrap().is_none());
}
