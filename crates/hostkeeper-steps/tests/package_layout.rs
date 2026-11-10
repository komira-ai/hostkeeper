//! A package directory: versions are sealed trees `<version>/` with `<version>.seal`, switched by
//! `current` and `previous` as the updater's own binaries are.

mod support;

use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_steps::selfupdate::Layout;
use hostkeeper_steps::{ArtifactError, SEAL_SCHEMA, Seal, SealError, Staged, tree_digest};
use support::Scratch;

const PACKAGE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A tree `<dir>/.stage-<version>` holding `bin/x` with `text`, modes as an unpack leaves them.
fn tree(layout: &Layout, version: &str, text: &str) -> PathBuf {
    let tree = layout.dir().join(format!(".stage-{version}"));
    fs::create_dir_all(tree.join("bin")).unwrap();
    fs::write(tree.join("bin/x"), text).unwrap();
    fs::write(tree.join("manifest.toml"), "schema = 1\n").unwrap();
    fs::set_permissions(tree.join("bin/x"), fs::Permissions::from_mode(0o555)).unwrap();
    tree
}

/// Writes the seal of `tree` as version `version`, then moves the tree in: the order unpack
/// keeps.
fn stage(layout: &Layout, version: &str, text: &str) {
    let tree = tree(layout, version, text);
    write_seal(layout, version, &tree);
    layout.stage_tree(version, &tree).unwrap();
}

fn write_seal(layout: &Layout, version: &str, tree: &Path) {
    let seal = Seal::new(&tree_digest(tree).unwrap(), PACKAGE).unwrap();
    fs::write(layout.seal_path(version), seal.to_json()).unwrap();
}

fn package(s: &Scratch) -> Layout {
    let layout = Layout::package(&s.path().join("tierstore"));
    fs::create_dir_all(layout.dir()).unwrap();
    layout
}

fn links(layout: &Layout) -> (Option<String>, Option<String>) {
    (layout.current().unwrap(), layout.previous().unwrap())
}

fn text(layout: &Layout, version: &str) -> String {
    fs::read_to_string(layout.version_path(version).join("bin/x")).unwrap()
}

/// Catches a staged tree that is not moved whole, or that moves a link: staging puts the tree at
/// `<version>/` and changes neither `current` nor `previous`; a version outside the rule of
/// design 11.8 (a leading `-` or `.`, a `/`, a `~`, a non-ASCII letter, 256 bytes), or a source
/// that is not a directory, is refused, and one with `+` build metadata is staged; a binary
/// directory does not stage trees and a package directory does not stage files.
#[test]
fn staging_a_tree_adds_a_version_and_moves_no_link() {
    let s = Scratch::new("pl-stage");
    let layout = package(&s);
    stage(&layout, "1.5.0", "new");
    assert_eq!(text(&layout, "1.5.0"), "new");
    assert_eq!(links(&layout), (None, None));
    assert!(!layout.dir().join(".stage-1.5.0").exists());
    layout.check_tree("1.5.0").unwrap();
    let source = tree(&layout, "x", "x");
    let long = "1".repeat(256);
    for bad in ["", ".stage-x", "a/b", "../x", "é", "-1", "1.0~rc1", &long] {
        let err = layout.stage_tree(bad, &source).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
    }
    stage(&layout, "1.4.0+g1a2b3c", "build");
    layout.check_tree("1.4.0+g1a2b3c").unwrap();
    let file = s.path().join("file");
    fs::write(&file, "f").unwrap();
    let err = layout.stage_tree("1.6.0", &file).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(
        layout
            .stage_tree("1.6.0", &s.path().join("absent"))
            .is_err()
    );
    assert_eq!(
        layout.stage("1.6.0", &file).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    let binaries = Layout::new(&s.path().join("bin"));
    assert_eq!(
        binaries.stage_tree("1.6.0", &source).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        binaries.check_tree("1.6.0").unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(source.exists());
}

/// Catches a version that keeps its rule but is too long for the names the layout puts beside it,
/// each layout counting the longest name it makes itself: in a package directory
/// `.<version>.seal.tmp`, so 245 bytes are staged and 246 or 255 are `InvalidFilename`; in the
/// binary directory `.<version>.stage`, so 248 bytes are staged and 249 are refused. A refused
/// version touches nothing.
#[test]
fn a_version_too_long_for_its_derived_names_is_refused() {
    let s = Scratch::new("pl-long");
    let layout = package(&s);
    let source = tree(&layout, "x", "x");
    for version in ["1".repeat(246), "1".repeat(255)] {
        let err = layout.stage_tree(&version, &source).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidFilename);
        assert_eq!(
            err.to_string(),
            format!(
                "a version of {} bytes is too long for the names the layout puts beside it: \
                 `.<version>.seal.tmp` would pass 255 bytes",
                version.len()
            )
        );
    }
    assert!(source.exists());
    let longest = "1".repeat(245);
    assert_eq!(format!(".{longest}.seal.tmp").len(), 255);
    let tree = layout.dir().join(".stage-long");
    fs::create_dir_all(tree.join("bin")).unwrap();
    fs::write(tree.join("bin/x"), "long").unwrap();
    fs::write(tree.join("manifest.toml"), "schema = 1\n").unwrap();
    fs::set_permissions(tree.join("bin/x"), fs::Permissions::from_mode(0o555)).unwrap();
    write_seal(&layout, &longest, &tree);
    layout.stage_tree(&longest, &tree).unwrap();
    layout.check_tree(&longest).unwrap();

    let binaries = Layout::new(&s.path().join("bin"));
    let file = s.path().join("file");
    fs::write(&file, "f").unwrap();
    let over = "1".repeat(249);
    let err = binaries.stage(&over, &file).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidFilename);
    assert_eq!(
        err.to_string(),
        "a version of 249 bytes is too long for the names the layout puts beside it: \
         `.<version>.stage` would pass 255 bytes"
    );
    assert!(!s.path().join("bin/versions").join(&over).exists());
    let fits = "1".repeat(248);
    assert_eq!(format!(".{fits}.stage").len(), 255);
    binaries.stage(&fits, &file).unwrap();
    assert_eq!(
        fs::read(s.path().join("bin/versions").join(&fits)).unwrap(),
        b"f"
    );
}

/// Catches a switch that loses the way back: `previous` becomes what `current` was, a second
/// switch to the current version changes nothing, the links are relative, and a version never
/// staged is refused.
#[test]
fn a_switch_remembers_the_previous_tree_and_is_idempotent() {
    let s = Scratch::new("pl-switch");
    let layout = package(&s);
    stage(&layout, "v1", "a");
    stage(&layout, "v2", "b");
    layout.switch("v1").unwrap();
    assert_eq!(links(&layout), (Some("v1".into()), None));
    layout.switch("v2").unwrap();
    layout.switch("v2").unwrap();
    assert_eq!(links(&layout), (Some("v2".into()), Some("v1".into())));
    assert_eq!(
        fs::read_to_string(layout.current_path().join("bin/x")).unwrap(),
        "b"
    );
    assert_eq!(
        fs::read_to_string(layout.previous_path().join("bin/x")).unwrap(),
        "a"
    );
    assert_eq!(
        fs::read_link(layout.current_path()).unwrap(),
        Path::new("v2")
    );
    assert_eq!(
        layout.switch("v3").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        layout.switch("../v1").unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

/// Catches a version the host runs, or would fall back to, being staged over: staging the
/// version `current` or `previous` points at is refused and its tree and seal are left as they
/// were; a version neither names may be staged again and is then replaced whole.
#[test]
fn the_versions_current_and_previous_point_at_are_not_staged_over() {
    let s = Scratch::new("pl-live");
    let layout = package(&s);
    stage(&layout, "v1", "a");
    stage(&layout, "v2", "b");
    layout.switch("v1").unwrap();
    layout.switch("v2").unwrap();
    let seals = |l: &Layout| {
        (
            fs::read(l.seal_path("v1")).unwrap(),
            fs::read(l.seal_path("v2")).unwrap(),
        )
    };
    let before = seals(&layout);
    for live in ["v1", "v2"] {
        let other = tree(&layout, live, "c");
        let err = layout.stage_tree(live, &other).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{live}");
        assert!(err.to_string().contains("current"), "{err}");
    }
    assert_eq!(
        (text(&layout, "v1"), text(&layout, "v2")),
        ("a".into(), "b".into())
    );
    assert_eq!(seals(&layout), before);
    assert_eq!(links(&layout), (Some("v2".into()), Some("v1".into())));
    stage(&layout, "v3", "c");
    stage(&layout, "v3", "d");
    assert_eq!(text(&layout, "v3"), "d");
    layout.check_tree("v3").unwrap();
}

/// Catches staging that writes beside its own version: with `v1` live and an unswitched `v2`
/// staged, staging `v2` again replaces only `v2` (a read-only tree included), leaves `v1` with
/// the same inode and bytes, and a leftover file in the place of `v3` is replaced by the tree.
#[test]
fn staging_one_version_never_writes_over_another() {
    use std::os::unix::fs::MetadataExt;
    let s = Scratch::new("pl-stage-name");
    let layout = package(&s);
    stage(&layout, "v1", "a");
    layout.switch("v1").unwrap();
    stage(&layout, "v2", "b");
    let v2 = layout.version_path("v2");
    fs::set_permissions(v2.join("bin"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(&v2, fs::Permissions::from_mode(0o555)).unwrap();
    let inode = |p: &Path| fs::symlink_metadata(p).unwrap().ino();
    let v1 = (inode(&layout.version_path("v1")), text(&layout, "v1"));
    stage(&layout, "v2", "b2");
    assert_eq!(text(&layout, "v2"), "b2");
    assert_eq!((inode(&layout.version_path("v1")), text(&layout, "v1")), v1);
    assert_eq!(links(&layout), (Some("v1".into()), None));
    fs::write(layout.version_path("v3"), "a leftover").unwrap();
    stage(&layout, "v3", "c");
    assert_eq!(text(&layout, "v3"), "c");
}

/// Catches a switch to a tree that is gone or no longer what was sealed: with `previous` removed,
/// or a byte of it changed, switching to it is an error and `current` stays.
#[test]
fn switching_to_a_missing_or_changed_tree_is_refused() {
    let s = Scratch::new("pl-switch-changed");
    let layout = package(&s);
    stage(&layout, "v1", "a");
    stage(&layout, "v2", "b");
    layout.switch("v1").unwrap();
    layout.switch("v2").unwrap();
    let x = layout.version_path("v1").join("bin/x");
    fs::set_permissions(&x, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(&x, "A").unwrap();
    fs::set_permissions(&x, fs::Permissions::from_mode(0o555)).unwrap();
    let err = layout.switch("v1").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
        err.to_string().contains("changed after it was sealed"),
        "{err}"
    );
    assert_eq!(links(&layout), (Some("v2".into()), Some("v1".into())));
    fs::remove_dir_all(layout.version_path("v1")).unwrap();
    assert_eq!(
        layout.switch("v1").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(links(&layout), (Some("v2".into()), Some("v1".into())));
}

/// Catches a tree check that passes what was not sealed or is not whole: a link in the place of
/// the version (dangling or to a sealed tree), a version writable by others, a missing,
/// unparseable or unreadable seal, a changed file, a link inside the tree and a foreign owner
/// are each refused, and the sealed tree passes.
#[test]
fn a_tree_check_refuses_a_link_a_missing_seal_a_changed_file_and_a_foreign_owner() {
    let s = Scratch::new("pl-check");
    let layout = package(&s);
    stage(&layout, "v1", "a");
    layout.check_tree("v1").unwrap();
    let me = rustix::process::geteuid().as_raw();
    let host = |meta: &fs::Metadata| {
        use std::os::unix::fs::MetadataExt;
        (meta.uid(), meta.gid())
    };
    let invalid = |version: &str| {
        let err = layout.check_tree(version).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{version}: {err}");
    };

    // A link where a version should be.
    symlink("v1", layout.version_path("v-link")).unwrap();
    write_seal(&layout, "v-link", &layout.version_path("v1"));
    invalid("v-link");
    symlink("nowhere", layout.version_path("v-dangling")).unwrap();
    invalid("v-dangling");
    assert_eq!(
        layout.check_tree("v-absent").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );

    // Open to others.
    let v1 = layout.version_path("v1");
    fs::set_permissions(&v1, fs::Permissions::from_mode(0o757)).unwrap();
    invalid("v1");
    fs::set_permissions(&v1, fs::Permissions::from_mode(0o755)).unwrap();

    // The seal: missing, not a seal, unreadable.
    let seal = fs::read(layout.seal_path("v1")).unwrap();
    fs::remove_file(layout.seal_path("v1")).unwrap();
    invalid("v1");
    fs::write(layout.seal_path("v1"), "{\"schema\":1").unwrap();
    invalid("v1");
    fs::write(layout.seal_path("v1"), &seal).unwrap();
    fs::set_permissions(layout.seal_path("v1"), fs::Permissions::from_mode(0o000)).unwrap();
    let err = layout.check_tree("v1").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    fs::set_permissions(layout.seal_path("v1"), fs::Permissions::from_mode(0o444)).unwrap();
    layout.check_tree("v1").unwrap();

    // A tree that cannot be read is an I/O error, not a changed tree.
    fs::set_permissions(v1.join("bin"), fs::Permissions::from_mode(0o000)).unwrap();
    let err = layout.check_tree("v1").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    fs::set_permissions(v1.join("bin"), fs::Permissions::from_mode(0o755)).unwrap();

    // A changed file, then a link inside the tree.
    fs::write(v1.join("manifest.toml"), "schema = 2\n").unwrap();
    invalid("v1");
    fs::write(v1.join("manifest.toml"), "schema = 1\n").unwrap();
    layout.check_tree("v1").unwrap();
    symlink("/", v1.join("bin/root")).unwrap();
    invalid("v1");
    fs::remove_file(v1.join("bin/root")).unwrap();

    // A foreign owner of the version, and of a file in it.
    layout.check_tree_as("v1", &[me, 0], &host).unwrap();
    let err = layout.check_tree_as("v1", &[4242], &host).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    let files_of_another = |meta: &fs::Metadata| {
        let (uid, gid) = host(meta);
        (if meta.is_file() { 4242 } else { uid }, gid)
    };
    let err = layout
        .check_tree_as("v1", &[me, 0], &files_of_another)
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("not trusted"), "{err}");
}

/// Catches a change of the on-disk names another release reads: after two trees are staged and
/// switched to in turn, the package directory holds exactly `<v1>/`, `<v1>.seal`, `<v2>/`,
/// `<v2>.seal`, `current -> <v2>` and `previous -> <v1>`, the targets relative.
#[test]
fn the_on_disk_names_are_current_previous_and_version_seal() {
    let s = Scratch::new("pl-names");
    let layout = package(&s);
    stage(&layout, "1.4.2", "a");
    stage(&layout, "1.5.0", "b");
    layout.switch("1.4.2").unwrap();
    layout.switch("1.5.0").unwrap();
    let mut listing: Vec<String> = fs::read_dir(layout.dir())
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = entry.file_type().unwrap();
            if kind.is_symlink() {
                let target = fs::read_link(entry.path()).unwrap();
                format!("{name} -> {}", target.display())
            } else if kind.is_dir() {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    listing.sort();
    assert_eq!(
        listing,
        [
            "1.4.2.seal",
            "1.4.2/",
            "1.5.0.seal",
            "1.5.0/",
            "current -> 1.5.0",
            "previous -> 1.4.2",
        ]
    );
    assert_eq!(layout.seal_path("1.5.0"), layout.dir().join("1.5.0.seal"));
    assert_eq!(layout.version_path("1.5.0"), layout.dir().join("1.5.0"));
}

/// Catches `Staged::check` passing a package tree that holds what an archive may not: a link
/// (to a file inside the tree), a setuid file and a byte changed after sealing are each refused,
/// the first two as untrusted and the last as changed.
#[test]
fn staged_check_refuses_a_symlink_a_setuid_file_and_a_changed_tree() {
    let s = Scratch::new("pl-staged");
    let layout = package(&s);
    stage(&layout, "v1", "a");
    let dir = layout.version_path("v1");
    let staged = Staged::seal(&dir).unwrap();
    let hook = Path::new("bin/x");
    assert_eq!(staged.check(hook).unwrap(), dir.join("bin/x"));

    symlink("x", dir.join("bin/y")).unwrap();
    let err = staged.check(hook).unwrap_err();
    assert!(
        matches!(&err, ArtifactError::Untrusted(p) if p == Path::new("bin/y")),
        "{err:?}"
    );
    fs::remove_file(dir.join("bin/y")).unwrap();

    fs::set_permissions(dir.join("bin/x"), fs::Permissions::from_mode(0o4555)).unwrap();
    let err = staged.check(hook).unwrap_err();
    assert!(matches!(&err, ArtifactError::Untrusted(_)), "{err:?}");
    fs::set_permissions(dir.join("bin/x"), fs::Permissions::from_mode(0o555)).unwrap();

    fs::set_permissions(dir.join("bin/x"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(dir.join("bin/x"), "b").unwrap();
    fs::set_permissions(dir.join("bin/x"), fs::Permissions::from_mode(0o555)).unwrap();
    let err = staged.check(hook).unwrap_err();
    assert!(matches!(&err, ArtifactError::Changed { .. }), "{err:?}");
}

/// Catches a seal reader that is not additive or that mistakes one failure for another: a seal
/// with a field it does not know reads; schema 2 is refused as a schema, not as unreadable;
/// a truncated seal, a non-UTF-8 one, one without a schema and one with a short digest are
/// unreadable; and a seal is written in exactly the published form.
#[test]
fn a_seal_reads_additively_and_writes_its_published_form() {
    let tree = "aa".repeat(32);
    let seal = Seal::new(&tree, PACKAGE).unwrap();
    assert_eq!(
        seal.to_json(),
        format!("{{\"schema\":1,\"tree_digest\":\"{tree}\",\"package_digest\":\"{PACKAGE}\"}}")
    );
    assert_eq!(SEAL_SCHEMA, 1);
    assert_eq!(Seal::parse(seal.to_json().as_bytes()).unwrap(), seal);
    let extra = format!(
        "{{\"schema\":1,\"tree_digest\":\"{tree}\",\"package_digest\":\"{PACKAGE}\",\"signed_by\":\"x\"}}"
    );
    let read = Seal::parse(extra.as_bytes()).unwrap();
    assert_eq!(
        (read.tree_digest(), read.package_digest()),
        (tree.as_str(), PACKAGE)
    );
    let newer = format!("{{\"schema\":2,\"tree_digest\":\"{tree}\"}}");
    assert_eq!(Seal::parse(newer.as_bytes()), Err(SealError::Schema(2)));
    assert_eq!(
        SealError::Schema(2).to_string(),
        "the seal has schema 2; this release reads schema 1 only"
    );
    let json = seal.to_json();
    let truncated = &json.as_bytes()[..json.len() - 1];
    for bad in [
        truncated,
        &[0xff, 0xfe][..],
        b"{\"tree_digest\":\"x\"}",
        b"[1]",
        format!("{{\"schema\":1,\"tree_digest\":\"ab\",\"package_digest\":\"{PACKAGE}\"}}")
            .as_bytes(),
    ] {
        let err = Seal::parse(bad).unwrap_err();
        assert!(matches!(err, SealError::Unreadable(_)), "{bad:?}: {err:?}");
        assert!(err.to_string().starts_with("the seal cannot be read: "));
    }
    for (t, p, bad) in [
        ("AB".repeat(32), PACKAGE.to_owned(), "AB".repeat(32)),
        (tree.clone(), "0".repeat(63), "0".repeat(63)),
    ] {
        let err = Seal::new(&t, &p).unwrap_err();
        assert_eq!(err, SealError::BadDigest(bad.clone()));
        assert_eq!(
            err.to_string(),
            format!("{bad} is not a SHA-256 of 64 lower-case hex digits")
        );
    }
}

/// Catches a version that is one of the package directory's own names: staging or switching to
/// `current`, `previous`, `<v>.seal` or `<v>.tmp` is refused before anything is touched, so the
/// links, the seals and the trees stay as they were; the binary directory, which holds none of
/// those beside its versions, still takes such a name.
#[test]
fn a_version_named_like_the_layouts_own_files_is_refused() {
    let s = Scratch::new("pl-own-names");
    let layout = package(&s);
    stage(&layout, "v1", "a");
    layout.switch("v1").unwrap();
    let source = tree(&layout, "x", "x");
    let listing = |l: &Layout| {
        let mut names: Vec<_> = fs::read_dir(l.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = listing(&layout);
    for name in ["current", "previous", "v1.seal", "current.tmp"] {
        let err = layout.stage_tree(name, &source).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{name}");
        assert_eq!(
            layout.switch(name).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "{name}"
        );
    }
    assert_eq!(listing(&layout), before);
    assert_eq!(links(&layout), (Some("v1".into()), None));
    layout.check_tree("v1").unwrap();
    let binaries = Layout::new(&s.path().join("bin"));
    let file = s.path().join("file");
    fs::write(&file, "f").unwrap();
    binaries.stage("previous", &file).unwrap();
}

/// Catches a staging source that is not a fresh staging directory: a live version (which the
/// rename would move away from under `current`), the destination itself, a directory inside a
/// version and a directory outside the package directory are each refused before anything is
/// removed or moved, and every version, seal and link stays.
#[test]
fn only_a_staging_directory_of_the_package_directory_is_staged() {
    let s = Scratch::new("pl-sources");
    let layout = package(&s);
    stage(&layout, "v1", "a");
    layout.switch("v1").unwrap();
    stage(&layout, "v2", "b");
    let nested = layout.version_path("v2").join(".stage-n");
    let outside = s.path().join(".stage-out");
    fs::create_dir_all(&outside).unwrap();
    let listing = |l: &Layout| {
        let mut names: Vec<_> = fs::read_dir(l.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = listing(&layout);
    for (what, version, source) in [
        ("the live version", "v3", layout.version_path("v1")),
        ("the destination", "v2", layout.version_path("v2")),
        ("a directory inside a version", "v3", nested),
        ("a directory elsewhere", "v3", outside),
    ] {
        let err = layout.stage_tree(version, &source).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{what}");
        assert_eq!(listing(&layout), before, "{what}");
    }
    assert_eq!(
        (text(&layout, "v1"), text(&layout, "v2")),
        ("a".into(), "b".into())
    );
    layout.check_tree("v1").unwrap();
    layout.check_tree("v2").unwrap();
    assert_eq!(links(&layout), (Some("v1".into()), None));
}

/// Catches a seal read that follows a link, waits on a pipe or has no bound: a link to a good
/// seal, a FIFO and a file over `MAX_SEAL_BYTES` in place of the seal make the tree check
/// refuse the version as invalid, at once, and `read_seal` says why.
#[test]
fn a_seal_that_is_a_link_a_fifo_or_too_large_is_refused() {
    let s = Scratch::new("pl-odd-seal");
    let layout = package(&s);
    stage(&layout, "v1", "a");
    let seal = layout.seal_path("v1");
    let good = s.path().join("good.seal");
    fs::copy(&seal, &good).unwrap();
    fs::remove_file(&seal).unwrap();
    symlink(&good, &seal).unwrap();
    let err = hostkeeper_steps::read_seal(&seal).unwrap_err();
    assert_eq!(err.to_string(), "the seal is a link");
    assert_eq!(
        layout.check_tree("v1").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    fs::remove_file(&seal).unwrap();
    hostkeeper_testkit::fifo(&seal, 0o644).unwrap();
    let err = hostkeeper_steps::read_seal(&seal).unwrap_err();
    assert_eq!(err.to_string(), "the seal is not a regular file");
    assert_eq!(
        layout.check_tree("v1").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    fs::remove_file(&seal).unwrap();
    let max = hostkeeper_steps::MAX_SEAL_BYTES as usize;
    let mut padded = fs::read(&good).unwrap();
    padded.resize(max, b' ');
    fs::write(&seal, &padded).unwrap();
    layout.check_tree("v1").unwrap();
    padded.push(b' ');
    fs::write(&seal, &padded).unwrap();
    let err = hostkeeper_steps::read_seal(&seal).unwrap_err();
    assert_eq!(err.to_string(), "the seal is larger than 4096 bytes");
    assert_eq!(
        layout.check_tree("v1").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
