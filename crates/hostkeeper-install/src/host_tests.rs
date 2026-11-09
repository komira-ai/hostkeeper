//! Tests of the host's file system and accounts.

use std::os::unix::fs::{MetadataExt, symlink};

use hostkeeper_testkit::Scratch;

use super::*;

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap();
    Scratch::under(&target.join("tmp"), name).unwrap()
}

/// The accounts of the test: the scratch owner is root, its group is `wheel`.
fn test_ids(dir: &Scratch) -> Ids {
    let meta = fs::metadata(dir.path()).unwrap();
    Ids {
        root: meta.uid(),
        wheel: meta.gid(),
        admin: meta.gid().wrapping_add(7),
    }
}

fn write(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
}

#[test]
fn ids_map_the_hosts_root_to_zero_and_never_map_another_account_to_zero() {
    let ids = Ids {
        root: 501,
        wheel: 20,
        admin: 21,
    };
    assert_eq!(ids.uid(501), 0);
    assert_eq!(ids.uid(7), 7);
    assert_eq!(ids.uid(0), 1);
}

/// Catches: a host account that is not wheel or admin taking the id of a Mac's wheel or admin.
#[test]
fn ids_map_wheel_and_admin_and_keep_other_groups_off_their_numbers() {
    let ids = Ids {
        root: 501,
        wheel: 20,
        admin: 21,
    };
    assert_eq!(ids.gid(20), GID_WHEEL);
    assert_eq!(ids.gid(21), GID_ADMIN);
    assert_eq!(ids.gid(100), 100);
    assert_eq!(ids.gid(GID_WHEEL), u32::MAX);
    assert_eq!(ids.gid(GID_ADMIN), u32::MAX);
    assert_eq!(Ids::MAC.gid(80), 80);
    assert_eq!(Ids::MAC.uid(0), 0);
}

#[test]
fn kind_of_mode_names_each_file_type() {
    assert_eq!(Kind::of_mode(0o100644), Kind::File);
    assert_eq!(Kind::of_mode(0o040755), Kind::Dir);
    assert_eq!(Kind::of_mode(0o120777), Kind::Link);
    assert_eq!(Kind::of_mode(0o010644), Kind::Other);
    assert_eq!(Kind::of_mode(0o140644), Kind::Other);
    assert_eq!(Kind::of_mode(0o020644), Kind::Other);
}

#[test]
fn real_places_a_logical_path_under_the_root() {
    let host = Host::new(Path::new("/scratch/root"), Ids::MAC);
    assert_eq!(
        host.real("/Library/x"),
        Path::new("/scratch/root/Library/x")
    );
    assert_eq!(host.real("//a"), Path::new("/scratch/root/a"));
    assert_eq!(host.root(), Path::new("/scratch/root"));
    assert_eq!(host.ids(), &Ids::MAC);
    assert_eq!(
        Host::mac().real("/usr/bin/lsbom"),
        Path::new("/usr/bin/lsbom")
    );
}

#[test]
fn meta_reads_kind_mode_and_accounts_without_following_a_link() {
    let dir = scratch("meta");
    let host = Host::new(dir.path(), test_ids(&dir));
    write(&dir.path().join("f"), "x");
    fs::set_permissions(dir.path().join("f"), fs::Permissions::from_mode(0o4755)).unwrap();
    symlink("f", dir.path().join("l")).unwrap();
    fs::create_dir(dir.path().join("d")).unwrap();

    let file = host.meta("/f").unwrap().unwrap();
    assert_eq!(file.kind, Kind::File);
    assert_eq!(file.mode, 0o4755);
    assert_eq!(file.uid, 0);
    assert_eq!(file.gid, GID_WHEEL);
    assert_eq!(host.meta("/l").unwrap().unwrap().kind, Kind::Link);
    assert_eq!(host.meta("/d").unwrap().unwrap().kind, Kind::Dir);
    assert_eq!(host.meta("/none").unwrap(), None);
}

#[test]
fn meta_through_a_file_is_an_error_not_nothing() {
    let dir = scratch("metaerr");
    let host = Host::new(dir.path(), test_ids(&dir));
    write(&dir.path().join("f"), "x");
    assert!(host.meta("/f/below").is_err());
}

#[test]
fn through_link_finds_a_link_or_a_file_among_the_parents() {
    let dir = scratch("through");
    let host = Host::new(dir.path(), test_ids(&dir));
    fs::create_dir_all(dir.path().join("real/sub")).unwrap();
    symlink("real", dir.path().join("link")).unwrap();
    write(&dir.path().join("file"), "x");

    assert!(host.through_link("/link/sub/x").unwrap());
    assert!(host.through_link("/file/x").unwrap());
    assert!(!host.through_link("/real/sub/x").unwrap());
    assert!(!host.through_link("/missing/sub/x").unwrap());
    assert!(!host.through_link("/x").unwrap());
}

#[test]
fn delete_file_deletes_a_file_and_a_link_and_leaves_a_directory_or_nothing() {
    let dir = scratch("delfile");
    let host = Host::new(dir.path(), test_ids(&dir));
    write(&dir.path().join("f"), "x");
    write(&dir.path().join("target"), "t");
    symlink("target", dir.path().join("l")).unwrap();
    fs::create_dir(dir.path().join("d")).unwrap();

    assert!(host.delete_file("/f").unwrap());
    assert!(!dir.path().join("f").exists());
    assert!(host.delete_file("/l").unwrap());
    assert!(
        dir.path().join("target").exists(),
        "the link's target stays"
    );
    assert!(!host.delete_file("/d").unwrap());
    assert!(dir.path().join("d").is_dir());
    assert!(!host.delete_file("/missing").unwrap());
}

#[test]
fn delete_file_reports_a_meta_error() {
    let dir = scratch("delfileerr");
    let host = Host::new(dir.path(), test_ids(&dir));
    write(&dir.path().join("f"), "x");
    assert!(host.delete_file("/f/below").is_err());
}

#[test]
fn delete_empty_dir_deletes_only_an_empty_directory() {
    let dir = scratch("deldir");
    let host = Host::new(dir.path(), test_ids(&dir));
    fs::create_dir(dir.path().join("empty")).unwrap();
    fs::create_dir_all(dir.path().join("full/inner")).unwrap();
    write(&dir.path().join("file"), "x");

    assert!(host.delete_empty_dir("/empty").unwrap());
    assert!(!dir.path().join("empty").exists());
    assert!(!host.delete_empty_dir("/full").unwrap());
    assert!(dir.path().join("full/inner").is_dir());
    assert!(!host.delete_empty_dir("/missing").unwrap());
    assert!(!host.delete_empty_dir("/file").unwrap());
    assert!(dir.path().join("file").exists());
}

#[test]
fn parents_lists_the_directories_above_a_path_outermost_first() {
    assert_eq!(
        parents("/Library/LaunchDaemons/x.plist"),
        ["/Library", "/Library/LaunchDaemons"]
    );
    assert!(parents("/x").is_empty());
    assert!(parents("/").is_empty());
    assert_eq!(parents("//a//b/c"), ["/a", "/a/b"]);
}

#[test]
fn walk_lists_nested_entries_by_logical_path_and_does_not_follow_a_link() {
    let dir = scratch("walk");
    let outside = scratch("walkout");
    write(&outside.path().join("secret"), "s");
    fs::create_dir_all(dir.path().join("a/b")).unwrap();
    write(&dir.path().join("a/b/f"), "x");
    symlink(outside.path(), dir.path().join("a/out")).unwrap();

    let found = walk(dir.path(), &test_ids(&dir)).unwrap();
    let listed: Vec<(&str, Kind)> = found.iter().map(|(p, m)| (p.as_str(), m.kind)).collect();
    assert_eq!(
        listed,
        [
            ("/a", Kind::Dir),
            ("/a/b", Kind::Dir),
            ("/a/b/f", Kind::File),
            ("/a/out", Kind::Link),
        ]
    );
}

#[test]
fn walk_of_a_missing_directory_is_an_error() {
    let dir = scratch("walkmissing");
    assert!(walk(&dir.path().join("none"), &test_ids(&dir)).is_err());
}

#[test]
fn walk_refuses_an_entry_whose_name_is_not_utf8() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let dir = scratch("walkname");
    write(&dir.path().join(OsStr::from_bytes(b"bad\xff")), "x");
    let err = walk(dir.path(), &test_ids(&dir)).unwrap_err();
    assert_eq!(err.to_string(), "an entry name that is not UTF-8");
}

#[test]
fn logical_refuses_a_path_outside_the_directory_and_a_non_plain_component() {
    let outside = logical(Path::new("/a/b"), Path::new("/c/d")).unwrap_err();
    assert_eq!(outside.to_string(), "an entry outside the walked directory");
    let dots = logical(Path::new("/a"), Path::new("/a/../b")).unwrap_err();
    assert_eq!(dots.to_string(), "an entry name that is not a plain name");
    assert_eq!(
        logical(Path::new("/a"), Path::new("/a/b/c")).unwrap(),
        "/b/c"
    );
}

#[test]
fn file_digest_hashes_a_regular_file_in_lower_case_hex() {
    let dir = scratch("digest");
    write(&dir.path().join("f"), "abc");
    assert_eq!(
        file_digest(&dir.path().join("f")).unwrap().as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
}

#[test]
fn file_digest_reads_a_file_larger_than_its_buffer() {
    let dir = scratch("digestbig");
    let bytes = vec![b'a'; 64 * 1024 * 2 + 5];
    fs::write(dir.path().join("big"), &bytes).unwrap();
    let want = hex::encode(Sha256::digest(&bytes));
    assert_eq!(file_digest(&dir.path().join("big")).unwrap(), Some(want));
}

/// Catches: a link or a directory hashed as if it were the file it stands for.
#[test]
fn file_digest_is_none_for_a_directory_a_link_and_nothing() {
    let dir = scratch("digestnone");
    write(&dir.path().join("f"), "abc");
    symlink("f", dir.path().join("l")).unwrap();
    fs::create_dir(dir.path().join("d")).unwrap();
    assert_eq!(file_digest(&dir.path().join("d")).unwrap(), None);
    assert_eq!(file_digest(&dir.path().join("l")).unwrap(), None);
    assert_eq!(file_digest(&dir.path().join("none")).unwrap(), None);
}

#[test]
fn file_digest_through_a_file_is_an_error() {
    let dir = scratch("digesterr");
    write(&dir.path().join("f"), "abc");
    assert!(file_digest(&dir.path().join("f/below")).is_err());
}

#[test]
fn link_target_gives_the_text_of_a_link_and_none_for_anything_else() {
    let dir = scratch("target");
    write(&dir.path().join("f"), "x");
    symlink("../elsewhere", dir.path().join("l")).unwrap();
    assert_eq!(
        link_target(&dir.path().join("l")).unwrap(),
        Some(PathBuf::from("../elsewhere"))
    );
    assert_eq!(link_target(&dir.path().join("f")).unwrap(), None);
    assert_eq!(link_target(&dir.path().join("none")).unwrap(), None);
    assert!(link_target(&dir.path().join("f/below")).is_err());
}

#[test]
fn remove_tree_deletes_a_sealed_tree() {
    let dir = scratch("tree");
    let tree = dir.path().join("tree");
    fs::create_dir_all(tree.join("a/b")).unwrap();
    write(&tree.join("a/b/f"), "x");
    write(&tree.join("top"), "y");
    for sealed in [tree.join("a/b"), tree.join("a"), tree.clone()] {
        fs::set_permissions(sealed, fs::Permissions::from_mode(0o555)).unwrap();
    }
    remove_tree(&tree).unwrap();
    assert!(!tree.exists());
}

#[test]
fn remove_tree_of_a_missing_path_is_done_and_of_a_file_deletes_it() {
    let dir = scratch("treefile");
    remove_tree(&dir.path().join("none")).unwrap();
    write(&dir.path().join("f"), "x");
    remove_tree(&dir.path().join("f")).unwrap();
    assert!(!dir.path().join("f").exists());
}

#[test]
fn remove_tree_through_a_file_is_an_error() {
    let dir = scratch("treeerr");
    write(&dir.path().join("f"), "x");
    assert!(remove_tree(&dir.path().join("f/below")).is_err());
}

/// Catches: a link in the tree followed, so its target's files are deleted.
#[test]
fn remove_tree_deletes_a_link_and_not_its_target() {
    let dir = scratch("treelink");
    let outside = scratch("treeout");
    write(&outside.path().join("keep"), "k");
    fs::create_dir(dir.path().join("t")).unwrap();
    symlink(outside.path(), dir.path().join("t/l")).unwrap();
    remove_tree(&dir.path().join("t")).unwrap();
    assert!(outside.path().join("keep").exists());
}
