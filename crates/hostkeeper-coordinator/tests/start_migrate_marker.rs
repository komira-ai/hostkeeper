//! `serve`'s start over the marker of a `host-groups migrate --apply` of release 0.1.253, a verb
//! this release does not have, that stopped partway
//! (`<state-file>.migrate-unfinished`): the whole start (`config::build`) refuses with the exact
//! text, and the start goes on once the operator has removed the marker.

mod support;

use hostkeeper_coordinator::config::StartError;
use support::serve::{Files, build_given, flags_of, refusal};

/// Proves: with `<state-file>.migrate-unfinished` beside the state file, a full `serve` start is
/// refused with `StartError::MigrateUnfinished` naming the marker and saying to restore the
/// backup taken before the migration, and the marker stays; once it is removed
/// the same flags start.
///
/// Catches: a start over state files that hold part of a migration, or a refusal that removes the
/// marker or says something else.
#[test]
fn serve_refuses_to_start_while_the_migrate_marker_is_there() {
    let files = Files::new("start-migrate-marker");
    let marker = files.path("desired.json.migrate-unfinished");
    files.put("desired.json.migrate-unfinished", "", 0o600);

    let why = refusal(files.args());

    assert!(
        matches!(&why, StartError::MigrateUnfinished { path } if *path == marker),
        "{why}"
    );
    assert_eq!(
        why.to_string(),
        format!(
            "{}: a host-groups migrate --apply of release 0.1.253 stopped partway; restore the \
             backup taken before it",
            marker.display()
        )
    );
    assert!(marker.exists(), "the marker stays for the operator");

    std::fs::remove_file(&marker).expect("removed by the operator");
    build_given(&flags_of(files.args())).expect("a start");
}
