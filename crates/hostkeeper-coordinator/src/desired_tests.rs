use std::fs;

use hostkeeper_testkit::Scratch;

use crate::desired::{
    Desired, DesiredError, DesiredStore, FORMAT_MIN_UP, FORMAT_ROLES, MAX_COUNT, StoreError, dir_of,
};
use crate::testutil::{LEGACY, kbf_services, scratch};

const FULL: &str = r#"{"version":"0.1.9+gabcdefabcdef-2",
    "max_updating":{"frontend":1,"storage":2,"daemon":3,"mac-daemon":4},
    "min_up":{"storage":3,"frontend":2}}"#;

fn dir(name: &str) -> Scratch {
    scratch(name)
}

/// A `POST /v1/desired` body read for the recorded kbf definition as the only service.
fn request(body: &str) -> Result<(String, Desired), DesiredError> {
    Desired::parse_request_for(body.as_bytes(), &kbf_services(), Some(LEGACY))
}

/// Catches: a desired state read with other values than it was written with (each limit is
/// per role and `mac-daemon` is spelled with the hyphen), and limits left out not falling
/// back to the defaults (1, 1, 2, 2).
#[test]
fn a_desired_state_reads_every_limit_and_defaults_the_rest() {
    let d = Desired::parse(FULL.as_bytes()).expect("parsed");
    let got = FORMAT_ROLES.map(|r| d.max_updating.max_updating_of(r));
    assert_eq!(got, [1, 2, 3, 4]);
    assert_eq!(
        (
            d.min_up.min_up_of("storage"),
            d.min_up.min_up_of("frontend")
        ),
        (3, 2)
    );
    assert_eq!(d.version.as_str(), "0.1.9+gabcdefabcdef-2");
    let short = r#"{"version":"1.0","min_up":{"storage":1,"frontend":1}}"#;
    let d = Desired::parse(short.as_bytes()).expect("parsed");
    assert_eq!(
        FORMAT_ROLES.map(|r| d.max_updating.max_updating_of(r)),
        [1, 1, 2, 2]
    );
    let partial =
        r#"{"version":"1.0","max_updating":{"daemon":7},"min_up":{"storage":1,"frontend":1}}"#;
    let d = Desired::parse(partial.as_bytes()).expect("parsed");
    assert_eq!(
        FORMAT_ROLES.map(|r| d.max_updating.max_updating_of(r)),
        [1, 1, 7, 2]
    );
}

/// Catches: a desired state accepted that the host-side rule or the schema refuses: a
/// version with a shell word or an option, a missing minimum, an unknown field (a URL for the
/// release, say), a count over the cap, a negative count, and text that is no JSON. A limit
/// key that is no role name is refused in `desired_role_tests`.
#[test]
fn a_desired_state_that_is_not_one_is_refused() {
    let over = format!(
        r#"{{"version":"1.0","min_up":{{"storage":{},"frontend":1}}}}"#,
        MAX_COUNT + 1
    );
    let at_cap = format!(r#"{{"version":"1.0","min_up":{{"storage":{MAX_COUNT},"frontend":1}}}}"#);
    assert!(Desired::parse(at_cap.as_bytes()).is_ok());
    assert_eq!(Desired::parse(over.as_bytes()), Err(DesiredError::Count));
    let max_over = format!(
        r#"{{"version":"1.0","max_updating":{{"mac-daemon":{}}},"min_up":{{"storage":1,"frontend":1}}}}"#,
        MAX_COUNT + 1
    );
    assert_eq!(
        Desired::parse(max_over.as_bytes()),
        Err(DesiredError::Count)
    );
    for bad in [
        r#"{"version":"-1","min_up":{"storage":1,"frontend":1}}"#,
        r#"{"version":"1.0;id","min_up":{"storage":1,"frontend":1}}"#,
        r#"{"version":"1.0"}"#,
        r#"{"min_up":{"storage":1,"frontend":1}}"#,
        r#"{"version":"1.0","min_up":{"storage":1,"frontend":1},"url":"https://x.example"}"#,
        r#"{"version":"1.0","min_up":{"storage":-1,"frontend":1}}"#,
        "not json",
        "",
    ] {
        assert!(
            matches!(
                Desired::parse(bad.as_bytes()),
                Err(DesiredError::Malformed(_))
            ),
            "{bad}"
        );
    }
    assert!(DesiredError::Count.to_string().contains("10000"));
}

/// Catches: the desired state lost by a restart: a state set is read back, equal, by a store
/// opened on the same file, and a second set replaces the first; a store on no file has none.
#[test]
fn the_desired_state_survives_a_restart() {
    let dir = dir("desired-restart");
    let file = dir.path().join("desired.json");
    let store = DesiredStore::open(&file, Some(LEGACY)).expect("opened");
    assert_eq!(store.get(LEGACY), None);
    let first = Desired::parse(FULL.as_bytes()).expect("parsed");
    store.set(LEGACY, first.clone()).expect("stored");
    assert_eq!(store.get(LEGACY), Some(first.clone()));
    drop(store);
    let again = DesiredStore::open(&file, Some(LEGACY)).expect("reopened");
    assert_eq!(again.get(LEGACY), Some(first));
    let second = Desired::parse(br#"{"version":"1.0","min_up":{"storage":1,"frontend":1}}"#)
        .expect("parsed");
    again.set(LEGACY, second.clone()).expect("stored");
    drop(again);
    assert_eq!(
        DesiredStore::open(&file, Some(LEGACY))
            .expect("reopened")
            .get(LEGACY),
        Some(second)
    );
}

/// Catches: a state file written in place (a crash mid-write would leave half a state): the
/// file is replaced whole, and no temporary file is left beside it.
#[test]
fn the_state_file_is_replaced_whole_and_leaves_nothing_behind() {
    let dir = dir("desired-atomic");
    let file = dir.path().join("desired.json");
    let store = DesiredStore::open(&file, Some(LEGACY)).expect("opened");
    store
        .set(LEGACY, Desired::parse(FULL.as_bytes()).expect("parsed"))
        .expect("stored");
    let names: Vec<_> = fs::read_dir(dir.path())
        .expect("listed")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(names, ["desired.json"]);
    let text = fs::read_to_string(&file).expect("read");
    assert!(text.ends_with("}\n"), "{text:?}");
    assert_eq!(
        Desired::parse(text.as_bytes()).expect("whole"),
        store.get(LEGACY).expect("set")
    );
}

/// Catches: the temporary file being created by following a name an attacker or a crash left
/// there, or with the process umask: a symlink at the temporary name is replaced, not written
/// through, the file it pointed at is untouched, and the state file is owner-only.
#[test]
fn a_symlink_at_the_temporary_name_is_not_followed() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = dir("desired-symlink");
    let file = dir.path().join("desired.json");
    let victim = dir.path().join("victim");
    fs::write(&victim, "precious").expect("written");
    symlink(&victim, format!("{}.new", file.display())).expect("linked");
    let store = DesiredStore::open(&file, Some(LEGACY)).expect("opened");
    store
        .set(LEGACY, Desired::parse(FULL.as_bytes()).expect("parsed"))
        .expect("stored");
    assert_eq!(fs::read_to_string(&victim).expect("read"), "precious");
    let mode = fs::metadata(&file).expect("stat").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// Catches: a temporary name that cannot be cleared (a directory is there) being written
/// anyway or passed over: the set fails with an I/O error and the old state stands.
#[test]
fn a_temporary_name_that_cannot_be_cleared_fails_the_set() {
    let dir = dir("desired-blocked");
    let file = dir.path().join("desired.json");
    fs::create_dir(format!("{}.new", file.display())).expect("made");
    let store = DesiredStore::open(&file, Some(LEGACY)).expect("opened");
    let d = Desired::parse(FULL.as_bytes()).expect("parsed");
    assert!(matches!(store.set(LEGACY, d), Err(StoreError::Io { .. })));
    assert_eq!(store.get(LEGACY), None);
}

/// Catches: a state file that cannot be written changing the state in memory (the answer to
/// CI would say the state was set while a restart would lose it), and the failure not being
/// an error.
#[test]
fn a_state_that_cannot_be_written_is_not_set() {
    let dir = dir("desired-unwritable");
    let file = dir.path().join("no-such-dir").join("desired.json");
    let store = DesiredStore::open(&file, Some(LEGACY)).expect("a missing file is no state");
    let d = Desired::parse(FULL.as_bytes()).expect("parsed");
    assert!(matches!(store.set(LEGACY, d), Err(StoreError::Io { .. })));
    assert_eq!(store.get(LEGACY), None);
}

/// Catches: a state file the coordinator cannot make sense of being taken for no state (it
/// would then order nothing, and a later set would overwrite what an operator wrote): start
/// refuses, and the error names the file.
#[test]
fn a_state_file_that_cannot_be_read_back_refuses_to_start() {
    let dir = dir("desired-bad");
    let file = dir.path().join("desired.json");
    fs::write(&file, "{}").expect("written");
    let why = DesiredStore::open(&file, Some(LEGACY)).expect_err("refused");
    assert!(matches!(why, StoreError::Invalid { .. }));
    assert!(why.to_string().contains("desired.json"), "{why}");
    let unreadable = dir.path().join("a-directory");
    fs::create_dir(&unreadable).expect("made");
    let why = DesiredStore::open(&unreadable, Some(LEGACY)).expect_err("refused");
    assert!(matches!(why, StoreError::Io { .. }));
}

/// Catches: the directory fsync of a state file with a bare name aimed at the empty path
/// (which is no directory), and a path with a parent aimed anywhere but at the parent.
#[test]
fn the_directory_of_a_bare_file_name_is_the_working_directory() {
    use std::path::Path;
    assert_eq!(dir_of(Path::new("desired.json")), Path::new("."));
    assert_eq!(dir_of(Path::new("state/desired.json")), Path::new("state"));
    assert_eq!(dir_of(Path::new("/desired.json")), Path::new("/"));
}

const SMALL: &str = r#"{"version":"1.0","min_up":{"storage":1,"frontend":1}}"#;

/// Catches: the optional `service` of a `POST /v1/desired` body mishandled: absent and an
/// explicit `kbf` must give the same service and state, any other name is refused as an
/// unknown service, a `service` that is not text or a body that is not an object is
/// malformed, a count over the cap is still refused with a service named, and a body that
/// lacks a `min_up` key the definition requires is refused.
#[test]
fn a_request_names_its_service_and_only_defined_services_are_known() {
    let plain = request(SMALL).expect("parsed");
    let named = request(r#"{"service":"kbf","version":"1.0","min_up":{"storage":1,"frontend":1}}"#)
        .expect("parsed");
    assert_eq!(plain, named);
    assert_eq!(plain.0, LEGACY);
    assert_eq!(plain.1, Desired::parse(SMALL.as_bytes()).expect("parsed"));
    let other = r#"{"service":"other","version":"1.0","min_up":{"storage":1,"frontend":1}}"#;
    assert_eq!(request(other), Err(DesiredError::UnknownService));
    assert!(DesiredError::UnknownService.to_string().contains("service"));
    for bad in [
        r#"{"service":7,"version":"1.0","min_up":{"storage":1,"frontend":1}}"#,
        r#"{"service":"kbf","min_up":{"storage":1,"frontend":1}}"#,
        r#"{"service":"kbf","version":"1.0","min_up":{"storage":1,"frontend":1},"x":1}"#,
        r#"{"service":"kbf","version":"1.0","min_up":{"storage":1}}"#,
        r#"{"service":"kbf","version":"1.0"}"#,
        "[]",
        "",
    ] {
        assert!(
            matches!(request(bad), Err(DesiredError::Malformed(_))),
            "{bad}"
        );
    }
    assert_eq!(
        request(r#"{"version":"1.0","min_up":{"storage":1}}"#),
        Err(DesiredError::Malformed("min_up lacks frontend".to_owned()))
    );
    let over = format!(
        r#"{{"service":"kbf","version":"1.0","min_up":{{"storage":{},"frontend":1}}}}"#,
        MAX_COUNT + 1
    );
    assert_eq!(request(&over), Err(DesiredError::Count));
    // A state file's own shape does not take a `service`.
    assert!(Desired::parse(other.as_bytes()).is_err());
}

/// Catches: a body without `service` being given a service when the coordinator has no legacy
/// service (it would set a state no one asked for): it is refused as malformed, and the same
/// body with `service` named is read.
#[test]
fn a_request_without_service_needs_a_legacy_service() {
    let parse = |body: &str, legacy| {
        Desired::parse_request_for(body.as_bytes(), &kbf_services(), legacy).map(|(name, _)| name)
    };
    assert_eq!(
        parse(SMALL, None),
        Err(DesiredError::Malformed("service is required".to_owned()))
    );
    let named = r#"{"service":"kbf","version":"1.0","min_up":{"storage":1,"frontend":1}}"#;
    assert_eq!(parse(named, None), Ok("kbf".to_owned()));
    assert_eq!(parse(SMALL, Some("kbf")), Ok("kbf".to_owned()));
}

/// Catches: a state file written by an earlier build (the single state, no map) not being
/// read as the state of `kbf`, or read as some other service, or its limits changed.
#[test]
fn a_state_file_of_an_earlier_build_is_kbfs_state() {
    let dir = dir("desired-old-file");
    let file = dir.path().join("desired.json");
    fs::write(&file, FULL).expect("written");
    let store = DesiredStore::open(&file, Some(LEGACY)).expect("opened");
    let expected = Desired::parse(FULL.as_bytes()).expect("parsed");
    assert_eq!(store.get(LEGACY), Some(expected.clone()));
    assert_eq!(store.get("other"), None);
    assert_eq!(store.all().keys().collect::<Vec<_>>(), [LEGACY]);
}

/// Catches: the file a build writes while `kbf` is the only service not being the single
/// state an earlier build reads (a downgrade would then refuse to start): after a set the
/// file parses as the old format, byte for byte what that format's writer produced (the
/// state, pretty-printed, one newline), and has no `services` key.
#[test]
fn while_kbf_is_the_only_service_the_file_is_the_old_format() {
    let dir = dir("desired-old-format");
    let file = dir.path().join("desired.json");
    let store = DesiredStore::open(&file, Some(LEGACY)).expect("opened");
    let d = Desired::parse(FULL.as_bytes()).expect("parsed");
    store.set(LEGACY, d.clone()).expect("stored");
    let text = fs::read_to_string(&file).expect("read");
    assert_eq!(
        text,
        format!("{}\n", serde_json::to_string_pretty(&d).expect("json"))
    );
    assert!(!text.contains("services"), "{text}");
    assert_eq!(Desired::parse(text.as_bytes()), Ok(d));
}

/// Catches: the old single-state format written for a store whose one service is not
/// `kbf` (an earlier build would read it as `kbf`'s state): it is the map form and reads
/// back under its own name.
#[test]
fn a_lone_service_other_than_kbf_is_written_in_the_map_form() {
    let dir = dir("desired-lone-other");
    let file = dir.path().join("desired.json");
    let store = DesiredStore::open(&file, Some(LEGACY)).expect("opened");
    let other = Desired::parse(SMALL.as_bytes()).expect("parsed");
    store.set("other", other.clone()).expect("stored");
    let text = fs::read_to_string(&file).expect("read");
    assert!(text.contains("\"services\""), "{text}");
    let again = DesiredStore::open(&file, Some(LEGACY)).expect("reopened");
    assert_eq!(again.get("other"), Some(other));
    assert_eq!(again.get(LEGACY), None);
}

/// Catches: the map form (written once a second service exists) losing a service on a set
/// of another one, not surviving a restart, or being written without `kbf`'s entry: both
/// services read back after a reopen, and a set of one leaves the other as it was.
#[test]
fn the_map_form_keeps_each_service_apart_across_a_restart() {
    let dir = dir("desired-map");
    let file = dir.path().join("desired.json");
    let store = DesiredStore::open(&file, Some(LEGACY)).expect("opened");
    let kbf = Desired::parse(FULL.as_bytes()).expect("parsed");
    let other = Desired::parse(SMALL.as_bytes()).expect("parsed");
    store.set(LEGACY, kbf.clone()).expect("stored");
    store.set("other", other.clone()).expect("stored");
    let text = fs::read_to_string(&file).expect("read");
    assert!(text.contains("\"services\""), "{text}");
    assert!(Desired::parse(text.as_bytes()).is_err());
    let again = DesiredStore::open(&file, Some(LEGACY)).expect("reopened");
    assert_eq!(again.get(LEGACY), Some(kbf));
    assert_eq!(again.get("other"), Some(other.clone()));
    let newer = Desired::parse(br#"{"version":"3.0","min_up":{"storage":1,"frontend":1}}"#)
        .expect("parsed");
    again.set(LEGACY, newer.clone()).expect("stored");
    assert_eq!(again.get("other"), Some(other));
    assert_eq!(again.all().len(), 2);
    assert_eq!(again.get(LEGACY), Some(newer));
}

/// Catches: a map file read more loosely than the single state: an entry over the count cap,
/// an unknown key beside `services`, and an entry that is no state all stop the start.
#[test]
fn a_map_file_that_is_not_a_map_of_states_refuses_to_start() {
    let dir = dir("desired-bad-map");
    let file = dir.path().join("desired.json");
    let over = format!(
        r#"{{"services":{{"kbf":{{"version":"1.0","min_up":{{"storage":{},"frontend":1}}}}}}}}"#,
        MAX_COUNT + 1
    );
    let unknown = format!(r#"{{"services":{{"kbf":{SMALL}}},"extra":1}}"#);
    for bad in [
        over.as_str(),
        unknown.as_str(),
        r#"{"services":{"kbf":{}}}"#,
    ] {
        fs::write(&file, bad).expect("written");
        let why = DesiredStore::open(&file, Some(LEGACY)).expect_err("refused");
        assert!(matches!(why, StoreError::Invalid { .. }), "{bad}");
    }
}

/// Catches: the spelling or order of the role names of the file format drifting: every list
/// in the coordinator's output follows `FORMAT_ROLES`, and the written `min_up` names
/// `FORMAT_MIN_UP` first, which the last kbf coordinator requires. (That a role no definition
/// names is never ordered is `decide_role_tests`.)
#[test]
fn the_format_role_names_are_the_ones_of_the_file_format() {
    assert_eq!(
        FORMAT_ROLES,
        ["frontend", "storage", "daemon", "mac-daemon"]
    );
    assert_eq!(FORMAT_MIN_UP, ["storage", "frontend"]);
}

/// Catches: a single-state file read as some service's when no legacy service is set (a
/// state would be taken for a service that does not exist), and a map file refused for the
/// lack of one: the single form is `NoLegacyService`, the map form opens.
#[test]
fn a_single_state_file_needs_a_legacy_service_and_a_map_file_does_not() {
    let dir = dir("desired-no-legacy");
    let file = dir.path().join("desired.json");
    fs::write(&file, FULL).expect("written");
    let why = DesiredStore::open(&file, None).expect_err("refused");
    assert!(
        matches!(
            why,
            StoreError::Invalid {
                source: DesiredError::NoLegacyService,
                ..
            }
        ),
        "{why}"
    );
    assert!(
        DesiredError::NoLegacyService
            .to_string()
            .contains("no legacy service is set")
    );
    fs::write(&file, format!(r#"{{"services":{{"other":{SMALL}}}}}"#)).expect("written");
    let store = DesiredStore::open(&file, None).expect("opened");
    assert!(store.get("other").is_some());
}

/// Catches: the single-state form written for a service that is not the legacy one (the
/// last kbf coordinator would read it as the legacy service's state): with the legacy service
/// named `other` its lone state is the single form and a lone `kbf` state is the map; with
/// no legacy service every file is the map.
#[test]
fn the_single_form_is_the_legacy_services_alone() {
    let d = Desired::parse(SMALL.as_bytes()).expect("parsed");
    let written = |legacy: Option<&str>, service: &str| {
        let dir = dir("desired-single-form");
        let file = dir.path().join("desired.json");
        let store = DesiredStore::open(&file, legacy).expect("opened");
        store.set(service, d.clone()).expect("stored");
        fs::read_to_string(&file)
            .expect("read")
            .contains("\"services\"")
    };
    assert!(!written(Some("other"), "other"));
    assert!(written(Some("other"), "kbf"));
    assert!(written(None, "kbf"));
}
