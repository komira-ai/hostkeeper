//! GoDaddy as the DNS provider of the challenge (`docs/design/coordinator-mdm.md`, section 4.10),
//! over the v1 domains API: `GET`, `PUT` and `DELETE` of `/v1/domains/{zone}/records/TXT/{name}`.
//!
//! **A `PUT` replaces every TXT record at its name.** So [`GoDaddy::publish`] reads the records
//! there and writes them back with the new value among them, and [`GoDaddy::remove`] writes the
//! rest back, or deletes the name when nothing else is there. Another TXT value at the challenge
//! name (a second order, a record an operator made) is kept.
//!
//! **Rate limit.** The provider allows 60 calls a minute for a credential; [`RateLimit`] holds the
//! calls of the last minute and sleeps on the injected [`Clock`] when the next would be the 61st.
//!
//! **Not measured.** The add and delete calls, the shortest TTL the zone accepts and how long a
//! record takes to show were never run against the live zone (the spike could not write DNS), so
//! the TTL and the poll are settings (`dns_ttl_s`, `dns_poll_s`, `dns_timeout_s`) and the
//! visibility is read back from this API (`GET`), not from a resolver. The error text of a call
//! holds the status and never the reply's body or the credential.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_mdm_push::BoxFuture;
use serde::{Deserialize, Serialize};

use super::Clock;
use super::dns::{
    Authorization, CredentialError, DnsCredential, DnsError, DnsProvider, DnsProviders, TxtRecord,
};
use super::https::{HttpReply, HttpRequest, Https};

/// The API's address.
pub const BASE: &str = "https://api.godaddy.com";

/// The provider's name in a credential.
pub const PROVIDER: &str = "godaddy";

/// The calls the provider allows a credential in a minute.
pub const CALLS_PER_MINUTE: usize = 60;

/// The calls of the last minute.
#[derive(Debug)]
pub struct RateLimit {
    per_minute: usize,
    calls: Mutex<VecDeque<u64>>,
}

impl RateLimit {
    /// A limit of `per_minute` calls in any 60 seconds.
    #[must_use]
    pub fn new(per_minute: usize) -> Self {
        Self {
            per_minute,
            calls: Mutex::new(VecDeque::new()),
        }
    }

    /// Returns when one more call is within the limit, and counts it.
    pub async fn take(&self, clock: &dyn Clock) {
        loop {
            let now = clock.now_unix();
            let wait = {
                let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
                while calls.front().is_some_and(|at| at.saturating_add(60) <= now) {
                    calls.pop_front();
                }
                match calls.front() {
                    Some(oldest) if calls.len() >= self.per_minute => {
                        Some(oldest.saturating_add(60).saturating_sub(now).max(1))
                    }
                    _ => {
                        calls.push_back(now);
                        None
                    }
                }
            };
            match wait {
                None => return,
                Some(seconds) => clock.sleep(Duration::from_secs(seconds)).await,
            }
        }
    }
}

/// Opens [`GoDaddy`] for a `godaddy` credential.
pub struct GoDaddyProviders {
    https: Arc<dyn Https>,
    clock: Arc<dyn Clock>,
    limit: Arc<RateLimit>,
}

impl GoDaddyProviders {
    /// Providers that send through `https`, limited to [`CALLS_PER_MINUTE`] by `clock`.
    #[must_use]
    pub fn new(https: Arc<dyn Https>, clock: Arc<dyn Clock>) -> Self {
        Self {
            https,
            clock,
            limit: Arc::new(RateLimit::new(CALLS_PER_MINUTE)),
        }
    }
}

impl DnsProviders for GoDaddyProviders {
    fn open(&self, credential: &DnsCredential) -> Result<Arc<dyn DnsProvider>, CredentialError> {
        if credential.provider != PROVIDER {
            return Err(CredentialError::UnknownProvider(
                credential.provider.clone(),
            ));
        }
        Ok(Arc::new(GoDaddy {
            https: Arc::clone(&self.https),
            clock: Arc::clone(&self.clock),
            limit: Arc::clone(&self.limit),
            authorization: credential.authorization.clone(),
        }))
    }
}

/// One TXT record as the API spells it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Entry {
    data: String,
    #[serde(default)]
    ttl: u32,
}

/// The v1 domains API of GoDaddy.
pub struct GoDaddy {
    https: Arc<dyn Https>,
    clock: Arc<dyn Clock>,
    limit: Arc<RateLimit>,
    authorization: Authorization,
}

fn failed(why: impl std::fmt::Display) -> DnsError {
    DnsError::Failed(why.to_string())
}

/// `value` as the API holds it: some zones return a TXT value in quotes.
fn bare(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(value)
}

/// Letters, digits, `.`, `-` and `_`: what a zone or a record name holds, and safe in a path.
fn path_safe(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

impl GoDaddy {
    fn url(record: &TxtRecord) -> Result<String, DnsError> {
        if !path_safe(&record.zone) || !path_safe(&record.name) {
            return Err(failed(
                "the zone or the record name has a character a path cannot hold",
            ));
        }
        Ok(format!(
            "{BASE}/v1/domains/{}/records/TXT/{}",
            record.zone, record.name
        ))
    }

    async fn call(
        &self,
        method: &str,
        record: &TxtRecord,
        body: Option<&[Entry]>,
    ) -> Result<HttpReply, DnsError> {
        let url = Self::url(record)?;
        let mut headers = vec![
            ("authorization".to_owned(), self.authorization.header()),
            ("accept".to_owned(), "application/json".to_owned()),
        ];
        let body = match body {
            Some(entries) => {
                headers.push(("content-type".to_owned(), "application/json".to_owned()));
                serde_json::to_vec(entries).map_err(failed)?
            }
            None => Vec::new(),
        };
        self.limit.take(&*self.clock).await;
        let reply = self
            .https
            .send(HttpRequest {
                method: method.to_owned(),
                url,
                headers,
                body,
            })
            .await
            .map_err(failed)?;
        match reply.status {
            401 | 403 => Err(DnsError::Rejected {
                status: reply.status,
            }),
            200..=299 => Ok(reply),
            status => Err(failed(format!("the provider answered status {status}"))),
        }
    }

    async fn read(&self, record: &TxtRecord) -> Result<Vec<Entry>, DnsError> {
        let reply = self.call("GET", record, None).await?;
        serde_json::from_slice(&reply.body).map_err(|_| failed("the records are not understood"))
    }
}

impl DnsProvider for GoDaddy {
    fn publish<'a>(&'a self, record: &'a TxtRecord) -> BoxFuture<'a, Result<(), DnsError>> {
        Box::pin(async move {
            let mut entries = self.read(record).await?;
            if entries
                .iter()
                .any(|entry| bare(&entry.data) == record.value)
            {
                return Ok(());
            }
            entries.push(Entry {
                data: record.value.clone(),
                ttl: record.ttl_s,
            });
            self.call("PUT", record, Some(&entries)).await.map(drop)
        })
    }

    fn is_published<'a>(&'a self, record: &'a TxtRecord) -> BoxFuture<'a, Result<bool, DnsError>> {
        Box::pin(async move {
            let entries = self.read(record).await?;
            Ok(entries
                .iter()
                .any(|entry| bare(&entry.data) == record.value))
        })
    }

    fn remove<'a>(&'a self, record: &'a TxtRecord) -> BoxFuture<'a, Result<(), DnsError>> {
        Box::pin(async move {
            let entries = self.read(record).await?;
            let rest: Vec<Entry> = entries
                .iter()
                .filter(|entry| bare(&entry.data) != record.value)
                .cloned()
                .collect();
            if rest.len() == entries.len() {
                return Ok(());
            }
            if rest.is_empty() {
                self.call("DELETE", record, None).await.map(drop)
            } else {
                self.call("PUT", record, Some(&rest)).await.map(drop)
            }
        })
    }
}

#[cfg(test)]
#[path = "godaddy_tests.rs"]
mod tests;
