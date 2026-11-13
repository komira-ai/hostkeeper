//! Trusted publishers: which GitHub Actions runs may set the desired version of a package, held
//! per package in coordinator state (docs/deploy/ci-client.md, "Trusted publishers").
//!
//! A package's entries are `TrustedPublisher`s: a repository (`owner/name`), the workflow file
//! that started the run (`.github/workflows/release.yml`), the ref it ran on (a branch,
//! `main` unless the entry says otherwise) and, only when the entry names one, the environment
//! the job ran in. A CI call sets a package only when its verified claims match one of that
//! package's entries ([`Coordinator::ci_admits`]); a token that matches no entry of any package is
//! refused `publisher` before its `jti` is spent ([`Coordinator::ci`]).
//!
//! The entries live in the ops store's marks of the package ([`crate::ops::ServiceOps`]), so they
//! are in the state files, in Postgres and in the backups as the marks are. `None` is a package
//! whose entries were never held: it is trusted as the allow-list file says
//! ([`Coordinator::with_allow_list`]: [`import`] turns each file entry into the publishers of each
//! package it names). `Some`, empty or not, is the state's own and wins from then on: the first put
//! or remove on a package writes its list, starting from the file's entries, and the file's are
//! not read for it again, so an entry removed stays removed across a restart. Nothing is written at
//! start, so an older release reads the state as it did and a coordinator that never changes a
//! package's list admits what it always admitted.
//!
//! The allow-list file (`--oidc-config`) is read at every start. Its issuer, audience, event,
//! runner and lifetime rules still decide which tokens are checked at all
//! ([`hostkeeper_oidc::Verifier::verify_open`]); what it says of the run (repository,
//! `job_workflow_ref`, refs, environments, services, host groups) is the import's. An imported
//! entry keeps what the file pinned: the repository's numeric id, the reusable workflows
//! (`job_workflow_ref`) it may call, and the host groups it may deploy to. Those three are not
//! editable through the calls below; to drop one, remove the entry and put it again.
//!
//! The calls (`Services.ListTrustedPublishers`, `Services.PutTrustedPublisher`,
//! `Services.RemoveTrustedPublisher`) take the operator token only. Their refusals are those of
//! `Services`: `unauthorized`, `forbidden`, `invalid_request` (the reason, the package's name or an
//! entry's field), `unknown_service`, `publisher_unknown` (a remove of an entry the package does not
//! hold), `store_failed` and `audit_unavailable`. A put or a remove writes one audit entry in the
//! commit of the change (`trusted_publisher_put`, `trusted_publisher_remove`) and takes effect
//! at once: the next CI call is judged on the new list. A refused call changes nothing.

use std::collections::BTreeMap;

use hostkeeper_oidc::{AllowList, OidcConfig, Refusal as OidcRefusal, Verified};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::is_service_name;
use serde::{Deserialize, Serialize};

use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::{AuditEntry, OpsState, ServiceOps, Write};
use crate::service::Coordinator;

type Refusal = proto::ServicesRefusal;

/// The ref of an entry that names none: the branch `main`.
pub const DEFAULT_REF: &str = "refs/heads/main";

/// The most bytes of a repository, of a workflow file, of a ref and of an environment.
const MAX_FIELD: usize = 200;

/// One trusted publisher of a package. An empty field is not matched, which only an imported
/// entry has (a file that names no `repository`, one whose `job_workflow_ref` is a reusable
/// workflow so that the workflow that started the run is not named); an entry made by
/// `PutTrustedPublisher` has a repository, a workflow and a ref.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedPublisher {
    /// The token's `repository` claim: `owner/name`.
    #[serde(default)]
    pub repository: String,
    /// The workflow file that started the run, from the root of the repository: the path of the
    /// token's `workflow_ref` claim.
    #[serde(default)]
    pub workflow: String,
    /// The token's `ref` claim: `refs/heads/<branch>` or another full ref.
    #[serde(default, rename = "ref")]
    pub git_ref: String,
    /// The token's `environment` claim; empty: the run's environment is not matched.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub environment: String,
    /// The token's `repository_id` claim, pinned by an imported entry.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repository_id: String,
    /// The token's `job_workflow_ref`: one of these, when an imported entry pinned any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub called_workflows: Vec<String>,
    /// The host groups a call of this entry may deploy to, when an imported entry limited them;
    /// `None`: any group that lists the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_groups: Option<Vec<String>>,
}

/// How far a rule is down the order of [`TrustedPublisher::broken`].
fn rank(rule: OidcRefusal) -> u8 {
    match rule {
        OidcRefusal::Repository => 0,
        OidcRefusal::Workflow => 1,
        OidcRefusal::Ref => 2,
        _ => 3,
    }
}

fn matches(want: &str, got: Option<&str>) -> bool {
    want.is_empty() || got == Some(want)
}

impl TrustedPublisher {
    /// Whether the run `who` is this publisher's: every claim the entry names equals the
    /// token's.
    pub(crate) fn admits(&self, who: &Verified) -> bool {
        self.broken(who).is_none()
    }

    /// The first rule of the entry the run `who` breaks, in the order of [`Refusal`]: the
    /// repository (or its id), the workflow (the file or the reusable ones), the ref, the
    /// environment; none when the entry admits it.
    fn broken(&self, who: &Verified) -> Option<OidcRefusal> {
        if !(matches(&self.repository, who.repository.as_deref())
            && matches(&self.repository_id, Some(&who.repository_id)))
        {
            Some(OidcRefusal::Repository)
        } else if !(self.starts(who.workflow_ref.as_deref())
            && (self.called_workflows.is_empty()
                || self.called_workflows.contains(&who.job_workflow_ref)))
        {
            Some(OidcRefusal::Workflow)
        } else if !matches(&self.git_ref, who.git_ref.as_deref()) {
            Some(OidcRefusal::Ref)
        } else if !matches(&self.environment, who.environment.as_deref()) {
            Some(OidcRefusal::Environment)
        } else {
            None
        }
    }

    /// Whether the token's `workflow_ref` (`owner/name/<file>@<ref>`) names this entry's workflow
    /// file in this entry's repository; an entry with no workflow file (an imported one) admits any.
    fn starts(&self, workflow_ref: Option<&str>) -> bool {
        self.workflow.is_empty()
            || workflow_ref
                .and_then(|r| r.split_once('@'))
                .is_some_and(|(path, _)| path == format!("{}/{}", self.repository, self.workflow))
    }

    /// The same entry as the four fields name it, the pins of an import aside.
    fn same_run(&self, other: &Self) -> bool {
        (
            &self.repository,
            &self.workflow,
            &self.git_ref,
            &self.environment,
        ) == (
            &other.repository,
            &other.workflow,
            &other.git_ref,
            &other.environment,
        )
    }

    fn to_proto(&self) -> proto::TrustedPublisher {
        proto::TrustedPublisher {
            repository: self.repository.clone(),
            workflow: self.workflow.clone(),
            r#ref: self.git_ref.clone(),
            environment: self.environment.clone(),
            repository_id: self.repository_id.clone(),
            called_workflows: self.called_workflows.clone(),
            limited_to_host_groups: self.host_groups.is_some(),
            host_groups: self.host_groups.clone().unwrap_or_default(),
        }
    }

    fn describe(&self) -> String {
        let environment = if self.environment.is_empty() {
            String::new()
        } else {
            format!(" environment {}", self.environment)
        };
        format!(
            "{} {} {}{environment}",
            self.repository, self.workflow, self.git_ref
        )
    }
}

/// The entries the allow-list `list` stands for, per package: for each entry, each package it
/// names (`legacy` when it names none), each of its `refs` (none: the ref is not matched) and
/// each of its `environments` (none: not matched), one publisher that keeps the entry's
/// repository, repository id, `job_workflow_ref`s and host groups. Entries of one package that
/// differ in nothing but their `job_workflow_ref`s (one per audience, or per pin) are one
/// publisher that lists both; of two that differ in their host groups the first's stand.
pub(crate) fn import(
    list: &AllowList,
    legacy: Option<&str>,
) -> BTreeMap<String, Vec<TrustedPublisher>> {
    let mut held: BTreeMap<String, Vec<TrustedPublisher>> = BTreeMap::new();
    for entry in list.entries() {
        for package in named(entry, legacy) {
            let publishers = held.entry(package).or_default();
            for made in publishers_of_entry(entry) {
                match publishers
                    .iter_mut()
                    .find(|p| p.same_run(&made) && p.repository_id == made.repository_id)
                {
                    Some(have) => {
                        for workflow in made.called_workflows {
                            if !have.called_workflows.contains(&workflow) {
                                have.called_workflows.push(workflow);
                            }
                        }
                    }
                    None => publishers.push(made),
                }
            }
        }
    }
    held
}

fn named(entry: &OidcConfig, legacy: Option<&str>) -> Vec<String> {
    match &entry.services {
        Some(names) => names.clone(),
        None => legacy.map(str::to_owned).into_iter().collect(),
    }
}

fn publishers_of_entry(entry: &OidcConfig) -> Vec<TrustedPublisher> {
    let refs = either(&entry.refs);
    let environments = either(&entry.environments);
    let mut made = Vec::new();
    for git_ref in &refs {
        for environment in &environments {
            made.push(TrustedPublisher {
                repository: entry.repository.clone().unwrap_or_default(),
                workflow: String::new(),
                git_ref: git_ref.clone(),
                environment: environment.clone(),
                repository_id: entry.repository_id.clone(),
                called_workflows: entry.job_workflow_refs.clone(),
                host_groups: entry.host_groups.clone(),
            });
        }
    }
    made
}

/// `list`, or one empty value (not matched) when it is empty.
fn either(list: &[String]) -> Vec<String> {
    if list.is_empty() {
        vec![String::new()]
    } else {
        list.to_vec()
    }
}

/// Whether `text` is 1 to [`MAX_FIELD`] bytes of printable ASCII, with none of `forbidden`.
fn printable(text: &str, forbidden: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_FIELD
        && text
            .bytes()
            .all(|b| b.is_ascii_graphic() && !forbidden.as_bytes().contains(&b))
}

/// The ref `text` names: empty is the branch `main`, a name with no `refs/` is a branch, and a
/// full ref stays as it is.
fn ref_of(text: &str) -> Option<String> {
    let text = if text.is_empty() { "main" } else { text };
    let full = if text.starts_with("refs/") {
        text.to_owned()
    } else {
        format!("refs/heads/{text}")
    };
    let ok = printable(&full, "~^:?*[\\@")
        && !full.contains("..")
        && !full.ends_with(['/', '.'])
        && full.len() > "refs/heads/".len();
    ok.then_some(full)
}

/// The entry the four fields of a request name, if they name one.
fn publisher_of(
    repository: &str,
    workflow: &str,
    git_ref: &str,
    environment: &str,
) -> Option<TrustedPublisher> {
    let slash = repository.split_once('/')?;
    let name = |part: &str| printable(part, "/@\\") && part != "." && part != "..";
    let file = workflow.strip_prefix(".github/workflows/")?;
    let file_ok = printable(file, "/@\\")
        && (file.ends_with(".yml") || file.ends_with(".yaml"))
        && file != ".yml"
        && file != ".yaml";
    let environment_ok =
        environment.is_empty() || (printable(environment, "") && environment.trim() == environment);
    (name(slash.0) && name(slash.1) && file_ok && environment_ok)
        .then(|| TrustedPublisher {
            repository: repository.to_owned(),
            workflow: workflow.to_owned(),
            git_ref: String::new(),
            environment: environment.to_owned(),
            ..TrustedPublisher::default()
        })
        .and_then(|made| ref_of(git_ref).map(|git_ref| TrustedPublisher { git_ref, ..made }))
}

/// The entry the four fields of a list name, for a remove: a field an imported entry leaves empty
/// (the repository, the workflow file, the ref) may be empty; one that is not follows the rules of
/// a put. Not all four are empty.
pub(crate) fn publisher_listed(
    repository: &str,
    workflow: &str,
    git_ref: &str,
    environment: &str,
) -> Option<TrustedPublisher> {
    // The fields that are given are judged as a put judges them, beside stand-ins for those left out.
    let judged = publisher_of(
        if repository.is_empty() {
            "a/b"
        } else {
            repository
        },
        if workflow.is_empty() {
            ".github/workflows/a.yml"
        } else {
            workflow
        },
        if git_ref.is_empty() { "main" } else { git_ref },
        environment,
    )?;
    let given = !(repository.is_empty()
        && workflow.is_empty()
        && git_ref.is_empty()
        && environment.is_empty());
    given.then(|| TrustedPublisher {
        repository: repository.to_owned(),
        workflow: workflow.to_owned(),
        git_ref: if git_ref.is_empty() {
            String::new()
        } else {
            judged.git_ref
        },
        environment: environment.to_owned(),
        ..TrustedPublisher::default()
    })
}

impl Coordinator {
    /// Whether `package` is one the coordinator knows: the rollout file defines it, a host group
    /// lists it, its repository is set, or it holds entries.
    fn package_known(&self, state: &OpsState, package: &str) -> bool {
        self.packages_with_repositories(state).contains(package)
            || self.listed_publishers.contains_key(package)
            || state
                .services
                .get(package)
                .is_some_and(|marks| marks.trusted_publishers.is_some())
    }

    /// The entries `package` has: the state's own when it holds any (`Some`, empty or not), else
    /// those the allow-list file stands for.
    pub(crate) fn publishers_of(&self, state: &OpsState, package: &str) -> Vec<TrustedPublisher> {
        match state
            .services
            .get(package)
            .and_then(|marks| marks.trusted_publishers.as_ref())
        {
            Some(held) => held.clone(),
            None => self
                .listed_publishers
                .get(package)
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Whether some package has an entry that admits `who`; if none does, the rule broken by the
    /// entry that got furthest (a token no entry names is refused for its repository).
    pub(crate) fn judge_publishers(
        &self,
        state: &OpsState,
        who: &Verified,
    ) -> Result<(), OidcRefusal> {
        let held = state
            .services
            .values()
            .flat_map(|marks| marks.trusted_publishers.iter().flatten());
        let listed = self
            .listed_publishers
            .iter()
            .filter(|(package, _)| {
                state
                    .services
                    .get(*package)
                    .is_none_or(|marks| marks.trusted_publishers.is_none())
            })
            .flat_map(|(_, list)| list);
        let mut furthest = OidcRefusal::Repository;
        for publisher in held.chain(listed) {
            match publisher.broken(who) {
                None => return Ok(()),
                Some(rule) if rank(rule) > rank(furthest) => furthest = rule,
                Some(_) => {}
            }
        }
        Err(furthest)
    }

    /// Whether a token of `verified` may set `package`: some entry of the package admits it. The
    /// answer is that entry's host groups (`None` inside: it limits none); `None` outside: no
    /// entry admits the token.
    pub(crate) fn publisher_admits(
        &self,
        verified: &Verified,
        package: &str,
    ) -> Option<Option<Vec<String>>> {
        self.publishers_of(&self.ops.state(), package)
            .iter()
            .find(|publisher| publisher.admits(verified))
            .map(|publisher| publisher.host_groups.clone())
    }

    /// `Services.ListTrustedPublishers`: the entries `package` holds, in the order they were put.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `unknown_service`.
    pub fn list_trusted_publishers(
        &self,
        caller: &Caller,
        request: &proto::ListTrustedPublishersRequest,
    ) -> Result<proto::ListTrustedPublishersResponse, Refusal> {
        caller.operator()?;
        let state = self.ops.state();
        if !is_service_name(&request.service) {
            return Err(Refusal::InvalidRequest);
        }
        if !self.package_known(&state, &request.service) {
            return Err(Refusal::UnknownService);
        }
        Ok(proto::ListTrustedPublishersResponse {
            publishers: self
                .publishers_of(&state, &request.service)
                .iter()
                .map(TrustedPublisher::to_proto)
                .collect(),
        })
    }

    /// `Services.PutTrustedPublisher`: adds the entry the request names to the package's list. An
    /// entry already held (the same repository, workflow, ref and environment) stays as it is.
    /// Audited as `trusted_publisher_put`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `unknown_service`, `store_failed`,
    /// `audit_unavailable`.
    pub fn put_trusted_publisher(
        &self,
        caller: &Caller,
        request: proto::PutTrustedPublisherRequest,
    ) -> Result<proto::PutTrustedPublisherResponse, Refusal> {
        let by = caller.operator()?;
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let made = self.publisher_named(&request.service, || {
            publisher_of(
                &request.repository,
                &request.workflow,
                &request.r#ref,
                &request.environment,
            )
        })?;
        let mut audit = AuditEntry::new(by, "trusted_publisher_put");
        audit.service = Some(request.service.clone());
        audit.reason = Some(format!("{}: {why}", made.describe()));
        let mut after = Vec::new();
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let mut held = self.publishers_of(state, &request.service);
            if !held.iter().any(|have| have.same_run(&made)) {
                held.push(made);
            }
            after = held.iter().map(TrustedPublisher::to_proto).collect();
            let marks: &mut ServiceOps = state.services.entry(request.service.clone()).or_default();
            marks.trusted_publishers = Some(held);
            Ok::<(), Refusal>(())
        })?;
        Ok(proto::PutTrustedPublisherResponse {
            entry: Some(entry_proto(&done)),
            publishers: after,
        })
    }

    /// `Services.RemoveTrustedPublisher`: takes the entry the request names out of the package's
    /// list. The list stays the state's own when it is left empty: the package then admits no CI
    /// call, and a restart does not import for it again. Audited as `trusted_publisher_remove`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `unknown_service`, `publisher_unknown`,
    /// `store_failed`, `audit_unavailable`.
    pub fn remove_trusted_publisher(
        &self,
        caller: &Caller,
        request: proto::RemoveTrustedPublisherRequest,
    ) -> Result<proto::RemoveTrustedPublisherResponse, Refusal> {
        let by = caller.operator()?;
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let named = self.publisher_named(&request.service, || {
            publisher_listed(
                &request.repository,
                &request.workflow,
                &request.r#ref,
                &request.environment,
            )
        })?;
        let mut audit = AuditEntry::new(by, "trusted_publisher_remove");
        audit.service = Some(request.service.clone());
        audit.reason = Some(format!("{}: {why}", named.describe()));
        let mut after = Vec::new();
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let mut held = self.publishers_of(state, &request.service);
            let before = held.len();
            held.retain(|have| !have.same_run(&named));
            if held.len() == before {
                return Err(Refusal::PublisherUnknown);
            }
            after = held.iter().map(TrustedPublisher::to_proto).collect();
            let marks: &mut ServiceOps = state.services.entry(request.service.clone()).or_default();
            marks.trusted_publishers = Some(held);
            Ok::<(), Refusal>(())
        })?;
        Ok(proto::RemoveTrustedPublisherResponse {
            entry: Some(entry_proto(&done)),
            publishers: after,
        })
    }

    /// The entry `named` makes, for a package the coordinator knows.
    fn publisher_named(
        &self,
        service: &str,
        named: impl FnOnce() -> Option<TrustedPublisher>,
    ) -> Result<TrustedPublisher, Refusal> {
        if !is_service_name(service) {
            return Err(Refusal::InvalidRequest);
        }
        let made = named().ok_or(Refusal::InvalidRequest)?;
        if !self.package_known(&self.ops.state(), service) {
            return Err(Refusal::UnknownService);
        }
        Ok(made)
    }
}
