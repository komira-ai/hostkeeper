//! The repository's `README.md` names as answering `UNIMPLEMENTED` exactly the operator API
//! methods that [`PENDING`] lists: a handler that lands, or a method that loses one, cannot leave
//! the README describing the other state.

use std::collections::BTreeSet;

use crate::grpc::PENDING;

/// The repository's `README.md`, read when the test is compiled.
const README: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"));

/// The word that marks a sentence of the README as listing methods no handler serves.
const MARKER: &str = "`UNIMPLEMENTED`";

/// The sentences of `text`, with every run of whitespace (line breaks included) folded to one
/// space. A sentence ends at a `.` followed by whitespace, or at the end of the text; the dots
/// inside a name such as `Hosts.List` are followed by a letter, so they end nothing.
fn sentences(text: &str) -> Vec<String> {
    let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
    folded.split(". ").map(str::to_owned).collect()
}

/// Whether `word` reads as `Service.Method`: two non-empty runs of ASCII letters and digits,
/// joined by one dot, each starting with an upper-case letter.
fn is_method(word: &str) -> bool {
    let parts: Vec<&str> = word.split('.').collect();
    parts.len() == 2
        && parts.iter().all(|part| {
            part.starts_with(|c: char| c.is_ascii_uppercase())
                && part.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

/// The `Service.Method` names written in backticks in each sentence of `readme` that contains
/// `` `UNIMPLEMENTED` ``.
fn readme_unimplemented(readme: &str) -> BTreeSet<String> {
    sentences(readme)
        .iter()
        .filter(|sentence| sentence.contains(MARKER))
        .flat_map(|sentence| {
            // The pieces at odd positions of a split on backticks are the spans inside them.
            sentence
                .split('`')
                .skip(1)
                .step_by(2)
                .filter(|word| is_method(word))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `paths` as `Service.Method`: `/hostkeeper.operator.v1.Hosts/List` reads `Hosts.List`, the
/// last segment of the service's full name (after its last `.` or `/`) and the method.
fn pending_methods(paths: &[&str]) -> BTreeSet<String> {
    paths
        .iter()
        .map(|path| {
            let (service, method) = path.rsplit_once('/').unwrap_or(("", path));
            let short = service.rsplit(['.', '/']).next().unwrap_or(service);
            format!("{short}.{method}")
        })
        .collect()
}

/// `Ok` when `readme` names exactly the methods of `pending` as `UNIMPLEMENTED`; otherwise the
/// message that prints both sets.
fn compare(readme: &str, pending: &[&str]) -> Result<(), String> {
    let named = readme_unimplemented(readme);
    let listed = pending_methods(pending);
    if named == listed {
        Ok(())
    } else {
        Err(format!(
            "README.md names as UNIMPLEMENTED {named:?}, but PENDING lists {listed:?}"
        ))
    }
}

/// Proves: the README's `UNIMPLEMENTED` sentences name exactly the methods of [`PENDING`].
#[test]
fn readme_unimplemented_methods_match_pending() {
    assert_eq!(compare(README, PENDING), Ok(()));
}

/// Proves: a sentence that wraps across lines is read whole, a backticked word that is not
/// `Service.Method` is skipped, and a method named in a sentence without the marker is not
/// counted.
#[test]
fn only_methods_of_marked_sentences_are_read() {
    let readme = "Served: `Services.Assign` and `Services.Unassign`.\n\
                  The API declares `Hosts.List` and\n`Hosts.Get`, which answer `UNIMPLEMENTED`\n\
                  (see `hostkeeper.toml`, `ctl`, `hosts.list`, `Hosts.List.Extra`, `Hosts.`, `Hosts.Get()`).\n\
                  Then `Releases.UnmarkBad` is served.";
    let named: Vec<String> = readme_unimplemented(readme).into_iter().collect();
    assert_eq!(named, ["Hosts.Get", "Hosts.List"]);
}

/// Proves: a README with no `UNIMPLEMENTED` sentence names nothing, and the last sentence of
/// the text, with no full stop after it, is read.
#[test]
fn unmarked_readme_names_nothing_and_last_sentence_counts() {
    assert_eq!(
        readme_unimplemented("Everything is served."),
        BTreeSet::new()
    );
    let named: Vec<String> = readme_unimplemented("`Hosts.Get` answers `UNIMPLEMENTED`")
        .into_iter()
        .collect();
    assert_eq!(named, ["Hosts.Get"]);
}

/// Proves: a gRPC path is normalised to `Service.Method` from the last segment of the
/// service's full name, whether or not the service has a package.
#[test]
fn pending_paths_read_as_service_dot_method() {
    let methods: Vec<String> =
        pending_methods(&["/hostkeeper.operator.v1.Releases/UnmarkBad", "/Hosts/List"])
            .into_iter()
            .collect();
    assert_eq!(methods, ["Hosts.List", "Releases.UnmarkBad"]);
}

/// Proves: the comparison fails, printing both sets in full, when the README names a method
/// that the pending list omits, and when the pending list holds one the README omits.
#[test]
fn comparison_fails_on_either_side_with_both_sets() {
    let readme = "`Services.Assign` and `Hosts.List` answer `UNIMPLEMENTED`.";
    assert_eq!(
        compare(readme, &["/hostkeeper.operator.v1.Hosts/List"]),
        Err(
            "README.md names as UNIMPLEMENTED {\"Hosts.List\", \"Services.Assign\"}, \
             but PENDING lists {\"Hosts.List\"}"
                .to_owned()
        )
    );
    assert_eq!(
        compare(
            "`Hosts.List` answers `UNIMPLEMENTED`.",
            &[
                "/hostkeeper.operator.v1.Hosts/List",
                "/hostkeeper.operator.v1.Hosts/Get"
            ]
        ),
        Err("README.md names as UNIMPLEMENTED {\"Hosts.List\"}, \
             but PENDING lists {\"Hosts.Get\", \"Hosts.List\"}"
            .to_owned())
    );
}
