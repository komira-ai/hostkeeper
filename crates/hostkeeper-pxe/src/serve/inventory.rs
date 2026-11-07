//! What `serve` reads from the coordinator for each answer (docs/design/network-install.md,
//! section 3, "Which machines may be installed: the answer set"): through the operator API on
//! the private listener, with the operator token, the inventory (`Enrollment.ListRecords`, each
//! record with its `host_held`, what the coordinator's durable enrollment store holds for its host
//! id, and its `state`, where its approval window stands) and the pending requests (`Enrollment.List`), every page of each. `Hosts.List` is not read:
//! it lists only the hosts heard from since the coordinator started (section 3, "`host_held` is
//! the rule").
//!
//! [`Coordinator::snapshot`] reads both and gives a [`Snapshot`], or a [`ReadError`] when
//! any call fails: the listener does not answer, the token is refused (any status the call ends
//! with), or an answer does not parse (a status of its own from the client, or a host id, role
//! or MAC that is not one). A read that fails gives no snapshot at all, so the caller can only
//! answer with the empty answer set. Nothing is kept from one read to the next.
//!
//! The TLS of every call trusts the coordinator's CA (`--ca-file`) alone, and names the host of
//! `--coordinator`; each call carries the token as `authorization: Bearer <token>`, and is
//! bounded by [`READ_TIMEOUT`].
//!
//! The record's platform (item N14 of the design) is read as `InventoryRecord.platform`:
//! `PLATFORM_UNSPECIFIED` is a platform that was not recorded ([`Record::platform`] `None`), and
//! the two Linux platforms are that [`LinuxPlatform`]. Any other value (`PLATFORM_DARWIN_ARM64`,
//! which the coordinator refuses at `AddRecord`, or a number a newer coordinator sends) is not a
//! platform this server installs: the record is read as one with no platform, and its host id
//! is kept in [`Snapshot::not_installable`] with the value, so the answer set leaves the record
//! out and logs why. One such record never fails the read, and never takes any other record out
//! of the answer set.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::enrollment_client::EnrollmentClient;
use hostkeeper_pxe_seed::inputs::LinuxPlatform;
use hostkeeper_wire::{HostId, RoleName};
use tonic::codegen::http::Uri;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use tonic::{Request, Status};

/// The bound of each call, and of the connection it opens.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The most pages one list is read in: a coordinator whose page tokens never end is refused
/// rather than read for ever. A page holds up to 256 entries.
pub const MAX_PAGES: usize = 1024;

/// An inventory record, as the answer set needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Its host id.
    pub host_id: HostId,
    /// Its role, when it names one.
    pub role: Option<RoleName>,
    /// Its data MACs; none for a Mac's record.
    pub data_macs: Vec<MacAddress>,
    /// A host is enrolled against it.
    pub enrolled: bool,
    /// What the enrollment store holds for its host id; a value this release does not know
    /// reads as `Unspecified`, which says nothing, so the record is left out.
    pub host_held: proto::HostHeld,
    /// Where its approval window stands; a value this release does not know reads as
    /// `Unspecified`, which says nothing, so the record is left out unless it is `Open`.
    pub state: proto::RecordState,
    /// Its platform; `None` when not recorded.
    pub platform: Option<LinuxPlatform>,
}

/// A pending request, as the answer set needs it: the host ids of the records it matches (the
/// one of `MATCHED`, or each of `AMBIGUOUS`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    /// Its id.
    pub request_id: String,
    /// The host ids of the records it matches.
    pub host_ids: Vec<String>,
}

/// One read of the coordinator: every record and every pending request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub records: Vec<Record>,
    pub pending: Vec<Pending>,
    /// The records whose platform is a value this server does not install, by host id, with the
    /// value as the coordinator sent it.
    pub not_installable: BTreeMap<HostId, i32>,
}

/// Why a read of the coordinator failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// A call ended with a status: no answer, a refused token, or an answer that did not
    /// decode.
    #[error("{rpc}: {code:?}: {message}")]
    Call {
        rpc: &'static str,
        code: tonic::Code,
        message: String,
    },
    /// An answer held a value that is not one.
    #[error("{rpc}: {what}")]
    Unparsable { rpc: &'static str, what: String },
    /// A list did not end within [`MAX_PAGES`] pages.
    #[error("{rpc}: no last page within {MAX_PAGES} pages")]
    TooManyPages { rpc: &'static str },
}

/// Why the coordinator client could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectError {
    /// `--coordinator` is not `https://<host>[:<port>]`.
    #[error("--coordinator {0:?} is not https://<host>[:<port>]")]
    Endpoint(String),
    /// The CA or the TLS configuration was refused.
    #[error("--coordinator {endpoint}: {why}")]
    Tls { endpoint: String, why: String },
}

const LIST_RECORDS: &str = "Enrollment.ListRecords";
const LIST_PENDING: &str = "Enrollment.List";

impl Snapshot {
    /// The snapshot of the two lists as the coordinator answered them.
    ///
    /// # Errors
    /// [`ReadError::Unparsable`] for a record whose host id, role or a data MAC is not one.
    pub fn of(
        records: Vec<proto::InventoryRecord>,
        pending: Vec<proto::PendingEnrollment>,
    ) -> Result<Snapshot, ReadError> {
        let read = records
            .into_iter()
            .map(record_of)
            .collect::<Result<Vec<_>, String>>()
            .map_err(|what| ReadError::Unparsable {
                rpc: LIST_RECORDS,
                what,
            })?;
        let mut records = Vec::new();
        let mut not_installable = BTreeMap::new();
        for (record, platform) in read {
            if let Platform::Other(value) = platform {
                not_installable.insert(record.host_id.clone(), value);
            }
            records.push(record);
        }
        Ok(Snapshot {
            records,
            not_installable,
            pending: pending
                .into_iter()
                .map(|request| {
                    let mut host_ids = request.candidate_host_ids;
                    host_ids.extend(
                        (!request.record_host_id.is_empty()).then_some(request.record_host_id),
                    );
                    Pending {
                        request_id: request.request_id,
                        host_ids,
                    }
                })
                .collect(),
        })
    }
}

/// What a record states as its platform.
enum Platform {
    /// Nothing, or a Linux platform: [`Record::platform`] says which.
    Installable,
    /// A value this server does not install.
    Other(i32),
}

/// The record of `record`, with the kind of its platform, or what of it is not well-formed.
fn record_of(record: proto::InventoryRecord) -> Result<(Record, Platform), String> {
    let host_id = record
        .host_id
        .parse::<HostId>()
        .map_err(|_| format!("the record host id {:?} is not a host id", record.host_id))?;
    let role = match record.role.as_str() {
        "" => None,
        role => Some(role.parse::<RoleName>().map_err(|_| {
            format!("the record {host_id} names the role {role:?}, which is not a role")
        })?),
    };
    let data_macs = record
        .data_macs
        .iter()
        .map(|mac| {
            MacAddress::parse(mac)
                .map_err(|_| format!("the record {host_id} holds {mac:?}, which is not a MAC"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let (platform, kind) = match agent::Platform::try_from(record.platform) {
        Ok(agent::Platform::Unspecified) => (None, Platform::Installable),
        Ok(agent::Platform::LinuxX8664) => (Some(LinuxPlatform::X86_64), Platform::Installable),
        Ok(agent::Platform::LinuxArm64) => (Some(LinuxPlatform::Arm64), Platform::Installable),
        Ok(agent::Platform::DarwinArm64) | Err(_) => (None, Platform::Other(record.platform)),
    };
    Ok((
        Record {
            host_id,
            role,
            data_macs,
            enrolled: record.enrolled,
            host_held: record.host_held(),
            state: record.state(),
            platform,
        },
        kind,
    ))
}

/// The coordinator's operator API, as `serve` reads it.
#[derive(Clone, Debug)]
pub struct Coordinator {
    channel: Channel,
    bearer: MetadataValue<Ascii>,
}

impl Coordinator {
    /// The client of `endpoint` (`https://<host>[:<port>]`), trusting the PEM CA `ca_pem` alone
    /// and sending `token`. It connects at its first call, and again after a failed one. It
    /// must be made inside the runtime it is used in.
    ///
    /// # Errors
    /// [`ConnectError`]: the endpoint is not one, or the TLS configuration is refused.
    pub fn new(endpoint: &str, ca_pem: &[u8], token: &str) -> Result<Coordinator, ConnectError> {
        let bad = || ConnectError::Endpoint(endpoint.to_owned());
        let uri = endpoint.parse::<Uri>().map_err(|_| bad())?;
        // `https://`, an authority with no user, and nothing after it.
        let host = uri
            .host()
            .filter(|_| uri.scheme_str() == Some("https"))
            .filter(|_| uri.to_string() == format!("{endpoint}/"))
            .filter(|_| !endpoint.contains('@'))
            .ok_or_else(bad)?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(ca_pem))
            .domain_name(host);
        let failed = |e: tonic::transport::Error| ConnectError::Tls {
            endpoint: endpoint.to_owned(),
            why: e.to_string(),
        };
        let channel = Endpoint::from(uri)
            .connect_timeout(READ_TIMEOUT)
            .timeout(READ_TIMEOUT)
            .tls_config(tls)
            .map_err(failed)?
            .connect_lazy();
        let bearer = format!("Bearer {token}")
            .parse()
            .map_err(|_| ConnectError::Tls {
                endpoint: endpoint.to_owned(),
                why: "the token cannot be sent as a header".to_owned(),
            })?;
        Ok(Coordinator { channel, bearer })
    }

    /// A request carrying the token.
    fn request<T>(&self, message: T) -> Request<T> {
        let mut request = Request::new(message);
        request
            .metadata_mut()
            .insert("authorization", self.bearer.clone());
        request.set_timeout(READ_TIMEOUT);
        request
    }

    /// The records and the pending requests, read now.
    ///
    /// # Errors
    /// [`ReadError`]: a call failed, or an answer does not parse.
    pub async fn snapshot(&self) -> Result<Snapshot, ReadError> {
        let records = pages(LIST_RECORDS, |page_token| {
            let mut client = EnrollmentClient::new(self.channel.clone());
            let request = self.request(proto::ListRecordsRequest {
                limit: 0,
                page_token,
                enrolled: None,
            });
            async move {
                let page = client.list_records(request).await?.into_inner();
                Ok((page.records, page.next_page_token))
            }
        })
        .await?;
        let pending = pages(LIST_PENDING, |page_token| {
            let mut client = EnrollmentClient::new(self.channel.clone());
            let request = self.request(proto::ListPendingRequest {
                limit: 0,
                page_token,
                unenrolled: false,
            });
            async move {
                let page = client.list(request).await?.into_inner();
                Ok((page.requests, page.next_page_token))
            }
        })
        .await?;
        Snapshot::of(records, pending)
    }
}

/// Every entry of the list `rpc`, whose page after the token it is given `call` reads, until a
/// page names no next token.
async fn pages<T, F, Fut>(rpc: &'static str, mut call: F) -> Result<Vec<T>, ReadError>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<(Vec<T>, String), Status>>,
{
    let mut entries = Vec::new();
    let mut token = String::new();
    for _ in 0..MAX_PAGES {
        let (page, next) = call(token).await.map_err(|status| ReadError::Call {
            rpc,
            code: status.code(),
            message: status.message().to_owned(),
        })?;
        entries.extend(page);
        if next.is_empty() {
            return Ok(entries);
        }
        token = next;
    }
    Err(ReadError::TooManyPages { rpc })
}
