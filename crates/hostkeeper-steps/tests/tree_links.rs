//! Links inside `tree/` of a version directory: hashed by their target text, accepted by the
//! check only there, never followed, and never on the way to a hook (docs/package-format.md,
//! "The tree digest").

mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use hostkeeper_steps::{ArtifactError, EntryKind, Staged, read_tree, tree_digest};
use sha2::{Digest, Sha256};
use support::Scratch;

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn host_owner(meta: &fs::Metadata) -> (u32, u32) {
    (meta.uid(), meta.gid())
}

fn mkdir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A version directory: `target.json`, `apt/pkg.deb` and `tree/` with a real hook, a directory
/// of it and a link `tree/usr/lib/x -> ../share/x` to a directory that is there.
fn version(name: &str) -> Scratch {
    let s = Scratch::new(name);
    let p = s.path();
    fs::write(p.join("target.json"), "{}").unwrap();
    mkdir(&p.join("apt"));
    fs::write(p.join("apt/pkg.deb"), "deb").unwrap();
    mkdir(&p.join("tree/usr/lib"));
    mkdir(&p.join("tree/usr/share/x"));
    mkdir(&p.join("tree/bin"));
    fs::write(p.join("tree/bin/hook"), "#!hook\n").unwrap();
    symlink("../share/x", p.join("tree/usr/lib/x")).unwrap();
    s
}

/// A link inside `tree/` is an entry of the tree: it seals, checks (its own 0777 mode is not
/// judged), and its digest follows its target text, which is never resolved.
#[test]
fn a_link_inside_tree_seals_checks_and_follows_its_target_text() {
    let s = version("tl-inside");
    let p = s.path();
    let staged = Staged::seal(p).unwrap();
    staged.check_tree().unwrap();
    staged.check(Path::new("tree/bin/hook")).unwrap();
    let before = tree_digest(p).unwrap();
    fs::remove_file(p.join("tree/usr/lib/x")).unwrap();
    symlink("../share/y", p.join("tree/usr/lib/x")).unwrap();
    assert_ne!(tree_digest(p).unwrap(), before);
    let err = staged.check_tree().unwrap_err();
    assert!(matches!(err, ArtifactError::Changed { .. }), "{err:?}");
    fs::remove_file(p.join("tree/usr/lib/x")).unwrap();
    symlink("../share/x", p.join("tree/usr/lib/x")).unwrap();
    assert_eq!(tree_digest(p).unwrap(), before);
    staged.check_tree().unwrap();
}

/// A link may dangle or point out of the version: it is hashed as text and never followed.
#[test]
fn a_link_is_never_followed_even_when_it_dangles_or_leaves_the_tree() {
    let s = version("tl-dangling");
    let p = s.path();
    symlink("/nowhere/at/all", p.join("tree/abs")).unwrap();
    symlink("../../../../..", p.join("tree/usr/up")).unwrap();
    let staged = Staged::seal(p).unwrap();
    staged.check_tree().unwrap();
}

/// The entry of a link carries kind, mode 0, the target's length as size and the SHA-256 of the
/// target text, and the digest holds the record `l`, name, `0000`, size, target.
#[test]
fn a_link_is_read_as_kind_l_with_its_target_text() {
    let s = Scratch::new("tl-entry");
    let p = s.path();
    mkdir(&p.join("tree"));
    symlink("../y", p.join("tree/x")).unwrap();
    let tree = read_tree(p).unwrap();
    let names: Vec<&[u8]> = tree.entries.iter().map(|e| e.name.as_slice()).collect();
    assert_eq!(names, [&b"tree"[..], b"tree/x"]);
    let link = &tree.entries[1];
    assert_eq!((link.kind, link.mode, link.size), (EntryKind::Link, 0, 4));
    assert_eq!(link.content, Some(Sha256::digest(b"../y").into()));

    let mut expected = Sha256::new();
    for (header, bytes) in [
        (&b"d\x00tree\x000755\x000\x00"[..], &b""[..]),
        (&b"l\x00tree/x\x000000\x004\x00"[..], &b"../y"[..]),
    ] {
        expected.update((header.len() as u64 + bytes.len() as u64).to_le_bytes());
        expected.update(header);
        expected.update(bytes);
    }
    let hex: String = expected
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(tree.digest, hex);
}

/// A link anywhere but inside `tree/` is refused: a top-level one, one under `apt/`, and `tree`
/// itself; unsealable without trust, untrusted under the check.
#[test]
fn a_link_outside_tree_is_refused_at_seal_and_at_check() {
    for (name, link, target_is_dir) in [
        ("tl-top", "x", false),
        ("tl-apt", "apt/x", false),
        ("tl-apt-dir", "apt/d", true),
    ] {
        let s = version(name);
        let p = s.path();
        let staged = Staged::seal(p).unwrap();
        let target = if target_is_dir { "tree" } else { "target.json" };
        symlink(target, p.join(link)).unwrap();
        let err = Staged::seal(p).unwrap_err();
        assert!(
            matches!(&err, ArtifactError::Unsupported(r) if r == Path::new(link)),
            "{name}: {err:?}"
        );
        let err = staged.check_tree().unwrap_err();
        assert!(
            matches!(&err, ArtifactError::Untrusted(r) if r == Path::new(link)),
            "{name}: {err:?}"
        );
    }
}

/// `tree` itself as a link is refused, though what it names is a directory with the same
/// contents.
#[test]
fn tree_itself_as_a_link_is_refused() {
    let s = Scratch::new("tl-tree-link");
    let p = s.path();
    mkdir(&p.join("real"));
    symlink("real", p.join("tree")).unwrap();
    let err = tree_digest(p).unwrap_err();
    assert!(
        matches!(&err, ArtifactError::Unsupported(r) if r == Path::new("tree")),
        "{err:?}"
    );
    let err = read_tree(p).unwrap_err();
    assert!(matches!(err, ArtifactError::Unsupported(_)), "{err:?}");

    // A sealed tree whose `tree` is swapped for a link to a copy of itself fails the check.
    // The copy is outside the version directory, so the link is the version's one defect.
    let v = version("tl-tree-swapped");
    let staged = Staged::seal(v.path()).unwrap();
    let copy = v.path().with_extension("copy");
    fs::rename(v.path().join("tree"), &copy).unwrap();
    symlink(&copy, v.path().join("tree")).unwrap();
    let err = staged.check_tree().unwrap_err();
    assert!(
        matches!(&err, ArtifactError::Untrusted(r) if r == Path::new("tree")),
        "{err:?}"
    );
    fs::remove_dir_all(&copy).unwrap();
}

/// A link inside `tree/` still needs a trusted owner (read without following it).
#[test]
fn a_link_inside_tree_owned_by_a_foreign_user_is_untrusted() {
    let s = version("tl-foreign");
    let staged = Staged::seal(s.path()).unwrap();
    staged.check_tree_as(&[me(), 0], &host_owner).unwrap();
    let links_of_another = |meta: &fs::Metadata| {
        let (uid, gid) = host_owner(meta);
        (if meta.is_symlink() { 4242 } else { uid }, gid)
    };
    let err = staged
        .check_tree_as(&[me(), 0], &links_of_another)
        .unwrap_err();
    assert!(
        matches!(&err, ArtifactError::Untrusted(r) if r == Path::new("tree/usr/lib/x")),
        "{err:?}"
    );
}

/// Files and directories inside `tree/` keep every rule beside a link: a setuid file and a
/// directory writable by others are still refused.
#[test]
fn files_and_directories_inside_tree_keep_their_rules() {
    let s = version("tl-rules");
    let p = s.path();
    let staged = Staged::seal(p).unwrap();
    fs::set_permissions(p.join("tree/bin/hook"), fs::Permissions::from_mode(0o4755)).unwrap();
    let err = staged.check_tree().unwrap_err();
    assert!(matches!(&err, ArtifactError::Untrusted(r) if r == Path::new("tree/bin/hook")));
    fs::set_permissions(p.join("tree/bin/hook"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(p.join("tree/usr"), fs::Permissions::from_mode(0o777)).unwrap();
    let err = staged.check_tree().unwrap_err();
    assert!(matches!(&err, ArtifactError::Untrusted(r) if r == Path::new("tree/usr")));
}

/// A hook never passes through a link: a directory component that is a link to a real directory
/// holding the hook, and a last component that is a link to a regular file, are both missing.
#[test]
fn a_hook_path_through_a_link_is_missing() {
    let s = version("tl-hook");
    let p = s.path();
    symlink("bin", p.join("tree/sbin")).unwrap();
    symlink("bin/hook", p.join("tree/hook")).unwrap();
    let staged = Staged::seal(p).unwrap();
    assert_eq!(
        staged.check(Path::new("tree/bin/hook")).unwrap(),
        staged.dir().join("tree/bin/hook")
    );
    for hook in [
        "tree/sbin/hook",
        "tree/hook",
        "tree/bin/absent",
        "tree/usr/lib/x",
    ] {
        let err = staged.check(Path::new(hook)).unwrap_err();
        assert!(
            matches!(&err, ArtifactError::HookMissing(h) if h == Path::new(hook)),
            "{hook}: {err:?}"
        );
    }
    // A directory as the last component, and a file used as a directory.
    for hook in ["tree/bin", "tree/bin/hook/more"] {
        let err = staged.check(Path::new(hook)).unwrap_err();
        assert!(
            matches!(err, ArtifactError::HookMissing(_)),
            "{hook}: {err:?}"
        );
    }
}

/// `check_tree` passes on an unchanged tree and is `Changed` after a file edit; it does not
/// look at any hook, and it still judges the place.
#[test]
fn check_tree_passes_unchanged_and_notices_an_edit() {
    let s = version("tl-check-tree");
    let p = s.path();
    let staged = Staged::seal(p).unwrap();
    staged.check_tree().unwrap();
    let err = staged.check_tree_as(&[], &host_owner).unwrap_err();
    assert!(matches!(err, ArtifactError::Untrusted(_)), "{err:?}");
    fs::write(p.join("tree/bin/hook"), "#!other\n").unwrap();
    let err = staged.check_tree().unwrap_err();
    assert!(matches!(err, ArtifactError::Changed { .. }), "{err:?}");
}
