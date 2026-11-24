//! The vendored protos of `cargo xtask mac-farm` (`crates/xtask/proto/third_party`, compiled by
//! xtask's `build.rs`): each file a line of their `PINS` names is kept byte for byte as its
//! upstream commit has it, which xtask's unit test `the_vendored_protos_are_their_pins` holds by
//! SHA-256. It is third-party text, with its authors' dates and the dotted names of its packages
//! (a method of `google` then `bytestream` then `ByteStream`, joined by dots, reads as a host name), and is not scanned, as the web
//! app's pinned vendored files are not. A file of that directory that no line names is scanned.

use std::collections::HashSet;

/// The directory of the vendored protos, as a path from the repository root.
pub const DIR: &str = "crates/xtask/proto/third_party/";

/// The pins: `<dir> <upstream repository> <commit> <sha256> <path>` lines, `#` comments.
pub const PINS_TEXT: &str = include_str!("../../proto/third_party/PINS");

/// `paths` but the vendored protos `pins` (the text of [`PINS_TEXT`]) names.
pub fn unpinned(paths: Vec<String>, pins: &str) -> Vec<String> {
    let pinned: HashSet<String> = pins
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| match line.split(' ').collect::<Vec<_>>()[..] {
            [dir, _, _, _, path] => Some(format!("{DIR}{dir}/{path}")),
            _ => None,
        })
        .collect();
    paths.into_iter().filter(|p| !pinned.contains(p)).collect()
}

#[test]
fn hygiene_lint_skips_the_pinned_vendored_protos_and_scans_the_rest() {
    // Catches: a file put under the vendored directory without a pin (it would escape every
    // rule), a pinned file still scanned (the upstream text fails the date and host rules), a
    // path elsewhere that merely ends like a pinned one skipped, and a malformed line taken as a
    // pin.
    let pins = "# comment googleapis x y z google/rpc/status.proto\n\
                googleapis https://github.com/googleapis/googleapis abc 0123 google/rpc/status.proto\n\
                remote-apis https://github.com/bazelbuild/remote-apis abc 0123 LICENSE\n\
                googleapis too few google/api/http.proto\n";
    let paths = [
        "crates/xtask/proto/third_party/googleapis/google/rpc/status.proto",
        "crates/xtask/proto/third_party/remote-apis/LICENSE",
        "crates/xtask/proto/third_party/googleapis/google/api/http.proto",
        "crates/xtask/proto/third_party/googleapis/google/rpc/unpinned.proto",
        "crates/xtask/proto/third_party/PINS",
        "crates/other/googleapis/google/rpc/status.proto",
        "LICENSE",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(
        unpinned(paths, pins),
        [
            "crates/xtask/proto/third_party/googleapis/google/api/http.proto",
            "crates/xtask/proto/third_party/googleapis/google/rpc/unpinned.proto",
            "crates/xtask/proto/third_party/PINS",
            "crates/other/googleapis/google/rpc/status.proto",
            "LICENSE",
        ]
    );
    assert!(
        PINS_TEXT
            .lines()
            .any(|line| line.ends_with(" build/bazel/remote/execution/v2/remote_execution.proto")),
        "the pins name the Remote Execution API"
    );
}
