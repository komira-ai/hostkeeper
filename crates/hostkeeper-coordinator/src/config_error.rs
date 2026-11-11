//! Why `serve` did not start ([`StartError`]), kept apart so that `config.rs` stays short.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

use hostkeeper_oidc::{ConfigError, KeySourceError};
use hostkeeper_tls::TlsError;
use hostkeeper_wire::identity::PrefixError;

use crate::accounts::AccountsError;
use crate::desired::StoreError;
use crate::enrollment::{AllowlistError, SetupError};
use crate::inventory::WindowAboveMax;
use crate::jti_file::JtiError;
use crate::mdm::MdmError;
use crate::ops::OpsError;
use crate::origin_https::OriginTlsError;
use crate::store::postgres::DatabaseError;
use crate::tokens::TokensError;
use crate::ui::UiDirError;
use crate::verdict_file::VerdictsError;

/// Why the coordinator did not start, for the operator.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The operator token file is refused.
    #[error("--operator-token-file: {0}")]
    OperatorToken(#[source] hostkeeper_wire::secret::SecretError),
    /// The tokens file is refused.
    #[error(transparent)]
    Tokens(#[from] TokensError),
    /// `--tokens-file` and `--operator-token-file` are both given.
    #[error(
        "--tokens-file and --operator-token-file are both given: name the operator's tokens one way"
    )]
    TokenFlags,
    /// Neither is given.
    #[error("one of --tokens-file and --operator-token-file is required")]
    NoTokenFlag,
    /// `--private-listen` is a wildcard address.
    #[error(
        "--private-listen: {0} is a wildcard address; the private listener takes CI's calls and the operator's, so name the address of one interface"
    )]
    PrivateListen(SocketAddr),
    /// `--lan-listen` is a wildcard address.
    #[error(
        "--lan-listen: {0} is a wildcard address; the LAN listener's address is named in the server certificate, so name the address of one interface"
    )]
    LanListen(SocketAddr),
    /// `--health-listen` is not a loopback address.
    #[error(
        "--health-listen: {0} is not a loopback address; the health listener answers with no token and no TLS, so name an address of 127.0.0.0/8 or ::1"
    )]
    HealthListen(SocketAddr),
    /// The allow-list of CI's OIDC token is refused.
    #[error(transparent)]
    Oidc(#[from] ConfigError),
    /// The two certificate URI prefixes are refused.
    #[error("--role-uri-prefix and --host-role-uri-prefix: {0}")]
    Prefixes(#[from] PrefixError),
    /// `--rollout-file` is given: `serve` does not read it.
    #[error(
        "--rollout-file is not read by serve: convert it once with hostkeeper-coordinator host-groups migrate (docs/operations/hostgroups.md)"
    )]
    RolloutFileRetired,
    /// An allow-list entry names no services and there is no legacy service for it to stand for.
    #[error("--oidc-config: entry {0} names no services and --legacy-service is not set")]
    OidcEntryUnnamed(usize),
    /// An allow-list entry's `host_groups` holds a name that is not a host group name.
    #[error(
        "--oidc-config: entry {entry} names the host group {name:?}, which is not a host group name (1 to 32 bytes of a-z, 0-9 and -, starting with a letter, not ending with - or holding --)"
    )]
    OidcEntryHostGroup {
        /// The entry, counting from 1.
        entry: usize,
        /// The name it gives.
        name: String,
    },
    /// The OIDC key fetcher could not be made.
    #[error("the issuer's keys: {0}")]
    Keys(#[source] KeySourceError),
    /// The desired-state file is refused.
    #[error(transparent)]
    Desired(#[from] StoreError),
    /// The ops file is refused.
    #[error("--ops-file: {0}")]
    Ops(#[from] OpsError),
    /// A TLS file is refused.
    #[error(transparent)]
    Tls(#[from] TlsError),
    /// Enrollment did not start: its CA, its store or the server certificate.
    #[error(transparent)]
    Enrollment(#[from] SetupError),
    /// The Mac serial allowlist is refused.
    #[error(transparent)]
    MacAllowlist(#[from] AllowlistError),
    /// An entry of the Mac serial allowlist names services, which the coordinator refuses: it
    /// places hosts by host group, and the entry's group decides what the Mac runs.
    #[error(
        "--mac-serial-allowlist: the entry of serial {0} names services; the coordinator places hosts by host group, and the Mac's group decides what it runs: name its host_group alone"
    )]
    MacServicesRetired(String),
    /// An entry of the Mac serial allowlist names a host group the coordinator does not hold.
    #[error("--mac-serial-allowlist names host group {0}, which the coordinator does not hold")]
    MacHostGroup(String),
    /// `--ui-dir` does not name a directory.
    #[error("--ui-dir: {0}")]
    UiDir(#[from] UiDirError),
    /// `--record-approve-for` is longer than `--record-approve-max`.
    #[error(transparent)]
    RecordWindow(#[from] WindowAboveMax),
    /// A socket, the runtime or the signal handlers failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Another coordinator holds the lock of the state files.
    #[error(
        "{path}: another coordinator holds the lock of these state files; one coordinator runs on them at a time"
    )]
    Locked {
        /// The lock file.
        path: PathBuf,
    },
    /// The lock of the state files could not be taken.
    #[error("{path}: the lock of the state files could not be taken: {source}")]
    Lock {
        /// The lock file.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// A halt was being written to the ops file when the coordinator stopped, and may not have
    /// landed.
    #[error(
        "{path}: a halt was being written to the ops file when the coordinator stopped and may be lost; read the halts file, settle it by hand, and remove this file to start"
    )]
    HaltWriteUnfinished {
        /// The marker file.
        path: PathBuf,
    },
    /// Changes were in memory and not yet written to the ops file when the coordinator stopped,
    /// and the ops file does not hold them.
    #[error(
        "{path}: the changes numbered {} were in memory and not yet written to the ops file when the coordinator stopped, and it does not hold them; look each up by its number in the coordinator's log (target hostkeeper_coordinator::audit), make it again by hand if it still matters (a halt is raised again by the host's next report while the host is up), and remove this file to start",
        numbers.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")
    )]
    ChangesUnwritten {
        /// The marker file.
        path: PathBuf,
        /// The audit numbers of the changes that did not land.
        numbers: Vec<u64>,
    },
    /// A restore into these state files stopped before it finished.
    #[error(
        "{path}: a restore into these state files stopped before it finished, so they may hold part of a snapshot; remove them and this file, and restore again"
    )]
    RestoreUnfinished {
        /// The marker file.
        path: PathBuf,
    },
    /// A `host-groups migrate --apply` on these state files stopped before it finished.
    #[error(
        "{path}: a host-groups migrate --apply stopped partway; restore the backup taken before it and run it again"
    )]
    MigrateUnfinished {
        /// The marker file.
        path: PathBuf,
    },
    /// `--quiet-secs` is below two heartbeat intervals.
    #[error(
        "--quiet-secs: {secs} is below {floor}, two heartbeat intervals: a restarted coordinator would order before it has heard the fleet"
    )]
    QuietBelowFloor {
        /// What the flag said.
        secs: u64,
        /// The floor.
        floor: u64,
    },
    /// The file of spent token ids is refused.
    #[error(transparent)]
    Jti(#[from] JtiError),
    /// The file of verdicts is refused.
    #[error(transparent)]
    Verdicts(#[from] VerdictsError),
    /// The file of users and API tokens is refused.
    #[error(transparent)]
    Accounts(#[from] AccountsError),
    /// The TLS client for the repository origins could not be made.
    #[error("repository origins: {0}")]
    OriginTls(#[from] OriginTlsError),
    /// The MDM policy gate did not start.
    #[error(transparent)]
    Mdm(#[from] MdmError),
    /// `--database-dsn-file` is given beside a flag of the file store.
    #[error(
        "--database-dsn-file and {0} are both given: {0} names a file of the file store, and the database holds what it held"
    )]
    DatabaseAndFileFlag(&'static str),
    /// `--ca-key-file` is given without a store for the enrollment records.
    #[error(
        "--ca-key-file needs --enrollment-dir, or --database-dsn-file, to keep the enrollment store"
    )]
    NoEnrollmentStore,
    /// The database could not be opened, or holds what this build refuses.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// The database's session was lost and, once it was back, another coordinator had led the
    /// database meanwhile: this coordinator's memory is stale, and it stops.
    #[error(
        "another coordinator led the database while this one's session was lost (epoch {found}, this one's {epoch}): this coordinator stops, and its supervisor starts it again on the database as that one left it"
    )]
    Deposed {
        /// The epoch this coordinator held.
        epoch: u64,
        /// The epoch it found.
        found: u64,
    },
}
