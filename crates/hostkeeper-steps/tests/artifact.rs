//! The verified artifact: a tree that changed after it was verified never yields a hook.

mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use hostkeeper_steps::{ArtifactError, EntryKind, Staged, read_tree, tree_digest};
use support::{Rig, Scratch};

fn tree(name: &str) -> Scratch {
    let s = Scratch::new(name);
    fs::create_dir_all(s.path().join("bin")).unwrap();
    fs::write(s.path().join("bin/hook"), "#!hook\n").unwrap();
    fs::write(s.path().join("VERSION"), "2.0.0\n").unwrap();
    s
}

/// Catches a digest that is not a function of the tree: the same tree twice gives one digest,
/// and it is 64 lower-case hex digits.
#[test]
fn the_digest_of_a_tree_is_stable_hex() {
    let s = tree("art-stable");
    let a = tree_digest(s.path()).unwrap();
    assert_eq!(a, tree_digest(s.path()).unwrap());
    assert_eq!(a.len(), 64);
    assert!(
        a.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
}

/// Catches a digest that misses a kind of change: content, mode, an added or removed file, a
/// rename, an empty directory added, a file made a directory.
#[test]
fn every_kind_of_change_moves_the_digest() {
    let s = tree("art-moves");
    let p = s.path();
    let base = tree_digest(p).unwrap();
    let mut seen = vec![base.clone()];
    let mut step = |change: &dyn Fn()| {
        change();
        let d = tree_digest(p).unwrap();
        assert!(
            !seen.contains(&d),
            "a change left the digest at a value it had before"
        );
        seen.push(d);
    };
    step(&|| fs::write(p.join("VERSION"), "2.0.1\n").unwrap());
    step(&|| fs::set_permissions(p.join("bin/hook"), fs::Permissions::from_mode(0o755)).unwrap());
    step(&|| fs::write(p.join("extra"), "").unwrap());
    step(&|| fs::rename(p.join("extra"), p.join("extra2")).unwrap());
    step(&|| fs::create_dir(p.join("empty")).unwrap());
    step(&|| fs::remove_file(p.join("extra2")).unwrap());
    step(&|| {
        fs::remove_dir(p.join("empty")).unwrap();
        fs::write(p.join("empty"), "").unwrap();
    });
}

/// Catches a framing that lets two different trees hash alike: a file `ab` holding `c` and a file
/// `a` holding `bc` are different trees.
#[test]
fn names_and_contents_cannot_run_together() {
    let (x, y) = (Scratch::new("art-frame-x"), Scratch::new("art-frame-y"));
    fs::write(x.path().join("ab"), "c").unwrap();
    fs::write(y.path().join("a"), "bc").unwrap();
    assert_ne!(
        tree_digest(x.path()).unwrap(),
        tree_digest(y.path()).unwrap()
    );
}

/// Catches a check that returns a hook of a tree it never compared: sealed, it yields the path;
/// altered by one byte, it is `Changed` with both digests named.
#[test]
fn a_tree_altered_after_sealing_is_refused() {
    let s = tree("art-altered");
    let staged = Staged::seal(s.path()).unwrap();
    assert_eq!(
        staged.check(Path::new("bin/hook")).unwrap(),
        s.path().join("bin/hook")
    );
    fs::write(s.path().join("bin/hook"), "#!hook!\n").unwrap();
    let err = staged.check(Path::new("bin/hook")).unwrap_err();
    let ArtifactError::Changed { expected, found } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(expected, staged.digest());
    assert_ne!(found, expected);
    assert!(err.to_string().contains("changed after it was verified"));
}

/// Catches a hook path that can leave the tree: absolute, `..`, empty, a directory, a link, a
/// missing file.
#[test]
fn only_a_plain_relative_regular_file_is_a_hook() {
    let s = tree("art-hook");
    let staged = Staged::seal(s.path()).unwrap();
    for bad in ["/usr/bin/hook", "../x", "", "bin/../bin/hook"] {
        let err = staged.check(Path::new(bad)).unwrap_err();
        assert!(
            matches!(err, ArtifactError::BadHookPath(_)),
            "{bad}: {err:?}"
        );
        assert!(err.to_string().contains("relative path"));
    }
    for missing in ["bin", "nothing"] {
        let err = staged.check(Path::new(missing)).unwrap_err();
        assert!(
            matches!(err, ArtifactError::HookMissing(_)),
            "{missing}: {err:?}"
        );
        assert!(err.to_string().contains("not a regular file"));
    }
}

/// Catches a digest that quietly skips what it cannot hash: a FIFO and a link in the tree are
/// refused by name, and a tree that is not there is an I/O error.
#[test]
fn what_cannot_be_hashed_is_refused() {
    let s = tree("art-fifo");
    hostkeeper_testkit::fifo(&s.path().join("pipe"), 0o600).unwrap();
    let err = tree_digest(s.path()).unwrap_err();
    assert!(matches!(&err, ArtifactError::Unsupported(p) if p == Path::new("pipe")));
    assert!(
        err.to_string()
            .contains("neither a regular file nor a directory")
    );
    fs::remove_file(s.path().join("pipe")).unwrap();
    symlink("VERSION", s.path().join("bin/link")).unwrap();
    let err = tree_digest(s.path()).unwrap_err();
    assert!(matches!(&err, ArtifactError::Unsupported(p) if p == Path::new("bin/link")));
    assert!(Staged::seal(s.path()).is_err());
    let err = tree_digest(&s.path().join("absent")).unwrap_err();
    assert!(matches!(err, ArtifactError::Io(_)));
    assert!(err.to_string().contains("cannot read the staged release"));
    assert!(Staged::seal(&s.path().join("absent")).is_err());
}

/// Catches the rig's two releases hashing alike (which would hide a swap of one for the other).
#[test]
fn the_rigs_two_releases_differ() {
    let rig = Rig::new("art-rig");
    assert_ne!(
        tree_digest(&rig.new_dir).unwrap(),
        tree_digest(&rig.old_dir).unwrap()
    );
}

/// Catches a staged release that others can change being trusted: a tree writable by group and
/// a parent writable by others are `Untrusted`, naming it; the same tree closed is accepted.
#[test]
fn the_tree_and_its_parents_must_be_closed() {
    let s = Scratch::new("art-place");
    let outer = fs::canonicalize(s.path()).unwrap().join("outer");
    let inner = outer.join("inner");
    fs::create_dir_all(inner.join("bin")).unwrap();
    fs::write(inner.join("bin/hook"), "#!hook\n").unwrap();
    let staged = Staged::seal(&inner).unwrap();
    let hook = Path::new("bin/hook");
    staged.check(hook).unwrap();

    fs::set_permissions(&inner, fs::Permissions::from_mode(0o775)).unwrap();
    let err = staged.check(hook).unwrap_err();
    assert!(matches!(&err, ArtifactError::Untrusted(p) if p == &inner));
    assert!(err.to_string().contains("closed to group and others"));
    fs::set_permissions(&inner, fs::Permissions::from_mode(0o755)).unwrap();

    fs::set_permissions(&outer, fs::Permissions::from_mode(0o757)).unwrap();
    assert!(matches!(
        staged.check(hook).unwrap_err(),
        ArtifactError::Untrusted(p) if p == outer
    ));
    fs::set_permissions(&outer, fs::Permissions::from_mode(0o755)).unwrap();
    staged.check(hook).unwrap();
}

/// Catches a tree sealed through a link above it being refused, which halted every package on
/// macOS (`/var` is a link to `/private/var`): the seal names the tree by its real path, with
/// the digest of the same tree sealed directly, and the check passes and returns the hook on
/// that path; the real parents are still judged (one open to others is `Untrusted`, named by
/// its real path), a byte changed is still `Changed`, and a tree that is itself a link is still
/// `Untrusted`, sealed directly or through the linked parent.
#[test]
fn a_linked_parent_is_judged_by_its_real_path() {
    let s = Scratch::new("art-linked-parent");
    let real = fs::canonicalize(s.path()).unwrap();
    let outer = real.join("private/var");
    let inner = outer.join("inner");
    fs::create_dir_all(inner.join("bin")).unwrap();
    fs::write(inner.join("bin/hook"), "#!hook\n").unwrap();
    let var = real.join("var");
    symlink("private/var", &var).unwrap();
    let hook = Path::new("bin/hook");

    let through = Staged::seal(&var.join("inner")).unwrap();
    assert_eq!(through.dir(), inner);
    assert_eq!(through.digest(), Staged::seal(&inner).unwrap().digest());
    assert_eq!(through.check(hook).unwrap(), inner.join("bin/hook"));

    fs::set_permissions(&outer, fs::Permissions::from_mode(0o757)).unwrap();
    assert!(matches!(
        through.check(hook).unwrap_err(),
        ArtifactError::Untrusted(p) if p == outer
    ));
    fs::set_permissions(&outer, fs::Permissions::from_mode(0o755)).unwrap();

    fs::write(inner.join("bin/hook"), "#!hoox\n").unwrap();
    let err = through.check(hook).unwrap_err();
    assert!(
        matches!(&err, ArtifactError::Changed { expected, found }
            if expected == through.digest() && found != expected),
        "{err:?}"
    );

    let linked_tree = outer.join("linked");
    symlink("inner", &linked_tree).unwrap();
    let sealed = Staged::seal(&linked_tree).unwrap();
    assert!(matches!(
        sealed.check(hook).unwrap_err(),
        ArtifactError::Untrusted(p) if p == linked_tree
    ));
    assert!(matches!(
        Staged::seal(&var.join("linked")).unwrap().check(hook).unwrap_err(),
        ArtifactError::Untrusted(p) if p == linked_tree
    ));
}

/// A tree whose parent does not exist (so its real path cannot be resolved) cannot be sealed:
/// an I/O error, `NotFound`, as for any tree that cannot be read.
#[test]
fn a_tree_whose_parent_is_missing_cannot_be_sealed() {
    let s = Scratch::new("art-parent-missing");
    let err = Staged::seal(&s.path().join("missing/tree")).unwrap_err();
    assert!(
        matches!(&err, ArtifactError::Io(e) if e.kind() == std::io::ErrorKind::NotFound),
        "{err:?}"
    );
}

fn host_owner(meta: &fs::Metadata) -> (u32, u32) {
    use std::os::unix::fs::MetadataExt;
    (meta.uid(), meta.gid())
}

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Catches a link inside a release being trusted: a tree with a link to a directory inside or
/// outside it cannot be sealed, and a link added after sealing (beside the hook, or in the
/// hook's place) makes the check refuse the tree as untrusted, naming the link.
#[test]
fn a_link_in_the_tree_is_refused_at_seal_and_at_check() {
    let s = Scratch::new("art-mid-link");
    let outside = Scratch::new("art-mid-link-out");
    fs::write(outside.path().join("hook"), "#!out\n").unwrap();
    fs::create_dir_all(s.path().join("real")).unwrap();
    fs::write(s.path().join("real/hook"), "#!in\n").unwrap();
    let staged = Staged::seal(s.path()).unwrap();
    staged.check(Path::new("real/hook")).unwrap();
    for (link, target) in [("inside", Path::new("real")), ("outside", outside.path())] {
        symlink(target, s.path().join(link)).unwrap();
        let err = Staged::seal(s.path()).unwrap_err();
        assert!(
            matches!(&err, ArtifactError::Unsupported(p) if p == Path::new(link)),
            "{err:?}"
        );
        let err = staged
            .check(Path::new(&format!("{link}/hook")))
            .unwrap_err();
        assert!(
            matches!(&err, ArtifactError::Untrusted(p) if p == Path::new(link)),
            "{err:?}"
        );
        fs::remove_file(s.path().join(link)).unwrap();
    }
    fs::remove_file(s.path().join("real/hook")).unwrap();
    symlink(outside.path().join("hook"), s.path().join("real/hook")).unwrap();
    let err = staged.check(Path::new("real/hook")).unwrap_err();
    assert!(
        matches!(&err, ArtifactError::Untrusted(p) if p == Path::new("real/hook")),
        "{err:?}"
    );
}

/// Catches an owner or a time in the digest (an unpacked package has neither from its archive,
/// so a digest holding them could not be recomputed from the package): a file whose times
/// change keeps the digest, and the entries read back carry kind, mode, size and contents.
#[test]
fn the_digest_holds_kind_name_mode_size_and_bytes_only() {
    let s = tree("art-owner-digest");
    let p = s.path();
    let before = read_tree(p).unwrap();
    let file = fs::File::options()
        .write(true)
        .open(p.join("VERSION"))
        .unwrap();
    file.set_modified(std::time::UNIX_EPOCH).unwrap();
    assert_eq!(read_tree(p).unwrap(), before);
    let names: Vec<&[u8]> = before.entries.iter().map(|e| e.name.as_slice()).collect();
    assert_eq!(names, [&b"VERSION"[..], b"bin", b"bin/hook"]);
    let version = &before.entries[0];
    assert_eq!(
        (version.kind, version.size, version.mode & 0o7000),
        (EntryKind::File, 6, 0)
    );
    assert!(version.content.is_some());
    assert_eq!(
        (before.entries[1].kind, before.entries[1].content),
        (EntryKind::Dir, None)
    );
}

/// Catches a foreign owner being trusted: with no trusted owner the tree is refused; an entry
/// inside the tree owned by another user is refused too, though the directories are fine.
#[test]
fn only_the_trusted_owners_may_own_the_tree_and_what_is_in_it() {
    let s = tree("art-foreign");
    let staged = Staged::seal(s.path()).unwrap();
    let hook = Path::new("bin/hook");
    staged.check_as(hook, &[me(), 0], &host_owner).unwrap();
    let err = staged.check_as(hook, &[], &host_owner).unwrap_err();
    assert!(matches!(err, ArtifactError::Untrusted(_)));
    let files_of_another = |meta: &fs::Metadata| {
        let (uid, gid) = host_owner(meta);
        (if meta.is_file() { 4242 } else { uid }, gid)
    };
    let err = staged
        .check_as(hook, &[me(), 0], &files_of_another)
        .unwrap_err();
    assert!(
        matches!(&err, ArtifactError::Untrusted(p) if p.ends_with("VERSION") || p.ends_with("hook")),
        "{err:?}"
    );
}

/// Catches an entry a service could rewrite or that runs with more than its owner's rights: a
/// setuid or setgid file, a file or directory writable by group or others, inside the tree.
#[test]
fn a_tree_with_setuid_or_writable_entries_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    for (name, mode, dir) in [
        ("suid", 0o4755, false),
        ("sgid", 0o2755, false),
        ("gw", 0o664, false),
        ("ow", 0o646, false),
        ("dgw", 0o775, true),
    ] {
        let s = tree(&format!("art-entry-{name}"));
        let target = if dir {
            s.path().join("bin")
        } else {
            s.path().join("VERSION")
        };
        let staged = Staged::seal(s.path()).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
        let err = staged.check(Path::new("bin/hook")).unwrap_err();
        assert!(
            matches!(&err, ArtifactError::Untrusted(_)),
            "{name}: {err:?}"
        );
    }
}

/// Catches a staged directory that is relative or has `..`: the checks of its ancestors would
/// then be made on something else than the tree that is run, so it is not even sealed.
#[test]
fn the_staged_directory_must_be_absolute_and_plain() {
    for dir in ["relative/tree", "/a/../b", "", "/"] {
        let err = Staged::seal(Path::new(dir)).unwrap_err();
        assert!(
            matches!(err, ArtifactError::Untrusted(_)),
            "{dir:?}: {err:?}"
        );
    }
}

/// Catches a change of the digest of a tree with no link (formats change additively only): a
/// fixed small tree keeps the digest it had before links were allowed in `tree/`.
#[test]
fn a_tree_with_no_link_keeps_its_digest() {
    let s = Scratch::new("art-golden");
    let p = s.path();
    fs::create_dir_all(p.join("bin")).unwrap();
    fs::write(p.join("bin/hook"), "#!hook\n").unwrap();
    fs::write(p.join("VERSION"), "2.0.0\n").unwrap();
    fs::set_permissions(p.join("bin"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(p.join("bin/hook"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(p.join("VERSION"), fs::Permissions::from_mode(0o444)).unwrap();
    let digest = tree_digest(p).unwrap();
    fs::set_permissions(p.join("bin"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        digest,
        "f5bc6d1e5400b1db4138cafc7349424d6ddfc966ee06fe95840acc9dca63f1d6"
    );
}
