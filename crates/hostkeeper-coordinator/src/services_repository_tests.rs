//! The package repository a service names: the rollout file's `repository` key, its default, the
//! names it refuses, and `Services.List` showing it.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::marks_support::{DB, WEB, operator, rig};
use crate::services::{DEFAULT_REPOSITORY, Services};

fn parsed(extra: &str) -> Result<Services, String> {
    Services::parse(&format!(
        "[[service]]\nname = \"web\"\nroles = [\"daemon\"]\n{extra}\n"
    ))
    .map_err(|why| why.to_string())
}

fn repository_of(services: &Services) -> String {
    services.get("web").expect("web").repository.clone()
}

/// Proves: a service that names no repository names `komira`, one that names one names it, and a
/// name of 1 and of 32 bytes of `[a-z0-9-]` is taken.
///
/// Catches: a default other than the one the held script wrote, or a key that is read and dropped.
#[test]
fn a_service_names_komira_unless_its_table_says_another() {
    assert_eq!(DEFAULT_REPOSITORY, "komira");
    assert_eq!(repository_of(&parsed("").expect("parsed")), "komira");
    let named = parsed("repository = \"other-1\"").expect("parsed");
    assert_eq!(repository_of(&named), "other-1");
    for name in ["a", &"a".repeat(32)] {
        let text = format!("repository = \"{name}\"");
        assert_eq!(repository_of(&parsed(&text).expect("parsed")), *name);
    }
}

/// Proves: a `repository` that is empty, 33 bytes, upper case, holds a dot or a space, or is not
/// text is refused by words that name the service and the value.
///
/// Catches: a repository name the registry could never hold, which every `SetDesired` of the
/// service would then be refused for.
#[test]
fn a_repository_that_is_no_repository_name_is_refused() {
    for name in ["", &"a".repeat(33), "Komira", "a.b", "a b"] {
        let refused = parsed(&format!("repository = \"{name}\"")).expect_err("refused");
        assert_eq!(
            refused,
            format!(
                "services: web: repository {name:?} is not 1 to 32 bytes of lowercase letters, digits and -"
            )
        );
    }
    assert!(parsed("repository = 5").is_err());
}

/// Proves: `Services.List` shows the repository each service names: `komira` for a service that
/// names none, the named one for another.
///
/// Catches: a list that leaves out the repository, which is where an operator reads why a host
/// gets none.
#[test]
fn services_list_shows_each_services_repository() {
    let rig = rig("services-repository", "repository = \"other\"");
    let listed = rig
        .coordinator
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("listed");
    let shown: Vec<(&str, &str)> = listed
        .services
        .iter()
        .map(|s| (s.service.as_str(), s.repository.as_str()))
        .collect();
    assert_eq!(shown, [(DB, "komira"), (WEB, "other")]);
}
