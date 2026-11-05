//! What the flow works through: the ports, and their real implementation.

use std::time::Duration;

use hostkeeper_ctl::Connect;
use hostkeeper_ctl::transport::Network;
use rustls_pki_types::CertificateDer;

use crate::token::{OidcTokens, TokenError};

/// The outside world of the flow.
pub(crate) trait Ports {
    /// A new token for `audience`; the coordinator accepts each token once.
    fn token(&mut self, audience: &str) -> Result<String, TokenError>;
    /// The transports `hostkeeper ctl` calls through.
    fn connect(&self) -> &dyn Connect;
    /// Waits `secs` seconds.
    fn pause(&mut self, secs: u64);
    /// Prints one line on stdout.
    fn say(&mut self, line: &str);
}

impl<P: Ports + ?Sized> Ports for &mut P {
    fn token(&mut self, audience: &str) -> Result<String, TokenError> {
        (**self).token(audience)
    }

    fn connect(&self) -> &dyn Connect {
        (**self).connect()
    }

    fn pause(&mut self, secs: u64) {
        (**self).pause(secs);
    }

    fn say(&mut self, line: &str) {
        (**self).say(line);
    }
}

/// The real ports: tokens from the GitHub Actions OIDC endpoint, the network transports of
/// `hostkeeper ctl`, the thread's sleep and stdout.
pub(crate) struct NetworkPorts {
    tokens: OidcTokens,
    network: Network,
}

impl NetworkPorts {
    /// Ports whose token request trusts `roots`.
    pub(crate) fn new(roots: &[CertificateDer<'static>]) -> NetworkPorts {
        NetworkPorts {
            tokens: OidcTokens::from_environment(roots),
            network: Network,
        }
    }
}

impl Ports for NetworkPorts {
    fn token(&mut self, audience: &str) -> Result<String, TokenError> {
        self.tokens.token(audience)
    }

    fn connect(&self) -> &dyn Connect {
        &self.network
    }

    fn pause(&mut self, secs: u64) {
        std::thread::sleep(Duration::from_secs(secs));
    }

    fn say(&mut self, line: &str) {
        println!("{line}");
    }
}
