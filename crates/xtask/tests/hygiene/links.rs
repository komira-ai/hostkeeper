//! The relative links of `README.md` and `MIGRATION.md` lead to tracked files: a link to a file
//! or a directory that was deleted or never added is refused, naming the file and the link.

use std::collections::BTreeSet;

use crate::{repo_root, texts, tracked_files};

/// The documents whose links are checked.
const DOCUMENTS: [&str; 2] = ["README.md", "MIGRATION.md"];

/// `target` resolved against the directory of `from`, with `.` and `..` components applied;
/// `None` when it climbs above the repository root.
fn resolve(from: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The targets of the inline links `[text](target)` of `text`, a title after the target left
/// out.
fn link_targets(text: &str) -> Vec<&str> {
    text.split("](")
        .skip(1)
        .filter_map(|rest| rest.split(')').next())
        .filter_map(|inside| inside.split_whitespace().next())
        .collect()
}

/// `<path>: the link <target> leads to no tracked file` for each relative link of `text` (the
/// document at `path`) whose target, its `#fragment` left out, is neither a tracked file nor a
/// directory holding one. A link with a scheme (`https:`) or to an anchor of the same document
/// is not checked.
fn broken_links(path: &str, text: &str, tracked: &BTreeSet<String>) -> Vec<String> {
    let mut found = Vec::new();
    for target in link_targets(text) {
        let file = target.split('#').next().unwrap_or_default();
        if file.is_empty() || file.contains(':') {
            continue;
        }
        let leads = resolve(path, file).is_some_and(|resolved| {
            let below = format!("{resolved}/");
            tracked.contains(&resolved) || tracked.iter().any(|t| t.starts_with(&below))
        });
        if !leads {
            found.push(format!(
                "{path}: the link {target} leads to no tracked file"
            ));
        }
    }
    found
}

/// Catches: a link of `README.md` or `MIGRATION.md` to a deleted crate (such as
/// `crates/hostkeeper-rights`), file or anchor target; and a check that passes because it read
/// nothing. A tracked file, a directory of tracked files, `..`, a fragment, a title, a web link
/// and an anchor of the same document pass; a missing file, a missing directory and a path that
/// climbs out of the repository are refused, each by name.
#[test]
fn markdown_links_in_readme_and_migration_resolve() {
    let root = repo_root();
    let paths = tracked_files(&root);
    let tracked: BTreeSet<String> = paths.iter().cloned().collect();
    let documents: Vec<(String, String)> = texts(&root, paths)
        .into_iter()
        .filter(|(path, _)| DOCUMENTS.contains(&path.as_str()))
        .collect();
    assert_eq!(
        documents.len(),
        DOCUMENTS.len(),
        "a document is not tracked"
    );
    for (path, text) in &documents {
        assert!(
            !link_targets(text).is_empty(),
            "{path} has no link to check"
        );
        assert_eq!(broken_links(path, text, &tracked), Vec::<String>::new());
    }

    let tracked: BTreeSet<String> = ["LICENSE", "crates/hostkeeper-hook/src/lib.rs", "docs/a.md"]
        .map(str::to_string)
        .into();
    let text = "[l](LICENSE) [h](crates/hostkeeper-hook \"the hook\") [a](docs/a.md#top) \
                [w](https://example.com/x) [s](#force) \
                [r](crates/hostkeeper-rights) [d](docs/b.md) [o](../outside.md) [x](./LICENSE)";
    assert_eq!(
        broken_links("README.md", text, &tracked),
        vec![
            "README.md: the link crates/hostkeeper-rights leads to no tracked file".to_string(),
            "README.md: the link docs/b.md leads to no tracked file".to_string(),
            "README.md: the link ../outside.md leads to no tracked file".to_string(),
        ]
    );
    assert_eq!(
        broken_links(
            "docs/a.md",
            "[up](../LICENSE) [gone](../crates/hostkeeper-rights)",
            &tracked
        ),
        vec![
            "docs/a.md: the link ../crates/hostkeeper-rights leads to no tracked file".to_string()
        ]
    );
}
