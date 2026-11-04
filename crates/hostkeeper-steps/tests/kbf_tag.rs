//! The step record and the names of the runner, held to the bytes and literals of kbf at the tag
//! `v0.0.64` (commit `8f90d0604794f3fee057e71647c7e1c1e034fa2e`), the source of this crate.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::record::{HookSel, PARAMS_FILE, Progress, RECORD_FILE, SCHEMA, State};
use hostkeeper_steps::runner::{
    DEFAULT_RETRY_SECS, MAX_RETRY_SECS, REASON_ARTIFACT_CHANGED, REASON_HALT_CLEARED,
    REASON_UPDATER_OLD, ROLLBACK_ATTEMPTS, UPDATER_PROTOCOL, WATCH_POLL_SECS,
};
use hostkeeper_steps::selfupdate::{Layout, START_LIMIT_BURST, fallback_unit, main_dropin};
use hostkeeper_steps::{Loaded, Record, RecordStore, RunError, Stamp};
use support::{Rig, Scratch};

const TAG: &str = "v0.0.64";
const COMMIT: &str = "8f90d0604794f3fee057e71647c7e1c1e034fa2e";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kbf-v0.0.64")
}

fn fixture(name: &str) -> String {
    fs::read_to_string(fixtures().join(format!("{name}.json"))).expect("read the fixture")
}

fn stamp(boot_id: &str, mono_secs: u64) -> Stamp {
    Stamp {
        boot_id: boot_id.to_owned(),
        mono_secs,
    }
}

/// The record each state fixture holds, built field by field.
fn expected(name: &str) -> Record {
    let hex = |pair: &str| pair.repeat(32);
    let base = |service: &str, version: &str, generation: u64, step: Step| Record {
        schema: 1,
        service: service.to_owned(),
        target_version: version.to_owned(),
        artifact_digest: hex("ab"),
        tarball_digest: hex("cd"),
        previous_digest: Some(hex("ef")),
        generation,
        params: PathBuf::from(format!("/var/lib/updater/state/{service}/params.toml")),
        step,
        attempt: 1,
        state: State::Pending,
        hook: HookSel::New,
        progress: Progress::default(),
        order_try: None,
        forced: false,
        force: None,
    };
    match name {
        "pending" => Record {
            previous_digest: None,
            ..base("web", "2.0.0", 1, Step::Prepare)
        },
        "running" => Record {
            state: State::Running {
                started: stamp("boot-one", 4200),
                deadline_secs: 1600,
            },
            progress: Progress {
                verify_started: Some(stamp("boot-one", 4100)),
                ..Progress::default()
            },
            ..base("web", "2.0.0", 3, Step::Verify)
        },
        "done" => Record {
            attempt: 2,
            state: State::Done {
                code: Some(75),
                last_line: Some(r#"{"retry_after_secs":100}"#.to_owned()),
            },
            hook: HookSel::Previous,
            progress: Progress {
                committed: true,
                promotion_pending: false,
                then: Some(Verdict::Failed {
                    phase: Phase::AfterCommit,
                    class: Class::Environment,
                }),
                retry: 2,
                verify_started: None,
                watching: None,
                reason: Some("remote_down".to_owned()),
            },
            ..base("api", "1.4.2", 12, Step::Rollback)
        },
        "finished" => Record {
            state: State::Finished {
                verdict: Verdict::Succeeded {
                    promotion_pending: true,
                },
                reason: None,
            },
            progress: Progress {
                promotion_pending: true,
                verify_started: Some(stamp("boot-two", 90)),
                watching: Some(stamp("boot-two", 95)),
                ..Progress::default()
            },
            ..base("api", "1.4.2", 13, Step::Verify)
        },
        other => panic!("no state fixture named {other}"),
    }
}

const STATES: [&str; 4] = ["pending", "running", "done", "finished"];

/// Catches a reader that no longer understands a record kbf wrote: each of the four states, with
/// its options, hook and progress, loads as the record it is.
#[test]
fn reads_step_record_written_by_kbf_tag() {
    for name in STATES {
        let dir = Scratch::new(&format!("kbf-read-{name}"));
        fs::write(dir.path().join(RECORD_FILE), fixture(name)).unwrap();
        assert_eq!(
            RecordStore::new(dir.path()).load().unwrap(),
            Loaded::Current(Box::new(expected(name))),
            "{name}"
        );
    }
}

/// Catches a reader that rejects a record whose `progress` has no `reason` (the field is
/// defaulted): it loads with `reason` `None`, as the canonical `pending` record, and saving it
/// writes the canonical bytes, `reason` included as null.
#[test]
fn reads_a_kbf_record_without_the_defaulted_reason() {
    let dir = Scratch::new("kbf-reason-absent");
    let text = fixture("reason-absent");
    assert!(!text.contains("reason"));
    fs::write(dir.path().join(RECORD_FILE), text).unwrap();
    let store = RecordStore::new(dir.path());
    let Loaded::Current(record) = store.load().unwrap() else {
        panic!("a schema 1 record loads as current");
    };
    assert_eq!(record.progress.reason, None);
    assert_eq!(*record, expected("pending"));
    store.save(&record).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join(RECORD_FILE)).unwrap(),
        fixture("pending")
    );
}

/// Catches a reader that refuses a record of the same schema carrying fields it does not know,
/// at the top level and inside `progress`: it loads as the canonical `pending` record, and
/// saving it writes the canonical bytes without the unknown fields.
#[test]
fn reads_a_kbf_record_with_an_unknown_field() {
    let dir = Scratch::new("kbf-unknown-field");
    let text = fixture("unknown-field");
    assert_eq!(text.matches("added_later").count(), 2);
    fs::write(dir.path().join(RECORD_FILE), text).unwrap();
    let store = RecordStore::new(dir.path());
    assert_eq!(
        store.load().unwrap(),
        Loaded::Current(Box::new(expected("pending")))
    );
    store.save(&expected("pending")).unwrap();
    let written = fs::read_to_string(dir.path().join(RECORD_FILE)).unwrap();
    assert!(!written.contains("added_later"));
    assert_eq!(written, fixture("pending"));
}

/// Catches a change of the on-disk format: saving each record writes exactly the bytes kbf's
/// writer wrote (field order, names, tags, indent, no trailing newline).
#[test]
fn writes_the_bytes_kbf_tag_wrote() {
    for name in STATES {
        let dir = Scratch::new(&format!("kbf-write-{name}"));
        RecordStore::new(dir.path()).save(&expected(name)).unwrap();
        let written = fs::read_to_string(dir.path().join(RECORD_FILE)).unwrap();
        assert_eq!(written, fixture(name), "{name}");
    }
}

/// Catches a record of a newer schema being run over, rewritten or answered as if it were
/// ours: it loads as `Newer`, every runner entry refuses it with its schema, the file keeps its
/// bytes, nothing else appears beside it and no step runs.
#[test]
fn a_newer_kbf_record_is_read_only() {
    let rig = Rig::new("kbf-newer");
    let text = fixture("newer-schema");
    fs::write(rig.state.join(RECORD_FILE), &text).unwrap();
    assert_eq!(rig.store().load().unwrap(), Loaded::Newer { schema: 2 });

    let err = rig.runner().run(&rig.order()).unwrap_err();
    assert!(matches!(err, RunError::RecordNewer(2)), "{err:?}");
    assert_eq!(
        err.to_string(),
        "the step record has schema 2, newer than this updater writes"
    );
    assert!(matches!(
        rig.runner().last_result(),
        Err(RunError::RecordNewer(2))
    ));
    assert!(matches!(
        rig.runner().clear_halt(),
        Err(RunError::RecordNewer(2))
    ));

    assert_eq!(
        fs::read_to_string(rig.state.join(RECORD_FILE)).unwrap(),
        text
    );
    let names: Vec<_> = fs::read_dir(&rig.state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, [RECORD_FILE]);
    assert!(rig.fake.events().is_empty());
}

/// Catches an import record that drifted from its documents: the migration note and the
/// fixtures' README name the tag and the commit, the note has its section naming every state,
/// the README lists exactly the files that exist, and the schema is the one they state.
#[test]
fn kbf_tag_reader_contract_documented() {
    let migration =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../MIGRATION.md"))
            .unwrap();
    let readme = fs::read_to_string(fixtures().join("README.md")).unwrap();
    for (what, text) in [("MIGRATION.md", &migration), ("README.md", &readme)] {
        assert!(text.contains(TAG), "{what} names the tag");
        assert!(text.contains(COMMIT), "{what} names the commit");
    }

    let heading = "## Step records written by kbf\n";
    let start = migration.find(heading).expect("the section exists");
    let rest = &migration[start + heading.len()..];
    let section = rest.split("\n## ").next().unwrap();
    for name in STATES {
        assert!(
            section.contains(&format!("`{name}`")),
            "section names {name}"
        );
    }

    let mut files: Vec<String> = fs::read_dir(fixtures())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "README.md",
            "done.json",
            "finished.json",
            "kbf-ci-pinned.json",
            "newer-schema.json",
            "pending.json",
            "reason-absent.json",
            "running.json",
            "unknown-field.json"
        ]
    );
    for file in &files {
        assert!(
            readme.contains(&format!("`{file}`")) || file == "README.md",
            "{file}"
        );
    }
    assert!(readme.contains("`SCHEMA` 1"));
    assert_eq!(SCHEMA, 1);
}

/// The record the tag's own CI test `the_record_on_disk_has_this_shape` builds, field by field.
fn kbf_ci_record() -> Record {
    Record {
        schema: SCHEMA,
        service: "svc".to_owned(),
        target_version: "2.0.0".to_owned(),
        artifact_digest: "abc".to_owned(),
        tarball_digest: "0123".to_owned(),
        previous_digest: Some("def".to_owned()),
        generation: 7,
        params: "/state/params.toml".into(),
        step: Step::Verify,
        attempt: 2,
        state: State::Pending,
        hook: HookSel::Previous,
        progress: Progress {
            committed: true,
            promotion_pending: false,
            then: Some(Verdict::Deferred {
                retry_after_secs: None,
            }),
            retry: 2,
            verify_started: Some(stamp("b", 7)),
            watching: None,
            reason: Some("remote_down".to_owned()),
        },
        order_try: None,
        forced: false,
        force: None,
    }
}

/// Catches a change to the record's JSON from what kbf's CI pinned at the tag: the fixture holds
/// the compact string that test asserted, this code reads it as the same record (directly and
/// through the store), and writes exactly those bytes from it.
#[test]
fn reads_and_writes_the_record_kbf_ci_pinned_at_the_tag() {
    let bytes = fs::read(fixtures().join("kbf-ci-pinned.json")).unwrap();
    assert!(!bytes.ends_with(b"\n"));
    assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 0);
    let record = kbf_ci_record();
    assert_eq!(serde_json::from_slice::<Record>(&bytes).unwrap(), record);
    assert_eq!(serde_json::to_string(&record).unwrap().into_bytes(), bytes);

    let dir = Scratch::new("kbf-ci-pinned");
    fs::write(dir.path().join(RECORD_FILE), &bytes).unwrap();
    assert_eq!(
        RecordStore::new(dir.path()).load().unwrap(),
        Loaded::Current(Box::new(record))
    );
}

/// Catches a renamed or re-shaped state or verdict: the JSON strings kbf's CI test asserted at the
/// tag are written and read identically here.
#[test]
fn the_state_and_verdict_forms_kbf_ci_pinned_at_the_tag() {
    let verdicts = [
        (
            Verdict::Succeeded {
                promotion_pending: true,
            },
            r#"{"Succeeded":{"promotion_pending":true}}"#,
        ),
        (
            Verdict::Failed {
                phase: Phase::AfterCommit,
                class: Class::Environment,
            },
            r#"{"Failed":{"phase":"after_commit","class":"environment"}}"#,
        ),
        (
            Verdict::Deferred {
                retry_after_secs: Some(5),
            },
            r#"{"Deferred":{"retry_after_secs":5}}"#,
        ),
        (
            Verdict::Deferred {
                retry_after_secs: None,
            },
            r#"{"Deferred":{"retry_after_secs":null}}"#,
        ),
        (Verdict::Halt, r#""Halt""#),
    ];
    for (verdict, json) in verdicts {
        assert_eq!(serde_json::to_string(&verdict).unwrap(), json);
        assert_eq!(serde_json::from_str::<Verdict>(json).unwrap(), verdict);
    }
    let states = [
        (State::Pending, r#"{"state":"pending"}"#),
        (
            State::Running {
                started: stamp("b", 7),
                deadline_secs: 9,
            },
            r#"{"state":"running","started":{"boot_id":"b","mono_secs":7},"deadline_secs":9}"#,
        ),
        (
            State::Done {
                code: Some(75),
                last_line: Some("{}".to_owned()),
            },
            r#"{"state":"done","code":75,"last_line":"{}"}"#,
        ),
        (
            State::Finished {
                verdict: Verdict::Halt,
                reason: Some("x".to_owned()),
            },
            r#"{"state":"finished","verdict":"Halt","reason":"x"}"#,
        ),
    ];
    for (state, json) in states {
        assert_eq!(serde_json::to_string(&state).unwrap(), json);
        assert_eq!(serde_json::from_str::<State>(json).unwrap(), state);
    }
}

/// Catches a name, number or text that a host, a unit or a file on disk sees changing from what
/// kbf had at the tag: the literals below are copied from its source.
#[test]
fn constants_match_kbf_at_the_tag() {
    assert_eq!(SCHEMA, 1);
    assert_eq!(RECORD_FILE, "step-record.json");
    assert_eq!(PARAMS_FILE, "params.toml");
    assert_eq!(UPDATER_PROTOCOL, 1);
    assert_eq!(ROLLBACK_ATTEMPTS, 3);
    assert_eq!(DEFAULT_RETRY_SECS, 60);
    assert_eq!(WATCH_POLL_SECS, 30);
    assert_eq!(MAX_RETRY_SECS, 3600);
    assert_eq!(REASON_ARTIFACT_CHANGED, "artifact_changed");
    assert_eq!(REASON_HALT_CLEARED, "halt_cleared");
    assert_eq!(REASON_UPDATER_OLD, "updater_old");
    assert_eq!(START_LIMIT_BURST, 3);

    let mut record = expected("running");
    record.service = "svc".to_owned();
    record.generation = 7;
    assert_eq!(record.unit(Step::Cutover), "updater-svc-cutover-7");
    assert_eq!(record.unit(Step::Cleanup), "updater-svc-cleanup-7");

    let layout = Layout::new(Path::new("/opt/updater"));
    assert_eq!(layout.current_path(), Path::new("/opt/updater/current"));
    assert_eq!(layout.previous_path(), Path::new("/opt/updater/previous"));
    assert_eq!(
        main_dropin("fallback.service", 3),
        "[Unit]\nOnFailure=fallback.service\nStartLimitIntervalSec=300\nStartLimitBurst=3\n\n\
         [Service]\nRestart=always\nRestartSec=1\n"
    );
    assert_eq!(
        fallback_unit(&layout, "updater.service"),
        "[Unit]\nDescription=Put the previous updater binary back after the new one failed to \
         start\n\n[Service]\nType=oneshot\n\
         ExecStart=/opt/updater/previous self-update-fallback --dir /opt/updater\n\
         ExecStartPost=/usr/bin/systemctl reset-failed updater.service\n\
         ExecStartPost=/usr/bin/systemctl start --no-block updater.service\n"
    );

    let dir = Scratch::new("kbf-layout");
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let source = dir.path().join("updater");
    fs::write(&source, "binary").unwrap();
    let layout = Layout::new(&bin);
    for version in ["1.0.0", "2.0.0"] {
        layout.stage(version, &source).unwrap();
        layout.switch(version).unwrap();
    }
    assert!(bin.join("versions/1.0.0").is_file());
    assert!(bin.join("versions/2.0.0").is_file());
    assert_eq!(
        fs::read_link(bin.join("current")).unwrap(),
        Path::new("versions/2.0.0")
    );
    assert_eq!(
        fs::read_link(bin.join("previous")).unwrap(),
        Path::new("versions/1.0.0")
    );
}
