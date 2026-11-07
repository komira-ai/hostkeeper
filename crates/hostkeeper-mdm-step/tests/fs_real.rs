//! The real host, under a scratch directory standing in for `/`.

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use hostkeeper_mdm_step::{Cmd, Host, Kind, RealHost, TEMP_SUFFIX};
use hostkeeper_testkit::Scratch;

/// The program the run tests start (tests/bin/run_fixture.rs).
const FIXTURE: &str = env!("CARGO_BIN_EXE_hostkeeper-mdm-step-run-fixture");

fn scratch(name: &str) -> (Scratch, RealHost) {
    let dir = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch");
    let host = RealHost::under(dir.path());
    (dir, host)
}

fn me(dir: &Scratch) -> (u32, u32) {
    let m = std::fs::metadata(dir.path()).unwrap();
    (m.uid(), m.gid())
}

/// The names in `dir`, sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Catches a write that leaves the mode to the umask or leaves a temporary file behind: the
/// mode is exact, the old content is replaced whole, and only the target remains.
#[test]
fn write_sets_the_exact_mode_and_replaces_whole() {
    let (dir, host) = scratch("write");
    let owner = me(&dir);
    host.mkdir("/etc/x", 0o750, owner).unwrap();
    host.write("/etc/x/f", 0o640, owner, b"first").unwrap();
    host.write("/etc/x/f", 0o600, owner, b"second").unwrap();
    host.write("/etc/x/f", 0o644, owner, b"third").unwrap();
    let meta = host.stat("/etc/x/f").unwrap().unwrap();
    assert_eq!((meta.kind, meta.mode, meta.size), (Kind::File, 0o644, 5));
    assert_eq!(host.read("/etc/x/f").unwrap().unwrap(), b"third");
    assert_eq!(names_in(&dir.path().join("etc/x")), ["f"]);
    let dir_meta = host.stat("/etc/x").unwrap().unwrap();
    assert_eq!((dir_meta.kind, dir_meta.mode), (Kind::Dir, 0o750));
}

/// Catches a writer that unlinks or reuses a temporary file of another writer of the same file
/// (a name shared by every writer, as `f` and the suffix): the other writer's file, still being
/// written, keeps its bytes, and the write publishes its own whole content.
#[test]
fn a_write_leaves_another_writers_temporary_file_alone() {
    let (dir, host) = scratch("othertemp");
    let owner = me(&dir);
    host.mkdir("/d", 0o700, owner).unwrap();
    let shared = format!("f{TEMP_SUFFIX}");
    let other = format!("f.1-0{TEMP_SUFFIX}");
    for name in [&shared, &other] {
        std::fs::write(dir.path().join("d").join(name), b"partial").unwrap();
    }
    host.write("/d/f", 0o600, owner, b"whole").unwrap();
    assert_eq!(std::fs::read(dir.path().join("d/f")).unwrap(), b"whole");
    for name in [&shared, &other] {
        assert_eq!(
            std::fs::read(dir.path().join("d").join(name)).unwrap(),
            b"partial",
            "{name}"
        );
    }
    assert_eq!(
        names_in(&dir.path().join("d")),
        ["f".to_owned(), other, shared]
    );
}

/// Catches writers of one file that share a temporary name: one would unlink or rename the
/// other's file, so a write fails or the file holds a mix. Eight threads write one file, each
/// its own content, many times: every write succeeds, the file holds one writer's whole
/// content, and no temporary file is left.
#[test]
fn concurrent_writers_of_one_file_each_publish_whole_content() {
    let (dir, host) = scratch("concurrent");
    let owner = me(&dir);
    host.mkdir("/d", 0o700, owner).unwrap();
    let payload = |writer: u8| vec![b'a' + writer; 64 * 1024];
    std::thread::scope(|s| {
        for writer in 0..8u8 {
            let host = host.clone();
            s.spawn(move || {
                for round in 0..25 {
                    let wrote = host.write("/d/f", 0o600, owner, &payload(writer));
                    assert!(wrote.is_ok(), "writer {writer} round {round}: {wrote:?}");
                }
            });
        }
    });
    let content = std::fs::read(dir.path().join("d/f")).unwrap();
    assert!((0..8u8).any(|w| content == payload(w)), "a mixed file");
    assert_eq!(names_in(&dir.path().join("d")), ["f"]);
}

/// Catches a mkdir that leaves an existing directory's mode alone (a rerun must fix it) and
/// a stat that follows a link or reports a missing path as an error.
#[test]
fn mkdir_fixes_an_existing_mode_and_stat_sees_links_and_gaps() {
    let (dir, host) = scratch("mkdir");
    let owner = me(&dir);
    host.mkdir("/d", 0o777, owner).unwrap();
    host.mkdir("/d", 0o700, owner).unwrap();
    assert_eq!(
        std::fs::metadata(dir.path().join("d"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    symlink("d", dir.path().join("l")).unwrap();
    assert_eq!(host.stat("/l").unwrap().unwrap().kind, Kind::Other);
    assert_eq!(host.stat("/nothing").unwrap(), None);
    assert_eq!(host.read("/nothing").unwrap(), None);
    host.remove("/nothing").unwrap();
    host.write("/d/f", 0o600, owner, b"x").unwrap();
    host.remove("/d/f").unwrap();
    assert_eq!(host.stat("/d/f").unwrap(), None);
}

/// Catches errors that are swallowed: a write into a missing directory fails, a read of a
/// directory fails, and a path the system rejects fails rather than reading as missing.
#[test]
fn errors_other_than_not_found_are_returned() {
    let (dir, host) = scratch("errors");
    let owner = me(&dir);
    assert!(host.write("/missing/f", 0o600, owner, b"x").is_err());
    host.mkdir("/d", 0o700, owner).unwrap();
    assert!(host.read("/d").is_err());
    assert!(host.stat("/d/../..\0x").is_err());
}

/// Catches a runner that goes through a shell, passes the environment on, or reports a
/// failing exit as success; a missing program that is not an error; and a program ended by a
/// signal reported as an exit.
#[test]
fn run_executes_without_a_shell_or_environment() {
    let (_dir, host) = scratch("run");
    let said = host
        .run(&Cmd::new(FIXTURE).args(&["echo", "a  b", "$HOME", "x;y"]))
        .unwrap();
    assert_eq!(
        (said.code, said.stdout.as_str(), said.stderr.as_str()),
        (0, "a  b $HOME x;y\n", "")
    );
    let env = host.run(&Cmd::new(FIXTURE).arg("environ")).unwrap();
    assert_eq!((env.code, env.stdout.as_str()), (0, ""));
    let failed = host.run(&Cmd::new(FIXTURE).args(&["exit", "3"])).unwrap();
    assert_eq!((failed.code, failed.stdout.as_str()), (3, ""));
    let killed = host.run(&Cmd::new(FIXTURE).arg("abort")).unwrap();
    assert_eq!(killed.code, -1, "{killed:?}");
    let missing = Path::new(env!("CARGO_TARGET_TMPDIR")).join("no-such-program");
    assert!(host.run(&Cmd::new(missing.to_str().unwrap())).is_err());
}

/// Catches effective ids that are not the process's, and a default host that is not rooted at
/// the system's `/`: the scratch directory's absolute path is stat'ed through it.
#[test]
fn the_default_host_is_the_machine_with_the_process_ids() {
    let (dir, host) = scratch("ids");
    assert_eq!(host.euid(), rustix::process::geteuid().as_raw());
    assert_eq!(host.egid(), rustix::process::getegid().as_raw());
    let absolute = dir.path().to_str().unwrap();
    let meta = RealHost::default().stat(absolute).unwrap().unwrap();
    assert_eq!((meta.kind, meta.uid), (Kind::Dir, me(&dir).0));
}

fn mode_and_owner(path: &Path) -> (u32, u32, u32) {
    let m = std::fs::metadata(path).unwrap();
    (m.permissions().mode() & 0o7777, m.uid(), m.gid())
}

/// Catches a mkdir that follows a link the service user planted where a planned subdirectory
/// should be: root would chmod and chown the link's target (any directory of the host). The
/// link is refused, and its target's mode and owner are exactly what they were.
#[test]
fn mkdir_refuses_a_link_at_a_planned_subdirectory_and_changes_nothing_behind_it() {
    let (dir, host) = scratch("mkdirlink");
    let owner = me(&dir);
    let victim = dir.path().join("victim");
    std::fs::create_dir(&victim).unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let before = mode_and_owner(&victim);
    host.mkdir("/state", 0o700, owner).unwrap();
    symlink(&victim, dir.path().join("state/nanohub")).unwrap();

    let err = host.mkdir("/state/nanohub", 0o700, owner).unwrap_err();
    assert_eq!(
        err.to_string(),
        "refused: /state/nanohub: nanohub is a symbolic link or not a directory; give the real path"
    );
    // A directory to be made below the link is not made behind it.
    let err = host
        .mkdir("/state/nanohub/inner", 0o700, owner)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "refused: /state/nanohub/inner: nanohub is a symbolic link; give the real path"
    );
    assert!(!victim.join("inner").exists());
    assert_eq!(mode_and_owner(&victim), before);
    // A link to a file is not a directory either.
    std::fs::write(dir.path().join("afile"), b"x").unwrap();
    symlink(dir.path().join("afile"), dir.path().join("state/scep")).unwrap();
    assert!(host.mkdir("/state/scep", 0o700, owner).is_err());
    assert_eq!(std::fs::read(dir.path().join("afile")).unwrap(), b"x");
}

/// Catches a write, or a remove, that goes through a link in a directory on the way: the file
/// must not appear behind the link (a rename through it would be a write anywhere), and what
/// is behind the link must stay. A link as the file's own name is replaced, not followed.
#[test]
fn write_and_remove_refuse_a_link_on_the_way_and_replace_a_link_as_the_name() {
    let (dir, host) = scratch("writelink");
    let owner = me(&dir);
    let victim = dir.path().join("victim");
    std::fs::create_dir(&victim).unwrap();
    std::fs::write(victim.join("keep"), b"keep").unwrap();
    host.mkdir("/state", 0o700, owner).unwrap();
    symlink(&victim, dir.path().join("state/removed")).unwrap();

    let err = host
        .write("/state/removed/unit", 0o644, owner, b"unit")
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "refused: /state/removed/unit: removed is a symbolic link; give the real path"
    );
    assert!(!victim.join("unit").exists());
    assert!(host.remove("/state/removed/keep").is_err());
    assert_eq!(std::fs::read(victim.join("keep")).unwrap(), b"keep");

    // The name itself a link: the link is replaced and its target keeps its bytes.
    symlink(victim.join("keep"), dir.path().join("state/f")).unwrap();
    host.write("/state/f", 0o600, owner, b"new").unwrap();
    assert_eq!(std::fs::read(victim.join("keep")).unwrap(), b"keep");
    let now = std::fs::symlink_metadata(dir.path().join("state/f")).unwrap();
    assert!(now.is_file() && !now.file_type().is_symlink());
    assert_eq!(std::fs::read(dir.path().join("state/f")).unwrap(), b"new");
    // Removing a name that is a link removes the link only.
    symlink(victim.join("keep"), dir.path().join("state/g")).unwrap();
    host.remove("/state/g").unwrap();
    assert!(victim.join("keep").exists() && !dir.path().join("state/g").exists());
}

/// Catches a directory owned by a user nobody trusts on the way to a path: it is refused,
/// unless it is owned by the owner the step asks for. And a path that names no file at all.
#[test]
fn a_directory_of_another_user_on_the_way_is_refused() {
    let (dir, _) = scratch("owners");
    let mine = me(&dir);
    let nobody_trusted = RealHost::trusting(dir.path(), &[]);
    let err = nobody_trusted
        .mkdir("/a/b", 0o700, (mine.0 + 1, mine.1))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "refused: /a/b: {} is owned by uid {}, which is not trusted for this path",
            dir.path().display(),
            mine.0
        )
    );
    // The owner the step asks for is trusted, and so is a user in the list.
    nobody_trusted.mkdir("/c/d", 0o700, mine).unwrap();
    RealHost::trusting(dir.path(), &[mine.0])
        .mkdir("/c/e", 0o700, mine)
        .unwrap();
    assert_eq!(
        nobody_trusted
            .write("/", 0o600, mine, b"x")
            .unwrap_err()
            .to_string(),
        "refused: / names no file"
    );
    assert!(nobody_trusted.mkdir("/", 0o700, mine).is_err());
}

/// Catches a host that trusts only root and itself on the way to a path, so that root, running
/// `install`, refuses the operator's directory of fetched binaries and, on a rerun, the
/// service's own state directory; and a grant that reaches past its directory.
///
/// The scratch directory and everything in it is the test's user's. A host that trusts no user
/// at all (`trusting(.., &[])`) stands for root on a host where those directories are another
/// user's: without a grant every read, stat and remove under it is refused. A grant to that
/// user below `/srv/operator/bin` (the operator's) or `/var/lib/svc` (the service's) lets every
/// operation through on the way to and below the granted directory, and nowhere else: not to
/// a sibling whose name only starts the same, not to a parent's other children.
#[test]
fn the_operators_and_the_services_directories_are_trusted_below_their_grants() {
    let (dir, _) = scratch("grants");
    let user = me(&dir).0;
    std::fs::create_dir_all(dir.path().join("srv/operator/bin")).unwrap();
    std::fs::create_dir_all(dir.path().join("var/lib/svc/nanohub")).unwrap();
    std::fs::create_dir_all(dir.path().join("var/lib/svc2")).unwrap();
    std::fs::write(dir.path().join("srv/operator/bin/nanohub"), b"binary").unwrap();
    std::fs::write(dir.path().join("srv/operator/notes"), b"not granted").unwrap();
    std::fs::write(dir.path().join("var/lib/svc/nanohub/db"), b"state").unwrap();
    std::fs::write(dir.path().join("var/lib/svc2/db"), b"other").unwrap();
    let not_trusted = |path: &str, at: &str| {
        format!("refused: {path}: {at} is owned by uid {user}, which is not trusted for this path")
    };
    let root = dir.path().display().to_string();

    let as_root = RealHost::trusting(dir.path(), &[]);
    for path in ["/srv/operator/bin/nanohub", "/var/lib/svc/nanohub/db"] {
        assert_eq!(
            as_root.read(path).unwrap_err().to_string(),
            not_trusted(path, &root)
        );
        assert_eq!(
            as_root.stat(path).unwrap_err().to_string(),
            not_trusted(path, &root)
        );
    }

    let granted = RealHost::trusting(dir.path(), &[])
        .trust_below("/srv/operator/bin", user)
        .trust_below("/var/lib/svc/", user);
    assert_eq!(
        granted.read("/srv/operator/bin/nanohub").unwrap().unwrap(),
        b"binary"
    );
    assert_eq!(
        granted.read("/var/lib/svc/nanohub/db").unwrap().unwrap(),
        b"state"
    );
    let state = granted.stat("/var/lib/svc").unwrap().unwrap();
    assert_eq!((state.kind, state.uid), (Kind::Dir, user));
    granted.remove("/var/lib/svc/nanohub/db").unwrap();
    assert!(!dir.path().join("var/lib/svc/nanohub/db").exists());

    // Nowhere else: a sibling with a longer name, another child of a granted path's parent,
    // and a granted path's parent itself.
    for path in ["/var/lib/svc2/db", "/srv/operator/notes", "/var/lib"] {
        assert_eq!(
            granted.read(path).unwrap_err().to_string(),
            not_trusted(path, &root),
            "{path}"
        );
    }
    // A grant whose directory has a `..` grants nothing.
    let dotted = RealHost::trusting(dir.path(), &[]).trust_below("/srv/operator/../operator/bin", user);
    assert_eq!(
        dotted.read("/srv/operator/bin/nanohub").unwrap_err().to_string(),
        not_trusted("/srv/operator/bin/nanohub", &root)
    );
}

/// Catches a failed write that leaves its temporary file behind (the next write would find
/// it, and a reader would see junk): nothing with the suffix is left after a rename that
/// fails.
#[test]
fn a_failed_write_leaves_no_temporary_file() {
    let (dir, host) = scratch("tmp");
    let owner = me(&dir);
    host.mkdir("/d", 0o700, owner).unwrap();
    std::fs::create_dir_all(dir.path().join("d/target/inside")).unwrap();
    assert!(host.write("/d/target", 0o600, owner, b"x").is_err());
    assert_eq!(names_in(&dir.path().join("d")), ["target"]);
    // A missing directory is a plain "not found", and removing from one is nothing to do.
    assert_eq!(
        host.write("/missing/f", 0o600, owner, b"x")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    host.remove("/missing/f").unwrap();
}

/// Catches a canonical path that does not resolve links on the way (a link into a work tree
/// would hide it) or that fails on a path that is not there yet: the existing part is resolved,
/// the rest is kept, and a file in the middle is an error.
#[test]
fn canonical_resolves_the_part_that_exists() {
    let (dir, host) = scratch("canon");
    std::fs::create_dir_all(dir.path().join("real/sub")).unwrap();
    symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
    assert_eq!(host.canonical("/link/sub").unwrap(), "/real/sub");
    assert_eq!(
        host.canonical("/link/sub/new/deeper").unwrap(),
        "/real/sub/new/deeper"
    );
    assert_eq!(host.canonical("/").unwrap(), "/");
    std::fs::write(dir.path().join("file"), b"x").unwrap();
    assert!(host.canonical("/file/x").is_err());
}

/// Catches system errors that are swallowed on the way: a name the file system cannot hold is
/// an error for mkdir and for write, and the error is the system's.
#[test]
fn system_errors_on_the_last_component_are_returned() {
    let (dir, host) = scratch("longname");
    let owner = me(&dir);
    let long = format!("/{}", "n".repeat(300));
    assert!(host.mkdir(&long, 0o700, owner).is_err());
    assert!(host.write(&long, 0o600, owner, b"x").is_err());
}

/// Catches a read or a stat that follows a link planted on the way: root would read (and
/// report on) whatever the service user points it at. Both are refused, for every way of
/// reading, and what is behind the link is untouched.
#[test]
fn stat_and_read_refuse_a_link_on_the_way() {
    let (dir, host) = scratch("readlink");
    let owner = me(&dir);
    let victim = dir.path().join("victim");
    std::fs::create_dir(&victim).unwrap();
    std::fs::write(victim.join("f"), b"behind the link").unwrap();
    host.mkdir("/state", 0o700, owner).unwrap();
    symlink(&victim, dir.path().join("state/sub")).unwrap();

    for path in ["/state/sub/f", "/state/sub/missing", "/state/sub/deeper/f"] {
        let want = format!("refused: {path}: sub is a symbolic link; give the real path");
        assert_eq!(host.stat(path).unwrap_err().to_string(), want);
        assert_eq!(host.read(path).unwrap_err().to_string(), want);
        assert_eq!(host.read_limited(path, 100).unwrap_err().to_string(), want);
    }
    assert_eq!(std::fs::read(victim.join("f")).unwrap(), b"behind the link");
}

/// Catches a read that follows a link as the file itself, and a stat that reports what the
/// link points at: a read of a final link is refused, and stat says the name is a link.
#[test]
fn a_final_link_is_not_read_and_stat_reports_it_as_a_link() {
    let (dir, host) = scratch("finallink");
    let owner = me(&dir);
    host.mkdir("/state", 0o700, owner).unwrap();
    std::fs::write(dir.path().join("state/real"), b"real").unwrap();
    symlink(dir.path().join("state/real"), dir.path().join("state/lnk")).unwrap();
    symlink(dir.path().join("state"), dir.path().join("state/dirlnk")).unwrap();

    assert_eq!(
        host.read("/state/lnk").unwrap_err().to_string(),
        "refused: /state/lnk: lnk is a symbolic link; give the real path"
    );
    assert_eq!(host.stat("/state/lnk").unwrap().unwrap().kind, Kind::Other);
    assert_eq!(
        host.stat("/state/dirlnk").unwrap().unwrap().kind,
        Kind::Other
    );
    assert_eq!(host.stat("/state/real").unwrap().unwrap().kind, Kind::File);
    assert_eq!(host.stat("/state").unwrap().unwrap().kind, Kind::Dir);
    assert_eq!(host.read("/state/real").unwrap().unwrap(), b"real");
    // Nothing there, and no directory there, are both "nothing".
    assert_eq!(host.stat("/state/none").unwrap(), None);
    assert_eq!(host.stat("/nodir/none").unwrap(), None);
    assert_eq!(host.read("/nodir/none").unwrap(), None);
    assert_eq!(host.read_limited("/state/none", 5).unwrap(), None);
}

/// Catches a read with no bound: a file above the limit is an error of kind `FileTooLarge`
/// after at most limit + 1 bytes, one exactly at the limit is read whole.
#[test]
fn read_limited_stops_above_the_limit() {
    let (dir, host) = scratch("limit");
    std::fs::write(dir.path().join("f"), b"0123456789").unwrap();
    assert_eq!(host.read_limited("/f", 10).unwrap().unwrap(), b"0123456789");
    let err = host.read_limited("/f", 9).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::FileTooLarge);
}

/// Catches a path the host layer lets through with a `.` or `..` component, whatever the
/// caller checked: each operation refuses it before touching anything.
#[test]
fn dot_and_dotdot_components_are_refused_by_every_operation() {
    let (dir, host) = scratch("dots");
    let owner = me(&dir);
    host.mkdir("/d", 0o700, owner).unwrap();
    std::fs::write(dir.path().join("outside"), b"x").unwrap();
    for path in ["/d/../outside", "/d/./f", "/../outside", "/d/.."] {
        for err in [
            host.stat(path).unwrap_err(),
            host.read(path).unwrap_err(),
            host.write(path, 0o600, owner, b"y").unwrap_err(),
            host.mkdir(path, 0o700, owner).unwrap_err(),
            host.remove(path).unwrap_err(),
        ] {
            assert_eq!(
                err.to_string(),
                format!("refused: {path} has a . or .. component")
            );
        }
    }
    assert!(dir.path().join("outside").exists());
}

/// Catches a root anchor that is followed: a host rooted at a link is refused for every
/// operation (the link is the thing a planted directory would be), and nothing is made
/// behind it.
#[test]
fn a_link_as_the_root_anchor_is_refused() {
    let (dir, _) = scratch("anchor");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let anchor = dir.path().join("anchor");
    symlink(&real, &anchor).unwrap();
    let host = RealHost::under(&anchor);
    let owner = me(&dir);
    assert_eq!(
        host.stat("/x").unwrap_err().to_string(),
        format!(
            "refused: /x: {} is a symbolic link; give the real path",
            anchor.display()
        )
    );
    assert!(host.read("/x").is_err());
    assert!(host.write("/x", 0o600, owner, b"y").is_err());
    assert!(host.mkdir("/x", 0o700, owner).is_err());
    assert!(host.remove("/x").is_err());
    assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0);
}

/// Catches a file named as a directory being called a link, or the other way round: the
/// message says "is not a directory" for a file and for the system's own error otherwise.
#[test]
fn a_file_where_a_directory_should_be_is_named_as_such() {
    let (dir, host) = scratch("notdir");
    std::fs::write(dir.path().join("file"), b"x").unwrap();
    let err = host.stat("/file/x").unwrap_err();
    assert_eq!(err.to_string(), "refused: /file/x: file is not a directory");
    assert!(host.read("/file/x").is_err());
}
