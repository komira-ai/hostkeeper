//! The trust list of `release channel-sync` (docs/deploy/release-channel.md): the services the
//! release channel stages, each from one repository and signed by one workflow.
//!
//! ```toml
//! [[service]]
//! name          = "js-agent"
//! repository    = "komira-ai/jobscheduler"
//! workflow      = ".github/workflows/release.yml"
//! refs          = ["refs/heads/main"]
//! hosted_runner = true
//! ```
//!
//! `workflow`, `refs` and `hosted_runner` are the keys of the daemon config's
//! `[trusted_signers]`, read by the same code into the same type, [`TrustedSigners`], whose
//! `organisation` is the owner of `repository`; so a signer the trust list accepts is described
//! as the daemons describe theirs, and the two cannot drift apart. Every key is required, an
//! unknown key is refused naming it, a service is listed once, and a list with no service is
//! refused.

use crate::daemon::{TrustedSigners, signer_keys};
use crate::names::ServiceName;
use crate::table::{self, Fields, Problem, Shape};

const SERVICE: &[(&str, Shape)] = &[
    ("name", Shape::Value),
    ("repository", Shape::Value),
    ("workflow", Shape::Value),
    ("refs", Shape::Value),
    ("hosted_runner", Shape::Value),
];

const TOP: &[(&str, Shape)] = &[("service", Shape::Tables(SERVICE))];

/// Why a trust list was refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TrustListError {
    #[error("the trust list is not TOML: line {line}: {message}")]
    Syntax { line: usize, message: String },
    /// `table` is `""` for the top level, `service[<n>]` for the `n`th service from 0.
    #[error("the trust list has the unknown key {key:?} in the table {table:?}")]
    UnknownKey { table: String, key: String },
    /// `key` is dotted (`service[0].refs`).
    #[error("the trust list's {key} {why}")]
    Invalid { key: String, why: String },
    #[error("the trust list lists no service; add a [[service]] table for each")]
    NoService,
    #[error("the trust list lists the service {0} twice")]
    Twice(ServiceName),
}

impl From<Problem> for TrustListError {
    fn from(problem: Problem) -> TrustListError {
        match problem {
            Problem::Unknown { table, key } => TrustListError::UnknownKey { table, key },
            Problem::Invalid { key, why } => TrustListError::Invalid { key, why },
        }
    }
}

/// A GitHub repository, `<owner>/<name>`: each part 1 or more of A-Z, a-z, 0-9, `-`, `_`, `.`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Repository(String);

impl Repository {
    /// `text` as a repository, `None` when it is not `<owner>/<name>`.
    pub fn new(text: &str) -> Option<Repository> {
        let part = |p: &str| {
            !p.is_empty()
                & p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() | matches!(b, b'-' | b'_' | b'.'))
        };
        let (owner, name) = text.split_once('/')?;
        (part(owner) & part(name)).then(|| Repository(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The owner: the organisation the repository belongs to.
    pub fn owner(&self) -> &str {
        self.0.split('/').next().unwrap_or_default()
    }
}

impl std::fmt::Display for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One service of the trust list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedService {
    /// The service, as its release index names it.
    pub service: ServiceName,
    /// The one repository whose releases may carry it.
    pub repository: Repository,
    /// Whose attestation its packages must carry; `organisation` is the repository's owner.
    pub signers: TrustedSigners,
}

/// The trust list, loaded and checked: its services in the order of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelTrust {
    services: Vec<TrustedService>,
}

fn service(at: usize, table: &toml::Table) -> Result<TrustedService, Problem> {
    let name = format!("service[{at}]");
    let fields = Fields::new(&name, table);
    let service = ServiceName::new(fields.string("name")?)
        .map_err(|e| fields.invalid("name", e.to_string()))?;
    let repository = Repository::new(fields.string("repository")?)
        .ok_or_else(|| fields.invalid("repository", "is not <owner>/<name>"))?;
    let signers = signer_keys(&fields, repository.owner().to_owned())?;
    Ok(TrustedService {
        service,
        repository,
        signers,
    })
}

impl ChannelTrust {
    /// Reads the text of a trust list: the syntax, then every key against the tables that may
    /// hold it, then each service in order.
    pub fn parse(text: &str) -> Result<ChannelTrust, TrustListError> {
        let document = table::parse(text)
            .map_err(|(line, message)| TrustListError::Syntax { line, message })?;
        if let Some(problem) = table::unknown_key("", &document, TOP) {
            return Err(problem.into());
        }
        let top = Fields::new("", &document);
        let mut services: Vec<TrustedService> = Vec::new();
        for (at, table) in top.tables("service")?.into_iter().enumerate() {
            let next = service(at, table)?;
            if services.iter().any(|s| s.service == next.service) {
                return Err(TrustListError::Twice(next.service));
            }
            services.push(next);
        }
        if services.is_empty() {
            return Err(TrustListError::NoService);
        }
        Ok(ChannelTrust { services })
    }

    /// Every service, in the order of the file.
    pub fn services(&self) -> &[TrustedService] {
        &self.services
    }

    /// The entry of `service`, `None` when the list does not list it.
    pub fn service(&self, service: &ServiceName) -> Option<&TrustedService> {
        self.services.iter().find(|s| s.service == *service)
    }

    /// Every repository the list names, once each, in the order each is first named.
    pub fn repositories(&self) -> Vec<&Repository> {
        let mut repositories: Vec<&Repository> = Vec::new();
        for s in &self.services {
            if !repositories.contains(&&s.repository) {
                repositories.push(&s.repository);
            }
        }
        repositories
    }
}
