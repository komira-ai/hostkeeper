//! Tests of the store of the files a first install kept: what is kept, how it is put back, and
//! each way a put-back can fail without failing the removal.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;

use hostkeeper_testkit::Scratch;

use super::*;

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap();
    Scratch::under(&target.join("tmp"), name).unwrap()
}

/// A host root and a store beside it.
struct Rig {
    scratch: Scratch,
    root: PathBuf,
    store: PathBuf,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let scratch = scratch(name);
        let root = scratch.path().join("host");
        fs::create_dir(&root).unwrap();
        let store = scratch.path().join("kept");
        Rig {
            root,
            store,
            scratch,
        }
    }

    fn kept(&self) -> Kept {
        Kept::new(&self.store, &self.root)
    }

    fn real(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    /// Writes `bytes` at the logical `path` with `mode`, its parents made.
    fn put(&self, path: &str, bytes: &[u8], mode: u32) {
        let real = self.real(path);
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, bytes).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// The bytes, mode, owner and group at the logical `path`.
    fn seen(&self, path: &str) -> (Vec<u8>, u32, u32, u32) {
        let real = self.real(path);
        let meta = fs::symlink_metadata(&real).unwrap();
        (
            fs::read(&real).unwrap(),
            meta.mode() & 0o7777,
            meta.uid(),
            meta.gid(),
        )
    }
}

fn service() -> ServiceName {
    ServiceName::new("js-agent-config").unwrap()
}

fn paths(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

const FLAGS: &str = "/etc/js-agent/flags";

#[test]
fn a_kept_file_is_put_back_with_its_bytes_mode_owner_and_group_over_the_packages_file() {
    let rig = Rig::new("kept-roundtrip");
    rig.put(FLAGS, b"--node-pool=gpu\n", 0o640);
    let before = rig.seen(FLAGS);
    rig.kept()
        .keep(&service(), "js-agent-config", &paths(&[FLAGS]))
        .unwrap();
    rig.put(FLAGS, b"the package's flags\n", 0o644);

    assert_eq!(rig.kept().put_back(&service()).unwrap(), Vec::new());
    assert_eq!(rig.seen(FLAGS), before);
    assert_eq!(
        names(&rig.real("/etc/js-agent")),
        ["flags"],
        "no temporary file is left beside it"
    );
}

#[test]
fn a_put_back_makes_again_the_parents_the_removal_took_away_with_their_modes() {
    let rig = Rig::new("kept-parents");
    rig.put("/etc/js-agent/conf.d/flags", b"x", 0o600);
    fs::set_permissions(rig.real("/etc/js-agent"), fs::Permissions::from_mode(0o751)).unwrap();
    fs::set_permissions(
        rig.real("/etc/js-agent/conf.d"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    let kept = rig.kept();
    kept.keep(&service(), "pkg", &paths(&["/etc/js-agent/conf.d/flags"]))
        .unwrap();
    fs::remove_dir_all(rig.real("/etc/js-agent")).unwrap();

    assert_eq!(kept.put_back(&service()).unwrap(), Vec::new());
    assert_eq!(rig.seen("/etc/js-agent/conf.d/flags").0, b"x");
    assert_eq!(mode_of(&rig.real("/etc/js-agent")), 0o751);
    assert_eq!(mode_of(&rig.real("/etc/js-agent/conf.d")), 0o750);
}

#[test]
fn the_store_its_set_and_each_copy_are_root_only_whatever_the_file_was() {
    let rig = Rig::new("kept-closed");
    rig.put(FLAGS, b"open", 0o644);
    fs::create_dir(&rig.store).unwrap();
    fs::set_permissions(&rig.store, fs::Permissions::from_mode(0o755)).unwrap();
    let kept = rig.kept();
    kept.keep(&service(), "pkg", &paths(&[FLAGS])).unwrap();

    let set = kept.dir_of(&service());
    assert_eq!(mode_of(&rig.store), DIR_MODE, "an open store is closed");
    assert_eq!(mode_of(&set), DIR_MODE);
    assert_eq!(names(&set), ["0", INDEX]);
    assert_eq!(mode_of(&set.join("0")), FILE_MODE);
    assert_eq!(mode_of(&set.join(INDEX)), FILE_MODE);
    assert_eq!(fs::read(set.join("0")).unwrap(), b"open");
    assert_eq!(
        names(&rig.store),
        ["js-agent-config"],
        "no staging directory is left"
    );
}

#[test]
fn the_index_records_the_package_and_each_file_with_its_digest_mode_owner_and_parents() {
    let rig = Rig::new("kept-index");
    rig.put(FLAGS, b"abc", 0o640);
    let kept = rig.kept();
    kept.keep(&service(), "js-agent-config", &paths(&[FLAGS]))
        .unwrap();
    let (uid, gid) = (rig.seen(FLAGS).2, rig.seen(FLAGS).3);
    let dir = |path: &str| {
        let meta = fs::symlink_metadata(rig.real(path)).unwrap();
        KeptDir {
            path: path.to_owned(),
            mode: meta.mode() & 0o7777,
            uid: meta.uid(),
            gid: meta.gid(),
        }
    };
    assert_eq!(
        kept.set_of(&service()).unwrap(),
        Some(KeptSet {
            package: "js-agent-config".to_owned(),
            files: vec![KeptFile {
                path: FLAGS.to_owned(),
                copy: "0".to_owned(),
                sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                    .to_owned(),
                mode: 0o640,
                uid,
                gid,
                parents: vec![dir("/etc"), dir("/etc/js-agent")],
            }],
        })
    );
}

#[test]
fn a_keep_replaces_the_set_an_earlier_try_kept_and_a_stale_staging_directory() {
    let rig = Rig::new("kept-replace");
    rig.put("/etc/a", b"a", 0o600);
    rig.put("/etc/b", b"b", 0o600);
    let kept = rig.kept();
    kept.keep(&service(), "pkg", &paths(&["/etc/a"])).unwrap();
    let staging = rig.store.join(".js-agent-config.new");
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("junk"), b"junk").unwrap();

    kept.keep(&service(), "pkg", &paths(&["/etc/b"])).unwrap();
    let set = kept.set_of(&service()).unwrap().unwrap();
    assert_eq!(
        set.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        ["/etc/b"]
    );
    assert_eq!(names(&rig.store), ["js-agent-config"]);
}

#[test]
fn a_keep_of_no_path_drops_an_earlier_set_and_makes_no_store() {
    let rig = Rig::new("kept-none");
    let kept = rig.kept();
    kept.keep(&service(), "pkg", &[]).unwrap();
    assert!(!rig.store.exists(), "nothing to keep makes no store");

    rig.put("/etc/a", b"a", 0o600);
    kept.keep(&service(), "pkg", &paths(&["/etc/a"])).unwrap();
    kept.keep(&service(), "pkg", &[]).unwrap();
    assert_eq!(kept.set_of(&service()).unwrap(), None);
    assert_eq!(names(&rig.store), Vec::<String>::new());
}

#[test]
fn a_path_that_holds_no_regular_file_is_not_kept() {
    let rig = Rig::new("kept-skip");
    fs::create_dir_all(rig.real("/etc/dir")).unwrap();
    rig.put("/etc/target", b"t", 0o600);
    symlink(rig.real("/etc/target"), rig.real("/etc/link")).unwrap();
    hostkeeper_testkit::fifo(&rig.real("/etc/fifo"), 0o600).unwrap();
    let kept = rig.kept();
    kept.keep(
        &service(),
        "pkg",
        &paths(&["/etc/dir", "/etc/link", "/etc/fifo", "/etc/missing"]),
    )
    .unwrap();
    assert_eq!(
        kept.set_of(&service()).unwrap(),
        Some(KeptSet {
            package: "pkg".to_owned(),
            files: Vec::new(),
        })
    );
    assert_eq!(kept.put_back(&service()).unwrap(), Vec::new());
}

#[test]
fn a_keep_that_cannot_read_a_path_fails_naming_it_and_keeps_nothing() {
    let rig = Rig::new("kept-unreadable");
    rig.put("/etc/file", b"f", 0o600);
    let kept = rig.kept();
    let error = kept
        .keep(&service(), "pkg", &paths(&["/etc/file/below"]))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "the kept files of js-agent-config: /etc/file/below: Not a directory (os error 20)"
    );
    assert_eq!(kept.set_of(&service()).unwrap(), None);
}

#[test]
fn a_keep_into_a_store_that_is_a_file_fails() {
    let rig = Rig::new("kept-no-store");
    rig.put("/etc/a", b"a", 0o600);
    fs::write(&rig.store, b"a file where the store goes").unwrap();
    let error = rig
        .kept()
        .keep(&service(), "pkg", &paths(&["/etc/a"]))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "the kept files of js-agent-config: the staging directory: Not a directory (os error 20)"
    );
}

#[test]
fn the_candidates_are_the_payload_entries_the_host_holds_as_regular_files() {
    let rig = Rig::new("kept-candidates");
    let tree = rig.scratch.path().join("tree");
    fs::create_dir_all(tree.join("etc/js-agent/conf.d")).unwrap();
    fs::write(tree.join("etc/js-agent/flags"), b"package").unwrap();
    fs::write(tree.join("etc/js-agent/new"), b"package").unwrap();
    fs::write(tree.join("etc/js-agent/conf.d/a"), b"package").unwrap();
    symlink("flags", tree.join("etc/js-agent/alias")).unwrap();
    fs::write(tree.join("etc/js-agent/dir"), b"package").unwrap();
    let odd = std::ffi::OsStr::from_bytes(b"odd-\xff");
    fs::write(tree.join("etc/js-agent").join(odd), b"package").unwrap();

    rig.put(FLAGS, b"hand", 0o600);
    rig.put("/etc/js-agent/conf.d/a", b"hand", 0o600);
    rig.put("/etc/js-agent/alias", b"hand", 0o600);
    fs::create_dir(rig.real("/etc/js-agent/dir")).unwrap();
    fs::write(rig.real("/etc/js-agent").join(odd), b"hand").unwrap();

    assert_eq!(
        rig.kept().candidates(&tree).unwrap(),
        paths(&[
            "/etc/js-agent/alias",
            "/etc/js-agent/conf.d/a",
            "/etc/js-agent/flags",
        ])
    );
    let error = rig.kept().candidates(&tree.join("missing")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn a_put_back_with_no_set_puts_nothing_back() {
    let rig = Rig::new("kept-no-set");
    assert_eq!(rig.kept().put_back(&service()).unwrap(), Vec::new());
    rig.kept().drop_set(&service()).unwrap();
}

#[test]
fn a_missing_copy_is_lost_naming_its_path_and_the_other_files_are_put_back() {
    let rig = Rig::new("kept-missing");
    rig.put("/etc/a", b"a", 0o600);
    rig.put("/etc/b", b"b", 0o600);
    let kept = rig.kept();
    kept.keep(&service(), "pkg", &paths(&["/etc/a", "/etc/b"]))
        .unwrap();
    fs::remove_file(rig.real("/etc/a")).unwrap();
    fs::remove_file(rig.real("/etc/b")).unwrap();
    fs::remove_file(kept.dir_of(&service()).join("0")).unwrap();

    assert_eq!(
        kept.put_back(&service()).unwrap(),
        [Lost {
            path: "/etc/a".to_owned(),
            why: "the kept copy: No such file or directory (os error 2)".to_owned(),
            copy: None,
        }]
    );
    assert!(!rig.real("/etc/a").exists(), "the path stays empty");
    assert_eq!(rig.seen("/etc/b").0, b"b");
    assert_eq!(kept.set_of(&service()).unwrap(), None, "the set is set aside");
    assert_eq!(names(&kept.aside_of(&service())), ["1", INDEX]);
    assert_eq!(kept.put_back(&service()).unwrap(), Vec::new(), "a rerun finds no set");
}

#[test]
fn a_copy_whose_bytes_changed_is_lost_and_its_path_is_left_as_it_was() {
    let rig = Rig::new("kept-changed");
    rig.put(FLAGS, b"hand", 0o600);
    let kept = rig.kept();
    kept.keep(&service(), "pkg", &paths(&[FLAGS])).unwrap();
    fs::write(kept.dir_of(&service()).join("0"), b"other").unwrap();
    rig.put(FLAGS, b"package", 0o644);

    assert_eq!(
        kept.put_back(&service()).unwrap(),
        [Lost {
            path: FLAGS.to_owned(),
            why: format!(
                "the kept copy's SHA-256 is {}, not the recorded {}",
                digest(b"other"),
                digest(b"hand")
            ),
            copy: Some(kept.aside_of(&service()).join("0")),
        }]
    );
    assert_eq!(rig.seen(FLAGS).0, b"package");
    assert_eq!(names(&rig.real("/etc/js-agent")), ["flags"]);
}

/// The SHA-256 of `bytes`, lower-case hex.
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[test]
fn an_unreadable_copy_is_lost() {
    let rig = Rig::new("kept-copy-dir");
    rig.put(FLAGS, b"hand", 0o600);
    let kept = rig.kept();
    kept.keep(&service(), "pkg", &paths(&[FLAGS])).unwrap();
    let copy = kept.dir_of(&service()).join("0");
    fs::remove_file(&copy).unwrap();
    fs::create_dir(&copy).unwrap();

    assert_eq!(
        kept.put_back(&service()).unwrap(),
        [Lost {
            path: FLAGS.to_owned(),
            why: "Is a directory (os error 21)".to_owned(),
            copy: Some(kept.aside_of(&service()).join("0")),
        }]
    );
    assert_eq!(rig.seen(FLAGS).0, b"hand", "the path is left as it was");
}

#[test]
fn a_path_that_cannot_be_written_is_lost() {
    let rig = Rig::new("kept-unwritable");
    rig.put(FLAGS, b"hand", 0o600);
    let kept = rig.kept();
    kept.keep(&service(), "pkg", &paths(&[FLAGS])).unwrap();
    fs::remove_dir_all(rig.real("/etc/js-agent")).unwrap();
    fs::write(rig.real("/etc/js-agent"), b"a file where the directory was").unwrap();

    assert_eq!(
        kept.put_back(&service()).unwrap(),
        [Lost {
            path: FLAGS.to_owned(),
            why: "Not a directory (os error 20)".to_owned(),
            copy: Some(kept.aside_of(&service()).join("0")),
        }]
    );
    assert_eq!(
        fs::read(kept.aside_of(&service()).join("0")).unwrap(),
        b"hand",
        "the operator can put the file back by hand from its copy"
    );
}

#[test]
fn a_second_loss_sets_its_set_aside_in_place_of_the_first() {
    let rig = Rig::new("kept-aside-twice");
    let kept = rig.kept();
    for text in [b"first", b"later"] {
        rig.put(FLAGS, text, 0o600);
        kept.keep(&service(), "pkg", &paths(&[FLAGS])).unwrap();
        fs::remove_dir_all(rig.real("/etc/js-agent")).unwrap();
        fs::write(rig.real("/etc/js-agent"), b"in the way").unwrap();
        assert_eq!(kept.put_back(&service()).unwrap().len(), 1);
        fs::remove_file(rig.real("/etc/js-agent")).unwrap();
    }
    assert_eq!(fs::read(kept.aside_of(&service()).join("0")).unwrap(), b"later");
}

#[test]
fn a_path_that_is_a_directory_now_is_lost_and_no_temporary_file_is_left() {
    let rig = Rig::new("kept-dir-now");
    rig.put(FLAGS, b"hand", 0o600);
    let kept = rig.kept();
    kept.keep(&service(), "pkg", &paths(&[FLAGS])).unwrap();
    fs::remove_file(rig.real(FLAGS)).unwrap();
    fs::create_dir(rig.real(FLAGS)).unwrap();
    fs::write(rig.real(FLAGS).join("inside"), b"x").unwrap();

    let lost = kept.put_back(&service()).unwrap();
    assert_eq!(lost.len(), 1);
    assert_eq!(lost[0].path, FLAGS);
    assert_eq!(names(&rig.real("/etc/js-agent")), ["flags"]);
}

#[test]
fn a_set_whose_path_names_no_file_is_lost() {
    let rig = Rig::new("kept-root-path");
    let kept = Kept::new(&rig.store, Path::new("/"));
    let dir = kept.dir_of(&service());
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("0"), b"").unwrap();
    let set = KeptSet {
        package: "pkg".to_owned(),
        files: vec![KeptFile {
            path: "/".to_owned(),
            copy: "0".to_owned(),
            sha256: digest(b""),
            mode: 0o600,
            uid: 0,
            gid: 0,
            parents: Vec::new(),
        }],
    };
    write_index(&dir.join(INDEX), &set).unwrap();
    assert_eq!(
        kept.put_back(&service()).unwrap(),
        [Lost {
            path: "/".to_owned(),
            why: "the path names no file".to_owned(),
            copy: Some(kept.aside_of(&service()).join("0")),
        }]
    );
}

#[test]
fn an_index_that_cannot_be_read_is_lost_naming_the_index() {
    let rig = Rig::new("kept-bad-index");
    rig.put("/etc/a", b"a", 0o600);
    let kept = rig.kept();
    kept.keep(&service(), "pkg", &paths(&["/etc/a"])).unwrap();
    let index = kept.dir_of(&service()).join(INDEX);
    fs::write(&index, b"not toml").unwrap();
    let lost = kept.put_back(&service()).unwrap();
    assert_eq!(lost.len(), 1);
    assert_eq!(lost[0].path, index.display().to_string());
    assert!(
        lost[0]
            .why
            .starts_with("the index of the kept files cannot be read: "),
        "{lost:?}"
    );
    assert_eq!(rig.seen("/etc/a").0, b"a", "nothing is put back");

    assert_eq!(lost[0].copy, Some(kept.aside_of(&service())));

    kept.keep(&service(), "pkg", &paths(&["/etc/a"])).unwrap();
    fs::remove_file(&index).unwrap();
    let lost = kept.put_back(&service()).unwrap();
    assert_eq!(
        lost,
        [Lost {
            path: index.display().to_string(),
            why: "the index of the kept files cannot be read: No such file or directory (os \
                  error 2)"
                .to_owned(),
            copy: Some(kept.aside_of(&service())),
        }]
    );
}

#[test]
fn a_store_that_cannot_be_read_is_an_error_and_its_drop_fails() {
    let rig = Rig::new("kept-store-file");
    fs::write(&rig.store, b"a file where the store goes").unwrap();
    let kept = rig.kept();
    assert_eq!(
        kept.set_of(&service()).unwrap_err().kind(),
        io::ErrorKind::NotADirectory
    );
    assert_eq!(
        kept.drop_set(&service()).unwrap_err().to_string(),
        "the kept files of js-agent-config: the set's drop: Not a directory (os error 20)"
    );
}

#[test]
fn a_drop_removes_the_set_and_a_stale_staging_directory() {
    let rig = Rig::new("kept-drop");
    rig.put("/etc/a", b"a", 0o600);
    let kept = rig.kept();
    kept.keep(&service(), "pkg", &paths(&["/etc/a"])).unwrap();
    fs::create_dir(rig.store.join(".js-agent-config.new")).unwrap();
    kept.drop_set(&service()).unwrap();
    assert_eq!(names(&rig.store), Vec::<String>::new());
    assert_eq!(rig.seen("/etc/a").0, b"a", "a drop puts nothing back");
}

#[test]
fn unowned_is_the_candidates_less_the_owned() {
    let owned: BTreeSet<String> = paths(&["/b"]).into_iter().collect();
    assert_eq!(unowned(&paths(&["/a", "/b", "/c"]), &owned), paths(&["/a", "/c"]));
}
