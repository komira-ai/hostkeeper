//! The list the daemon holds hostkeeper's own `.pkg` to (`hostkeeper_install::macos_pkg::own`)
//! against the payload this verb builds.

use std::collections::BTreeMap;

use hostkeeper_install::host::Kind;
use hostkeeper_install::macos_pkg::own;

use super::*;

/// The daemon's list of the pkg's payload is exactly the payload [`payload`] lays out: each path
/// (the install location itself aside), its kind and its mode, and the pkg's identifier is the
/// identifier the daemon recognises as its own.
#[test]
fn the_daemons_list_for_its_own_pkg_is_the_payload_this_verb_builds() {
    let contents = Contents {
        binary: Vec::new(),
        plist: Vec::new(),
        gh: Vec::new(),
        trusted_root: Vec::new(),
    };
    let built: BTreeMap<String, (Kind, u32)> = payload(contents)
        .into_iter()
        .filter(|(path, _)| !path.is_empty())
        .map(|(path, want)| {
            let entry = match want {
                Want::Dir => (Kind::Dir, DIR_MODE),
                Want::File { mode, .. } => (Kind::File, mode),
            };
            (format!("/{path}"), entry)
        })
        .collect();
    assert_eq!(own::list(), built);
    assert_eq!(own::identifier(), identifier());
    assert!(own::is_own(own::SERVICE, &identifier()));
    assert_eq!(own::POSTINSTALL, scripts::POSTINSTALL);
}
