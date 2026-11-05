//! What the flow works through: the ports, and their real implementation.

use std::io::{self, Write};
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
/// `hostkeeper ctl`, the thread's sleep, and lines written to `out` (stdout, where the runner
/// reads its log and its `::add-mask::` and `::error::` commands).
pub(crate) struct NetworkPorts {
    tokens: OidcTokens,
    network: Network,
    out: Box<dyn Write>,
}

impl NetworkPorts {
    /// Ports whose token request trusts `roots`, writing their lines to stdout.
    pub(crate) fn new(roots: &[CertificateDer<'static>]) -> NetworkPorts {
        NetworkPorts::writing_to(roots, Box::new(io::stdout()))
    }

    /// Ports whose token request trusts `roots`, writing their lines to `out`.
    pub(crate) fn writing_to(
        roots: &[CertificateDer<'static>],
        out: Box<dyn Write>,
    ) -> NetworkPorts {
        NetworkPorts {
            tokens: OidcTokens::from_environment(roots),
            network: Network,
            out,
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

    /// One line and its newline, flushed at once, so that the runner sees `::add-mask::` before
    /// anything that follows it. A line that cannot be written is dropped: the log is not the
    /// outcome, which goes to the job's output file.
    fn say(&mut self, line: &str) {
        drop(writeln!(self.out, "{line}").and_then(|()| self.out.flush()));
    }
}
