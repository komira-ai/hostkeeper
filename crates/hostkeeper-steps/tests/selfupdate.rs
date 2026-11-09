//! The updater's own update: stage beside, switch, restart; a binary that exits at start is
//! replaced by the previous one.

mod support;

use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::record::{HookSel, Loaded, Progress, RECORD_FILE, SCHEMA, State};
use hostkeeper_steps::selfupdate::{
    FollowOn, Layout, START_LIMIT_BURST, SelfUpdateError, fallback_unit, follow_on, main_dropin,
    settle,
};
use hostkeeper_steps::{ArtifactError, Finished, Record, RecordStore, Settled, Staged};
use support::Scratch;

fn binary(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, text).unwrap();
    path
}

/// The updater binary of a release, in its own staged tree.
const UPDATER: &str = "bin/updater";

/// A release tree under `dir` whose updater binary is a copy of `from`, sealed.
fn release(dir: &Path, name: &str, from: &Path) -> Staged {
    let tree = dir.join(format!("release-{name}"));
    fs::create_dir_all(tree.join("bin")).unwrap();
    fs::copy(from, tree.join(UPDATER)).unwrap();
    Staged::seal(&tree).unwrap()
}

fn done(verdict: Verdict) -> Finished {
    Finished {
        verdict,
        reason: None,
    }
}

fn ok() -> Finished {
    done(Verdict::Succeeded {
        promotion_pending: false,
    })
}

/// The step record of the updater's own update to `version` whose steps ran, as the runner leaves
/// it when it holds the success back: `done` at `cleanup`, unfinished.
fn steps_ran(version: &str) -> Record {
    Record {
        schema: SCHEMA,
        service: "hostkeeper".to_owned(),
        target_version: version.to_owned(),
        artifact_digest: "ab".repeat(32),
        tarball_digest: "cd".repeat(32),
        previous_digest: Some("ef".repeat(32)),
        generation: 1,
        params: "/state/hostkeeper/params.toml".into(),
        step: Step::Cleanup,
        attempt: 1,
        state: State::Done {
            code: Some(0),
            last_line: None,
        },
        hook: HookSel::New,
        progress: Progress {
            committed: true,
            ..Progress::default()
        },
        order_try: Some(1),
        forced: false,
        force: None,
    }
}

/// The step record store of the scratch directory `s` (`<s>/state`), holding the updater's own
/// update to `version` whose steps ran ([`steps_ran`]).
fn updating(s: &Scratch, version: &str) -> RecordStore {
    let dir = s.path().join("state");
    fs::create_dir_all(&dir).unwrap();
    let store = RecordStore::new(&dir);
    store.save(&steps_ran(version)).unwrap();
    store
}

/// Catches a staged binary that is not whole, not executable, or that moves a link: staging puts
/// the file in `versions/` with mode 0755 and changes neither `current` nor `previous`; a version
/// that is not a plain file name is refused.
#[test]
fn staging_adds_a_version_and_moves_no_link() {
    let s = Scratch::new("su-stage");
    let layout = Layout::new(&s.path().join("bin"));
    let src = binary(s.path(), "new", "#!new\n");
    layout.stage("1.2.3", &src).unwrap();
    let staged = layout.dir().join("versions/1.2.3");
    assert_eq!(fs::read_to_string(&staged).unwrap(), "#!new\n");
    assert_eq!(
        fs::metadata(&staged).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(layout.current().unwrap(), None);
    assert_eq!(layout.previous().unwrap(), None);
    assert!(!layout.dir().join("versions/.1.2.3.stage").exists());
    for bad in ["", ".hidden", "a/b", "a b", "../x", "é"] {
        let err = layout.stage(bad, &src).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
    }
    assert!(layout.stage("1.2.4", &s.path().join("absent")).is_err());
}

/// Catches a `next` that points at what no hand-over can start, or that moves another link: only a
/// staged executable binary of the binary directory is linked, a repeat changes nothing, and
/// `current` and `previous` stay as they were.
#[test]
fn next_points_only_at_a_staged_binary_of_the_binary_directory() {
    let s = Scratch::new("su-next");
    let layout = Layout::new(&s.path().join("bin"));
    layout.stage("1.0.0", &binary(s.path(), "a", "a")).unwrap();
    assert_eq!(layout.next().unwrap(), None);
    assert_eq!(
        layout.set_next("1.1.0").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        layout.set_next("a/b").unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(layout.next().unwrap(), None);
    layout.set_next("1.0.0").unwrap();
    layout.set_next("1.0.0").unwrap();
    assert_eq!(layout.next().unwrap(), Some("1.0.0".to_owned()));
    assert_eq!(links(&layout), (None, None));
    layout.remove_next().unwrap();
    assert_eq!(layout.next().unwrap(), None);
    let packages = Layout::package(&s.path().join("packages"));
    assert_eq!(
        packages.set_next("1.0.0").unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

/// Catches a switch that loses the way back: `previous` becomes what `current` was, a switch to
/// the version already current changes nothing (so a repeat after a crash keeps the real previous
/// one), and a version that was never staged is refused.
#[test]
fn a_switch_remembers_the_previous_binary_and_is_idempotent() {
    let s = Scratch::new("su-switch");
    let layout = Layout::new(&s.path().join("bin"));
    layout.stage("v1", &binary(s.path(), "a", "a")).unwrap();
    layout.stage("v2", &binary(s.path(), "b", "b")).unwrap();
    layout.switch("v1").unwrap();
    assert_eq!(
        (layout.current().unwrap(), layout.previous().unwrap()),
        (Some("v1".into()), None)
    );
    layout.switch("v2").unwrap();
    layout.switch("v2").unwrap();
    assert_eq!(
        (layout.current().unwrap(), layout.previous().unwrap()),
        (Some("v2".into()), Some("v1".into()))
    );
    assert_eq!(fs::read_to_string(layout.current_path()).unwrap(), "b");
    assert_eq!(fs::read_to_string(layout.previous_path()).unwrap(), "a");
    assert!(layout.switch("v3").is_err());
    assert_eq!(
        layout.switch("../v1").unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

/// Catches a flip back that does nothing, or flips twice: with no previous binary it reports
/// `false`; after a switch it points `current` at the previous one once.
#[test]
fn flipping_back_restores_the_previous_binary_once() {
    let s = Scratch::new("su-flip");
    let layout = Layout::new(&s.path().join("bin"));
    assert!(!layout.flip_back().unwrap());
    layout.stage("v1", &binary(s.path(), "a", "a")).unwrap();
    layout.stage("v2", &binary(s.path(), "b", "b")).unwrap();
    layout.switch("v1").unwrap();
    layout.switch("v2").unwrap();
    assert!(layout.flip_back().unwrap());
    assert_eq!(layout.current().unwrap(), Some("v1".to_owned()));
    assert!(!layout.flip_back().unwrap());
}

/// Catches a link move that trips on what a crash left: a temporary link from an earlier attempt
/// is replaced, and one that cannot be removed is an error rather than a silent skip; a `current`
/// that is not a link is an error too.
#[test]
fn leftovers_of_a_crash_are_replaced_and_odd_files_refused() {
    let s = Scratch::new("su-leftover");
    let layout = Layout::new(&s.path().join("bin"));
    layout.stage("v1", &binary(s.path(), "a", "a")).unwrap();
    fs::write(layout.dir().join("current.tmp"), "stale").unwrap();
    layout.switch("v1").unwrap();
    assert_eq!(layout.current().unwrap(), Some("v1".to_owned()));

    layout.stage("v2", &binary(s.path(), "b", "b")).unwrap();
    fs::create_dir(layout.dir().join("previous.tmp")).unwrap();
    assert!(layout.switch("v2").is_err());

    let other = Layout::new(&s.path().join("odd"));
    fs::create_dir_all(other.dir()).unwrap();
    fs::write(other.current_path(), "not a link").unwrap();
    assert!(other.current().is_err());
}

/// Catches a follow-on that restarts when it should not or not when it should: only a succeeded
/// update that is not already this version stages, points `next` and asks for the restart, and
/// moves no `current`; a repeat after a crash gives the same state; a failed update, or the new
/// binary started, does nothing.
#[test]
fn the_follow_on_restarts_only_after_a_success_that_changes_the_binary() {
    let s = Scratch::new("su-follow");
    let layout = Layout::new(&s.path().join("bin"));
    let v1 = binary(s.path(), "a", "a");
    let v2 = release(s.path(), "v2", &binary(s.path(), "b", "b"));
    let updater = Path::new(UPDATER);
    layout.stage("v1", &v1).unwrap();
    layout.switch("v1").unwrap();

    for verdict in [
        Verdict::Failed {
            phase: Phase::GateRelease,
            class: Class::Release,
        },
        Verdict::Deferred {
            retry_after_secs: None,
        },
        Verdict::Halt,
    ] {
        assert_eq!(
            follow_on(
                &done(verdict),
                &updating(&s, "v2"),
                &layout,
                "v1",
                "v2",
                &v2,
                updater,
                None
            )
            .unwrap(),
            FollowOn::NotNeeded
        );
        assert_eq!(layout.current().unwrap(), Some("v1".to_owned()));
    }
    assert_eq!(
        follow_on(
            &ok(),
            &updating(&s, "v2"),
            &layout,
            "v2",
            "v2",
            &v2,
            updater,
            None
        )
        .unwrap(),
        FollowOn::NotNeeded
    );
    assert!(!layout.dir().join("versions/v2").exists());

    assert_eq!(
        follow_on(
            &ok(),
            &updating(&s, "v2"),
            &layout,
            "v1",
            "v2",
            &v2,
            updater,
            None
        )
        .unwrap(),
        FollowOn::Restart
    );
    assert_eq!(
        follow_on(
            &ok(),
            &updating(&s, "v2"),
            &layout,
            "v1",
            "v2",
            &v2,
            updater,
            None
        )
        .unwrap(),
        FollowOn::Restart
    );
    assert_eq!(
        (
            layout.current().unwrap(),
            layout.previous().unwrap(),
            layout.next().unwrap()
        ),
        (Some("v1".into()), None, Some("v2".into()))
    );
    let pending = done(Verdict::Succeeded {
        promotion_pending: true,
    });
    assert_eq!(
        follow_on(
            &pending,
            &updating(&s, "v2"),
            &layout,
            "v2",
            "v2",
            &v2,
            updater,
            None
        )
        .unwrap(),
        FollowOn::NotNeeded
    );
    assert!(
        follow_on(
            &ok(),
            &updating(&s, "v3"),
            &layout,
            "v2",
            "v3",
            &v2,
            Path::new("bin/absent"),
            None
        )
        .is_err()
    );
}

fn system_binary(name: &str) -> PathBuf {
    ["/bin", "/usr/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|p| p.exists())
        .expect("a system binary")
}

/// Catches a fallback that leaves the host with a broken updater: a binary that exits at
/// start fails `START_LIMIT_BURST` starts of the link the unit runs; the flip the fallback unit
/// makes puts the previous binary back, and the next start succeeds.
#[test]
fn a_binary_that_exits_at_start_is_replaced_by_the_previous() {
    let s = Scratch::new("su-exits-at-start");
    let layout = Layout::new(&s.path().join("bin"));
    layout.stage("good", &system_binary("true")).unwrap();
    layout.switch("good").unwrap();
    let ran = Command::new(layout.current_path()).status().unwrap();
    assert!(ran.success());

    // The old way, which the fallback unit of a release before the hand-over still ran this
    // verb for: the new binary is switched to before it has run.
    layout.stage("bad", &system_binary("false")).unwrap();
    layout.switch("bad").unwrap();
    for _ in 0..START_LIMIT_BURST {
        let started = Command::new(layout.current_path()).status().unwrap();
        assert!(!started.success());
    }
    assert!(layout.flip_back().unwrap());
    assert!(
        Command::new(layout.current_path())
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(layout.current().unwrap(), Some("good".to_owned()));
}

/// Catches the unit texts drifting from what the fallback needs: the drop-in restarts the unit and
/// names the fallback after the burst; the fallback runs the previous binary's verb, then starts
/// the main unit again.
#[test]
fn the_units_run_the_fallback_after_the_burst() {
    assert_eq!(
        main_dropin("updater-fallback.service", START_LIMIT_BURST),
        "[Unit]\nOnFailure=updater-fallback.service\nStartLimitIntervalSec=300\nStartLimitBurst=3\n\n[Service]\nRestart=always\nRestartSec=1\nRestartMode=direct\n"
    );
    let layout = Layout::new(Path::new("/usr/libexec/kbf-updater"));
    assert_eq!(
        fallback_unit(&layout, "kbf-updater.service"),
        "[Unit]\nDescription=Put the previous updater binary back after the new one failed to start\nStartLimitIntervalSec=0\n\n[Service]\nType=oneshot\nExecStart=-/usr/libexec/kbf-updater/previous self-update-fallback --dir /usr/libexec/kbf-updater\nExecStartPost=/usr/bin/systemctl reset-failed kbf-updater.service\nExecStartPost=/usr/bin/systemctl start --no-block kbf-updater.service\n"
    );
}

fn links(layout: &Layout) -> (Option<String>, Option<String>) {
    (layout.current().unwrap(), layout.previous().unwrap())
}

fn version_text(layout: &Layout, version: &str) -> String {
    fs::read_to_string(layout.dir().join("versions").join(version)).unwrap()
}

/// Catches a version the unit runs, or falls back to, being staged over (a crash between that
/// staging and the switch left the fallback replaced by a binary nothing had verified): staging
/// the version `current` or `previous` points at is refused and its file is left as it was; a
/// version neither names may be staged again.
#[test]
fn the_versions_current_and_previous_point_at_are_not_staged_over() {
    let s = Scratch::new("su-live");
    let layout = Layout::new(&s.path().join("bin"));
    layout.stage("v1", &binary(s.path(), "a", "a")).unwrap();
    layout.stage("v2", &binary(s.path(), "b", "b")).unwrap();
    layout.switch("v1").unwrap();
    layout.switch("v2").unwrap();
    let other = binary(s.path(), "c", "c");
    for live in ["v1", "v2"] {
        let err = layout.stage(live, &other).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{live}");
        assert!(err.to_string().contains("current"), "{err}");
        assert!(
            !layout
                .dir()
                .join(format!("versions/.{live}.stage"))
                .exists()
        );
    }
    assert_eq!(version_text(&layout, "v1"), "a");
    assert_eq!(version_text(&layout, "v2"), "b");
    assert_eq!(links(&layout), (Some("v2".into()), Some("v1".into())));
    layout.stage("v3", &other).unwrap();
    layout.stage("v3", &binary(s.path(), "d", "d")).unwrap();
    assert_eq!(version_text(&layout, "v3"), "d");
}

/// Catches a flip back to a binary that is not there (the unit would then run nothing): with
/// `versions/<previous>` missing, `flip_back` is an error and `current` is not changed.
#[test]
fn flipping_back_to_a_missing_binary_is_refused() {
    let s = Scratch::new("su-flip-missing");
    let layout = Layout::new(&s.path().join("bin"));
    layout.stage("v1", &binary(s.path(), "a", "a")).unwrap();
    layout.stage("v2", &binary(s.path(), "b", "b")).unwrap();
    layout.switch("v1").unwrap();
    layout.switch("v2").unwrap();
    fs::remove_file(layout.dir().join("versions/v1")).unwrap();
    let err = layout.flip_back().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert_eq!(links(&layout), (Some("v2".into()), Some("v1".into())));
    assert_eq!(fs::read_to_string(layout.current_path()).unwrap(), "b");
}

/// Catches a follow-on that stages a file nothing verified: the binary is taken from the release
/// only through `Staged::check`, so a release changed since it was sealed, and a binary path
/// that leaves the tree, stage nothing and move no link.
#[test]
fn the_follow_on_stages_only_a_binary_of_the_verified_release() {
    let s = Scratch::new("su-follow-verified");
    let layout = Layout::new(&s.path().join("bin"));
    layout.stage("v1", &binary(s.path(), "a", "a")).unwrap();
    layout.switch("v1").unwrap();

    let changed = release(s.path(), "v2", &binary(s.path(), "b", "b"));
    fs::write(changed.dir().join(UPDATER), "swapped").unwrap();
    let err = follow_on(
        &ok(),
        &updating(&s, "v2"),
        &layout,
        "v1",
        "v2",
        &changed,
        Path::new(UPDATER),
        None,
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            SelfUpdateError::Artifact(ArtifactError::Changed { .. })
        ),
        "{err:?}"
    );

    let sound = release(s.path(), "v3", &binary(s.path(), "c", "c"));
    let outside = system_binary("true");
    for path in [Path::new("../release-v2/bin/updater"), outside.as_path()] {
        let err = follow_on(
            &ok(),
            &updating(&s, "v3"),
            &layout,
            "v1",
            "v3",
            &sound,
            path,
            None,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                SelfUpdateError::Artifact(ArtifactError::BadHookPath(_))
            ),
            "{err:?}"
        );
    }
    assert!(!layout.dir().join("versions/v2").exists());
    assert!(!layout.dir().join("versions/v3").exists());
    assert_eq!(links(&layout), (Some("v1".into()), None));
    assert_eq!(layout.next().unwrap(), None);
}

/// Catches a follow-on to a version already in place copying over it, or never getting there: when
/// `current` or `previous` already names the version (a return to the binary before), `next` is
/// pointed at it without staging it again if its file holds the verified binary's bytes, and the
/// follow-on is refused with nothing changed if it does not.
#[test]
fn the_follow_on_to_a_version_in_place_stages_nothing() {
    let s = Scratch::new("su-follow-in-place");
    let layout = Layout::new(&s.path().join("bin"));
    let a = binary(s.path(), "a", "a");
    layout.stage("v1", &a).unwrap();
    layout.switch("v1").unwrap();
    let updater = Path::new(UPDATER);
    let v2 = release(s.path(), "v2", &binary(s.path(), "b", "b"));
    assert_eq!(
        follow_on(
            &ok(),
            &updating(&s, "v2"),
            &layout,
            "v1",
            "v2",
            &v2,
            updater,
            None
        )
        .unwrap(),
        FollowOn::Restart
    );
    // The start of v2 finishes the switch, as its settlement does.
    layout.switch("v2").unwrap();
    // Back to the binary before, which `previous` names.
    let v1 = release(s.path(), "v1", &a);
    assert_eq!(
        follow_on(
            &ok(),
            &updating(&s, "v1"),
            &layout,
            "v2",
            "v1",
            &v1,
            updater,
            None
        )
        .unwrap(),
        FollowOn::Restart
    );
    assert_eq!(links(&layout), (Some("v2".into()), Some("v1".into())));
    assert_eq!(layout.next().unwrap(), Some("v1".into()));
    assert_eq!(version_text(&layout, "v1"), "a");
    // A release that calls itself v2 with other bytes than the v2 in place.
    let not_v2 = release(s.path(), "not-v2", &binary(s.path(), "z", "z"));
    let err = follow_on(
        &ok(),
        &updating(&s, "v2"),
        &layout,
        "v1",
        "v2",
        &not_v2,
        updater,
        None,
    )
    .unwrap_err();
    assert!(
        matches!(&err, SelfUpdateError::Io(e) if e.kind() == io::ErrorKind::AlreadyExists),
        "{err:?}"
    );
    assert_eq!(version_text(&layout, "v2"), "b");
    assert_eq!(links(&layout), (Some("v2".into()), Some("v1".into())));
    assert_eq!(layout.next().unwrap(), Some("v1".into()));
}

/// The file of a version, its inode and its bytes, to see that it was not written over.
fn file_of(layout: &Layout, version: &str) -> (u64, String) {
    use std::os::unix::fs::MetadataExt;
    let path = layout.dir().join("versions").join(version);
    let ino = fs::symlink_metadata(&path).unwrap().ino();
    (ino, fs::read_to_string(&path).unwrap())
}

/// Catches a staging copy made under a name a version can have: `v1.tmp` is a valid version,
/// and when it is the one `current` or `previous` points at, staging `v1` wrote its copy over
/// that file and renamed it away, leaving the link dangling. The copy is made under a name no
/// version has, so the live binary and both links stay as they were; a copy a crash left is
/// replaced, and a directory in its place is an error.
#[test]
fn staging_one_version_never_writes_over_another() {
    for live in ["current", "previous"] {
        let s = Scratch::new(&format!("su-stage-name-{live}"));
        let layout = Layout::new(&s.path().join("bin"));
        layout.stage("v1.tmp", &binary(s.path(), "a", "a")).unwrap();
        layout.stage("v0", &binary(s.path(), "z", "z")).unwrap();
        layout.switch("v1.tmp").unwrap();
        if live == "previous" {
            layout.switch("v0").unwrap();
        }
        let before = (links(&layout), file_of(&layout, "v1.tmp"));
        layout.stage("v1", &binary(s.path(), "b", "b")).unwrap();
        assert_eq!(
            (links(&layout), file_of(&layout, "v1.tmp")),
            before,
            "{live}"
        );
        assert_eq!(version_text(&layout, "v1"), "b");
        assert_eq!(fs::read_to_string(layout.dir().join(live)).unwrap(), "a");
    }

    let s = Scratch::new("su-stage-leftover");
    let layout = Layout::new(&s.path().join("bin"));
    fs::create_dir_all(layout.dir().join("versions")).unwrap();
    let leftover = layout.dir().join("versions/.v2.stage");
    fs::write(&leftover, "half a binary").unwrap();
    layout.stage("v2", &binary(s.path(), "b", "b")).unwrap();
    assert_eq!(version_text(&layout, "v2"), "b");
    assert!(!leftover.exists());
    fs::create_dir(layout.dir().join("versions/.v3.stage")).unwrap();
    assert!(layout.stage("v3", &binary(s.path(), "c", "c")).is_err());
    assert!(!layout.dir().join("versions/v3").exists());
}

/// Catches a fallback that is not a binary being taken for one: with `versions/<previous>` a
/// dangling link, a link to a binary, a directory or a file without an execute bit, `flip_back`
/// and `switch` are errors and change no link; and a missing fallback is an error even when
/// `current` already points at it (the fallback unit would otherwise report a flip it cannot
/// run as nothing to do).
#[test]
fn a_fallback_that_is_not_an_executable_file_is_refused() {
    let s = Scratch::new("su-flip-odd");
    let layout = Layout::new(&s.path().join("bin"));
    let a = binary(s.path(), "a", "a");
    layout.stage("v1", &a).unwrap();
    layout.stage("v2", &binary(s.path(), "b", "b")).unwrap();
    layout.switch("v1").unwrap();
    layout.switch("v2").unwrap();
    let v1 = layout.dir().join("versions/v1");
    let plain = s.path().join("plain");
    fs::write(&plain, "p").unwrap();
    type Make = Box<dyn Fn(&Path)>;
    let odd: [(&str, Make); 4] = [
        (
            "a dangling link",
            Box::new(|p: &Path| symlink("nowhere", p).unwrap()),
        ),
        (
            "a link to a binary",
            Box::new(move |p: &Path| symlink(&a, p).unwrap()),
        ),
        (
            "a directory",
            Box::new(|p: &Path| fs::create_dir(p).unwrap()),
        ),
        (
            "a file nobody may execute",
            Box::new(move |p: &Path| {
                fs::copy(&plain, p).unwrap();
                fs::set_permissions(p, fs::Permissions::from_mode(0o644)).unwrap();
            }),
        ),
    ];
    for (what, make) in odd {
        fs::remove_file(&v1)
            .or_else(|_| fs::remove_dir(&v1))
            .unwrap();
        make(&v1);
        let err = layout.flip_back().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{what}");
        assert_eq!(
            links(&layout),
            (Some("v2".into()), Some("v1".into())),
            "{what}"
        );
        assert!(layout.switch("v1").is_err(), "{what}");
        assert_eq!(
            links(&layout),
            (Some("v2".into()), Some("v1".into())),
            "{what}"
        );
    }

    // `current` and `previous` both name v3, whose file is gone.
    let s = Scratch::new("su-flip-same-missing");
    let layout = Layout::new(&s.path().join("bin"));
    layout.stage("v3", &binary(s.path(), "c", "c")).unwrap();
    layout.switch("v3").unwrap();
    symlink("versions/v3", layout.dir().join("previous")).unwrap();
    assert_eq!(links(&layout), (Some("v3".into()), Some("v3".into())));
    fs::remove_file(layout.dir().join("versions/v3")).unwrap();
    let err = layout.flip_back().unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert_eq!(links(&layout), (Some("v3".into()), Some("v3".into())));
}

/// Catches the wait for the restart losing the order it ran for, or a record of a daemon from
/// before the id that no longer reads: the follow-on of an order with an id keeps it in the wait
/// (`order_id`), one with none writes the wait without the key, byte for byte as before, and a
/// wait written without it loads with no id.
#[test]
fn the_wait_for_the_restart_keeps_the_order_id_it_was_given() {
    let s = Scratch::new("su-follow-order-id");
    let layout = Layout::new(&s.path().join("bin"));
    layout.stage("v1", &binary(s.path(), "a", "a")).unwrap();
    layout.switch("v1").unwrap();
    let v2 = release(s.path(), "v2", &binary(s.path(), "b", "b"));
    let store = updating(&s, "v2");
    let followed = follow_on(
        &ok(),
        &store,
        &layout,
        "v1",
        "v2",
        &v2,
        Path::new(UPDATER),
        Some("update-0123abcd"),
    );
    assert_eq!(followed.unwrap(), FollowOn::Restart);
    let Loaded::Current(rec) = store.load().unwrap() else {
        panic!("no record");
    };
    let awaiting = rec.progress.awaiting_restart.expect("the wait");
    assert_eq!(
        (awaiting.version.as_str(), awaiting.order_id.as_deref()),
        ("v2", Some("update-0123abcd"))
    );

    let s = Scratch::new("su-follow-no-order-id");
    let layout = Layout::new(&s.path().join("bin"));
    layout.stage("v1", &binary(s.path(), "a", "a")).unwrap();
    layout.switch("v1").unwrap();
    let v2 = release(s.path(), "v2", &binary(s.path(), "b", "b"));
    let store = updating(&s, "v2");
    let followed = follow_on(
        &ok(),
        &store,
        &layout,
        "v1",
        "v2",
        &v2,
        Path::new(UPDATER),
        None,
    );
    assert_eq!(followed.unwrap(), FollowOn::Restart);
    let text = fs::read_to_string(store.dir().join(RECORD_FILE)).unwrap();
    assert!(text.contains("\"awaiting_restart\""), "{text}");
    assert!(!text.contains("order_id"), "{text}");
    let Loaded::Current(rec) = store.load().unwrap() else {
        panic!("no record");
    };
    assert_eq!(rec.progress.awaiting_restart.and_then(|a| a.order_id), None);
}

/// Catches a settlement that drops the order the update ran for, an acknowledgement that clears
/// another order's verdict, and a record that writes the new key when it has nothing to report:
/// a confirmed and a fallen-back settlement each keep the wait's order id as the one to report;
/// an acknowledgement of another id or another try leaves it, the right one clears it (and
/// writes the record without the key), and a second one writes nothing; a wait with no id
/// settles with none to report.
#[test]
fn a_settlement_keeps_the_order_to_report_until_its_verdict_is_acknowledged() {
    for (running, outcome) in [("v2", Settled::Confirmed), ("v1", Settled::FellBack)] {
        let s = Scratch::new(&format!("su-report-{running}"));
        let layout = Layout::new(&s.path().join("bin"));
        layout.stage("v1", &binary(s.path(), "a", "a")).unwrap();
        layout.switch("v1").unwrap();
        let v2 = release(s.path(), "v2", &binary(s.path(), "b", "b"));
        let store = updating(&s, "v2");
        let id = Some("update-0123abcd");
        follow_on(
            &ok(),
            &store,
            &layout,
            "v1",
            "v2",
            &v2,
            Path::new(UPDATER),
            id,
        )
        .unwrap();
        if running == "v1" {
            layout.flip_back().unwrap();
        }
        assert_eq!(settle(&layout, &store, running).unwrap(), outcome);
        let reported = |store: &RecordStore| match store.load().unwrap() {
            Loaded::Current(rec) => rec.progress.report_order_id,
            _ => panic!("no record"),
        };
        assert_eq!(reported(&store).as_deref(), id, "{running}");

        assert!(!store.verdict_acknowledged("update-other", 1).unwrap());
        assert!(!store.verdict_acknowledged("update-0123abcd", 2).unwrap());
        assert_eq!(reported(&store).as_deref(), id, "{running}");
        assert!(store.verdict_acknowledged("update-0123abcd", 1).unwrap());
        assert_eq!(reported(&store), None, "{running}");
        let text = fs::read_to_string(store.dir().join(RECORD_FILE)).unwrap();
        assert!(!text.contains("report_order_id"), "{text}");
        assert!(!store.verdict_acknowledged("update-0123abcd", 1).unwrap());
    }

    let s = Scratch::new("su-report-none");
    let layout = Layout::new(&s.path().join("bin"));
    layout.stage("v1", &binary(s.path(), "a", "a")).unwrap();
    layout.switch("v1").unwrap();
    let v2 = release(s.path(), "v2", &binary(s.path(), "b", "b"));
    let store = updating(&s, "v2");
    follow_on(
        &ok(),
        &store,
        &layout,
        "v1",
        "v2",
        &v2,
        Path::new(UPDATER),
        None,
    )
    .unwrap();
    assert_eq!(settle(&layout, &store, "v2").unwrap(), Settled::Confirmed);
    let Loaded::Current(rec) = store.load().unwrap() else {
        panic!("no record");
    };
    assert_eq!(rec.progress.report_order_id, None);
    assert!(!store.verdict_acknowledged("update-0123abcd", 1).unwrap());
    // No record at all: nothing to acknowledge.
    let empty = s.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    assert!(
        !RecordStore::new(&empty)
            .verdict_acknowledged("x", 1)
            .unwrap()
    );
}
