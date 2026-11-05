//! The frozen JSON routes through the entry point of the listeners: the recorded sequences of
//! `tests/fixtures/recorded/sequences`, replayed through `grpc::route` instead of `http::handle`,
//! answer with the same bytes. Serving gRPC and the gateway beside the JSON routes moves none of
//! their bytes.

mod support;

use support::replay::{Sequence, replay_routed, sequence_files};

/// The recorded sequences that cover each frozen route, by file stem.
const ROUTES: &[(&str, &str)] = &[
    ("02-farm-restart", "heartbeat and restart"),
    ("08-halt-raise-hold-clear", "operator"),
    (
        "13-kbf-status-and-convergence-bytes",
        "desired, status, convergence",
    ),
    ("14-v0018-updater-and-deploy-farm", "desired and heartbeat"),
];

/// Proves: every call of every recorded sequence (the heartbeat, `/v1/desired`, `/v1/operator`,
/// `/v1/status` and `/v1/convergence` of the JSON routes) answers, through `grpc::route`, with the
/// status and the exact body bytes that are recorded. The sequences named in `ROUTES` must be
/// among those replayed, so the desired, status, convergence and operator routes are all covered.
#[tokio::test]
async fn the_frozen_json_routes_are_byte_identical() {
    let files = sequence_files();
    assert!(
        files.len() >= 30,
        "the recorded sequences are missing: {files:?}"
    );
    let mut stems = Vec::new();
    let mut differences = Vec::new();
    for file in &files {
        let stem = file.file_stem().and_then(|s| s.to_str()).expect("a name");
        stems.push(stem.to_owned());
        let sequence = Sequence::load(file);
        if let Err(why) = replay_routed(&sequence, &format!("frozen-{stem}")).await {
            differences.push(why);
        }
    }
    for (stem, covers) in ROUTES {
        assert!(
            stems.iter().any(|s| s == stem),
            "the sequence {stem} ({covers}) is not recorded: {stems:?}"
        );
    }
    assert!(
        differences.is_empty(),
        "{} of {} sequences differ through grpc::route:\n{}",
        differences.len(),
        stems.len(),
        differences.join("\n")
    );
}
