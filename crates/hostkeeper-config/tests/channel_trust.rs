//! The trust list of `release channel-sync`: its format, the signer it reads with the daemon
//! config's own keys, and each refusal.

use hostkeeper_config::channel_trust::{ChannelTrust, Repository, TrustListError};
use hostkeeper_config::daemon::TrustedSigners;
use hostkeeper_config::names::ServiceName;

/// Two services of one repository, and one of another whose name is not its repository's.
const LIST: &str = r#"
[[service]]
name          = "tierstore"
repository    = "komira-ai/tierstore"
workflow      = ".github/workflows/artifacts.yml"
refs          = ["refs/heads/main"]
hosted_runner = true

[[service]]
name          = "js-agent"
repository    = "komira-ai/jobscheduler"
workflow      = ".github/workflows/release.yml"
refs          = ["refs/heads/main", "refs/tags/v*"]
hosted_runner = false

[[service]]
name          = "tierstore-gc"
repository    = "komira-ai/tierstore"
workflow      = ".github/workflows/artifacts.yml"
refs          = ["refs/heads/main"]
hosted_runner = true
"#;

/// The one service of a list, its keys as given.
fn one(keys: &str) -> String {
    format!("[[service]]\n{keys}\n")
}

const GOOD_KEYS: &str = r#"name = "js-agent"
repository = "komira-ai/jobscheduler"
workflow = ".github/workflows/release.yml"
refs = ["refs/heads/main"]
hosted_runner = true"#;

fn refusal(text: &str) -> TrustListError {
    ChannelTrust::parse(text).expect_err("the list is refused")
}

fn service(name: &str) -> ServiceName {
    ServiceName::new(name).expect("a service name")
}

/// Every service is read in the file's order, with the daemon's signer type, whose organisation
/// is the repository's owner; the repositories are listed once each in the order first named.
/// Catches a signer read with other keys or another organisation, a lost order, or a repository
/// synced twice.
#[test]
fn the_list_loads_every_service_with_its_signer_in_order() {
    let trust = ChannelTrust::parse(LIST).expect("the list loads");
    let names: Vec<&str> = trust
        .services()
        .iter()
        .map(|s| s.service.as_str())
        .collect();
    assert_eq!(names, ["tierstore", "js-agent", "tierstore-gc"]);
    let agent = trust
        .service(&service("js-agent"))
        .expect("js-agent is listed");
    assert_eq!(agent.repository.as_str(), "komira-ai/jobscheduler");
    assert_eq!(
        agent.signers,
        TrustedSigners {
            organisation: "komira-ai".to_owned(),
            workflow: ".github/workflows/release.yml".to_owned(),
            refs: vec!["refs/heads/main".to_owned(), "refs/tags/v*".to_owned()],
            hosted_runner: false,
        }
    );
    assert!(trust.service(&service("hello")).is_none());
    let repositories: Vec<String> = trust
        .repositories()
        .into_iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        repositories,
        ["komira-ai/tierstore", "komira-ai/jobscheduler"]
    );
}

/// An unknown key is refused naming it and its table, at the top and in a service.
/// Catches a misspelt key (`hosted-runner`) silently ignored, so the list says less than it reads.
#[test]
fn an_unknown_key_is_refused_naming_it_and_its_table() {
    assert_eq!(
        refusal(&format!("{}services = 1\n", one(GOOD_KEYS))),
        TrustListError::UnknownKey {
            table: "service[0]".to_owned(),
            key: "services".to_owned(),
        }
    );
    assert_eq!(
        refusal(&format!("organisation = \"komira-ai\"\n{}", one(GOOD_KEYS))),
        TrustListError::UnknownKey {
            table: String::new(),
            key: "organisation".to_owned(),
        }
    );
    assert_eq!(
        refusal(&format!(
            "{}\n{}",
            one(GOOD_KEYS),
            one("hosted-runner = true")
        )),
        TrustListError::UnknownKey {
            table: "service[1]".to_owned(),
            key: "hosted-runner".to_owned(),
        }
    );
}

/// Each required key, left out, is refused by its dotted name.
/// Catches a key given a default, which would trust more than the list says.
#[test]
fn every_key_is_required() {
    for key in ["name", "repository", "workflow", "refs", "hosted_runner"] {
        let keys: Vec<&str> = GOOD_KEYS
            .lines()
            .filter(|line| !line.starts_with(&format!("{key} ")))
            .collect();
        assert_eq!(
            refusal(&one(&keys.join("\n"))),
            TrustListError::Invalid {
                key: format!("service[0].{key}"),
                why: "is missing".to_owned(),
            },
            "{key}"
        );
    }
}

/// Each value that is not what its key holds is refused, naming the key and why, with the daemon
/// config's own rules for the signer's keys.
/// Catches a service name or repository taken unchecked, or the signer's keys read by rules of
/// their own.
#[test]
fn each_bad_value_is_refused_naming_its_key() {
    let cases = [
        (
            r#"name = "Js Agent""#,
            "service[0].name",
            "\"Js Agent\" is not a service name",
        ),
        (
            r#"repository = "jobscheduler""#,
            "service[0].repository",
            "is not <owner>/<name>",
        ),
        (
            r#"repository = "komira-ai/""#,
            "service[0].repository",
            "is not <owner>/<name>",
        ),
        (
            r#"repository = "/jobscheduler""#,
            "service[0].repository",
            "is not <owner>/<name>",
        ),
        (
            r#"repository = "komira ai/jobscheduler""#,
            "service[0].repository",
            "is not <owner>/<name>",
        ),
        (
            r#"repository = "komira-ai/job/scheduler""#,
            "service[0].repository",
            "is not <owner>/<name>",
        ),
        (
            r#"workflow = "release workflow.yml""#,
            "service[0].workflow",
            "is not 1 to 256 printable characters without white space",
        ),
        (
            "refs = []",
            "service[0].refs",
            "is empty; at least one ref glob is needed",
        ),
        (
            r#"refs = ["refs/heads/main", "refs/heads/a b"]"#,
            "service[0].refs",
            "holds \"refs/heads/a b\", which is not 1 to 256 printable characters without white space",
        ),
        (
            r#"hosted_runner = "yes""#,
            "service[0].hosted_runner",
            "is a string, not a boolean",
        ),
    ];
    for (line, key, why) in cases {
        let field = line.split(' ').next().expect("a key");
        let keys: Vec<&str> = GOOD_KEYS
            .lines()
            .map(|l| {
                if l.starts_with(&format!("{field} ")) {
                    line
                } else {
                    l
                }
            })
            .collect();
        match refusal(&one(&keys.join("\n"))) {
            TrustListError::Invalid {
                key: got,
                why: said,
            } => {
                assert_eq!(got, key, "{line}");
                assert!(said.contains(why), "{line}: {said}");
            }
            other => panic!("{line}: {other:?}"),
        }
    }
}

/// A service listed twice is refused naming it, even with the same signer.
/// Catches the second entry silently winning or losing, so the list trusts what it does not say.
#[test]
fn a_service_listed_twice_is_refused_naming_it() {
    let text = format!("{}\n{}", one(GOOD_KEYS), one(GOOD_KEYS));
    let error = refusal(&text);
    assert_eq!(error, TrustListError::Twice(service("js-agent")));
    assert_eq!(
        error.to_string(),
        "the trust list lists the service js-agent twice"
    );
}

/// A list with no service, an empty file or a `service` that is not an array of tables, and a
/// text that is not TOML, are each refused.
/// Catches an empty list accepted, so a run syncs nothing and succeeds.
#[test]
fn a_list_of_no_service_or_not_toml_is_refused() {
    assert_eq!(refusal(""), TrustListError::NoService);
    assert_eq!(
        TrustListError::NoService.to_string(),
        "the trust list lists no service; add a [[service]] table for each"
    );
    assert_eq!(
        refusal("service = 1\n"),
        TrustListError::Invalid {
            key: "service".to_owned(),
            why: "is an integer, not an array of tables".to_owned(),
        }
    );
    let syntax = refusal("[[service]]\nname = \n");
    assert!(
        matches!(syntax, TrustListError::Syntax { line: 2, .. }),
        "{syntax:?}"
    );
    assert!(
        syntax
            .to_string()
            .starts_with("the trust list is not TOML: line 2: "),
        "{syntax}"
    );
}

/// The messages name the key and its table.
#[test]
fn the_messages_name_the_key() {
    let unknown = TrustListError::UnknownKey {
        table: "service[0]".to_owned(),
        key: "x".to_owned(),
    };
    assert_eq!(
        unknown.to_string(),
        "the trust list has the unknown key \"x\" in the table \"service[0]\""
    );
    let invalid = TrustListError::Invalid {
        key: "service[0].refs".to_owned(),
        why: "is missing".to_owned(),
    };
    assert_eq!(
        invalid.to_string(),
        "the trust list's service[0].refs is missing"
    );
}

/// A repository is `<owner>/<name>`, its owner the part before the `/`.
#[test]
fn a_repository_has_an_owner() {
    let repository = Repository::new("komira-ai/jobscheduler").expect("a repository");
    assert_eq!(repository.owner(), "komira-ai");
    assert_eq!(repository.to_string(), "komira-ai/jobscheduler");
    for bad in ["", "a", "a/", "/b", "a/b/c", "a b/c", "a/b!"] {
        assert_eq!(Repository::new(bad), None, "{bad:?}");
    }
}
