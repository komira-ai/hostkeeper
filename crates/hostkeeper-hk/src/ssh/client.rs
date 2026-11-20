//! The channel to the coordinator's operator listener: TLS that trusts the CA file given and
//! nothing else, the operator's token as `authorization: Bearer <token>` on every call, and the
//! HTTP/2 keepalive a tunnel needs.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1::ssh_client::SshClient;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Uri};
use tonic::{Request, Status};

use crate::ssh::Coordinator;
use crate::ssh::error::Error;

/// Adds `authorization: Bearer <token>` to every request, marked sensitive.
#[derive(Clone)]
pub struct Bearer(MetadataValue<Ascii>);

impl Interceptor for Bearer {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        request
            .metadata_mut()
            .insert("authorization", self.0.clone());
        Ok(request)
    }
}

/// The `Ssh` client on its channel.
pub type Client = SshClient<InterceptedService<tonic::transport::Channel, Bearer>>;

fn read(path: &Path, what: &'static str) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|source| Error::Read {
        what,
        path: path.to_owned(),
        source,
    })
}

/// The token in `path`: one line of visible ASCII, in a file only its owner may read.
fn token(path: &Path) -> Result<MetadataValue<Ascii>, Error> {
    let mode = std::fs::metadata(path)
        .map_err(|source| Error::Read {
            what: "the token",
            path: path.to_owned(),
            source,
        })?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(Error::Usage(format!(
            "--token-file {} may be read by its group or others; make it 0600",
            path.display()
        )));
    }
    let text = String::from_utf8(read(path, "the token")?).unwrap_or_default();
    let text = text.strip_suffix('\n').unwrap_or(&text);
    // Visible ASCII is always a valid header value; the conversion is the second judge.
    let valid = !text.is_empty() && text.bytes().all(|b| b.is_ascii_graphic());
    let mut value = valid
        .then(|| MetadataValue::try_from(format!("Bearer {text}")).ok())
        .flatten()
        .ok_or_else(|| {
            Error::Usage("the token is empty or holds a character that is not visible ASCII".into())
        })?;
    value.set_sensitive(true);
    Ok(value)
}

/// The URI and TLS name of `endpoint`, which is `https://<host>[:<port>]` and nothing more.
fn https(endpoint: &str) -> Result<(Uri, String), Error> {
    let refuse = || Error::Usage("--endpoint takes https://<host>[:<port>]".into());
    let uri: Uri = endpoint.parse().map_err(|_| refuse())?;
    let authority = uri.authority().ok_or_else(refuse)?;
    let plain = uri.scheme_str() == Some("https")
        && !authority.as_str().contains('@')
        && uri.path() == "/"
        && uri.query().is_none()
        && !endpoint.ends_with('/');
    if !plain {
        return Err(refuse());
    }
    let host = authority.host().trim_matches(['[', ']']).to_owned();
    Ok((uri, host))
}

/// A client of the coordinator. The channel connects at its first call. A unary call is cut off
/// after the coordinator's `timeout_secs`; a tunnel is not (it is a stream), and a connection
/// that stays silent is probed every `keepalive_secs`.
pub fn connect(coordinator: &Coordinator, keepalive_secs: u64) -> Result<Client, Error> {
    let (uri, host) = https(&coordinator.endpoint)?;
    let ca = read(&coordinator.ca_file, "the CA")?;
    let bearer = Bearer(token(&coordinator.token_file)?);
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(ca))
        .domain_name(host);
    let keepalive = Duration::from_secs(keepalive_secs);
    let endpoint = Endpoint::from(uri)
        .connect_timeout(Duration::from_secs(coordinator.timeout_secs))
        .http2_keep_alive_interval(keepalive)
        .keep_alive_timeout(keepalive)
        .keep_alive_while_idle(true)
        .tls_config(tls)
        .map_err(|e| Error::Usage(format!("--ca-file is not a CA certificate: {e}")))?;
    Ok(SshClient::with_interceptor(endpoint.connect_lazy(), bearer))
}

/// `message` with the unary call's time limit.
pub fn unary<T>(message: T, coordinator: &Coordinator) -> Request<T> {
    let mut request = Request::new(message);
    request.set_timeout(Duration::from_secs(coordinator.timeout_secs));
    request
}

/// The error of a call that ended with `status`: the coordinator's refusal when its message is
/// one token of `SshRefusal` (lower case and underscores), else a failure to get an answer.
pub fn from_status(status: &Status) -> Error {
    let message = status.message();
    let token = !message.is_empty() && message.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    if token {
        Error::Refused(message.to_owned())
    } else {
        Error::Transport(format!("{:?}: {message}", status.code()))
    }
}
