//! The registry (packages.md section 5.1): it holds exactly the installers this build has the
//! backend of, `apt` alone, each naming itself.

use hostkeeper_install::{REGISTERED, backend_of};
use hostkeeper_release::Installer;

/// Proves the registry holds apt's backend and nothing else: an installer has a backend exactly
/// when it is registered, and the backend names the installer it is looked up by.
#[test]
fn the_registry_holds_apt_alone() {
    assert_eq!(REGISTERED, [Installer::Apt]);
    for installer in Installer::ALL {
        let backend = backend_of(installer);
        assert_eq!(
            backend.is_some(),
            REGISTERED.contains(&installer),
            "{installer}"
        );
        if let Some(backend) = backend {
            assert_eq!(backend.installer(), installer);
        }
    }
}
