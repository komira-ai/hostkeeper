//! The connection string of `--database-dsn-file` (`docs/design/coordinator-storage.md`,
//! section 8): one libpq connection string in keyword form, read from a secret file
//! ([`read_secret`]: a regular file, not a link, owned by the coordinator's user, mode 0600 or
//! 0400, at most [`MAX_DSN_BYTES`]). Nothing about the database is on the command line or in the
//! environment, and no error and no `Debug` shows the string or the file's path: an error names
//! the flag.
//!
//! The first build connects without TLS, so a string that asks for TLS (`sslmode` `require`,
//! `verify-ca` or `verify-full`) is refused rather than quietly downgraded. `disable` and
//! `prefer`, which the client's default is and which allows a connection without TLS, are
//! taken. The session's bounded waits ([`TIMEOUTS`]) are set here over whatever the string says:
//! a stalled database must not freeze the coordinator.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use hostkeeper_wire::secret::{SecretError, read_secret};
use postgres::config::SslMode;

/// The flag that names the file, which errors name instead of its path.
pub(crate) const FLAG: &str = "--database-dsn-file";

/// The most bytes the file holds.
pub(crate) const MAX_DSN_BYTES: u64 = 4096;

/// The application name a string that names none is given, which the server shows for the
/// coordinator's sessions.
pub(crate) const APPLICATION: &str = "hostkeeper-coordinator";

/// The bounded waits of every session (section 4.4). A wait that runs out loses the session:
/// a heartbeat waits at most one of them, once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Timeouts {
    /// How long a connect may take.
    pub(crate) connect: Duration,
    /// How long one statement may run (`statement_timeout`).
    pub(crate) statement: Duration,
    /// How long a statement may wait for a lock (`lock_timeout`), shorter than a statement.
    pub(crate) lock: Duration,
    /// How long sent data may stay unacknowledged before the connection is dropped
    /// (`tcp_user_timeout`), and the keepalives' idle time before the first probe.
    pub(crate) tcp: Duration,
}

/// The waits of the coordinator's sessions: each a few seconds.
pub(crate) const TIMEOUTS: Timeouts = Timeouts {
    connect: Duration::from_secs(5),
    statement: Duration::from_secs(5),
    lock: Duration::from_secs(3),
    tcp: Duration::from_secs(5),
};

/// Why the connection string is refused. The text names the flag, never the path or the string.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DsnError {
    /// The file breaks the rule of a secret file, or cannot be read.
    #[error(transparent)]
    File(#[from] SecretError),
    /// The file holds more than [`MAX_DSN_BYTES`].
    #[error("--database-dsn-file: it holds more than 4096 bytes")]
    TooLong,
    /// The file does not hold one connection string the client reads.
    #[error("--database-dsn-file: it does not hold one libpq connection string in keyword form")]
    Malformed,
    /// The string names no host.
    #[error("--database-dsn-file: the connection string names no host")]
    NoHost,
    /// The string asks for TLS, which this build does not connect with.
    #[error(
        "--database-dsn-file: the connection string asks for TLS (sslmode), which this build does not connect with; set sslmode=disable for a database on this machine's loopback"
    )]
    Tls,
}

/// A connection string, read and checked. Its `Debug` shows nothing of it.
#[derive(Clone)]
pub(crate) struct Dsn {
    config: postgres::Config,
}

impl fmt::Debug for Dsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Dsn(..)")
    }
}

impl Dsn {
    /// The connection string in the file at `path`.
    ///
    /// # Errors
    /// [`DsnError`]: the file breaks the secret file's rule, is too long, or holds no string
    /// this build connects with.
    pub(crate) fn read(path: &Path) -> Result<Dsn, DsnError> {
        let bytes = read_secret(path, FLAG, MAX_DSN_BYTES)?;
        if bytes.len() as u64 > MAX_DSN_BYTES {
            return Err(DsnError::TooLong);
        }
        let text = String::from_utf8(bytes).map_err(|_| DsnError::Malformed)?;
        Dsn::parse(&text)
    }

    /// The connection string `text`, surrounding whitespace dropped.
    ///
    /// # Errors
    /// [`DsnError`]: it does not parse, names no host, or asks for TLS.
    pub(crate) fn parse(text: &str) -> Result<Dsn, DsnError> {
        let text = text.trim();
        if text.is_empty() || text.contains('\n') {
            return Err(DsnError::Malformed);
        }
        let mut config: postgres::Config = text.parse().map_err(|_| DsnError::Malformed)?;
        if config.get_hosts().is_empty() {
            return Err(DsnError::NoHost);
        }
        if !matches!(config.get_ssl_mode(), SslMode::Disable | SslMode::Prefer) {
            return Err(DsnError::Tls);
        }
        if config.get_application_name().is_none() {
            config.application_name(APPLICATION);
        }
        Ok(Dsn { config })
    }

    /// The client's configuration, with the bounded waits of `timeouts` set over the string's.
    pub(crate) fn config(&self, timeouts: Timeouts) -> postgres::Config {
        let mut config = self.config.clone();
        config
            .ssl_mode(SslMode::Disable)
            .connect_timeout(timeouts.connect)
            .tcp_user_timeout(timeouts.tcp)
            .keepalives(true)
            .keepalives_idle(timeouts.tcp)
            .keepalives_interval(Duration::from_secs(1))
            .keepalives_retries(3)
            .options(&format!(
                "-c statement_timeout={} -c lock_timeout={} -c idle_in_transaction_session_timeout={}",
                timeouts.statement.as_millis(),
                timeouts.lock.as_millis(),
                timeouts.statement.as_millis(),
            ));
        config
    }

    /// The same string with its application name set to `name`: what a test finds the sessions
    /// of one coordinator by.
    #[cfg(test)]
    pub(crate) fn named(mut self, name: &str) -> Dsn {
        self.config.application_name(name);
        self
    }
}

#[cfg(test)]
#[path = "dsn_tests.rs"]
mod tests;
