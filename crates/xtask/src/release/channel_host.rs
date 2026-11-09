//! `release channel-host`: the host of the release channel's base URL, the name or address the
//! channel job of `release.yml` pings when it joins the tailnet before it reads the channel.
//!
//! The channel is given as the flag `--channel`, `https://<host>[:<port>][/<path>]` (the
//! repository variable `HOSTKEEPER_RELEASE_CHANNEL`). The host is a DNS name or an IPv4
//! address: labels of ASCII letters, digits and `-`, separated by single dots. The answer is
//! `hostname=<host>`, appended to `--output` (on Actions, `GITHUB_OUTPUT`) when given. Anything
//! else is refused, the reason named: another scheme, user info, an empty or bracketed host, a
//! port that is not 1 to 65535, a query or fragment straight after the authority, or a control
//! character or space anywhere, so the step fails before the join instead of pinging nothing.

use std::path::Path;

use super::{Error, answer};

/// What a refusal names as the expected form.
pub(crate) const FORM: &str = "https://<host>[:<port>][/<path>]";

/// The host of the channel's base URL `channel`, or why it has none.
pub(crate) fn host_of(channel: &str) -> Result<&str, String> {
    if channel.chars().any(|c| c.is_control() || c == ' ') {
        return Err("it holds a space or a control character".to_owned());
    }
    let Some(rest) = channel.strip_prefix("https://") else {
        return Err("it does not start with https://".to_owned());
    };
    let authority = rest
        .split_once('/')
        .map_or(rest, |(authority, _)| authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if let Some(port) = port {
        let digits = port.bytes().all(|b| b.is_ascii_digit());
        if !(digits && port.parse::<u16>().is_ok_and(|n| n > 0)) {
            return Err(format!("its port `{port}` is not a number from 1 to 65535"));
        }
    }
    if host.is_empty() {
        return Err("it names no host".to_owned());
    }
    let label_ok = |label: &str| {
        !label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    if !host.split('.').all(label_ok) {
        return Err(format!(
            "its host `{host}` is not a name or IPv4 address (letters, digits and `-`, in labels separated by single dots)"
        ));
    }
    Ok(host)
}

/// The verb: `hostname=<host>` of `channel`, appended to `output` when given.
pub(crate) fn channel_host(channel: &str, output: Option<&Path>) -> Result<Vec<String>, Error> {
    let host = host_of(channel).map_err(|why| {
        Error::Failed(format!(
            "the release channel {channel:?} is not {FORM}: {why}"
        ))
    })?;
    answer(output, vec![format!("hostname={host}")])
}

#[cfg(test)]
mod tests;
