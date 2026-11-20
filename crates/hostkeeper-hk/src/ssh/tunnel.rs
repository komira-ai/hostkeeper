//! The byte stream: `Ssh/Tunnel` between this process's stdin and stdout and the host's
//! port 22 (docs/design/ssh-through-coordinator.md section 7).

use hostkeeper_proto::hostkeeper::operator::v1::{Close, Open, TunnelFrame, tunnel_frame};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Streaming;

use crate::ssh::client::{Client, from_status};
use crate::ssh::error::Error;

/// The most a data frame carries.
const FRAME: usize = 16 * 1024;

/// Frames waiting to be sent; with this bound a slow coordinator slows the reading of stdin.
const QUEUE: usize = 4;

fn frame(frame: tunnel_frame::Frame) -> TunnelFrame {
    TunnelFrame { frame: Some(frame) }
}

type Opened = (mpsc::Sender<TunnelFrame>, Streaming<TunnelFrame>);

/// One try at opening the tunnel to `host` for the certificate `serial`: the sending half and
/// the answers, after the coordinator's `opened`.
async fn try_open(client: &mut Client, host: &str, serial: u64) -> Result<Opened, Error> {
    let (sender, receiver) = mpsc::channel(QUEUE);
    let first = frame(tunnel_frame::Frame::Open(Open {
        host_id: host.to_owned(),
        cert_serial: serial,
    }));
    // The queue is empty and has room: this send cannot wait or fail.
    let _ = sender.try_send(first);
    let response = client
        .tunnel(ReceiverStream::new(receiver))
        .await
        .map_err(|status| from_status(&status))?;
    let mut answers = response.into_inner();
    match answers.message().await {
        Ok(Some(TunnelFrame {
            frame: Some(tunnel_frame::Frame::Opened(_)),
        })) => Ok((sender, answers)),
        Ok(_) => Err(Error::Unusable(
            "the coordinator's first answer is not `opened`".into(),
        )),
        Err(status) => Err(from_status(&status)),
    }
}

/// Opens the tunnel. An instance that is stopping refuses with `draining`; asking again reaches
/// another instance through Envoy, once, before any data has moved.
async fn open(client: &mut Client, host: &str, serial: u64) -> Result<Opened, Error> {
    match try_open(client, host, serial).await {
        Err(error) if error.refusal() == Some("draining") => try_open(client, host, serial).await,
        other => other,
    }
}

/// Relays `input` to the host and the host to `output` until the host closes the stream or
/// the coordinator ends it. End of `input` is sent as `Close{eof}` and the relay goes on until
/// the host closes (what `scp` and `rsync` need). Returns how the stream ended: `Ok` for the
/// host's close or an end after `input` did, an error when the coordinator closed it for
/// another reason.
pub async fn proxy(
    client: &mut Client,
    host: &str,
    serial: u64,
    mut input: Box<dyn AsyncRead + Unpin + Send>,
    output: &mut (dyn AsyncWrite + Unpin),
) -> Result<(), Error> {
    let (sender, mut answers) = open(client, host, serial).await?;
    let upstream = tokio::spawn(async move {
        let mut buffer = vec![0_u8; FRAME];
        loop {
            let read = input.read(&mut buffer).await.unwrap_or(0);
            let next = if read == 0 {
                frame(tunnel_frame::Frame::Close(Close {
                    cause: "eof".into(),
                }))
            } else {
                frame(tunnel_frame::Frame::Data(buffer[..read].to_vec()))
            };
            // A stream that has gone away ends the relay from the other half, which aborts this.
            let _ = sender.send(next).await;
            if read == 0 {
                break;
            }
        }
        // The sender stays alive until the relay ends: dropping it would end the request
        // stream, and with it the half-close the host's reply still needs.
        sender
    });
    let ended = loop {
        match answers.message().await {
            Ok(Some(TunnelFrame {
                frame: Some(tunnel_frame::Frame::Data(bytes)),
            })) => {
                let delivered = output.write_all(&bytes).await.and(output.flush().await);
                if delivered.is_err() {
                    break Ok(());
                }
            }
            Ok(Some(TunnelFrame {
                frame: Some(tunnel_frame::Frame::Close(Close { cause })),
            })) => {
                break match cause.as_str() {
                    "eof" | "host_closed" => Ok(()),
                    other => Err(Error::Unusable(format!("the tunnel was closed: {other}"))),
                };
            }
            Ok(Some(_)) => {}
            Ok(None) => break Ok(()),
            Err(status) => break Err(from_status(&status)),
        }
    };
    upstream.abort();
    ended
}
