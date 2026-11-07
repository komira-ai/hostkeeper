//! `serve`'s start over the marker of changes that were in memory and not yet written to the ops
//! file when the coordinator stopped (`<ops-file>.halts.unfinished`,
//! `docs/design/coordinator-storage.md` section 4.3): a marker that lists changes the ops file
//! holds is removed and the start goes on; one that lists a change the ops file does not hold
//! refuses the start, naming that change; one that lists nothing this build reads refuses it as
//! the release before did.

mod support;

use hostkeeper_coordinator::config::StartError;
use hostkeeper_coordinator::ops::OpsStore;
use support::recorded;
use support::serve::{Files, build_given, flags_of, refusal};

/// The ops file and its halts file, as an operator's changes left them: the ring holds the
/// changes numbered 1 to 3.
fn with_recorded_ops(files: &Files) {
    files.put("desired.json.ops", &recorded("ops.json"), 0o600);
    files.put("desired.json.ops.halts", &recorded("ops.json.halts"), 0o600);
}

/// Catches: a start that goes on while the marker names a change the ops file does not hold,
/// whose refusal does not name the change, or that removes the marker; and a marker this build
/// cannot read taken as settled.
#[test]
fn serve_refuses_to_start_while_the_halt_marker_is_there() {
    let files = Files::new("start-marker-refused");
    with_recorded_ops(&files);
    let marker = files.path("desired.json.ops.halts.unfinished");
    files.put(
        "desired.json.ops.halts.unfinished",
        "{\"unwritten\":[3,4,5]}\n",
        0o600,
    );

    let why = refusal(files.args());
    assert!(
        matches!(
            &why,
            StartError::ChangesUnwritten { path, numbers } if *path == marker && *numbers == [4, 5]
        ),
        "{why}"
    );
    assert_eq!(
        why.to_string(),
        format!(
            "{}: the changes numbered 4, 5 were in memory and not yet written to the ops file \
             when the coordinator stopped, and it does not hold them; look each up by its number \
             in the coordinator's log (target hostkeeper_coordinator::audit), make it again by \
             hand if it still matters (a halt is raised again by the host's next report while \
             the host is up), and remove this file to start",
            marker.display()
        )
    );
    assert!(marker.exists(), "the marker stays for the operator");

    std::fs::remove_file(&marker).expect("removed by the operator");
    std::fs::create_dir(&marker).expect("a marker that cannot be read");
    let why = refusal(files.args());
    assert!(
        matches!(&why, StartError::HaltWriteUnfinished { path } if *path == marker),
        "{why}"
    );

    std::fs::remove_dir(&marker).expect("removed by the operator");
    build_given(&flags_of(files.args())).expect("a start");
}

/// Catches: a marker whose every change landed before the coordinator stopped left to stop the
/// next start, or kept after it.
#[test]
fn a_marker_whose_changes_all_landed_is_removed_at_start() {
    let files = Files::new("start-marker-landed");
    with_recorded_ops(&files);
    let marker = files.path("desired.json.ops.halts.unfinished");
    files.put(
        "desired.json.ops.halts.unfinished",
        "{\"unwritten\":[2,3]}\n",
        0o600,
    );

    let parts = build_given(&flags_of(files.args())).expect("a start");
    assert!(!marker.exists(), "the marker is removed");
    drop(parts);
    let halt = OpsStore::open(&files.path("desired.json.ops"))
        .expect("the ops file")
        .marks("kbf")
        .halt
        .expect("the halt the marker named stands");
    assert_eq!(halt.node_id, "s1");
}
