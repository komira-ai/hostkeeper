//! Trusted publishers ([`crate::trusted_publishers`]): which CI runs may set a package, held per
//! package, imported from the allow-list file until the state holds the package's own, and
//! changed by `Services.PutTrustedPublisher` and `Services.RemoveTrustedPublisher`.

use hostkeeper_oidc::AllowList;
use hostkeeper_oidc::testkit::{WORKFLOW, WORKFLOW_REF};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::*;
use crate::marks_support::operator;
use crate::operator::Caller;
use crate::testutil::with_kbf;
use crate::trusted_publishers::{import, publisher_listed};

type Refusal = proto::ServicesRefusal;

const REPO: &str = "example-org/example-repo";
const FILE: &str = ".github/workflows/deploy.yml";
const OTHER_BODY: &str =
    r#"{"service":"other","version":"3.0","min_up":{"storage":0,"frontend":0}}"#;
const OTHER_SERVICE: &str = "[[service]]\nname = \"other\"\nroles = [\"daemon\"]\n";

/// A rig of the services `kbf` (the legacy service) and `other`, whose allow-list is the
/// testkit's: one entry for REPO that names no services.
fn rig(name: &str) -> Rig {
    let mut rig = Rig::new(name);
    rig.coordinator = Rig::start(&rig.dir, &rig.clock).with_services(with_kbf(OTHER_SERVICE));
    rig
}

fn put(
    c: &Coordinator,
    service: &str,
    fields: [&str; 4],
) -> Result<proto::PutTrustedPublisherResponse, Refusal> {
    c.put_trusted_publisher(
        &operator(),
        proto::PutTrustedPublisherRequest {
            service: service.to_owned(),
            repository: fields[0].to_owned(),
            workflow: fields[1].to_owned(),
            r#ref: fields[2].to_owned(),
            environment: fields[3].to_owned(),
            reason: "for a test".to_owned(),
        },
    )
}

fn remove(
    c: &Coordinator,
    service: &str,
    fields: [&str; 4],
) -> Result<proto::RemoveTrustedPublisherResponse, Refusal> {
    c.remove_trusted_publisher(
        &operator(),
        proto::RemoveTrustedPublisherRequest {
            service: service.to_owned(),
            repository: fields[0].to_owned(),
            workflow: fields[1].to_owned(),
            r#ref: fields[2].to_owned(),
            environment: fields[3].to_owned(),
            reason: "for a test".to_owned(),
        },
    )
}

fn list(c: &Coordinator, service: &str) -> Result<Vec<proto::TrustedPublisher>, Refusal> {
    c.list_trusted_publishers(
        &operator(),
        &proto::ListTrustedPublishersRequest {
            service: service.to_owned(),
        },
    )
    .map(|reply| reply.publishers)
}

/// The refusal name `set_desired` answers for `body` with a token whose claims `change` edits.
async fn refused_with(
    c: &Coordinator,
    body: &str,
    change: impl FnOnce(&mut serde_json::Value),
) -> String {
    let mut claims = good_claims(STATIC_ISSUER);
    change(&mut claims);
    match c
        .set_desired(Some(&mint(&keys()[0], &claims)), body.as_bytes())
        .await
    {
        Ok(_) => "set".to_owned(),
        Err(SetError::Unauthorized(why)) => why.to_owned(),
        Err(SetError::Forbidden) => "forbidden".to_owned(),
        Err(other) => panic!("{other}"),
    }
}

/// Proves: a coordinator started with the allow-list file of the release before (the testkit's
/// entry: one repository, one reusable workflow, one ref, one environment, no `services`) and a
/// state that holds no publishers sets what that file let it set: the legacy service, with the
/// token the file admits, and a token of another repository, workflow, ref or environment is
/// refused for the reason the file's own rule gives. The package lists the entry the file stands
/// for, with its pins.
///
/// Catches: a release that locks CI out when it first starts on the old allow-list, or one that
/// admits a run the file refused.
#[tokio::test]
async fn the_allow_list_of_the_release_before_sets_what_it_set() {
    let rig = rig("tp-old-list");
    let c = &rig.coordinator;
    assert_eq!(
        list(c, "kbf").expect("listed"),
        [proto::TrustedPublisher {
            repository: REPO.to_owned(),
            workflow: String::new(),
            r#ref: "refs/heads/main".to_owned(),
            environment: "canary".to_owned(),
            repository_id: "4242".to_owned(),
            called_workflows: vec![WORKFLOW.to_owned()],
            limited_to_host_groups: false,
            host_groups: Vec::new(),
        }]
    );
    assert_eq!(refused_with(c, STATE, |_| {}).await, "set");
    let cases: [(&str, serde_json::Value, &str); 4] = [
        ("repository_id", json!("1"), "repository"),
        ("ref", json!("refs/heads/other"), "ref"),
        ("environment", json!("production"), "environment"),
        (
            "job_workflow_ref",
            json!("owner/repo/.github/workflows/x.yml@refs/heads/main"),
            "workflow",
        ),
    ];
    for (claim, value, why) in cases {
        assert_eq!(
            refused_with(c, STATE, |claims| claims[claim] = value).await,
            why
        );
    }
    // The file stands for kbf alone: it does not set `other`.
    assert_eq!(refused_with(c, OTHER_BODY, |_| {}).await, "forbidden");
}

/// Proves: a publisher put for a package lets a run of that repository, workflow file and branch
/// set it, and only that run: another repository, workflow file, branch or environment is refused,
/// each by its exact refusal, and so is another package. An entry that names no environment admits
/// a run in any.
///
/// Catches: a match that ignores a field of the entry, or an entry that admits other packages.
#[tokio::test]
async fn a_matching_publisher_is_allowed_and_any_other_run_or_package_is_refused() {
    let rig = rig("tp-match");
    let c = &rig.coordinator;
    // Take the file's entry out, so the console's is the only one.
    remove(c, "kbf", [REPO, "", "refs/heads/main", "canary"]).expect("the imported entry");
    assert_eq!(list(c, "kbf").expect("listed"), []);
    assert_eq!(refused_with(c, STATE, |_| {}).await, "repository");

    let done = put(c, "kbf", [REPO, FILE, "", ""]).expect("put");
    let entry = done.entry.expect("audited");
    assert_eq!(entry.action, "trusted_publisher_put");
    assert_eq!(entry.service.as_deref(), Some("kbf"));
    assert_eq!(
        done.publishers,
        [proto::TrustedPublisher {
            repository: REPO.to_owned(),
            workflow: FILE.to_owned(),
            r#ref: "refs/heads/main".to_owned(),
            ..Default::default()
        }]
    );
    assert_eq!(refused_with(c, STATE, |_| {}).await, "set");
    let cases: [(&str, serde_json::Value, &str); 4] = [
        ("repository", json!("example-org/other"), "repository"),
        (
            "workflow_ref",
            json!("example-org/example-repo/.github/workflows/other.yml@refs/heads/main"),
            "workflow",
        ),
        ("ref", json!("refs/heads/dev"), "ref"),
        ("workflow_ref", json!("none"), "workflow"),
    ];
    for (claim, value, want) in cases {
        assert_eq!(
            refused_with(c, STATE, |claims| claims[claim] = value).await,
            want,
            "{claim}"
        );
    }
    assert_eq!(refused_with(c, OTHER_BODY, |_| {}).await, "forbidden");
    assert_eq!(WORKFLOW_REF, format!("{REPO}/{FILE}@refs/heads/main"));

    // An entry that names an environment needs the run in it.
    put(c, "other", [REPO, FILE, "refs/heads/main", "canary"]).expect("put for other");
    assert_eq!(refused_with(c, OTHER_BODY, |_| {}).await, "set");
    assert_eq!(
        refused_with(c, &OTHER_BODY.replace("3.0", "3.1"), |claims| {
            claims["environment"] = json!("production");
        })
        .await,
        "forbidden",
        "the run matches the entry of kbf, which has no environment, but `other` names one"
    );
}

/// Proves: a remove takes effect at once: the run that set the package a moment ago is refused the
/// next time; a restart does not bring the file's entries back, because the state's own (empty)
/// list wins over the file; and the package's entries are the state's, not the file's, once the
/// state holds any.
///
/// Catches: a removed entry that still admits, or one the allow-list file imports again.
#[tokio::test]
async fn a_removed_publisher_is_refused_at_once_and_a_restart_does_not_bring_it_back() {
    let mut rig = rig("tp-remove");
    assert_eq!(refused_with(&rig.coordinator, STATE, |_| {}).await, "set");
    let done = remove(
        &rig.coordinator,
        "kbf",
        [REPO, "", "refs/heads/main", "canary"],
    )
    .expect("removed");
    assert_eq!(
        done.entry.expect("audited").action,
        "trusted_publisher_remove"
    );
    assert_eq!(done.publishers, []);
    assert_eq!(
        refused_with(&rig.coordinator, STATE, |_| {}).await,
        "repository"
    );

    rig.restart();
    assert_eq!(list(&rig.coordinator, "kbf").expect("listed"), []);
    assert_eq!(
        refused_with(&rig.coordinator, STATE, |_| {}).await,
        "repository"
    );

    // The console's entry for another repository replaces the file's for good.
    put(
        &rig.coordinator,
        "kbf",
        ["example-org/new", FILE, "main", ""],
    )
    .expect("put");
    rig.restart();
    assert_eq!(list(&rig.coordinator, "kbf").expect("listed").len(), 1);
    assert_eq!(
        refused_with(&rig.coordinator, STATE, |_| {}).await,
        "repository"
    );
    let new = |claims: &mut serde_json::Value| {
        claims["repository"] = json!("example-org/new");
        claims["workflow_ref"] = json!(format!("example-org/new/{FILE}@refs/heads/main"));
    };
    assert_eq!(refused_with(&rig.coordinator, STATE, new).await, "set");
}

/// Proves: the calls refuse what they should by name: a caller that is no operator, a package the
/// coordinator does not know or a name that is no package, a field that breaks its rule, a reason
/// that is not one, and a remove of an entry the package does not hold; a put of an entry already
/// held changes nothing; the branch is taken as a short name or a full ref.
#[tokio::test]
async fn the_calls_refuse_by_name_and_a_put_of_a_held_entry_changes_nothing() {
    let rig = rig("tp-calls");
    let c = &rig.coordinator;
    let ok = [REPO, FILE, "main", ""];
    for caller in [Caller::Anonymous, Caller::Ci] {
        let want = if matches!(caller, Caller::Ci) {
            Refusal::Forbidden
        } else {
            Refusal::Unauthorized
        };
        let request = proto::PutTrustedPublisherRequest {
            service: "kbf".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            c.put_trusted_publisher(&caller, request)
                .expect_err("caller"),
            want
        );
        let request = proto::RemoveTrustedPublisherRequest {
            service: "kbf".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            c.remove_trusted_publisher(&caller, request)
                .expect_err("caller"),
            want
        );
        let request = proto::ListTrustedPublishersRequest {
            service: "kbf".to_owned(),
        };
        assert_eq!(
            c.list_trusted_publishers(&caller, &request)
                .expect_err("caller"),
            want
        );
    }
    assert_eq!(
        put(c, "nobody", ok).expect_err("unknown"),
        Refusal::UnknownService
    );
    assert_eq!(
        list(c, "nobody").expect_err("unknown"),
        Refusal::UnknownService
    );
    assert_eq!(
        remove(c, "nobody", ok).expect_err("unknown"),
        Refusal::UnknownService
    );
    for service in ["", "Kbf"] {
        assert_eq!(
            put(c, service, ok).expect_err("name"),
            Refusal::InvalidRequest
        );
        assert_eq!(list(c, service).expect_err("name"), Refusal::InvalidRequest);
    }
    let bad: [[&str; 4]; 14] = [
        ["", FILE, "main", ""],
        ["example-org", FILE, "main", ""],
        ["example-org/", FILE, "main", ""],
        ["example-org/..", FILE, "main", ""],
        ["a b/c", FILE, "main", ""],
        [REPO, "", "main", ""],
        [REPO, "release.yml", "main", ""],
        [REPO, ".github/workflows/a/b.yml", "main", ""],
        [REPO, ".github/workflows/a.txt", "main", ""],
        [REPO, ".github/workflows/.yml", "main", ""],
        [REPO, FILE, "a b", ""],
        [REPO, FILE, "refs/heads/", ""],
        [REPO, FILE, "x..y", ""],
        [REPO, FILE, "main", " padded"],
    ];
    for fields in bad {
        assert_eq!(
            put(c, "kbf", fields).expect_err("field"),
            Refusal::InvalidRequest,
            "{fields:?}"
        );
    }
    let long = "x".repeat(300);
    assert_eq!(
        put(c, "kbf", [REPO, FILE, &long, ""]).expect_err("long"),
        Refusal::InvalidRequest
    );
    let mut request = proto::PutTrustedPublisherRequest {
        service: "kbf".to_owned(),
        repository: REPO.to_owned(),
        workflow: FILE.to_owned(),
        ..Default::default()
    };
    assert_eq!(
        c.put_trusted_publisher(&operator(), request.clone())
            .expect_err("reason"),
        Refusal::InvalidRequest
    );
    request.reason = "tab\there".to_owned();
    assert_eq!(
        c.put_trusted_publisher(&operator(), request)
            .expect_err("reason"),
        Refusal::InvalidRequest
    );
    assert_eq!(
        remove(c, "kbf", ["", "", "", ""]).expect_err("empty"),
        Refusal::InvalidRequest
    );
    assert_eq!(
        remove(c, "kbf", [REPO, FILE, "main", ""]).expect_err("absent"),
        Refusal::PublisherUnknown
    );

    let first = put(c, "kbf", ok).expect("put");
    let again = put(c, "kbf", [REPO, FILE, "refs/heads/main", ""]).expect("the same entry");
    assert_eq!(first.publishers, again.publishers);
    assert_eq!(
        again.publishers.len(),
        2,
        "the file's entry and the console's"
    );
    put(c, "kbf", [REPO, FILE, "refs/tags/v1", ""]).expect("a full ref");
    let held = list(c, "kbf").expect("listed");
    assert_eq!(held[2].r#ref, "refs/tags/v1");
    remove(c, "kbf", [REPO, FILE, "refs/tags/v1", ""]).expect("removed");
    assert_eq!(list(c, "kbf").expect("listed").len(), 2);
}

/// Proves: each allow-list entry stands for one publisher of each service it names (the legacy
/// service when it names none, none for `services = []`), for each of its refs and environments;
/// entries of one package that differ in nothing but their reusable workflows are one publisher
/// listing both; the pins (repository id, reusable workflows, host groups) are kept; an entry that
/// names no repository leaves it empty; of two that differ in host groups both stand.
#[test]
fn an_allow_list_is_imported_per_package_with_its_pins() {
    let entry = |id: &str, extra: &str, workflow: &str| {
        format!(
            "[[remote_operator]]\nissuer = \"{STATIC_ISSUER}\"\naudience = \"a\"\nrepository_id = \"{id}\"\n\
             job_workflow_ref = [\"{workflow}\"]\n{extra}\n"
        )
    };
    let wide = "repository = \"o/r\"\nrefs = [\"refs/heads/main\", \"refs/heads/rel\"]\nenvironments = [\"p\", \"q\"]\n";
    let text = [
        entry("7", &format!("{wide}services = [\"a\", \"b\"]"), "w@1"),
        entry("7", &format!("{wide}services = [\"a\"]"), "w@2"),
        entry("7", "environments = [\"p\"]\nservices = []", "w@4"),
        entry(
            "7",
            "refs = [\"refs/heads/main\"]\nhost_groups = [\"lab\"]",
            "w@3",
        ),
        entry("8", "refs = [\"refs/heads/main\"]", "w@3"),
    ]
    .concat();
    let held = import(&AllowList::parse(&text).expect("list"), Some("legacy"));
    assert_eq!(
        held.keys().map(String::as_str).collect::<Vec<_>>(),
        ["a", "b", "legacy"]
    );
    // Two refs by two environments, the two pins of `a` in each, listed once.
    assert_eq!(held["a"].len(), 4);
    assert!(
        held["a"]
            .iter()
            .all(|p| p.called_workflows == ["w@1", "w@2"]
                && p.repository == "o/r"
                && p.repository_id == "7"
                && p.workflow.is_empty())
    );
    assert_eq!(held["b"].len(), 4);
    assert!(held["b"].iter().all(|p| p.called_workflows == ["w@1"]));
    // The entries that name no services stand for the legacy service.
    assert_eq!(held["legacy"].len(), 2);
    assert_eq!(held["legacy"][0].host_groups, Some(vec!["lab".to_owned()]));
    assert_eq!(held["legacy"][1].host_groups, None);
    assert!(
        held["legacy"]
            .iter()
            .all(|p| p.repository.is_empty() && p.git_ref == "refs/heads/main")
    );
    // With no legacy service such an entry stands for nothing.
    let none = import(&AllowList::parse(&text).expect("list"), None);
    assert!(!none.contains_key("legacy"));
}

/// Proves: a remove names an entry as the list shows it: a field an imported entry leaves empty
/// (the repository, the workflow file, the ref) stays empty, one that is given is judged as a put
/// judges it, and four empty fields name nothing.
#[test]
fn a_remove_names_an_entry_as_listed_with_empty_fields_left_empty() {
    let listed = |fields: [&str; 4]| publisher_listed(fields[0], fields[1], fields[2], fields[3]);
    let only_ref = listed(["", "", "main", ""]).expect("a ref alone");
    assert_eq!(
        (only_ref.repository.as_str(), only_ref.git_ref.as_str()),
        ("", "refs/heads/main")
    );
    let no_ref = listed([REPO, FILE, "", "canary"]).expect("an environment, no ref");
    assert_eq!(
        (no_ref.git_ref.as_str(), no_ref.environment.as_str()),
        ("", "canary")
    );
    let repository_alone = listed([REPO, "", "", ""]).expect("a repository alone");
    assert_eq!(repository_alone.workflow, "");
    assert_eq!(listed(["", "", "", ""]), None);
    assert_eq!(listed(["a b/c", "", "", ""]), None);
    assert_eq!(listed(["", "x.yml", "", ""]), None);
}

/// Proves: the allow-list the control plane's coordinator runs with (the four entries of
/// komira-internal's `controlplane/hostkeeper-coordinator/oidc.toml`: tierstore's two workflows,
/// jobscheduler's and hostkeeper's hello) imports as one publisher for each of `tierstore`,
/// `js-agent` and `hello`, each keeping its repository, its repository id and the pins of
/// `set-desired.yml` it called, with tierstore's two entries one publisher listing both pins; and
/// the token of each entry still sets the service the entry named and no other.
///
/// Catches: a CI deploy of the live coordinator that stops working at the first start of this
/// release, or one that gains a service its entry never named.
#[test]
fn the_control_planes_allow_list_imports_as_the_ci_deploys_it_runs_today() {
    let entry = |audience: &str,
                 id: &str,
                 repository: &str,
                 pin: &str,
                 events: &str,
                 service: &str| {
        format!(
            "[[remote_operator]]\nissuer = \"{STATIC_ISSUER}\"\naudience = \"{audience}\"\n\
             repository_id = \"{id}\"\nrepository = \"{repository}\"\n\
             job_workflow_ref = [\"komira-ai/hostkeeper/.github/workflows/set-desired.yml@{pin}\"]\n\
             refs = [\"refs/heads/main\"]\nevent_names = [{events}]\n\
             runner_environment = \"github-hosted\"\nservices = [\"{service}\"]\n"
        )
    };
    let (a, b, c) = (
        "2c767d1842a88edb7120073d2dff3b4bfccc834c",
        "2c684190fbe5b6cfb11f42573e044edb334f27aa",
        "318bcc93c9bcd009d40597c45227278742acc622",
    );
    let text = [
        entry(
            "tierstore-hostkeeper-desired",
            "1440761269",
            "komira-ai/tierstore",
            a,
            "\"workflow_run\", \"workflow_dispatch\"",
            "tierstore",
        ),
        entry(
            "tierstore-hostkeeper-release",
            "1440761269",
            "komira-ai/tierstore",
            b,
            "\"workflow_run\", \"workflow_dispatch\"",
            "tierstore",
        ),
        entry(
            "jobscheduler-hostkeeper-desired",
            "1434971668",
            "komira-ai/jobscheduler",
            c,
            "\"workflow_run\", \"workflow_dispatch\"",
            "js-agent",
        ),
        entry(
            "hostkeeper-coordinator-devbox",
            "1443364626",
            "komira-ai/hostkeeper",
            a,
            "\"workflow_dispatch\"",
            "hello",
        ),
    ]
    .concat();
    let held = import(&AllowList::parse(&text).expect("the file reads"), None);
    let pins = |pins: &[&str]| -> Vec<String> {
        pins.iter()
            .map(|p| format!("komira-ai/hostkeeper/.github/workflows/set-desired.yml@{p}"))
            .collect()
    };
    let want = |repository: &str, id: &str, called: &[&str]| {
        vec![crate::trusted_publishers::TrustedPublisher {
            repository: repository.to_owned(),
            git_ref: "refs/heads/main".to_owned(),
            repository_id: id.to_owned(),
            called_workflows: pins(called),
            ..Default::default()
        }]
    };
    assert_eq!(
        held.keys().map(String::as_str).collect::<Vec<_>>(),
        ["hello", "js-agent", "tierstore"]
    );
    assert_eq!(
        held["tierstore"],
        want("komira-ai/tierstore", "1440761269", &[a, b])
    );
    assert_eq!(
        held["js-agent"],
        want("komira-ai/jobscheduler", "1434971668", &[c])
    );
    assert_eq!(
        held["hello"],
        want("komira-ai/hostkeeper", "1443364626", &[a])
    );
}
