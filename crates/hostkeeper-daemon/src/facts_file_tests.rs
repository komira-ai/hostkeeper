//! The tests of `facts_file`: the host's facts written to a file in a scratch directory, owned by
//! the test's own user, with no real `/var/lib`.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use hostkeeper_link::testing::RecordingLog;
use hostkeeper_testkit::Scratch;

use super::*;

/// A scratch directory under the target directory's `tmp`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// The effective uid of the test.
fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn facts(generation: u64, rack: &str) -> PushedFacts {
    PushedFacts {
        generation,
        host_id: "web-04".to_owned(),
        host_group: "web".to_owned(),
        labels: BTreeMap::from([
            ("rack".to_owned(), rack.to_owned()),
            ("az".to_owned(), "b".to_owned()),
        ]),
    }
}

/// A facts file at `path` with a log the test reads.
fn file_at(path: &Path) -> (FactsFile, Arc<RecordingLog>) {
    let log = Arc::new(RecordingLog::default());
    let sink = FactsFile::new(path.to_owned(), Platform::LinuxX86_64, log.clone());
    (sink, log)
}

/// The inode of the file at `path`: a new one after each write, since a write renames a new file
/// over it.
fn inode(path: &Path) -> u64 {
    fs::metadata(path).expect("the facts file").ino()
}

/// Proves the file's bytes: the fields in the design's order, the platform the daemon names, the
/// labels by key, and a newline; a string that JSON must escape is escaped, and a host with no
/// labels has an empty object.
#[test]
fn the_file_is_the_schema_1_object_of_the_design() {
    assert_eq!(
        String::from_utf8(render(&facts(3, "r12"), Platform::LinuxX86_64)).expect("text"),
        "{\"schema\":1,\"host_id\":\"web-04\",\"host_group\":\"web\",\"platform\":\"linux-x86_64\",\"labels\":{\"az\":\"b\",\"rack\":\"r12\"}}\n"
    );
    let odd = PushedFacts {
        generation: 1,
        host_id: "a\"b\\c".to_owned(),
        host_group: "g\n".to_owned(),
        labels: BTreeMap::new(),
    };
    assert_eq!(
        String::from_utf8(render(&odd, Platform::DarwinArm64)).expect("text"),
        "{\"schema\":1,\"host_id\":\"a\\\"b\\\\c\",\"host_group\":\"g\\n\",\"platform\":\"darwin-arm64\",\"labels\":{}}\n"
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&render(&odd, Platform::LinuxArm64)).expect("JSON");
    assert_eq!(parsed["host_id"], "a\"b\\c");
    assert_eq!(parsed["platform"], "linux-arm64");
}

/// Proves the first facts are written whole, mode 0644, owned by the daemon's user, in a
/// directory the write makes (0755) when it is missing, leaving no temporary file; the sink
/// reports no generation before and the generation it wrote after, and says so in its log.
#[test]
fn facts_are_written_whole_with_mode_0644_and_reported() {
    let scratch = scratch("facts-write");
    let path = scratch.path().join("var/lib/hostkeeper/facts.json");
    let (sink, log) = file_at(&path);
    assert_eq!(sink.generation(), None);
    assert!(sink.apply(&facts(5, "r12")));
    assert_eq!(sink.generation(), Some(5));
    assert_eq!(
        fs::read(&path).expect("the file"),
        render(&facts(5, "r12"), Platform::LinuxX86_64)
    );
    let meta = fs::metadata(&path).expect("the file");
    assert_eq!(meta.permissions().mode() & 0o7777, 0o644);
    assert_eq!(meta.uid(), me());
    let dir = fs::metadata(path.parent().expect("a parent")).expect("the directory");
    assert_eq!(dir.permissions().mode() & 0o7777, 0o755);
    let names: Vec<_> = fs::read_dir(path.parent().expect("a parent"))
        .expect("the directory")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(names, ["facts.json"], "no temporary file is left");
    assert_eq!(
        log.lines(),
        [format!("facts generation 5: written to {}", path.display())]
    );
}

/// Proves the file is rewritten only when its content changes: facts sent again with another
/// generation and the same values leave the file as it is (the same inode, nothing logged) while
/// the sink reports the new generation; changed values replace it.
#[test]
fn the_file_is_rewritten_only_when_its_content_changes() {
    let scratch = scratch("facts-unchanged");
    let path = scratch.path().join("facts.json");
    let (sink, log) = file_at(&path);
    assert!(sink.apply(&facts(1, "r12")));
    let first = inode(&path);
    assert!(sink.apply(&facts(2, "r12")));
    assert_eq!(inode(&path), first, "the same content is not written again");
    assert_eq!(sink.generation(), Some(2));
    assert_eq!(log.lines().len(), 1);

    assert!(sink.apply(&facts(3, "r13")));
    assert_ne!(inode(&path), first);
    assert_eq!(
        fs::read(&path).expect("the file"),
        render(&facts(3, "r13"), Platform::LinuxX86_64)
    );
    assert_eq!(sink.generation(), Some(3));
    assert_eq!(
        log.lines()[1],
        format!("facts generation 3: written to {}", path.display())
    );
}

/// Proves a daemon that starts beside a file already holding the facts writes nothing when they
/// are sent again: it reports no generation until then, and the generation sent after.
#[test]
fn a_restarted_daemon_finds_the_facts_already_written() {
    let scratch = scratch("facts-restart");
    let path = scratch.path().join("facts.json");
    let (before, _) = file_at(&path);
    assert!(before.apply(&facts(4, "r12")));
    let first = inode(&path);
    let (after, log) = file_at(&path);
    assert_eq!(after.generation(), None);
    assert!(after.apply(&facts(9, "r12")));
    assert_eq!(after.generation(), Some(9));
    assert_eq!(inode(&path), first);
    assert!(log.lines().is_empty());
}

/// Proves a file with the same bytes but another mode, or owned by another user than the one the
/// sink writes as, is replaced, so the file always ends 0644 and the daemon's.
#[test]
fn a_file_of_the_wrong_mode_or_owner_is_replaced() {
    let scratch = scratch("facts-mode");
    let path = scratch.path().join("facts.json");
    let (sink, log) = file_at(&path);
    assert!(sink.apply(&facts(1, "r12")));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    let first = inode(&path);
    assert!(sink.apply(&facts(1, "r12")));
    assert_ne!(inode(&path), first);
    assert_eq!(
        fs::metadata(&path).expect("the file").permissions().mode() & 0o7777,
        0o644
    );
    assert_eq!(log.lines().len(), 2);

    let other = FactsFile::owned_by(
        path.clone(),
        Platform::LinuxX86_64,
        me().wrapping_add(1),
        log.clone(),
    );
    let second = inode(&path);
    assert!(other.apply(&facts(1, "r12")));
    assert_ne!(inode(&path), second, "a file another user owns is not left");
}

/// Proves something other than a file at the path is not replaced: the write fails with a line
/// naming the generation, the path and why, and the sink keeps reporting the generation before.
#[test]
fn a_directory_at_the_path_is_refused_and_the_generation_stays() {
    let scratch = scratch("facts-dir");
    let path = scratch.path().join("facts.json");
    let (sink, log) = file_at(&path);
    fs::create_dir(&path).expect("a directory where the file goes");
    assert!(!sink.apply(&facts(2, "r12")));
    assert_eq!(sink.generation(), None);
    assert_eq!(
        log.lines(),
        [format!(
            "error: facts generation 2 is not written to {}: something other than a file is at its path",
            path.display()
        )]
    );
    assert!(path.is_dir());
}

/// Proves a write that fails on the filesystem (the directory cannot be made, a file being where
/// it goes) is logged with the system's reason and leaves the generation the sink reported, so
/// the coordinator sends the facts again; the next write that can succeed does.
#[test]
fn a_write_that_fails_keeps_the_generation_before() {
    let scratch = scratch("facts-io");
    let blocker = scratch.path().join("lib");
    let path = blocker.join("facts.json");
    let (sink, log) = file_at(&path);
    fs::write(&blocker, "not a directory").expect("a file where the directory goes");
    assert!(!sink.apply(&facts(3, "r12")));
    assert_eq!(sink.generation(), None);
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    let prefix = format!(
        "error: facts generation 3 is not written to {}: ",
        path.display()
    );
    assert!(lines[0].starts_with(&prefix), "{}", lines[0]);
    assert!(lines[0].len() > prefix.len(), "the reason is named");

    fs::remove_file(&blocker).expect("the blocker removed");
    assert!(sink.apply(&facts(4, "r12")));
    assert_eq!(sink.generation(), Some(4));
}
