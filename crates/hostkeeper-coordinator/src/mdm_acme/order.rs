//! One ACME order for the MDM name by DNS-01 (`docs/design/coordinator-mdm.md`, section 4.10), on
//! `instant-acme` over the [`Https`] port.
//!
//! [`open_account`] loads the ACME account from its stored credentials, or makes one at the
//! directory; an account made at another directory than the stored one's is made again. [`issue`]
//! orders the name, publishes the challenge's TXT value through the [`DnsProvider`], waits until
//! the provider reports it, answers the challenge, finalizes with a new key and reads the chain.
//! **The TXT record is removed after the order whether it ended well or not**; a removal that
//! fails is logged and does not change the order's answer.
//!
//! The waits are polls on the injected [`Clock`], within `dns_timeout_s` for the record and
//! `acme_timeout_s` for the CA, so a test sleeps nowhere.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::http::{Request, Response};
use instant_acme::{
    Account, AuthorizationStatus, BodyWrapper, BytesResponse, ChallengeType, HttpClient,
    Identifier, NewAccount, NewOrder, OrderStatus,
};

use super::Clock;
use super::dns::{DnsError, DnsProvider, TxtRecord, challenge_record};
use super::https::{HttpRequest, Https};

/// Why an order did not end in a certificate.
#[derive(Debug, thiserror::Error)]
pub enum OrderError {
    /// The DNS provider failed.
    #[error(transparent)]
    Dns(#[from] DnsError),
    /// The ACME server or the exchange with it failed.
    #[error("the ACME exchange failed: {0}")]
    Acme(String),
    /// A wait ran out.
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    /// The name is not the zone or a name under it.
    #[error("the name {name} is not inside the zone {zone}")]
    OutsideZone {
        /// The name.
        name: String,
        /// The zone.
        zone: String,
    },
    /// The authorization offers no DNS-01 challenge.
    #[error("the CA offers no dns-01 challenge for the name")]
    NoDnsChallenge,
    /// The authorization or the order ended invalid.
    #[error("the CA ended the order {0}")]
    Refused(String),
    /// The stored account credentials are not readable.
    #[error("the stored ACME account is not readable")]
    AccountUnreadable,
}

impl OrderError {
    /// Whether the DNS provider refused the credential.
    #[must_use]
    pub fn dns_rejected(&self) -> bool {
        matches!(self, Self::Dns(DnsError::Rejected { .. }))
    }
}

fn acme(why: impl std::fmt::Display) -> OrderError {
    OrderError::Acme(why.to_string())
}

/// What an order is for and how long it waits.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The name to certify.
    pub name: String,
    /// The DNS zone the name lives in.
    pub zone: String,
    /// The ACME directory URL.
    pub directory: String,
    /// The contact address of a new account, if one is set.
    pub contact: Option<String>,
    /// The TTL of the challenge record, seconds.
    pub ttl_s: u32,
    /// The pause between two polls, seconds.
    pub poll_s: u32,
    /// How long the record may take to show in the provider, seconds.
    pub dns_timeout_s: u32,
    /// The pause after the record shows and before the CA is asked, seconds.
    pub settle_s: u32,
    /// How long the CA may take for each of its states, seconds.
    pub acme_timeout_s: u32,
}

/// What an order issued: the chain and the key made for it, both PEM.
pub struct Issued {
    /// The certificate chain, leaf first.
    pub chain_pem: String,
    /// The private key of the leaf.
    pub key_pem: String,
}

/// The ACME client's HTTP, over the port.
struct AcmeHttp(Arc<dyn Https>);

type AcmeReply = Pin<Box<dyn Future<Output = Result<BytesResponse, instant_acme::Error>> + Send>>;

impl HttpClient for AcmeHttp {
    fn request(&self, request: Request<BodyWrapper<Bytes>>) -> AcmeReply {
        let https = Arc::clone(&self.0);
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body = match body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(never) => match never {},
            };
            let headers = parts
                .headers
                .iter()
                .filter_map(|(name, value)| {
                    Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
                })
                .collect();
            let reply = https
                .send(HttpRequest {
                    method: parts.method.as_str().to_owned(),
                    url: parts.uri.to_string(),
                    headers,
                    body: body.to_vec(),
                })
                .await
                .map_err(|why| instant_acme::Error::Other(Box::new(why)))?;
            let mut response = Response::builder().status(reply.status);
            for (name, value) in &reply.headers {
                response = response.header(name, value);
            }
            let response = response
                .body(Full::new(Bytes::from(reply.body)))
                .map_err(instant_acme::Error::Http)?;
            Ok(BytesResponse::from(response))
        })
    }
}

/// The directory a stored credentials JSON was made at.
fn directory_of(credentials: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(credentials).ok()?;
    value.get("directory")?.as_str().map(str::to_owned)
}

/// The account of `stored` credentials when they are for `plan`'s directory, else a new account at
/// it. Answers the account and, when it is new, the credentials to store (JSON, sealed).
///
/// # Errors
/// [`OrderError::AccountUnreadable`]: credentials for this directory that do not load;
/// [`OrderError::Acme`].
pub async fn open_account(
    https: &Arc<dyn Https>,
    plan: &Plan,
    stored: Option<&[u8]>,
) -> Result<(Account, Option<Vec<u8>>), OrderError> {
    let builder = || Account::builder_with_http(Box::new(AcmeHttp(Arc::clone(https))));
    if let Some(stored) = stored
        && directory_of(stored).as_deref() == Some(plan.directory.as_str())
    {
        let credentials =
            serde_json::from_slice(stored).map_err(|_| OrderError::AccountUnreadable)?;
        let account = builder()
            .from_credentials(credentials)
            .await
            .map_err(acme)?;
        return Ok((account, None));
    }
    let contact = plan
        .contact
        .as_ref()
        .map(|address| format!("mailto:{address}"));
    let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
    let (account, credentials) = builder()
        .create(
            &NewAccount {
                contact: &contacts,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            plan.directory.clone(),
            None,
        )
        .await
        .map_err(acme)?;
    let json = serde_json::to_vec(&credentials).map_err(acme)?;
    Ok((account, Some(json)))
}

/// Orders `plan.name` on `account`, proving it through `dns`, and removes the record afterwards.
///
/// # Errors
/// [`OrderError`]; the record is removed on every one.
pub async fn issue(
    account: &Account,
    plan: &Plan,
    dns: &dyn DnsProvider,
    clock: &dyn Clock,
) -> Result<Issued, OrderError> {
    let mut published = Vec::new();
    let outcome = order(account, plan, dns, clock, &mut published).await;
    for record in &published {
        if let Err(why) = dns.remove(record).await {
            tracing::error!(%why, "the ACME challenge record could not be removed");
        }
    }
    outcome
}

async fn order(
    account: &Account,
    plan: &Plan,
    dns: &dyn DnsProvider,
    clock: &dyn Clock,
    published: &mut Vec<TxtRecord>,
) -> Result<Issued, OrderError> {
    let record_name =
        challenge_record(&plan.name, &plan.zone).ok_or_else(|| OrderError::OutsideZone {
            name: plan.name.clone(),
            zone: plan.zone.clone(),
        })?;
    let identifiers = [Identifier::Dns(plan.name.clone())];
    let mut order = account
        .new_order(&NewOrder::new(&identifiers))
        .await
        .map_err(acme)?;
    {
        let mut authorizations = order.authorizations();
        while let Some(next) = authorizations.next().await {
            let mut authorization = next.map_err(acme)?;
            match authorization.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                other => {
                    return Err(OrderError::Refused(format!(
                        "with an authorization {other:?}"
                    )));
                }
            }
            let mut challenge = authorization
                .challenge(ChallengeType::Dns01)
                .ok_or(OrderError::NoDnsChallenge)?;
            let record = TxtRecord {
                zone: plan.zone.clone(),
                name: record_name.clone(),
                value: challenge.key_authorization().dns_value(),
                ttl_s: plan.ttl_s,
            };
            dns.publish(&record).await?;
            published.push(record.clone());
            wait_published(dns, &record, plan, clock).await?;
            clock.sleep(Duration::from_secs(plan.settle_s.into())).await;
            challenge.set_ready().await.map_err(acme)?;
        }
    }
    let wait = Wait::new(clock, plan);
    loop {
        let state = order.refresh().await.map_err(acme)?;
        if let Some(problem) = &state.error {
            return Err(acme(problem));
        }
        match state.status {
            OrderStatus::Ready | OrderStatus::Valid => break,
            OrderStatus::Invalid => return Err(OrderError::Refused("invalid".to_owned())),
            OrderStatus::Pending | OrderStatus::Processing => {
                wait.next("the CA to check the challenge").await?;
            }
        }
    }
    let key_pem = order.finalize().await.map_err(acme)?;
    let wait = Wait::new(clock, plan);
    let chain_pem = loop {
        if let Some(chain) = order.certificate().await.map_err(acme)? {
            break chain;
        }
        wait.next("the CA to issue the certificate").await?;
    };
    Ok(Issued { chain_pem, key_pem })
}

/// Polls the provider until it reports `record`, within `dns_timeout_s`.
async fn wait_published(
    dns: &dyn DnsProvider,
    record: &TxtRecord,
    plan: &Plan,
    clock: &dyn Clock,
) -> Result<(), OrderError> {
    let deadline = clock
        .now_unix()
        .saturating_add(u64::from(plan.dns_timeout_s));
    loop {
        if dns.is_published(record).await? {
            return Ok(());
        }
        if clock.now_unix() >= deadline {
            return Err(OrderError::Timeout("the DNS record to show"));
        }
        clock.sleep(Duration::from_secs(plan.poll_s.into())).await;
    }
}

/// A wait of the CA's states: a poll at a time until its deadline.
struct Wait<'a> {
    clock: &'a dyn Clock,
    deadline: u64,
    poll: Duration,
}

impl<'a> Wait<'a> {
    fn new(clock: &'a dyn Clock, plan: &Plan) -> Self {
        Self {
            clock,
            deadline: clock
                .now_unix()
                .saturating_add(u64::from(plan.acme_timeout_s)),
            poll: Duration::from_secs(plan.poll_s.into()),
        }
    }

    async fn next(&self, what: &'static str) -> Result<(), OrderError> {
        if self.clock.now_unix() >= self.deadline {
            return Err(OrderError::Timeout(what));
        }
        self.clock.sleep(self.poll).await;
        Ok(())
    }
}

#[cfg(test)]
#[path = "order_tests.rs"]
mod tests;
