//! The probe: the certificate chain a coordinator presents, read before anything is sent.
//!
//! It opens a TCP connection, runs the TLS handshake on it with a verifier that accepts every
//! chain, and drops the connection. No application data is sent on it, and nothing it learns is
//! trusted: it only gives the enroller the CA the chain ends in, to compare with the pinned
//! `ca_sha256`. Every call after it verifies the coordinator's certificate against that CA alone
//! (`grpc.rs`).

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};

/// A verifier that accepts every chain and every handshake signature. It is the probe's only:
/// the connection it verifies carries no application data.
#[derive(Debug)]
struct AcceptAny {
    schemes: Vec<SignatureScheme>,
}

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes.clone()
    }
}

/// The client configuration of the probe: the ring provider, no client certificate, and
/// [`AcceptAny`].
fn client_config() -> Result<ClientConfig, rustls::Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let schemes = provider
        .signature_verification_algorithms
        .supported_schemes();
    ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map(|builder| {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAny { schemes }))
                .with_no_client_auth()
        })
}

/// A TCP connection to `host:port`, to the first address that accepts one within `timeout`; each
/// read and write on it is bounded by `timeout` too. A resolver failure is a host that has no
/// address to try.
fn connect(host: &str, port: u16, timeout: Duration) -> io::Result<TcpStream> {
    let mut failure = io::Error::new(
        io::ErrorKind::NotFound,
        format!("{host} has no address that accepted a connection"),
    );
    for address in (host, port).to_socket_addrs().into_iter().flatten() {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => {
                return stream
                    .set_read_timeout(Some(timeout))
                    .and_then(|()| stream.set_write_timeout(Some(timeout)))
                    .map(|()| stream);
            }
            Err(error) => failure = error,
        }
    }
    Err(failure)
}

/// The certificates the coordinator at `host:port` presents, in the order it sends them (its own
/// first), after a TLS handshake for `name` that checks nothing.
pub(super) fn presented_chain(
    host: &str,
    port: u16,
    name: ServerName<'static>,
    timeout: Duration,
) -> io::Result<Vec<CertificateDer<'static>>> {
    chain_by(client_config(), host, port, name, timeout)
}

/// [`presented_chain`] with the result of making the client configuration given. The connection
/// is made from it before anything is dialled.
fn chain_by(
    config: Result<ClientConfig, rustls::Error>,
    host: &str,
    port: u16,
    name: ServerName<'static>,
    timeout: Duration,
) -> io::Result<Vec<CertificateDer<'static>>> {
    let mut connection = config
        .and_then(|config| ClientConnection::new(Arc::new(config), name))
        .map_err(io::Error::other)?;
    let mut stream = connect(host, port, timeout)?;
    while connection.is_handshaking() {
        connection.complete_io(&mut stream)?;
    }
    Ok(connection
        .peer_certificates()
        .map(<[CertificateDer<'static>]>::to_vec)
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches a client configuration that could not be made going unreported, or a coordinator
    /// being dialled with no configuration to speak TLS with: the error is the configuration's,
    /// and nothing is connected (the address is one nothing is listening on).
    #[test]
    fn a_client_configuration_that_cannot_be_made_is_reported_before_dialling() {
        let name = ServerName::try_from("127.0.0.1").expect("a name");
        let failure = rustls::Error::General("no configuration".to_owned());
        let expected = failure.to_string();
        let error = chain_by(Err(failure), "127.0.0.1", 1, name, Duration::from_secs(1))
            .expect_err("no configuration");
        assert_eq!(error.to_string(), expected);
    }
}
