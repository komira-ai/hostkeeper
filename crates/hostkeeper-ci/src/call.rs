//! One call of the Releases service, with a token of its own.

use crate::flags::Settings;
use crate::grpc::{Failure, Releases, Target};
use crate::ports::Ports;

/// What a call came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Answer<T> {
    /// The coordinator answered, with this reply.
    Reply(T),
    /// The coordinator refused, with this token (`[a-z0-9_]{1,64}`).
    Refused(String),
    /// Nothing was sent: no token, or a call that could not be made. The text says why.
    NotSent(String),
    /// A call was made and no answer came out of it. The text is the status the call ended with,
    /// with the token left out.
    Lost(String),
}

/// Whether `text` is a refusal token: 1 to 64 of `a-z 0-9 _`.
fn is_refusal_token(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

/// The token of a refusal: the message of a status of one of the four codes with which the
/// coordinator refuses, when the message is a refusal token.
fn refusal(status: &tonic::Status) -> Option<String> {
    matches!(
        status.code(),
        tonic::Code::InvalidArgument
            | tonic::Code::FailedPrecondition
            | tonic::Code::Unauthenticated
            | tonic::Code::PermissionDenied
    )
    .then(|| status.message())
    .filter(|message| is_refusal_token(message))
    .map(str::to_owned)
}

/// The text of a status that is no refusal: its code and its message, with `token` left out.
fn lost(status: &tonic::Status, token: &str) -> String {
    let message = status.message().replace(token, "<token>");
    if message.is_empty() {
        format!("{:?}", status.code())
    } else {
        format!("{:?}: {message}", status.code())
    }
}

/// Makes one call with `send`, on a new token.
///
/// The line `::add-mask::<token>` goes out first, so that the runner masks the token in every
/// log from then on. The token is kept in memory and in the call's metadata; it is written
/// nowhere.
pub(crate) fn call<T>(
    ports: &mut dyn Ports,
    settings: &Settings,
    send: impl FnOnce(&dyn Releases, &Target, &str) -> Result<T, Failure>,
) -> Answer<T> {
    let token = match ports.token(&settings.audience) {
        Ok(token) => token,
        Err(e) => return Answer::NotSent(format!("cannot get an OIDC token: {e}")),
    };
    ports.say(&format!("::add-mask::{token}"));
    let target = Target {
        endpoint: settings.endpoint.clone(),
        ca_file: settings.ca_file.clone(),
    };
    match send(ports.releases(), &target, &token) {
        Ok(reply) => Answer::Reply(reply),
        Err(Failure::NotSent(why)) => Answer::NotSent(why),
        Err(Failure::Status(status)) => {
            refusal(&status).map_or_else(|| Answer::Lost(lost(&status, &token)), Answer::Refused)
        }
    }
}
