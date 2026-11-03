//! What a loaded declaration reports: the hook's resolved account and the host's declared names.

mod support;

use std::collections::BTreeSet;

use hostkeeper_rights::UserIds;
use support::{declared, sample};

/// Catches `hook` and `names` reporting anything but what the declaration was loaded with: the
/// hook's account ids as resolved, and every declared service of the host, this one included.
#[test]
fn the_loaded_declaration_reports_its_resolved_accounts_and_the_hosts_names() {
    let declaration = sample();
    let own = declaration.name.clone();
    let loaded = declared(declaration, 991, &[own.as_str(), "other"]);
    assert_eq!(loaded.hook(), UserIds { uid: 991, gid: 991 });
    let expected: BTreeSet<String> = [own, "other".to_owned()].into_iter().collect();
    assert_eq!(loaded.names(), &expected);
}
