//! The feature strings daemons announce and the coordinator advertises, pinned: each name
//! `agent.proto` gives in backquotes (`deferred-v1`). Callers branch on each, and the daemon
//! and the coordinator are built against them apart.

/// The feature names `agent.proto` gives (`deferred-v1`), sorted, each once.
fn feature_names() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("proto/hostkeeper/agent/v1/agent.proto");
    let text = std::fs::read_to_string(path).expect("read the file");
    let mut out: Vec<String> = Vec::new();
    for quoted in text.split('`').skip(1).step_by(2) {
        let feature = quoted.rsplit_once("-v").is_some_and(|(name, n)| {
            !name.is_empty()
                && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                && !n.is_empty()
                && n.bytes().all(|b| b.is_ascii_digit())
        });
        if feature && !out.iter().any(|f| f == quoted) {
            out.push(quoted.to_owned());
        }
    }
    out.sort();
    out
}

/// Proves the features `agent.proto` names are exactly these, `operator-keys-v1` among them.
/// Catches: a feature renamed, or named by the file without this test changing with it.
#[test]
fn feature_names_are_pinned() {
    assert_eq!(
        feature_names(),
        [
            "abort-v1",
            "clear-record-v1",
            "deferred-v1",
            "facts-v1",
            "forced-v1",
            "halt-v1",
            "heartbeat-v2",
            "install-v1",
            "operator-keys-v1",
            "private-repository-v1",
            "repositories-v1",
            "service-identity-v1",
            "target-v1",
            "verdict-acks-v1"
        ]
    );
}
